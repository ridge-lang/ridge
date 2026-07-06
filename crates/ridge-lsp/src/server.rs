//! `RidgeLanguageServer` — `tower_lsp::LanguageServer` implementation.
//!
//! # Transport
//!
//! Stdio only.  The binary entry point in `main.rs` wires this via
//! `tower_lsp::Server::new(stdin, stdout, socket).serve(service)`.
//!
//! # Workspace lifecycle
//!
//! 1. `initialize`: read `rootUri` and every `workspaceFolders` entry, then walk
//!    each up to its nearest `[workspace]` manifest. Distinct manifest roots
//!    become one independent workspace apiece — its own retained engine and
//!    analysis index — so a multi-folder window with several Ridge projects
//!    analyses them all, routing each request to the workspace that owns the
//!    document. When no `[workspace]` manifest is found at or above any folder —
//!    or no folder is given — the server enters standalone mode and type-checks
//!    each open `.ridge` file on its own, so a loose file still gets full
//!    analysis.
//! 2. `textDocument/didChange`: debounce 250 ms, then recompile the edited modules
//!    against their editor buffers via the retained incremental engine.
//! 3. `textDocument/didSave`: reseed the engine from disk (no debounce).
//! 4. Diagnostics published via `client.publish_diagnostics(...)`.
//!
//! # Compile model
//!
//! The retained incremental engine lives behind a blocking mutex. Each compile
//! runs on `tokio::task::spawn_blocking`, locks the engine, applies the buffer
//! edits (or reseeds from disk), and builds a fresh `WorkspaceIndex`. The index
//! and diagnostics install only under a generation guard, so a slow compile that
//! a newer edit superseded clobbers nothing. Editor queries read the installed
//! index `Arc` and never touch the engine, so a recompile never blocks a hover.

// LSP server module-local stylistic allow: `significant_drop_tightening`
// (nursery) — the suggested rewrites push lock acquisitions into single
// expressions and lose visual clarity around "snapshot then act on snapshot"
// patterns; the lock holds are short.
#![allow(clippy::significant_drop_tightening)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tower_lsp::jsonrpc::Result as LspResult;
// LSP server uses 25+ types from `tower_lsp::lsp_types`; an explicit `use`
// list churns on every protocol revision.  Wildcard import is the idiomatic
// pattern in `tower-lsp`-based servers.
#[allow(clippy::wildcard_imports)]
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use ridge_driver::{
    check_standalone_incremental, check_workspace_incremental, collect_diagnostics, CheckError,
    CheckOptions, IncrementalState,
};
use ridge_lexer::LineIndex;
use ridge_manifest::find_workspace_root;
use ridge_resolve::ModuleId;

use crate::cancel::{Cancel, CancelOnDrop};
use crate::diagnostics::{source_id_to_uri, to_lsp_diagnostic, uri_key};
use crate::index::{
    collect_capability_fixes, collect_nesting_hints, collect_syntax_fixes, collect_uncurry_fixes,
    diff_tokens, CodeLensConfig, WorkspaceIndex,
};

/// A workspace's retained incremental engine, shared between the state snapshot
/// and the `spawn_blocking` compile task that owns it for the compile's duration.
type SharedEngine = Arc<StdMutex<Option<IncrementalState>>>;

/// One unit of work for a compile pass: the target workspace's slot index (a
/// stable handle into [`WorkspaceSnapshot::workspaces`]), its engine, and what to
/// compile. Snapshotted under the state lock, then handed to a blocking thread.
type CompileJob = (usize, SharedEngine, CompileTarget);

/// Command id for the `workspace/executeCommand` bridge that applies a quick-fix
/// edit on clients that can't accept a `CodeAction` literal (see [`ClientCaps`]).
const APPLY_EDIT_COMMAND: &str = "ridge.applyWorkspaceEdit";

// ── ClientCaps ────────────────────────────────────────────────────────────────

/// The subset of client capabilities that decide how individual responses are
/// encoded. The protocol lets a server send richer forms — Markdown content,
/// parameter label offsets, a hierarchical outline, `CodeAction` literals — only
/// when the client advertised support; otherwise the server must fall back to the
/// plainer form the client can actually render. These gates are read once at
/// `initialize` and consulted by the handlers that have a choice of encoding.
//
// Each gate is an independent, orthogonal capability, so a struct of named bools
// is the clearest representation.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
struct ClientCaps {
    /// `signatureHelp.signatureInformation.parameterInformation.labelOffsetSupport`.
    /// When false, each parameter label is sent as the substring it covers rather
    /// than the `[start, end)` offset pair the client could not map onto the label.
    sig_label_offsets: bool,
    /// `textDocument.documentSymbol.hierarchicalDocumentSymbolSupport`. When false,
    /// the outline is flattened to `SymbolInformation[]` with each member's
    /// container named, since the client cannot render a nested tree.
    hierarchical_symbols: bool,
    /// `textDocument.codeAction.codeActionLiteralSupport`. When false, a quick-fix
    /// is delivered as a `Command` that applies its edit through
    /// `workspace/executeCommand`, because the client cannot accept a `CodeAction`
    /// literal carrying a `WorkspaceEdit`.
    code_action_literals: bool,
    /// Whether `textDocument.hover.contentFormat` admits Markdown. Defaults to
    /// true: hover stays Markdown unless the client lists formats without it, in
    /// which case the card is sent as plain text.
    hover_markdown: bool,
    /// Whether `textDocument.completion.completionItem.documentationFormat` admits
    /// Markdown, with the same default and fallback as `hover_markdown`.
    completion_doc_markdown: bool,
    /// `textDocument.documentLink.tooltipSupport`. When false, document links are
    /// sent without their tooltip, which only a supporting client would show.
    document_link_tooltip: bool,
}

impl Default for ClientCaps {
    fn default() -> Self {
        // Conservative for the forms a non-supporting client would mis-render
        // (offsets, nested outlines, action literals); lenient for Markdown, which
        // every mainstream client renders and which only a client that explicitly
        // asks for plain text turns off.
        Self {
            sig_label_offsets: false,
            hierarchical_symbols: false,
            code_action_literals: false,
            hover_markdown: true,
            completion_doc_markdown: true,
            document_link_tooltip: false,
        }
    }
}

/// Read the response-shaping gates from the client's advertised capabilities.
fn negotiate_client_caps(caps: &ClientCapabilities) -> ClientCaps {
    let td = caps.text_document.as_ref();
    // An explicit, non-empty content-format list that omits Markdown means plain
    // text only; an absent or empty list keeps Markdown, which every mainstream
    // client renders.
    let admits_markdown = |formats: Option<&[MarkupKind]>| match formats {
        Some(f) if !f.is_empty() => f.contains(&MarkupKind::Markdown),
        _ => true,
    };
    ClientCaps {
        sig_label_offsets: td
            .and_then(|t| t.signature_help.as_ref())
            .and_then(|s| s.signature_information.as_ref())
            .and_then(|s| s.parameter_information.as_ref())
            .and_then(|p| p.label_offset_support)
            .unwrap_or(false),
        hierarchical_symbols: td
            .and_then(|t| t.document_symbol.as_ref())
            .and_then(|d| d.hierarchical_document_symbol_support)
            .unwrap_or(false),
        code_action_literals: td
            .and_then(|t| t.code_action.as_ref())
            .and_then(|c| c.code_action_literal_support.as_ref())
            .is_some(),
        hover_markdown: admits_markdown(
            td.and_then(|t| t.hover.as_ref())
                .and_then(|h| h.content_format.as_deref()),
        ),
        completion_doc_markdown: admits_markdown(
            td.and_then(|t| t.completion.as_ref())
                .and_then(|c| c.completion_item.as_ref())
                .and_then(|i| i.documentation_format.as_deref()),
        ),
        document_link_tooltip: td
            .and_then(|t| t.document_link.as_ref())
            .and_then(|d| d.tooltip_support)
            .unwrap_or(false),
    }
}

/// Locate the `codeLens` settings object inside a client-supplied blob.
///
/// Accepts it either at the root — matching `initializationOptions.codeLens` —
/// or nested under a `ridge` section, which is how an editor that namespaces its
/// settings delivers them in `workspace/didChangeConfiguration`. Returns `None`
/// when neither is present, so a caller can tell "no opinion" apart from "all
/// lenses off".
fn locate_code_lens_settings(root: &serde_json::Value) -> Option<&serde_json::Value> {
    root.get("codeLens")
        .or_else(|| root.get("ridge").and_then(|r| r.get("codeLens")))
}

/// Read the opt-in code-lens flags from a `codeLens` settings object.
///
/// Shape: `{ "references": true, "implementations": true, "run": true,
/// "runTest": true }`. Each flag defaults to `false`, so a flag the client omits
/// — or a client that doesn't register the lens commands — is treated as off.
fn parse_code_lens_flags(code_lens: &serde_json::Value) -> CodeLensConfig {
    let flag = |name: &str| {
        code_lens
            .get(name)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    CodeLensConfig {
        references: flag("references"),
        implementations: flag("implementations"),
        run: flag("run"),
        run_test: flag("runTest"),
    }
}

// ── WorkspaceSnapshot ─────────────────────────────────────────────────────────

/// In-memory state of the LSP workspace.
///
/// Held behind `Arc<Mutex<…>>` and shared between the `initialize` handler and
/// the compile task.
// Each flag tracks an independent, orthogonal piece of session state (mode,
// warn-once latches, client-capability gates); a struct of named bools is the
// clearest representation.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default)]
struct WorkspaceSnapshot {
    /// Every independent Ridge workspace the client opened — one per distinct
    /// `[workspace]` manifest root — each with its own engine and analysis index.
    /// In standalone mode this holds exactly one workspace whose root is `None`.
    /// Empty until `initialize` is handled. Fixed for the session: the order and
    /// membership never change after `initialize`, so a slot index is a stable
    /// handle for installing a compile result.
    workspaces: Vec<Workspace>,
    /// Open document contents keyed by `Url` (LSP file URI).
    open_docs: std::collections::HashMap<Url, String>,
    /// Set of file URIs for which we've already emitted the L803 orphan warning.
    /// Reserved for future use (0.2.0 orphan-file warn-once logic).
    #[allow(dead_code)]
    warned_orphan: HashSet<String>,
    /// True when no `[workspace]` manifest was found at or above any opened
    /// folder (or no folder was given at all). In this mode the single workspace
    /// type-checks each open `.ridge` file on its own, so a loose file still gets
    /// diagnostics, hover, and navigation. Mutually exclusive with a real
    /// manifest root being present.
    standalone: bool,
    /// True when the client advertised dynamic registration for type hierarchy.
    /// lsp-types 0.94 has no static `typeHierarchyProvider` server capability,
    /// so `textDocument/prepareTypeHierarchy` is registered dynamically in
    /// `initialized` when — and only when — the client supports it.
    supports_type_hierarchy: bool,
    /// True when the client advertised dynamic registration for file watching.
    /// `workspace/didChangeWatchedFiles` is a client-driven watch the server
    /// opts into via dynamic registration in `initialized`; without it the
    /// client never reports on-disk changes to files that aren't open.
    supports_watched_files: bool,
    /// True when the client advertised support for work-done progress
    /// (`window.workDoneProgress`). Server-initiated `$/progress` reporting
    /// around a reseed compile is gated on this; without it the server stays
    /// silent rather than emitting progress tokens the client would reject.
    supports_work_done_progress: bool,
    /// True when the client supports pull diagnostics (`textDocument.diagnostic`)
    /// *and* a server-initiated refresh (`workspace.diagnostics.refreshSupport`).
    /// In that mode the server advertises a diagnostic provider, answers
    /// `textDocument/diagnostic` and `workspace/diagnostic` from `last_diagnostics`,
    /// and asks the client to re-pull after each compile instead of pushing.
    /// Refresh is required because pull mode stops pushing: a change the user did
    /// not type into the focused file — an on-disk edit, or an error surfacing in
    /// another module — would otherwise leave stale diagnostics on screen. A
    /// client missing either half stays on the push model, which is unchanged.
    supports_pull_diagnostics: bool,
    /// True when the client supports `workspace/codeLens/refresh`
    /// (`workspace.codeLens.refreshSupport`). A live `workspace/didChangeConfiguration`
    /// that flips a code-lens flag asks the client to re-query lenses through this;
    /// without it the server updates its config silently and the change shows on
    /// the next lens query the client makes on its own.
    supports_code_lens_refresh: bool,
    /// How the client wants individual responses encoded (Markdown, label
    /// offsets, hierarchical outline, action literals). Read once at `initialize`
    /// so handlers can fall back to a form a non-supporting client can render.
    client_caps: ClientCaps,
    /// File URIs edited since the last compile. Drained by the debounced
    /// incremental compile so a burst of edits across files is applied together.
    dirty: HashSet<Url>,
    /// The diagnostics from the most recent compile, keyed by document URI, so the
    /// pull handlers can answer without recompiling. Only maintained in pull mode;
    /// the push path delivers straight through `publish_diagnostics` and never
    /// reads this map.
    last_diagnostics: HashMap<Url, Vec<Diagnostic>>,
    /// Which code lenses the client wants shown. Seeded from
    /// `initializationOptions.codeLens` and updated live by
    /// `workspace/didChangeConfiguration`. All-off for a client that didn't opt
    /// in, so a generic editor is served no lenses whose commands it couldn't act
    /// on. Whether the provider is advertised at all is a separate, fixed decision
    /// made at `initialize` (see `code_lens_opted_in` there).
    code_lens_config: CodeLensConfig,
    /// The most recent semantic-token stream returned per document, keyed by the
    /// normalization-stable `uri_key`, together with the `result_id` stamped on
    /// it. A follow-up `semanticTokens/full/delta` diffs the fresh stream against
    /// the cached one when the client's `previousResultId` matches; a mismatch (a
    /// closed document, a server that never served a full result) falls back to a
    /// full reply. Only the latest result per document is kept — clients always
    /// delta against the id they last received.
    semantic_tokens: HashMap<String, CachedSemanticTokens>,
    /// Monotonic source of semantic-token `result_id`s for the session.
    semantic_tokens_seq: u64,
}

/// One document's last-served semantic-token stream and the id stamped on it,
/// the base a `semanticTokens/full/delta` request computes its edits against.
#[derive(Debug)]
struct CachedSemanticTokens {
    result_id: String,
    data: Vec<SemanticToken>,
}

/// One independent Ridge workspace: a manifest root (or the synthetic standalone
/// project), its retained incremental engine, and its latest analysis index.
///
/// A multi-folder window (e.g. VS Code "Add Folder to Workspace") with several
/// Ridge projects gets one `Workspace` per `[workspace]` manifest root. Each
/// compiles and is queried on its own, so names never leak between unrelated
/// projects, and a request routes to the workspace that owns the document.
struct Workspace {
    /// The directory holding this workspace's root `ridge.toml` with a
    /// `[workspace]` table. `None` in standalone mode, where the project is
    /// synthesised from the open `.ridge` files instead.
    root: Option<PathBuf>,
    /// This workspace's retained incremental engine (see the note on
    /// [`RidgeLanguageServer`]'s former single engine). Reseeded on a full
    /// compile, recompiled in place on an edit; held behind a blocking mutex so
    /// the `spawn_blocking` compile task owns it without moving it out.
    engine: SharedEngine,
    /// This workspace's most recent completed analysis, or `None` until its first
    /// compile lands. Reads clone the `Arc` and release the lock before querying.
    index: Option<Arc<WorkspaceIndex>>,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The engine is large and not `Debug`; report only the identifying bits.
        f.debug_struct("Workspace")
            .field("root", &self.root)
            .field("has_index", &self.index.is_some())
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// A fresh workspace for `root` (`None` for standalone), with an unseeded
    /// engine and no analysis yet.
    fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            engine: Arc::new(StdMutex::new(None)),
            index: None,
        }
    }

    /// The compile target for this workspace: a real manifest root, or the
    /// standalone file set derived from the current open documents. Returns
    /// `None` for a standalone workspace with no open `.ridge` files, where there
    /// is nothing to analyse yet.
    fn target(&self, open_docs: &HashMap<Url, String>) -> Option<CompileTarget> {
        let Some(root) = &self.root else {
            // Standalone: nothing to analyse until at least one `.ridge` is open.
            let files = standalone_files(open_docs);
            return (!files.is_empty()).then_some(CompileTarget::Standalone(files));
        };
        Some(CompileTarget::Workspace(root.clone()))
    }
}

// ── RidgeLanguageServer ───────────────────────────────────────────────────────

/// The Ridge Language Server.
///
/// Implements [`tower_lsp::LanguageServer`] over stdio transport.
pub struct RidgeLanguageServer {
    client: Client,
    state: Arc<Mutex<WorkspaceSnapshot>>,
    /// Handle to the in-flight compile task (if any).  Guarded separately so
    /// the debounce timer can abort it without holding the state lock.
    compile_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Handle to the debounce timer task.
    debounce_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    /// Monotonic compile-generation counter. Each compile claims a fresh value;
    /// the installer only swaps in a result whose generation beats the one
    /// already stored, so a slow aborted compile cannot clobber a newer one.
    compile_generation: Arc<AtomicU64>,
    /// Monotonic counter for unique work-done progress tokens. Each reseed
    /// compile that reports progress claims a fresh token, so two compiles that
    /// briefly overlap (a newer one aborting an older) never share a token and
    /// each `end` matches its own `begin`.
    progress_counter: Arc<AtomicU64>,
}

impl RidgeLanguageServer {
    /// Create a new server bound to the given LSP client.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(Mutex::new(WorkspaceSnapshot::default())),
            compile_handle: Arc::new(Mutex::new(None)),
            debounce_handle: Arc::new(Mutex::new(None)),
            compile_generation: Arc::new(AtomicU64::new(0)),
            progress_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Return the analysis index of the first workspace that has compiled.
    ///
    /// With a single workspace open (the common case) this is that workspace's
    /// index. It is the entry point for tests and any caller that has no specific
    /// document in hand; document-keyed requests use [`index_for_uri`] instead so
    /// they route to the workspace that actually owns the file.
    ///
    /// Clones the `Arc` under a short lock and releases the lock before
    /// returning, so a query never holds the state mutex while it reads the
    /// index.
    ///
    /// [`index_for_uri`]: Self::index_for_uri
    #[must_use]
    pub async fn workspace_index(&self) -> Option<Arc<WorkspaceIndex>> {
        let snap = self.state.lock().await;
        snap.workspaces.iter().find_map(|ws| ws.index.clone())
    }

    /// Return the analysis index of the workspace that owns `uri`, if any.
    ///
    /// Each open workspace has its own index; a document belongs to exactly one
    /// of them. Routing through [`WorkspaceIndex::contains_uri`] keeps the
    /// per-workspace `ModuleId` numbering honest — a request never resolves
    /// against an index whose module ids mean something else. Returns `None` when
    /// no compiled workspace owns the document (e.g. a freshly opened file before
    /// its first compile), exactly as the single-index path did. This is the
    /// read-path primitive for hover, go-to-definition, and completion.
    pub async fn index_for_uri(&self, uri: &Url) -> Option<Arc<WorkspaceIndex>> {
        let snap = self.state.lock().await;
        snap.workspaces.iter().find_map(|ws| {
            ws.index
                .as_ref()
                .filter(|idx| idx.contains_uri(uri))
                .cloned()
        })
    }

    /// The negotiated client-capability gates (see [`ClientCaps`]). Copied out of
    /// the snapshot so a handler can pick a response encoding without holding the
    /// state lock across its own work.
    async fn client_caps(&self) -> ClientCaps {
        self.state.lock().await.client_caps
    }

    /// Stamp a fresh `result_id` on a document's semantic-token stream, cache the
    /// stream under it so a later `semanticTokens/full/delta` can diff against it,
    /// and return the id.
    async fn cache_semantic_tokens(&self, uri: &Url, data: Vec<SemanticToken>) -> String {
        let mut snap = self.state.lock().await;
        snap.semantic_tokens_seq += 1;
        let result_id = snap.semantic_tokens_seq.to_string();
        snap.semantic_tokens.insert(
            uri_key(uri),
            CachedSemanticTokens {
                result_id: result_id.clone(),
                data,
            },
        );
        result_id
    }

    /// Every compiled workspace's index, in workspace order. Used by the
    /// workspace-wide requests (`workspace/symbol`, file renames) that span all
    /// open projects rather than a single document.
    async fn all_indices(&self) -> Vec<Arc<WorkspaceIndex>> {
        let snap = self.state.lock().await;
        snap.workspaces
            .iter()
            .filter_map(|ws| ws.index.clone())
            .collect()
    }

    /// Run a compile across the open workspaces and publish diagnostics.
    ///
    /// `reseed` forces a fresh full check from disk; otherwise each retained
    /// incremental engine is reused (seeded on first use). `edits` are
    /// `(uri, buffer)` pairs applied before each result is built, so diagnostics
    /// and the analysis index reflect the editor's buffers rather than stale disk
    /// text; an edit is a no-op for a workspace that does not own its document.
    ///
    /// A reseed recompiles every workspace. An incremental compile only touches
    /// the workspaces that currently own an edited document, so typing in one
    /// project never recompiles an unrelated one. Each workspace's heavy work
    /// runs on its own blocking thread and installs into its own slot under the
    /// shared generation guard, so a slow compile superseded by a newer one is
    /// discarded.
    async fn run_compile(&self, reseed: bool, edits: Vec<(Url, String)>) {
        let compile_handle_arc = Arc::clone(&self.compile_handle);
        {
            let mut ch = compile_handle_arc.lock().await;
            if let Some(handle) = ch.take() {
                handle.abort();
            }
        }

        // Select the workspaces to compile and snapshot each one's engine and
        // target. A reseed takes them all; an incremental compile takes only the
        // ones whose current index owns an edited document.
        let jobs: Vec<CompileJob> = {
            let snap = self.state.lock().await;
            snap.workspaces
                .iter()
                .enumerate()
                .filter(|(_, ws)| {
                    reseed
                        || ws
                            .index
                            .as_ref()
                            .is_some_and(|idx| edits.iter().any(|(uri, _)| idx.contains_uri(uri)))
                })
                .filter_map(|(slot, ws)| {
                    ws.target(&snap.open_docs)
                        .map(|target| (slot, Arc::clone(&ws.engine), target))
                })
                .collect()
        };
        if jobs.is_empty() {
            return;
        }

        // Surface a work-done progress indicator for reseed compiles (initial
        // load, save, on-disk refresh) when the client supports it. Incremental
        // recompiles while typing are fast and frequent, so they stay silent to
        // keep a spinner from flickering on every keystroke.
        let progress_token = if reseed {
            let supports = {
                let snap = self.state.lock().await;
                snap.supports_work_done_progress
            };
            supports.then(|| {
                ProgressToken::String(format!(
                    "ridge/index/{}",
                    self.progress_counter.fetch_add(1, Ordering::Relaxed)
                ))
            })
        } else {
            None
        };

        let gen_counter = Arc::clone(&self.compile_generation);
        let state_for_install = Arc::clone(&self.state);
        let client = self.client.clone();
        // Shared across the per-workspace compiles, which each read but never
        // mutate the buffer overlay.
        let edits = Arc::new(edits);

        let handle = tokio::spawn(async move {
            // Hold the progress guard for the whole batch: it ends the indicator
            // on drop, so even when a newer compile aborts this task mid-loop the
            // client's spinner is cleared rather than left hanging.
            let _progress = match progress_token {
                Some(token) => IndexingProgress::begin(client.clone(), token).await,
                None => None,
            };

            // Accumulate every installed workspace's diagnostics and deliver them
            // in one batch after the loop, so the pull model sends a single
            // refresh for the whole compile rather than one per workspace.
            let mut delivered: Vec<(Url, Vec<Diagnostic>)> = Vec::new();
            for (slot, engine, target) in jobs {
                let gen_counter = Arc::clone(&gen_counter);
                let edits = Arc::clone(&edits);
                let result = tokio::task::spawn_blocking(move || {
                    compile_blocking(&engine, &gen_counter, &target, reseed, edits.as_slice())
                })
                .await;

                match result {
                    Err(_join_err) => {} // aborted or panicked; discard
                    Ok(Err(check_err)) => {
                        tracing::error!("L804 LspInternal: driver fatal error: {check_err}");
                    }
                    Ok(Ok(out)) => {
                        // Install into this workspace's slot and deliver only if
                        // the result is still the newest for that slot — gated on
                        // the generation so a superseded result clobbers nothing.
                        let install = {
                            let mut snap = state_for_install.lock().await;
                            match snap.workspaces.get_mut(slot) {
                                Some(ws)
                                    if ws.index.as_ref().is_none_or(|existing| {
                                        out.generation > existing.generation
                                    }) =>
                                {
                                    ws.index = Some(Arc::clone(&out.index));
                                    true
                                }
                                _ => false,
                            }
                        };
                        if install {
                            delivered.extend(out.diagnostics_by_file);
                        }
                    }
                }
            }
            deliver_diagnostics(&client, &state_for_install, delivered).await;
        });

        let mut ch = compile_handle_arc.lock().await;
        *ch = Some(handle);
    }

    /// Reseed the engine from disk and recompile against every open buffer.
    /// Used on open and save, where the on-disk state is authoritative.
    async fn trigger_compile(&self) {
        let edits: Vec<(Url, String)> = {
            let snap = self.state.lock().await;
            snap.open_docs
                .iter()
                .map(|(uri, text)| (uri.clone(), text.clone()))
                .collect()
        };
        self.run_compile(true, edits).await;
    }

    /// Drain the dirty set and incrementally recompile those files' buffers.
    async fn flush_dirty_compile(&self) {
        let edits: Vec<(Url, String)> = {
            let mut guard = self.state.lock().await;
            let WorkspaceSnapshot {
                dirty, open_docs, ..
            } = &mut *guard;
            dirty
                .drain()
                .filter_map(|uri| open_docs.get(&uri).map(|text| (uri, text.clone())))
                .collect()
        };
        if edits.is_empty() {
            return;
        }
        self.run_compile(false, edits).await;
    }

    /// Schedule a debounced incremental compile (250 ms delay).
    ///
    /// Cancels any pending debounce timer and restarts it, so a burst of
    /// `didChange` notifications collapses into one recompile of the dirty set.
    async fn schedule_debounced_compile(&self) {
        let debounce_arc = Arc::clone(&self.debounce_handle);
        {
            let mut dh = debounce_arc.lock().await;
            if let Some(handle) = dh.take() {
                handle.abort();
            }
        }

        let self_clone = Self {
            client: self.client.clone(),
            state: Arc::clone(&self.state),
            compile_handle: Arc::clone(&self.compile_handle),
            debounce_handle: Arc::clone(&debounce_arc),
            compile_generation: Arc::clone(&self.compile_generation),
            progress_counter: Arc::clone(&self.progress_counter),
        };

        let handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
            self_clone.flush_dirty_compile().await;
        });

        let mut dh = debounce_arc.lock().await;
        *dh = Some(handle);
    }
}

// ── Compile helpers (run off the async runtime) ───────────────────────────────

/// One compile's product: the analysis index plus diagnostics bucketed by file,
/// ready to install and publish under the generation guard.
struct CompileOutput {
    generation: u64,
    index: Arc<WorkspaceIndex>,
    diagnostics_by_file: Vec<(Url, Vec<Diagnostic>)>,
}

/// What a compile analyses: a real on-disk workspace, or a set of standalone
/// files that live outside any workspace manifest.
enum CompileTarget {
    /// The directory holding the root `ridge.toml` with a `[workspace]` table.
    Workspace(PathBuf),
    /// Open `.ridge` files analysed individually, each as its own project.
    Standalone(Vec<PathBuf>),
}

impl CompileTarget {
    /// A best-effort root path for error reporting when no engine is available.
    fn root_hint(&self) -> PathBuf {
        match self {
            Self::Workspace(root) => root.clone(),
            Self::Standalone(files) => files
                .first()
                .and_then(|f| f.parent())
                .map_or_else(|| PathBuf::from("."), Path::to_owned),
        }
    }
}

/// The open `.ridge` documents, as a sorted, deduplicated list of file paths.
///
/// Sorted so the synthesised project/module ids stay stable across reseeds for
/// the same file set; non-`file:` URIs and non-`.ridge` documents are dropped.
fn standalone_files(open_docs: &HashMap<Url, String>) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = open_docs
        .keys()
        .filter_map(|uri| uri.to_file_path().ok())
        .filter(|p| p.extension().is_some_and(|e| e == "ridge"))
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Whether a watched URI is one the server reacts to: a `.ridge` source or a
/// `ridge.toml` manifest. Other paths the client may report are ignored.
fn is_watched_ridge_path(uri: &Url) -> bool {
    let Ok(path) = uri.to_file_path() else {
        return false;
    };
    // `.ridge` is matched case-sensitively, per R003.
    path.extension().is_some_and(|e| e == "ridge")
        || path.file_name().is_some_and(|n| n == "ridge.toml")
}

/// Whether a watched URI is a `.ridge` source file (not a manifest).
fn is_ridge_source(uri: &Url) -> bool {
    uri.to_file_path()
        .is_ok_and(|path| path.extension().is_some_and(|e| e == "ridge"))
}

/// RAII reporter for one server-initiated work-done progress.
///
/// [`begin`](IndexingProgress::begin) asks the client to create the progress
/// token (`window/workDoneProgress/create`) and sends the `begin` notification;
/// it returns `None` if the client refuses the create request, so an unpaired
/// `begin`/`end` is never emitted. The matching `end` is sent on drop — including
/// when a newer compile aborts the task mid-flight — so the client's progress
/// indicator always clears.
struct IndexingProgress {
    client: Client,
    token: ProgressToken,
}

impl IndexingProgress {
    /// Create the progress token and announce its start. Returns `None` when the
    /// client rejects the create request, leaving nothing to end.
    async fn begin(client: Client, token: ProgressToken) -> Option<Self> {
        client
            .send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                token: token.clone(),
            })
            .await
            .ok()?;
        client
            .send_notification::<notification::Progress>(ProgressParams {
                token: token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title: "Ridge: analyzing".to_owned(),
                        cancellable: Some(false),
                        message: None,
                        percentage: None,
                    },
                )),
            })
            .await;
        Some(Self { client, token })
    }
}

impl Drop for IndexingProgress {
    fn drop(&mut self) {
        // `end` is async, but `Drop` is not; spawn it on the current runtime so
        // the indicator clears even when the compile task is aborted. Use
        // `try_current` rather than `tokio::spawn` so a drop outside a runtime
        // (e.g. teardown) is a no-op instead of a panic.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let client = self.client.clone();
        let token = self.token.clone();
        handle.spawn(async move {
            client
                .send_notification::<notification::Progress>(ProgressParams {
                    token,
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                        WorkDoneProgressEnd { message: None },
                    )),
                })
                .await;
        });
    }
}

/// `$/progress` notification carrying a partial result.
///
/// lsp-types' typed `$/progress` ([`notification::Progress`]) only models
/// work-done progress, but partial results stream the raw result array — the
/// value the client appends to what it already holds. This thin notification
/// carries that free-form payload under the request's `partialResultToken`.
enum PartialResultProgress {}

impl notification::Notification for PartialResultProgress {
    type Params = serde_json::Value;
    const METHOD: &'static str = "$/progress";
}

/// Results per partial-result `$/progress` chunk. Small enough that a large
/// find-references fills the panel in visible steps, large enough to keep the
/// notification count modest.
const PARTIAL_RESULT_CHUNK: usize = 64;

/// Stream `values` to the client under `token` as partial-result `$/progress`
/// notifications, [`PARTIAL_RESULT_CHUNK`] items at a time. Each notification is
/// an array the client appends to the results it already has, so a large
/// find-references or symbol search fills in progressively and no single message
/// has to carry the whole set. Returns once every chunk is queued, so the empty
/// final response the caller sends next lands after them on the wire.
///
/// The reference/symbol scans run entirely in memory, so this chunks the
/// finished result rather than threading a sink through the index — the same
/// wire behaviour with far less surface.
async fn stream_partial_results(
    client: &Client,
    token: &ProgressToken,
    values: &[serde_json::Value],
) {
    for chunk in values.chunks(PARTIAL_RESULT_CHUNK) {
        let mut params = serde_json::Map::new();
        params.insert(
            "token".to_owned(),
            serde_json::to_value(token).unwrap_or(serde_json::Value::Null),
        );
        params.insert("value".to_owned(), serde_json::Value::Array(chunk.to_vec()));
        client
            .send_notification::<PartialResultProgress>(serde_json::Value::Object(params))
            .await;
    }
}

/// Seed-or-reuse the engine, apply the buffer edits, and produce the index and
/// diagnostics. Holds the engine mutex for the whole call, so concurrent
/// compiles serialise on the shared engine; the generation is claimed inside
/// that lock so its order matches the order edits were applied.
fn compile_blocking(
    engine: &StdMutex<Option<IncrementalState>>,
    gen_counter: &AtomicU64,
    target: &CompileTarget,
    reseed: bool,
    edits: &[(Url, String)],
) -> Result<CompileOutput, CheckError> {
    let mut guard = engine
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reseed || guard.is_none() {
        let seeded = match target {
            CompileTarget::Workspace(root) => {
                let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
                check_workspace_incremental(opts)?
            }
            CompileTarget::Standalone(files) => check_standalone_incremental(files),
        };
        *guard = Some(seeded);
    }
    let Some(state) = guard.as_mut() else {
        return Err(CheckError::NoWorkspaceRoot {
            path: target.root_hint(),
        });
    };

    for (uri, buffer) in edits {
        if let Some(mid) = module_for_uri(state, uri) {
            state.recompile(mid, buffer);
        }
    }

    let generation = gen_counter.fetch_add(1, Ordering::SeqCst) + 1;
    // Diagnostics' source ids resolve against the graph's own root — the
    // workspace dir in workspace mode, or the synthetic root in standalone mode.
    let workspace_root = state.resolved.graph.root.clone();
    let sources = state.source_cache();
    let structured = collect_diagnostics(
        &state.disc_resolve_errors,
        &state.resolved,
        &state.type_errors,
        &sources,
    );
    let mut index = WorkspaceIndex::build(generation, &state.typed, &state.resolved, &sources);
    index.capability_fixes = collect_capability_fixes(
        &index.line_indices,
        &index.module_uris,
        &state.typed,
        &state.type_errors,
    );
    index.syntax_fixes = collect_syntax_fixes(
        &index.line_indices,
        &index.module_uris,
        &state.resolved.parse_errors,
    );
    index.syntax_fixes.extend(collect_uncurry_fixes(
        &index.line_indices,
        &index.module_uris,
        &index.module_text,
        &state.typed,
        &state.type_errors,
    ));
    let index = Arc::new(index);

    // Pre-populate every module's URI so a now-clean file gets its stale
    // diagnostics cleared, then bucket the current diagnostics by file.
    let mut by_file: HashMap<Url, Vec<Diagnostic>> = index
        .uri_to_module
        .keys()
        .map(|uri| (uri.clone(), Vec::new()))
        .collect();
    for diag in &structured {
        let source_key = diag.source_id.as_str();
        let uri = source_id_to_uri(&workspace_root, source_key);
        let src_text = sources.text(source_key);
        let lsp_diag = to_lsp_diagnostic(diag, &uri, src_text);
        by_file.entry(uri).or_default().push(lsp_diag);
    }

    // Style hints computed from the AST (not compile errors): deeply nested
    // `if` staircases in Result/Unit functions get a nudge toward `guard`/`?`.
    for (uri, diag) in collect_nesting_hints(&index.line_indices, &index.module_uris, &state.typed)
    {
        by_file.entry(uri).or_default().push(diag);
    }

    Ok(CompileOutput {
        generation,
        index,
        diagnostics_by_file: by_file.into_iter().collect(),
    })
}

/// The workspace module a document URI maps to. Compares through [`uri_key`] so
/// an edit routes to its module even when the client's URI spelling differs from
/// the server's path round-trip (drive-letter case / colon encoding on Windows);
/// a raw `Url` equality check misses there and the edit never recompiles.
fn module_for_uri(state: &IncrementalState, uri: &Url) -> Option<ModuleId> {
    let sources = state.source_cache();
    let root = &state.resolved.graph.root;
    let target = uri_key(uri);
    state.resolved.graph.modules.iter().find_map(|module| {
        let module_uri = source_id_to_uri(root, sources.id_for_module(module.id).as_str());
        (uri_key(&module_uri) == target).then_some(module.id)
    })
}

/// Run a workspace-scale query on a blocking thread under a cooperative
/// cancellation token.
///
/// The token is tripped if the returned future is dropped — exactly what
/// tower-lsp does to a handler future on `$/cancelRequest` — so the blocking scan
/// can poll the flag between modules and stop, freeing the CPU instead of
/// finishing a result the client has already discarded. The guard lives across
/// the `.await`, so the trip happens whether the future completes or is dropped
/// mid-flight. Running off the request task also keeps a heavy scan from stalling
/// the concurrent point queries (hover, completion) that share the server's
/// bounded request concurrency.
///
/// Returns the closure's value on normal completion, or `None` if the blocking
/// task was cancelled or panicked, in which case the handler yields no result.
async fn run_cancellable<T, F>(f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(&Cancel) -> T + Send + 'static,
{
    let cancel = Cancel::new();
    // Held across the await: dropped on normal completion (a no-op once the scan
    // has returned) or when the future is aborted, which trips the flag the
    // detached blocking task is still polling.
    let _guard = CancelOnDrop::new(cancel.clone());
    tokio::task::spawn_blocking(move || f(&cancel)).await.ok()
}

// ── LanguageServer impl ───────────────────────────────────────────────────────

#[tower_lsp::async_trait]
impl LanguageServer for RidgeLanguageServer {
    // A linear sequence of capability negotiations and workspace discovery; the
    // steps don't factor cleanly and read best in one place.
    #[allow(clippy::too_many_lines)]
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        // Type hierarchy has no static server capability in lsp-types 0.94, so it
        // is registered dynamically later — but only if the client accepts that.
        let supports_type_hierarchy = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|td| td.type_hierarchy.as_ref())
            .and_then(|th| th.dynamic_registration)
            .unwrap_or(false);

        // File watching is likewise opt-in via dynamic registration.
        let supports_watched_files = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|ws| ws.did_change_watched_files.as_ref())
            .and_then(|w| w.dynamic_registration)
            .unwrap_or(false);

        // Work-done progress is server-initiated, so the client must opt in via
        // `window.workDoneProgress` before we may create progress tokens.
        let supports_work_done_progress = params
            .capabilities
            .window
            .as_ref()
            .and_then(|w| w.work_done_progress)
            .unwrap_or(false);

        // Pull diagnostics require both halves: the pull request itself and a
        // server-initiated refresh, since pull mode replaces push entirely (see
        // `WorkspaceSnapshot::supports_pull_diagnostics`).
        let supports_pull_diagnostics = {
            let pull = params
                .capabilities
                .text_document
                .as_ref()
                .and_then(|td| td.diagnostic.as_ref())
                .is_some();
            let refresh = params
                .capabilities
                .workspace
                .as_ref()
                .and_then(|ws| ws.diagnostic.as_ref())
                .and_then(|d| d.refresh_support)
                .unwrap_or(false);
            pull && refresh
        };

        // Code-lens refresh lets the server nudge the client to re-query lenses
        // after a runtime `workspace/didChangeConfiguration` flips a flag.
        let supports_code_lens_refresh = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|ws| ws.code_lens.as_ref())
            .and_then(|cl| cl.refresh_support)
            .unwrap_or(false);

        // How the client wants each response encoded (see `ClientCaps`).
        let client_caps = negotiate_client_caps(&params.capabilities);

        // Code lenses are opt-in via `initializationOptions.codeLens`, so a generic
        // client is never served lenses whose commands it can't run. Advertising
        // the provider is gated on the client expressing interest at all (the
        // `codeLens` key being present), not on a flag being on: that keeps the
        // capability stable so a later `didChangeConfiguration` can toggle an
        // individual lens on and have the refresh actually land. Which lenses are
        // emitted is the per-flag config, which can change at runtime.
        let code_lens_node = params
            .initialization_options
            .as_ref()
            .and_then(locate_code_lens_settings);
        let code_lens_opted_in = code_lens_node.is_some();
        let code_lens_config = code_lens_node
            .map(parse_code_lens_flags)
            .unwrap_or_default();

        // Collect every folder the client opened — `rootUri` plus all
        // `workspaceFolders` — and walk each up to its nearest `[workspace]`
        // manifest. Distinct manifest roots become independent workspaces, so a
        // multi-folder window with several Ridge projects analyses them all.
        // `rootUri` usually duplicates the first folder, so dedup by the canonical
        // path, which also collapses drive-letter-case spellings on Windows.
        let mut folder_uris: Vec<Url> = Vec::new();
        if let Some(root) = params.root_uri {
            folder_uris.push(root);
        }
        if let Some(folders) = params.workspace_folders {
            folder_uris.extend(folders.into_iter().map(|f| f.uri));
        }

        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut roots: Vec<PathBuf> = Vec::new();
        for uri in &folder_uris {
            let Ok(path) = uri.to_file_path() else {
                continue;
            };
            let Some(root) = find_workspace_root(&path) else {
                continue;
            };
            // Canonicalise only to compare identity; the original path is what the
            // driver compiles, matching the long-standing single-root behaviour.
            let key = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            if seen.insert(key) {
                roots.push(root);
            }
        }

        // With no manifest anywhere, fall back to standalone mode (one synthetic
        // workspace over the open files). That is strictly better than going dark,
        // and it makes a loose file or a manifest-less folder usable.
        let standalone = roots.is_empty();
        let workspaces: Vec<Workspace> = if standalone {
            vec![Workspace::new(None)]
        } else {
            roots
                .into_iter()
                .map(|root| Workspace::new(Some(root)))
                .collect()
        };
        let workspace_count = workspaces.len();

        {
            let mut snap = self.state.lock().await;
            snap.workspaces = workspaces;
            snap.standalone = standalone;
            snap.supports_type_hierarchy = supports_type_hierarchy;
            snap.supports_watched_files = supports_watched_files;
            snap.supports_work_done_progress = supports_work_done_progress;
            snap.supports_pull_diagnostics = supports_pull_diagnostics;
            snap.supports_code_lens_refresh = supports_code_lens_refresh;
            snap.client_caps = client_caps;
            snap.code_lens_config = code_lens_config;
        }
        if standalone {
            tracing::info!(
                "no [workspace] manifest found at or above any folder; entering standalone mode"
            );
            self.client
                .log_message(
                    MessageType::INFO,
                    "ridge-lsp: no workspace manifest found; analyzing open files individually \
                     (standalone mode). Add a ridge.toml with a [workspace] table for \
                     cross-module analysis.",
                )
                .await;
        } else if workspace_count > 1 {
            tracing::info!("multi-root workspace: analyzing {workspace_count} Ridge projects");
        }

        Ok(InitializeResult {
            capabilities: server_capabilities(supports_pull_diagnostics, code_lens_opted_in),
            server_info: Some(ServerInfo {
                name: "ridge-lsp".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "ridge-lsp initialized")
            .await;

        // lsp-types 0.94 cannot express the static `typeHierarchyProvider`
        // capability, so register `textDocument/prepareTypeHierarchy` at runtime
        // when the client supports dynamic registration. Without this the client
        // never sends type-hierarchy requests.
        let supports_type_hierarchy = {
            let snap = self.state.lock().await;
            snap.supports_type_hierarchy
        };
        if supports_type_hierarchy {
            let registration = Registration {
                id: "ridge-type-hierarchy".to_owned(),
                method: "textDocument/prepareTypeHierarchy".to_owned(),
                register_options: None,
            };
            if let Err(err) = self.client.register_capability(vec![registration]).await {
                tracing::warn!("failed to register type hierarchy capability: {err}");
            }
        }

        // Watch `.ridge` sources and `ridge.toml` manifests so on-disk changes
        // outside the open buffers (git checkout, external edits, files created
        // or deleted in the explorer) refresh the index and diagnostics.
        let supports_watched_files = {
            let snap = self.state.lock().await;
            snap.supports_watched_files
        };
        if supports_watched_files {
            let options = DidChangeWatchedFilesRegistrationOptions {
                watchers: vec![
                    FileSystemWatcher {
                        glob_pattern: GlobPattern::String("**/*.ridge".to_owned()),
                        kind: None,
                    },
                    FileSystemWatcher {
                        glob_pattern: GlobPattern::String("**/ridge.toml".to_owned()),
                        kind: None,
                    },
                ],
            };
            match serde_json::to_value(options) {
                Ok(register_options) => {
                    let registration = Registration {
                        id: "ridge-watched-files".to_owned(),
                        method: "workspace/didChangeWatchedFiles".to_owned(),
                        register_options: Some(register_options),
                    };
                    if let Err(err) = self.client.register_capability(vec![registration]).await {
                        tracing::warn!("failed to register watched-files capability: {err}");
                    }
                }
                Err(err) => {
                    tracing::warn!("failed to encode watched-files registration: {err}");
                }
            }
        }
    }

    async fn shutdown(&self) -> LspResult<()> {
        // Cancel any in-flight compile.
        let mut ch = self.compile_handle.lock().await;
        if let Some(handle) = ch.take() {
            handle.abort();
        }
        let mut dh = self.debounce_handle.lock().await;
        if let Some(handle) = dh.take() {
            handle.abort();
        }
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        {
            let mut snap = self.state.lock().await;
            snap.open_docs.insert(uri.clone(), text);
        }
        // Run compile immediately on open.
        self.trigger_compile().await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // Apply the last content change (Incremental — but for simplicity we
        // accept a full-text replacement if a single change covers the whole
        // document.  For LSP Incremental sync, each change has a range; we
        // apply them sequentially.
        {
            let mut snap = self.state.lock().await;
            {
                let entry = snap.open_docs.entry(uri.clone()).or_default();
                for change in params.content_changes {
                    if let Some(range) = change.range {
                        // Apply incremental edit: replace the byte range with new text.
                        apply_incremental_edit(entry, range, &change.text);
                    } else {
                        // Full-text replacement.
                        *entry = change.text;
                    }
                }
            }
            snap.dirty.insert(uri.clone());
        }
        // Debounced incremental compile — 250 ms.
        self.schedule_debounced_compile().await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        // didSave is unconditional — no debounce.
        let uri = params.text_document.uri;
        tracing::debug!("didSave: {uri}");
        // Update doc if text was included (save.includeText = false so typically not).
        if let Some(text) = params.text {
            let mut snap = self.state.lock().await;
            snap.open_docs.insert(uri, text);
        }
        self.trigger_compile().await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        let standalone = {
            let mut snap = self.state.lock().await;
            snap.open_docs.remove(&uri);
            // Drop the document's cached token stream; a reopened file gets a
            // fresh full result before it can delta again.
            snap.semantic_tokens.remove(&uri_key(&uri));
            snap.standalone
        };
        // Clear diagnostics for the closed file.
        deliver_diagnostics(&self.client, &self.state, vec![(uri, Vec::new())]).await;
        // In standalone mode the closed file was a synthetic project member, so
        // rebuild the graph from the remaining open files to drop it.
        if standalone {
            self.trigger_compile().await;
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        // Apply a live change to which code lenses are shown. The settings blob is
        // client-shaped and only sometimes carries our keys, so act solely when it
        // actually contains a `codeLens` object: a pull-model notification
        // (`settings: null`) or an unrelated change must never be read as "all
        // lenses off" and silently wipe the config.
        let Some(code_lens) = locate_code_lens_settings(&params.settings) else {
            return;
        };
        let new_config = parse_code_lens_flags(code_lens);

        let (changed, supports_refresh) = {
            let mut snap = self.state.lock().await;
            let changed = snap.code_lens_config != new_config;
            if changed {
                snap.code_lens_config = new_config;
            }
            (changed, snap.supports_code_lens_refresh)
        };

        // Nudge the client to re-query lenses so the change shows without a
        // restart. Only when something actually changed (no spurious refresh on a
        // no-op notification) and only when the client advertised refresh support
        // (otherwise the request is unanswered noise).
        if changed && supports_refresh {
            if let Err(err) = self.client.code_lens_refresh().await {
                tracing::warn!("workspace/codeLens/refresh was rejected: {err}");
            }
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut relevant = false;
        let mut deleted: Vec<Url> = Vec::new();
        for change in &params.changes {
            if !is_watched_ridge_path(&change.uri) {
                continue;
            }
            relevant = true;
            if change.typ == FileChangeType::DELETED && is_ridge_source(&change.uri) {
                deleted.push(change.uri.clone());
            }
        }
        if !relevant {
            return;
        }

        // A reseed only publishes diagnostics for modules that still exist, so a
        // deleted module's diagnostics must be cleared explicitly. Drop it from
        // the open-doc overlay too, in case it was being edited.
        if !deleted.is_empty() {
            let mut snap = self.state.lock().await;
            for uri in &deleted {
                snap.open_docs.remove(uri);
            }
        }
        let cleared: Vec<(Url, Vec<Diagnostic>)> =
            deleted.into_iter().map(|uri| (uri, Vec::new())).collect();
        deliver_diagnostics(&self.client, &self.state, cleared).await;

        // Reseed from disk: re-runs discovery so files created or deleted on disk
        // and manifest edits are reflected, then recompiles against open buffers.
        self.trigger_compile().await;
    }

    /// Pull diagnostics for a single document (LSP 3.17). Served from the cache
    /// the last compile left in `last_diagnostics`, so it never recompiles — the
    /// client decides when to ask, and a background compile nudges it to re-ask
    /// through `workspace/diagnostic/refresh`. Only reachable when the client put
    /// the server in pull mode (`supports_pull_diagnostics`); a file with no cached
    /// entry is simply clean. `result_id` is unused: the server always returns a
    /// full report rather than tracking per-document result ids.
    async fn diagnostic(
        &self,
        params: DocumentDiagnosticParams,
    ) -> LspResult<DocumentDiagnosticReportResult> {
        let uri = params.text_document.uri;
        let items = {
            let snap = self.state.lock().await;
            snap.last_diagnostics.get(&uri).cloned().unwrap_or_default()
        };
        Ok(DocumentDiagnosticReportResult::Report(
            DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items,
                },
            }),
        ))
    }

    /// Pull diagnostics for the whole workspace (LSP 3.17). Reports every file the
    /// last compile produced diagnostics for, across all open projects, so a
    /// pull-model client can populate its Problems view with errors in files it
    /// never opened — the parity the push model gave by publishing every file.
    async fn workspace_diagnostic(
        &self,
        _params: WorkspaceDiagnosticParams,
    ) -> LspResult<WorkspaceDiagnosticReportResult> {
        let items = {
            let snap = self.state.lock().await;
            snap.last_diagnostics
                .iter()
                .map(|(uri, diags)| {
                    WorkspaceDocumentDiagnosticReport::Full(WorkspaceFullDocumentDiagnosticReport {
                        uri: uri.clone(),
                        version: None,
                        full_document_diagnostic_report: FullDocumentDiagnosticReport {
                            result_id: None,
                            items: diags.clone(),
                        },
                    })
                })
                .collect()
        };
        Ok(WorkspaceDiagnosticReportResult::Report(
            WorkspaceDiagnosticReport { items },
        ))
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        // Clone the snapshot Arc and release the lock before querying, so a
        // hover never blocks on (or triggers) a compile. A stale snapshot may
        // yield a stale type; that is acceptable and resolves on the next compile.
        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        let Some((markdown, span)) = index.hover_at(&uri, pos.line, pos.character) else {
            return Ok(None);
        };
        // Send the card as Markdown unless the client asked for plain text, in
        // which case strip the fences so it still reads cleanly.
        let (kind, value) = if self.client_caps().await.hover_markdown {
            (MarkupKind::Markdown, markdown)
        } else {
            (MarkupKind::PlainText, markdown_to_plaintext(&markdown))
        };
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent { kind, value }),
            range: index.span_to_range(&uri, span),
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .definition_at(&uri, pos.line, pos.character)
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn goto_declaration(
        &self,
        params: request::GotoDeclarationParams,
    ) -> LspResult<Option<request::GotoDeclarationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .declaration_at(&uri, pos.line, pos.character)
            .map(request::GotoDeclarationResponse::Scalar))
    }

    async fn goto_type_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .type_definition_at(&uri, pos.line, pos.character)
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn goto_implementation(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .implementations_at(&uri, pos.line, pos.character)
            .map(GotoDefinitionResponse::Array))
    }

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let partial_token = params.partial_result_params.partial_result_token;
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

        let Some(index) = self.index_for_uri(&uri).await else {
            return Ok(None);
        };
        // A find-references scan spans every module, so run it off the request
        // task and let `$/cancelRequest` stop it (see `run_cancellable`).
        let result = run_cancellable(move |cancel| {
            index.references_at(&uri, pos.line, pos.character, include_declaration, cancel)
        })
        .await;
        let locations = result.flatten();
        // A client that opted into partial results gets the locations streamed as
        // `$/progress` chunks; the final response is then empty, since the client
        // appends every chunk it received.
        if let (Some(token), Some(locs)) = (partial_token, &locations) {
            let values: Vec<serde_json::Value> = locs
                .iter()
                .filter_map(|loc| serde_json::to_value(loc).ok())
                .collect();
            stream_partial_results(&self.client, &token, &values).await;
            return Ok(Some(Vec::new()));
        }
        Ok(locations)
    }

    async fn code_lens(&self, params: CodeLensParams) -> LspResult<Option<Vec<CodeLens>>> {
        let cfg = self.state.lock().await.code_lens_config;
        if !cfg.any() {
            return Ok(None);
        }
        let uri = params.text_document.uri;
        let Some(index) = self.index_for_uri(&uri).await else {
            return Ok(None);
        };
        Ok(index.code_lenses_at(&uri, cfg))
    }

    async fn code_lens_resolve(&self, params: CodeLens) -> LspResult<CodeLens> {
        // The executable Run/Run-test lenses already carry their command; only the
        // navigational lenses need a count, which is a workspace-wide scan — run it
        // off the request task under cooperative cancellation, like `references`.
        if params.command.is_some() {
            return Ok(params);
        }
        let uri = params
            .data
            .as_ref()
            .and_then(|d| d.get("uri"))
            .and_then(serde_json::Value::as_str)
            .and_then(|s| Url::parse(s).ok());
        let Some(uri) = uri else {
            return Ok(params);
        };
        let Some(index) = self.index_for_uri(&uri).await else {
            return Ok(params);
        };
        let fallback = params.clone();
        let resolved = run_cancellable(move |cancel| index.resolve_code_lens(params, cancel)).await;
        Ok(resolved.unwrap_or(fallback))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> LspResult<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.document_highlights_at(&uri, pos.line, pos.character))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> LspResult<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let pos = params.position;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.prepare_rename_at(&uri, pos.line, pos.character))
    }

    async fn rename(&self, params: RenameParams) -> LspResult<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name;

        let Some(index) = self.index_for_uri(&uri).await else {
            return Ok(None);
        };
        // Rename scans the whole workspace for use sites; cancellable like
        // find-references.
        let result = run_cancellable(move |cancel| {
            index.rename_at(&uri, pos.line, pos.character, &new_name, cancel)
        })
        .await;
        match result {
            Some(Ok(edit)) => Ok(edit),
            Some(Err(message)) => Err(tower_lsp::jsonrpc::Error::invalid_params(message)),
            // Cancelled or the blocking task failed: no edit to apply.
            None => Ok(None),
        }
    }

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        };
        let items = index
            .completions_at(&uri, pos.line, pos.character)
            .into_iter()
            .map(|d| CompletionItem {
                label: d.label,
                kind: Some(d.kind),
                sort_text: Some(d.sort_text),
                detail: d.detail,
                data: d.data,
                ..CompletionItem::default()
            })
            .collect();
        Ok(Some(CompletionResponse::Array(items)))
    }

    /// `completionItem/resolve` — fill in a workspace symbol's signature and doc
    /// when the editor highlights it, so the completion list itself stays cheap.
    /// Items without a `data` payload (locals, keywords, stdlib members) come
    /// back unchanged.
    async fn completion_resolve(&self, mut item: CompletionItem) -> LspResult<CompletionItem> {
        let Some(data) = item.data.clone() else {
            return Ok(item);
        };
        let markdown = self.client_caps().await.completion_doc_markdown;
        // The completion's `data` carries the target module's URI and symbol
        // name, so try each workspace until one resolves it; only the owning
        // workspace's index will. A name+URI lookup never aliases a `ModuleId`,
        // so scanning across workspaces is safe.
        for index in self.all_indices().await {
            if let Some((detail, doc)) = index.resolve_completion(&data) {
                item.detail = Some(detail);
                item.documentation = doc.map(|d| {
                    let (kind, value) = if markdown {
                        (MarkupKind::Markdown, d)
                    } else {
                        (MarkupKind::PlainText, markdown_to_plaintext(&d))
                    };
                    Documentation::MarkupContent(MarkupContent { kind, value })
                });
                break;
            }
        }
        Ok(item)
    }

    /// `textDocument/formatting` — reformat the whole document with `ridge-fmt`.
    ///
    /// Emits a single full-document replacement when the formatter produces a
    /// different string. A buffer the parser rejects (`FormatError`) yields no
    /// edits — the parse diagnostics already flag it, and the formatter never
    /// rewrites a broken file — and so does an already-formatted buffer.
    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;

        let text = {
            let snap = self.state.lock().await;
            snap.open_docs.get(&uri).cloned()
        };
        let Some(text) = text else {
            return Ok(None);
        };

        let Ok(formatted) = ridge_fmt::format_source(&text) else {
            return Ok(None);
        };
        if formatted == text {
            return Ok(None);
        }

        let index = LineIndex::new(&text);
        let end_byte = u32::try_from(text.len()).unwrap_or(u32::MAX);
        let (end_line, end_char) = index.byte_to_utf16(end_byte);
        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position::new(0, 0),
                end: Position::new(end_line, end_char),
            },
            new_text: formatted,
        }]))
    }

    /// `textDocument/rangeFormatting` — reformat only the selected lines.
    ///
    /// The whole buffer is run through `ridge-fmt` (the one formatter the CLI and
    /// `textDocument/formatting` share), then diffed against the original by line;
    /// only the change hunks that overlap the requested range are returned, so a
    /// "format selection" leaves the rest of the file alone. An unparseable buffer
    /// or a selection already in formatted shape yields no edits.
    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;

        let text = {
            let snap = self.state.lock().await;
            snap.open_docs.get(&uri).cloned()
        };
        let Some(text) = text else {
            return Ok(None);
        };

        let Ok(formatted) = ridge_fmt::format_source(&text) else {
            return Ok(None);
        };
        if formatted == text {
            return Ok(None);
        }

        let line_index = LineIndex::new(&text);
        let edits = crate::format::range_format_edits(&text, &formatted, params.range, &line_index);
        if edits.is_empty() {
            Ok(None)
        } else {
            Ok(Some(edits))
        }
    }

    /// `textDocument/onTypeFormatting` — auto-indent the line a newline opened.
    ///
    /// Registered only on `\n`. The fresh line's indentation is derived from the
    /// offside structure of the preceding line (purely lexical, so it works on the
    /// half-written buffer a full parse would reject). This gives every client the
    /// same offside auto-indent the VS Code grammar already provides via
    /// `increaseIndentPattern`.
    async fn on_type_formatting(
        &self,
        params: DocumentOnTypeFormattingParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        // Only the newline trigger is registered; ignore anything else.
        if params.ch != "\n" {
            return Ok(None);
        }
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        // Indent with the same width the client uses for its own auto-indent, so
        // the two never disagree.
        let step = params.options.tab_size as usize;

        let text = {
            let snap = self.state.lock().await;
            snap.open_docs.get(&uri).cloned()
        };
        let Some(text) = text else {
            return Ok(None);
        };

        let edits = crate::format::on_type_newline_edits(&text, position, step);
        if edits.is_empty() {
            Ok(None)
        } else {
            Ok(Some(edits))
        }
    }

    /// `textDocument/documentSymbol` — the outline for one document (the
    /// breadcrumb bar, the outline view, and `Ctrl-Shift-O`).
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        let Some(symbols) = index.document_symbols_at(&uri) else {
            return Ok(None);
        };
        // A client without hierarchical support can't render a nested tree, so
        // flatten the outline to `SymbolInformation[]` with each member's
        // container named; otherwise keep the richer nested form.
        if self.client_caps().await.hierarchical_symbols {
            Ok(Some(DocumentSymbolResponse::Nested(symbols)))
        } else {
            Ok(Some(DocumentSymbolResponse::Flat(flatten_symbols(
                &uri, &symbols,
            ))))
        }
    }

    /// `textDocument/foldingRange` — collapsible regions (declaration bodies and
    /// blocks of consecutive imports).
    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> LspResult<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.folding_ranges_at(&uri))
    }

    /// `textDocument/selectionRange` — smart expand/shrink selection: the chain
    /// of progressively larger source ranges around each requested position.
    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> LspResult<Option<Vec<SelectionRange>>> {
        let uri = params.text_document.uri;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.selection_ranges_at(&uri, &params.positions))
    }

    /// `textDocument/documentLink` — turn each workspace-module import path into
    /// a link to that module's source file.
    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> LspResult<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;

        let Some(index) = self.index_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(mut links) = index.document_links_at(&uri) else {
            return Ok(None);
        };
        // The tooltip is optional in the protocol and shown only by a client that
        // advertises support; drop it otherwise so we never send a field the
        // client asked us not to.
        if !self.client_caps().await.document_link_tooltip {
            for link in &mut links {
                link.tooltip = None;
            }
        }
        Ok(Some(links))
    }

    /// `textDocument/prepareCallHierarchy` — anchor a call-hierarchy session on
    /// the function under the cursor.
    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> LspResult<Option<Vec<CallHierarchyItem>>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.prepare_call_hierarchy_at(&uri, pos.position.line, pos.position.character))
    }

    /// `callHierarchy/incomingCalls` — the callers of a prepared item.
    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> LspResult<Option<Vec<CallHierarchyIncomingCall>>> {
        // Route to the workspace the prepared item lives in: its `data` holds
        // per-workspace `ModuleId`s, so it must be read against that same index.
        let Some(index) = self.index_for_uri(&params.item.uri).await else {
            return Ok(None);
        };
        let Some(data) = params.item.data.clone() else {
            return Ok(None);
        };
        // Finding callers scans every module; cancellable off the request task.
        let result = run_cancellable(move |cancel| index.incoming_calls(&data, cancel)).await;
        Ok(result.flatten())
    }

    /// `callHierarchy/outgoingCalls` — the functions a prepared item calls.
    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> LspResult<Option<Vec<CallHierarchyOutgoingCall>>> {
        let Some(index) = self.index_for_uri(&params.item.uri).await else {
            return Ok(None);
        };
        let Some(data) = params.item.data.as_ref() else {
            return Ok(None);
        };
        Ok(index.outgoing_calls(data))
    }

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> LspResult<Option<Vec<TypeHierarchyItem>>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.prepare_type_hierarchy_at(&uri, pos.position.line, pos.position.character))
    }

    /// `typeHierarchy/supertypes` — the superclasses of a class, or the class an
    /// instance implements.
    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> LspResult<Option<Vec<TypeHierarchyItem>>> {
        // Route to the workspace the prepared item lives in: its `data` holds
        // per-workspace `ModuleId`s, so it must be read against that same index.
        let Some(index) = self.index_for_uri(&params.item.uri).await else {
            return Ok(None);
        };
        let Some(data) = params.item.data.as_ref() else {
            return Ok(None);
        };
        Ok(index.type_supertypes(data))
    }

    /// `typeHierarchy/subtypes` — the subclasses and instances of a class.
    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> LspResult<Option<Vec<TypeHierarchyItem>>> {
        let Some(index) = self.index_for_uri(&params.item.uri).await else {
            return Ok(None);
        };
        let Some(data) = params.item.data.as_ref() else {
            return Ok(None);
        };
        Ok(index.type_subtypes(data))
    }

    /// `workspace/willRenameFiles` — when the user moves or renames `.ridge`
    /// files, return the edits that keep every importing module pointing at the
    /// moved module's new path. The client applies these atomically with the
    /// rename, so imports never break on a move.
    async fn will_rename_files(
        &self,
        params: RenameFilesParams,
    ) -> LspResult<Option<WorkspaceEdit>> {
        // A renamed file belongs to one workspace; ask each index for the import
        // fixes it owns and merge them. An importing module lives in exactly one
        // workspace, so the per-file edit lists never collide across workspaces.
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for index in self.all_indices().await {
            let Some(edit) = index.rename_files_edit(&params.files) else {
                continue;
            };
            for (uri, edits) in edit.changes.into_iter().flatten() {
                changes.entry(uri).or_default().extend(edits);
            }
        }
        if changes.is_empty() {
            Ok(None)
        } else {
            Ok(Some(WorkspaceEdit {
                changes: Some(changes),
                ..WorkspaceEdit::default()
            }))
        }
    }

    /// `workspace/symbol` — declarations across the workspace matching a query
    /// (`Ctrl-T`).
    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> LspResult<Option<Vec<SymbolInformation>>> {
        // `Ctrl-T` spans every open project and every module within each, so run
        // the merge off the request task and let `$/cancelRequest` stop it — a
        // broad query as the user types should never stall a hover.
        let partial_token = params.partial_result_params.partial_result_token;
        let indices = self.all_indices().await;
        let query = params.query;
        let symbols = run_cancellable(move |cancel| {
            let mut symbols = Vec::new();
            for index in &indices {
                if cancel.is_cancelled() {
                    break;
                }
                symbols.extend(index.workspace_symbols(&query, cancel));
            }
            symbols
        })
        .await
        .unwrap_or_default();
        // Stream the matches as `$/progress` chunks for a client that opted into
        // partial results, then answer with an empty final response (see
        // `references`).
        if let Some(token) = partial_token {
            let values: Vec<serde_json::Value> = symbols
                .iter()
                .filter_map(|sym| serde_json::to_value(sym).ok())
                .collect();
            stream_partial_results(&self.client, &token, &values).await;
            return Ok(Some(Vec::new()));
        }
        Ok(Some(symbols))
    }

    /// `textDocument/inlayHint` — inferred types after un-annotated `let`/`var`
    /// binders within the requested range.
    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let range = params.range;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.inlay_hints(&uri, range))
    }

    /// `textDocument/signatureHelp` — parameter hints for the call being typed.
    /// Resolves the callee of the enclosing call (or the bare function name just
    /// typed) to a signature and marks the parameter the cursor is filling in.
    /// Returns `None` away from any call so the popup stays quiet.
    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> LspResult<Option<SignatureHelp>> {
        let pos = params.text_document_position_params;
        let uri = pos.text_document.uri;

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        // Clients that didn't advertise label-offset support get each parameter
        // label as the substring it covers instead of an offset pair.
        let label_offsets = self.client_caps().await.sig_label_offsets;
        Ok(index.signature_help_at(
            &uri,
            pos.position.line,
            pos.position.character,
            label_offsets,
        ))
    }

    /// `textDocument/semanticTokens/full` — semantic highlighting for the whole
    /// document. The reply carries a `resultId` and the stream is cached under it,
    /// so a client that then edits can ask for just the delta.
    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> LspResult<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(index) = self.index_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(data) = index.semantic_token_data(&uri) else {
            return Ok(None);
        };
        let result_id = self.cache_semantic_tokens(&uri, data.clone()).await;
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: Some(result_id),
            data,
        })))
    }

    /// `textDocument/semanticTokens/full/delta` — re-highlight after an edit by
    /// sending only the changed span. The fresh stream is diffed against the one
    /// the client last received (matched by `previousResultId`); on a cache miss —
    /// a stale id, or a document we never served a full result for — it falls back
    /// to a full reply, which the protocol permits.
    async fn semantic_tokens_full_delta(
        &self,
        params: SemanticTokensDeltaParams,
    ) -> LspResult<Option<SemanticTokensFullDeltaResult>> {
        let uri = params.text_document.uri;
        let Some(index) = self.index_for_uri(&uri).await else {
            return Ok(None);
        };
        let Some(new_data) = index.semantic_token_data(&uri) else {
            return Ok(None);
        };
        // Read the cached base (only if its id is the one the client edited from)
        // and install the fresh stream under a new id in one critical section.
        let (result_id, previous) = {
            let mut snap = self.state.lock().await;
            let key = uri_key(&uri);
            let previous = snap
                .semantic_tokens
                .get(&key)
                .filter(|cached| cached.result_id == params.previous_result_id)
                .map(|cached| cached.data.clone());
            snap.semantic_tokens_seq += 1;
            let result_id = snap.semantic_tokens_seq.to_string();
            snap.semantic_tokens.insert(
                key,
                CachedSemanticTokens {
                    result_id: result_id.clone(),
                    data: new_data.clone(),
                },
            );
            (result_id, previous)
        };
        Ok(Some(match previous {
            Some(old) => SemanticTokensFullDeltaResult::TokensDelta(SemanticTokensDelta {
                result_id: Some(result_id),
                edits: diff_tokens(&old, &new_data),
            }),
            None => SemanticTokensFullDeltaResult::Tokens(SemanticTokens {
                result_id: Some(result_id),
                data: new_data,
            }),
        }))
    }

    /// `textDocument/semanticTokens/range` — semantic highlighting restricted to
    /// the editor's visible region, for large files.
    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> LspResult<Option<SemanticTokensRangeResult>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .semantic_tokens_in_range(&uri, range)
            .map(SemanticTokensRangeResult::Tokens))
    }

    /// `textDocument/codeAction` — quick-fixes. For a `T014` capability error
    /// on a function that declares no capabilities, offers an edit that adds the
    /// inferred capabilities to its signature: the annotation stays explicit and
    /// visible, you just don't have to type it out.
    async fn code_action(&self, params: CodeActionParams) -> LspResult<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let range = params.range;
        // Match the request URI to a fix the same normalization-stable way the
        // index resolves documents, so quick-fixes still surface on Windows
        // (where the client's URI spelling differs from the server's).
        let target_key = uri_key(&uri);

        let index = self.index_for_uri(&uri).await;
        let Some(index) = index else {
            return Ok(None);
        };

        // A client that didn't advertise `CodeAction` literal support can't accept
        // an inline `WorkspaceEdit`; for those, the fix is delivered as a `Command`
        // that the `execute_command` handler applies via `workspace/applyEdit`.
        let literals = self.client_caps().await.code_action_literals;

        // Turn one ready-to-apply fix (edit range + text + the diagnostic code it
        // answers) into a `CodeAction` literal, or a `Command` bridge for clients
        // without literal support. Shared by capability (`T014`) and syntax
        // (`P034`/`P035`) quick-fixes.
        let build_action = |title: &str, edit_range, new_text: &str, code: &str, decl_range| {
            let mut changes = HashMap::new();
            changes.insert(
                uri.clone(),
                vec![TextEdit {
                    range: edit_range,
                    new_text: new_text.to_owned(),
                }],
            );
            let edit = WorkspaceEdit {
                changes: Some(changes),
                ..WorkspaceEdit::default()
            };
            if !literals {
                return CodeActionOrCommand::Command(Command {
                    title: title.to_owned(),
                    command: APPLY_EDIT_COMMAND.to_owned(),
                    arguments: Some(vec![
                        serde_json::to_value(&edit).unwrap_or(serde_json::Value::Null)
                    ]),
                });
            }
            let diagnostics: Vec<Diagnostic> = params
                .context
                .diagnostics
                .iter()
                .filter(|d| {
                    d.code == Some(NumberOrString::String(code.to_owned()))
                        && ranges_overlap(d.range, decl_range)
                })
                .cloned()
                .collect();
            CodeActionOrCommand::CodeAction(CodeAction {
                title: title.to_owned(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: if diagnostics.is_empty() {
                    None
                } else {
                    Some(diagnostics)
                },
                edit: Some(edit),
                ..CodeAction::default()
            })
        };

        let mut actions: Vec<CodeActionOrCommand> = index
            .capability_fixes
            .iter()
            .filter(|fix| uri_key(&fix.uri) == target_key && ranges_overlap(fix.decl_range, range))
            .map(|fix| {
                build_action(
                    &fix.title,
                    fix.edit_range,
                    &fix.new_text,
                    "T014",
                    fix.decl_range,
                )
            })
            .collect();

        actions.extend(
            index
                .syntax_fixes
                .iter()
                .filter(|fix| {
                    uri_key(&fix.uri) == target_key && ranges_overlap(fix.decl_range, range)
                })
                .map(|fix| {
                    build_action(
                        &fix.title,
                        fix.edit_range,
                        &fix.new_text,
                        fix.code,
                        fix.decl_range,
                    )
                }),
        );

        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    /// `workspace/executeCommand` — applies a quick-fix edit for a client that
    /// can't accept a `CodeAction` literal. The matching `code_action` `Command`
    /// carries the `WorkspaceEdit` as its sole argument; here it is forwarded
    /// straight back via `workspace/applyEdit`. Any other command is ignored.
    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> LspResult<Option<serde_json::Value>> {
        if params.command != APPLY_EDIT_COMMAND {
            return Ok(None);
        }
        let Some(arg) = params.arguments.into_iter().next() else {
            return Ok(None);
        };
        let Ok(edit) = serde_json::from_value::<WorkspaceEdit>(arg) else {
            return Ok(None);
        };
        if let Err(err) = self.client.apply_edit(edit).await {
            tracing::warn!("workspace/applyEdit was rejected: {err}");
        }
        Ok(None)
    }
}

/// Whether two LSP ranges intersect (touching counts as overlap).
fn ranges_overlap(a: Range, b: Range) -> bool {
    a.start <= b.end && b.start <= a.end
}

/// Flatten a hierarchical outline into `SymbolInformation[]` for clients that
/// don't support nested `DocumentSymbol`s. Each entry's `container_name` is its
/// parent's name, so a union's variants still read as belonging to the union and
/// a record's fields to the record.
#[allow(deprecated)] // `SymbolInformation::deprecated` is deprecated; set to None.
fn flatten_symbols(uri: &Url, symbols: &[DocumentSymbol]) -> Vec<SymbolInformation> {
    fn walk(
        uri: &Url,
        symbols: &[DocumentSymbol],
        container: Option<&str>,
        out: &mut Vec<SymbolInformation>,
    ) {
        for sym in symbols {
            out.push(SymbolInformation {
                name: sym.name.clone(),
                kind: sym.kind,
                tags: sym.tags.clone(),
                deprecated: None,
                // The selection range points at the name, the best target for a
                // "go to symbol" jump.
                location: Location {
                    uri: uri.clone(),
                    range: sym.selection_range,
                },
                container_name: container.map(ToOwned::to_owned),
            });
            if let Some(children) = &sym.children {
                walk(uri, children, Some(&sym.name), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(uri, symbols, None, &mut out);
    out
}

/// Render a Markdown hover/documentation string as plain text for a client that
/// only asked for `plaintext`: drop the code-fence delimiter lines and strip the
/// backticks used for inline code, so the content reads cleanly without markup.
fn markdown_to_plaintext(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.extend(line.chars().filter(|&c| c != '`'));
    }
    out
}

/// The static set of capabilities the server advertises at `initialize`.
/// Deliver a batch of `(uri, diagnostics)` updates to the client.
///
/// On the push model each file is published immediately. On the pull model the
/// server records the diagnostics in `last_diagnostics` and asks the client to
/// re-pull (`workspace/diagnostic/refresh`) instead, so the same results reach
/// the editor through the pull endpoints. Publishing in pull mode too would
/// duplicate every diagnostic against the pulled set, so the two paths are
/// mutually exclusive. An empty batch is a no-op.
///
/// Free function rather than a method so the detached compile task — which holds
/// only clones of the client and state, not `&self` — can call it too.
async fn deliver_diagnostics(
    client: &Client,
    state: &Mutex<WorkspaceSnapshot>,
    updates: Vec<(Url, Vec<Diagnostic>)>,
) {
    if updates.is_empty() {
        return;
    }
    let pull = {
        let mut snap = state.lock().await;
        if snap.supports_pull_diagnostics {
            for (uri, diags) in &updates {
                snap.last_diagnostics.insert(uri.clone(), diags.clone());
            }
            true
        } else {
            false
        }
    };
    if pull {
        if let Err(err) = client.workspace_diagnostic_refresh().await {
            tracing::warn!("workspace/diagnostic/refresh was rejected: {err}");
        }
    } else {
        for (uri, diags) in updates {
            client.publish_diagnostics(uri, diags, None).await;
        }
    }
}

fn server_capabilities(pull_diagnostics: bool, code_lens: bool) -> ServerCapabilities {
    ServerCapabilities {
        // Positions are exchanged as UTF-16 code-unit offsets, the LSP default.
        // Advertising it explicitly documents the contract; the server converts
        // via `ridge_lexer::LineIndex`.
        position_encoding: Some(PositionEncodingKind::UTF16),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        // Go-to-declaration jumps to a name's import clause, where go-to-definition
        // jumps past the import to the original `fn`/`type`/`const` (see
        // `WorkspaceIndex::declaration_at`); the two coincide for everything else.
        declaration_provider: Some(DeclarationCapability::Simple(true)),
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
        implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        // Auto-indent the line a newline opens, so non-VS-Code clients get the
        // offside indentation the VS Code grammar supplies via its indent rules.
        document_on_type_formatting_provider: Some(DocumentOnTypeFormattingOptions {
            first_trigger_character: "\n".to_owned(),
            more_trigger_character: None,
        }),
        document_symbol_provider: Some(OneOf::Left(true)),
        // Underline each `import` path as a link to the imported module's file.
        // The target is resolved eagerly (it's a map lookup), so no resolve step.
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: None,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        // Backs the quick-fix `Command` bridge for clients that can't accept a
        // `CodeAction` literal: the command carries the `WorkspaceEdit` the server
        // then asks the client to apply (see `code_action` / `execute_command`).
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![APPLY_EDIT_COMMAND.to_owned()],
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        // Inline Run / Run-test / references / implementations lenses, gated on the
        // client opting in via `initializationOptions.codeLens`. The reference and
        // implementation counts are a workspace-wide scan, computed lazily in
        // `codeLens/resolve` only for the lenses the editor shows, so the provider
        // advertises `resolve_provider`. The Run/Run-test commands (`ridge.run` /
        // `ridge.test`) are handled client-side and stay out of
        // `execute_command_provider` on purpose (see `RUN_COMMAND`).
        code_lens_provider: code_lens.then_some(CodeLensOptions {
            resolve_provider: Some(true),
        }),
        // Ridge calls are juxtaposition (`joinOn a b c`), so a space — not `(`
        // or `,` — separates arguments. Trigger and re-trigger on it.
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec![" ".to_owned()]),
            retrigger_characters: Some(vec![" ".to_owned()]),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        // Semantic highlighting over the resolved program: it colours
        // identifiers the TextMate grammar can't disambiguate, and surfaces the
        // capability annotations as their own token type.
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: crate::index::SEMANTIC_TOKEN_TYPES.to_vec(),
                    token_modifiers: crate::index::SEMANTIC_TOKEN_MODIFIERS.to_vec(),
                },
                // Full results carry a `resultId`, so a client that edits can ask
                // for `semanticTokens/full/delta` and we reply with just the
                // changed span instead of the whole stream.
                full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                range: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            },
        )),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_owned()]),
            resolve_provider: Some(true),
            ..CompletionOptions::default()
        }),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                will_save: None,
                will_save_wait_until: None,
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
            },
        )),
        // File-operation support: react to `.ridge` file renames — and to folder
        // renames, which carry every `.ridge` module beneath them — by fixing the
        // imports that referenced the moved modules. Only `willRename` is needed:
        // it returns the edit the client applies together with the move. The
        // folder filter is a bare `**` because a glob can't tell which folders
        // hold `.ridge` files; the server returns no edit for one that doesn't.
        workspace: Some(WorkspaceServerCapabilities {
            workspace_folders: None,
            file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                will_rename: Some(FileOperationRegistrationOptions {
                    filters: vec![
                        FileOperationFilter {
                            scheme: Some("file".to_owned()),
                            pattern: FileOperationPattern {
                                glob: "**/*.ridge".to_owned(),
                                matches: Some(FileOperationPatternKind::File),
                                options: None,
                            },
                        },
                        FileOperationFilter {
                            scheme: Some("file".to_owned()),
                            pattern: FileOperationPattern {
                                glob: "**".to_owned(),
                                matches: Some(FileOperationPatternKind::Folder),
                                options: None,
                            },
                        },
                    ],
                }),
                ..WorkspaceFileOperationsServerCapabilities::default()
            }),
        }),
        // Diagnostics are pushed via `client.publish_diagnostics(...)` for clients
        // on the push model. A client that supports both pull and refresh moves to
        // the pull model instead: advertise the provider and serve
        // `textDocument/diagnostic` + `workspace/diagnostic`. The provider is gated
        // on that support so a client without a pull engine never calls the
        // endpoint and so never logs `-32601 Method not found` for it. Ridge has
        // cross-module diagnostics, so `inter_file_dependencies` is set.
        diagnostic_provider: pull_diagnostics.then(|| {
            DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("ridge".to_owned()),
                inter_file_dependencies: true,
                workspace_diagnostics: true,
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })
        }),
        ..ServerCapabilities::default()
    }
}

// ── Incremental edit helper ───────────────────────────────────────────────────

/// Apply an incremental LSP text edit to an in-memory document string.
///
/// LSP positions are 0-indexed line / UTF-16 character. [`LineIndex`] converts
/// them to byte offsets so an edit lands on the right bytes even on lines that
/// contain non-ASCII text.
fn apply_incremental_edit(doc: &mut String, range: Range, new_text: &str) {
    let index = LineIndex::new(doc);
    let start = index.utf16_to_byte(range.start.line, range.start.character) as usize;
    let end = index.utf16_to_byte(range.end.line, range.end.character) as usize;
    if start <= end && end <= doc.len() {
        doc.replace_range(start..end, new_text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn incremental_edit_replaces_multibyte_char() {
        // "café": replace the é (UTF-16 column 3..4) with "e".
        let mut doc = "café".to_owned();
        apply_incremental_edit(
            &mut doc,
            Range {
                start: at(0, 3),
                end: at(0, 4),
            },
            "e",
        );
        assert_eq!(doc, "cafe");
    }

    #[test]
    fn incremental_edit_after_emoji_hits_correct_bytes() {
        // "😀ab": insert "!" at UTF-16 column 2 (just past the surrogate pair),
        // which is byte 4 — a naive byte==column reading would split the emoji.
        let mut doc = "😀ab".to_owned();
        apply_incremental_edit(
            &mut doc,
            Range {
                start: at(0, 2),
                end: at(0, 2),
            },
            "!",
        );
        assert_eq!(doc, "😀!ab");
    }

    #[test]
    fn incremental_edit_second_line() {
        let mut doc = "alpha\nbeta".to_owned();
        apply_incremental_edit(
            &mut doc,
            Range {
                start: at(1, 0),
                end: at(1, 4),
            },
            "gamma",
        );
        assert_eq!(doc, "alpha\ngamma");
    }

    #[tokio::test]
    async fn run_cancellable_returns_value_when_not_cancelled() {
        let out = run_cancellable(|cancel| {
            assert!(!cancel.is_cancelled(), "a fresh request is not cancelled");
            21 * 2
        })
        .await;
        assert_eq!(out, Some(42));
    }

    // Proves the cooperative half of `$/cancelRequest`: dropping the handler
    // future (what tower-lsp does on cancel) must stop the detached blocking
    // scan, not just discard its result. The closure stands in for a workspace
    // scan that polls the flag between modules.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborting_handler_cancels_the_blocking_scan() {
        use std::time::Duration;

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();

        let task = tokio::spawn(run_cancellable(move |cancel| {
            // Signal that the blocking scan is running, then poll the flag.
            let _ = started_tx.send(());
            while !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = done_tx.send(());
        }));

        // Once the scan is definitely running, abort the handler. Aborting drops
        // its future and the `CancelOnDrop` guard, tripping the flag the still
        // detached blocking task is polling.
        started_rx.await.expect("scan started");
        task.abort();

        tokio::time::timeout(Duration::from_secs(5), done_rx)
            .await
            .expect("blocking scan was not cancelled within the timeout")
            .expect("scan exited without signalling");
    }
}
