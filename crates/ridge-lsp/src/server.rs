//! `RidgeLanguageServer` — `tower_lsp::LanguageServer` implementation.
//!
//! # Transport
//!
//! Stdio only.  The binary entry point in `main.rs` wires this via
//! `tower_lsp::Server::new(stdin, stdout, socket).serve(service)`.
//!
//! # Workspace lifecycle
//!
//! 1. `initialize`: read `rootUri` / first `workspaceFolders` entry → set workspace root.
//!    Extra workspace folders trigger one-time `L802 LspMultiRootUnsupported` warn.
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
    check_workspace_incremental, collect_diagnostics, CheckError, CheckOptions, IncrementalState,
};
use ridge_lexer::LineIndex;
use ridge_manifest::find_workspace_root;
use ridge_resolve::ModuleId;

use crate::diagnostics::{source_id_to_uri, to_lsp_diagnostic};
use crate::index::{collect_capability_fixes, WorkspaceIndex};

// ── WorkspaceSnapshot ─────────────────────────────────────────────────────────

/// In-memory state of the LSP workspace.
///
/// Held behind `Arc<Mutex<…>>` and shared between the `initialize` handler and
/// the compile task.
#[derive(Debug, Default)]
struct WorkspaceSnapshot {
    /// Absolute path to the workspace root (the directory containing the
    /// root `ridge.toml`).  `None` until `initialize` is handled.
    workspace_root: Option<PathBuf>,
    /// Open document contents keyed by `Url` (LSP file URI).
    open_docs: std::collections::HashMap<Url, String>,
    /// Set of file URIs for which we've already emitted the L803 orphan warning.
    /// Reserved for future use (0.2.0 orphan-file warn-once logic).
    #[allow(dead_code)]
    warned_orphan: HashSet<String>,
    /// True if we've already emitted the L802 multi-root warning.
    warned_multi_root: bool,
    /// True if `workspace_root` was found to be missing `ridge.toml`.
    missing_workspace: bool,
    /// The most recent completed analysis, if any. Replaced wholesale on each
    /// successful compile; reads clone the `Arc` and release the lock before
    /// querying. `None` until the first compile lands.
    index: Option<Arc<WorkspaceIndex>>,
    /// File URIs edited since the last compile. Drained by the debounced
    /// incremental compile so a burst of edits across files is applied together.
    dirty: HashSet<Url>,
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
    /// The retained incremental engine. A full compile reseeds it; an edit
    /// recompiles the affected modules in place. Held behind a blocking mutex so
    /// the `spawn_blocking` compile task can own it without moving it out (and
    /// thus never lose it to a task abort). Editor queries never touch it — they
    /// read the derived `WorkspaceIndex` snapshot instead.
    engine: Arc<StdMutex<Option<IncrementalState>>>,
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
            engine: Arc::new(StdMutex::new(None)),
        }
    }

    /// Return the most recent analysis index, if a compile has completed.
    ///
    /// Clones the `Arc` under a short lock and releases the lock before
    /// returning, so a query never holds the state mutex while it reads the
    /// index. This is the read-path primitive for hover, go-to-definition, and
    /// completion.
    #[must_use]
    pub async fn workspace_index(&self) -> Option<Arc<WorkspaceIndex>> {
        let snap = self.state.lock().await;
        snap.index.clone()
    }

    /// Run a workspace compile and publish diagnostics.
    ///
    /// `reseed` forces a fresh full check from disk; otherwise the retained
    /// incremental engine is reused (seeded on first use). `edits` are
    /// `(uri, buffer)` pairs applied to the engine before the result is built,
    /// so diagnostics and the analysis index reflect the editor's buffers rather
    /// than stale disk text. The heavy work runs on a blocking thread; its index
    /// and diagnostics install under the generation guard, so a slow compile
    /// superseded by a newer one is discarded.
    async fn run_compile(&self, reseed: bool, edits: Vec<(Url, String)>) {
        let compile_handle_arc = Arc::clone(&self.compile_handle);
        {
            let mut ch = compile_handle_arc.lock().await;
            if let Some(handle) = ch.take() {
                handle.abort();
            }
        }

        let workspace_root = {
            let snap = self.state.lock().await;
            if snap.missing_workspace {
                return;
            }
            match snap.workspace_root.clone() {
                Some(root) => root,
                None => return,
            }
        };

        let engine = Arc::clone(&self.engine);
        let gen_counter = Arc::clone(&self.compile_generation);
        let state_for_install = Arc::clone(&self.state);
        let client = self.client.clone();

        let handle = tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                compile_blocking(&engine, &gen_counter, &workspace_root, reseed, &edits)
            })
            .await;

            match result {
                Err(_join_err) => {} // aborted or panicked; discard
                Ok(Err(check_err)) => {
                    tracing::error!("L804 LspInternal: driver fatal error: {check_err}");
                }
                Ok(Ok(out)) => {
                    // Install the index and publish diagnostics only if this
                    // compile is still the newest — both are gated on the same
                    // generation so a superseded result clobbers nothing.
                    let install = {
                        let mut snap = state_for_install.lock().await;
                        if snap
                            .index
                            .as_ref()
                            .is_none_or(|existing| out.generation > existing.generation)
                        {
                            snap.index = Some(Arc::clone(&out.index));
                            true
                        } else {
                            false
                        }
                    };
                    if install {
                        for (uri, diags) in out.diagnostics_by_file {
                            client.publish_diagnostics(uri, diags, None).await;
                        }
                    }
                }
            }
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
            engine: Arc::clone(&self.engine),
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

/// Seed-or-reuse the engine, apply the buffer edits, and produce the index and
/// diagnostics. Holds the engine mutex for the whole call, so concurrent
/// compiles serialise on the shared engine; the generation is claimed inside
/// that lock so its order matches the order edits were applied.
fn compile_blocking(
    engine: &StdMutex<Option<IncrementalState>>,
    gen_counter: &AtomicU64,
    workspace_root: &Path,
    reseed: bool,
    edits: &[(Url, String)],
) -> Result<CompileOutput, CheckError> {
    let mut guard = engine
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reseed || guard.is_none() {
        let opts = CheckOptions::new(workspace_root.to_path_buf()).with_retain_indices(true);
        *guard = Some(check_workspace_incremental(opts)?);
    }
    let Some(state) = guard.as_mut() else {
        return Err(CheckError::NoWorkspaceRoot {
            path: workspace_root.to_path_buf(),
        });
    };

    for (uri, buffer) in edits {
        if let Some(mid) = module_for_uri(state, uri) {
            state.recompile(mid, buffer);
        }
    }

    let generation = gen_counter.fetch_add(1, Ordering::SeqCst) + 1;
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
        let uri = source_id_to_uri(workspace_root, source_key);
        let src_text = sources.text(source_key);
        let lsp_diag = to_lsp_diagnostic(diag, &uri, src_text);
        by_file.entry(uri).or_default().push(lsp_diag);
    }

    Ok(CompileOutput {
        generation,
        index,
        diagnostics_by_file: by_file.into_iter().collect(),
    })
}

/// The workspace module a document URI maps to, keyed the same way the index and
/// diagnostics are (workspace root joined with the source id).
fn module_for_uri(state: &IncrementalState, uri: &Url) -> Option<ModuleId> {
    let sources = state.source_cache();
    let root = &state.resolved.graph.root;
    state.resolved.graph.modules.iter().find_map(|module| {
        (source_id_to_uri(root, sources.id_for_module(module.id).as_str()) == *uri)
            .then_some(module.id)
    })
}

// ── LanguageServer impl ───────────────────────────────────────────────────────

#[tower_lsp::async_trait]
impl LanguageServer for RidgeLanguageServer {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        // Determine the workspace root from rootUri or first workspaceFolders entry.
        let root_uri: Option<Url> = params.root_uri.or_else(|| {
            params
                .workspace_folders
                .as_ref()
                .and_then(|folders| folders.first().map(|f| f.uri.clone()))
        });

        // Warn if multiple workspace roots were provided.
        if let Some(folders) = &params.workspace_folders {
            if folders.len() > 1 {
                let mut snap = self.state.lock().await;
                if !snap.warned_multi_root {
                    snap.warned_multi_root = true;
                    tracing::warn!(
                        "L802 LspMultiRootUnsupported: multi-root workspace detected; \
                         only the first root is supported in 0.1.0"
                    );
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            "ridge-lsp: L802 LspMultiRootUnsupported — multi-root workspace \
                             not supported in 0.1.0; only the first root is used.",
                        )
                        .await;
                }
            }
        }

        if let Some(uri) = root_uri {
            match uri.to_file_path() {
                Ok(path) => {
                    // Verify a ridge.toml exists at or above this path.
                    let manifest_root = find_workspace_root(&path);
                    let mut snap = self.state.lock().await;

                    if manifest_root.is_none() {
                        snap.missing_workspace = true;
                        tracing::warn!(
                            "L801 LspWorkspaceMissing: no ridge.toml found at or above {}",
                            path.display()
                        );
                        // Publish a workspace-level diagnostic.
                        let ws_uri = uri.clone();
                        let diag = Diagnostic {
                            range: Range::default(),
                            severity: Some(DiagnosticSeverity::WARNING),
                            code: Some(NumberOrString::String("L801".to_owned())),
                            code_description: None,
                            source: Some("ridge".to_owned()),
                            message: format!(
                                "L801 LspWorkspaceMissing: no ridge.toml found at or above {}",
                                path.display()
                            ),
                            related_information: None,
                            tags: None,
                            data: None,
                        };
                        drop(snap);
                        self.client
                            .publish_diagnostics(ws_uri, vec![diag], None)
                            .await;
                    } else {
                        snap.workspace_root = manifest_root;
                    }
                }
                Err(()) => {
                    tracing::warn!("initialize: rootUri is not a file URI; ignoring");
                }
            }
        }

        Ok(InitializeResult {
            capabilities: server_capabilities(),
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
        {
            let mut snap = self.state.lock().await;
            snap.open_docs.remove(&uri);
        }
        // Clear diagnostics for the closed file.
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        // Clone the snapshot Arc and release the lock before querying, so a
        // hover never blocks on (or triggers) a compile. A stale snapshot may
        // yield a stale type; that is acceptable and resolves on the next compile.
        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        let Some((markdown, span)) = index.hover_at(&uri, pos.line, pos.character) else {
            return Ok(None);
        };
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown,
            }),
            range: index.span_to_range(&uri, span),
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .definition_at(&uri, pos.line, pos.character)
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn goto_type_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .type_definition_at(&uri, pos.line, pos.character)
            .map(GotoDefinitionResponse::Scalar))
    }

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.references_at(&uri, pos.line, pos.character, include_declaration))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> LspResult<Option<Vec<DocumentHighlight>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
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

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.prepare_rename_at(&uri, pos.line, pos.character))
    }

    async fn rename(&self, params: RenameParams) -> LspResult<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name;

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        match index.rename_at(&uri, pos.line, pos.character, &new_name) {
            Ok(edit) => Ok(edit),
            Err(message) => Err(tower_lsp::jsonrpc::Error::invalid_params(message)),
        }
    }

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
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
                ..CompletionItem::default()
            })
            .collect();
        Ok(Some(CompletionResponse::Array(items)))
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

    /// `textDocument/documentSymbol` — the outline for one document (the
    /// breadcrumb bar, the outline view, and `Ctrl-Shift-O`).
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .document_symbols_at(&uri)
            .map(DocumentSymbolResponse::Nested))
    }

    /// `workspace/symbol` — declarations across the workspace matching a query
    /// (`Ctrl-T`).
    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> LspResult<Option<Vec<SymbolInformation>>> {
        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(Some(index.workspace_symbols(&params.query)))
    }

    /// `textDocument/inlayHint` — inferred types after un-annotated `let`/`var`
    /// binders within the requested range.
    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;
        let range = params.range;

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
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

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index.signature_help_at(&uri, pos.position.line, pos.position.character))
    }

    /// `textDocument/semanticTokens/full` — semantic highlighting for the whole
    /// document.
    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> LspResult<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };
        Ok(index
            .semantic_tokens(&uri)
            .map(SemanticTokensResult::Tokens))
    }

    /// `textDocument/semanticTokens/range` — semantic highlighting restricted to
    /// the editor's visible region, for large files.
    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> LspResult<Option<SemanticTokensRangeResult>> {
        let uri = params.text_document.uri;
        let range = params.range;
        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
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

        let index = {
            let snap = self.state.lock().await;
            snap.index.clone()
        };
        let Some(index) = index else {
            return Ok(None);
        };

        let actions: Vec<CodeActionOrCommand> = index
            .capability_fixes
            .iter()
            .filter(|fix| fix.uri == uri && ranges_overlap(fix.decl_range, range))
            .map(|fix| {
                let mut changes = HashMap::new();
                changes.insert(
                    uri.clone(),
                    vec![TextEdit {
                        range: fix.edit_range,
                        new_text: fix.new_text.clone(),
                    }],
                );
                let diagnostics: Vec<Diagnostic> = params
                    .context
                    .diagnostics
                    .iter()
                    .filter(|d| {
                        d.code == Some(NumberOrString::String("T014".to_owned()))
                            && ranges_overlap(d.range, fix.decl_range)
                    })
                    .cloned()
                    .collect();
                CodeActionOrCommand::CodeAction(CodeAction {
                    title: fix.title.clone(),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: if diagnostics.is_empty() {
                        None
                    } else {
                        Some(diagnostics)
                    },
                    edit: Some(WorkspaceEdit {
                        changes: Some(changes),
                        ..WorkspaceEdit::default()
                    }),
                    ..CodeAction::default()
                })
            })
            .collect();

        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }
}

/// Whether two LSP ranges intersect (touching counts as overlap).
fn ranges_overlap(a: Range, b: Range) -> bool {
    a.start <= b.end && b.start <= a.end
}

/// The static set of capabilities the server advertises at `initialize`.
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        // Positions are exchanged as UTF-16 code-unit offsets, the LSP default.
        // Advertising it explicitly documents the contract; the server converts
        // via `ridge_lexer::LineIndex`.
        position_encoding: Some(PositionEncodingKind::UTF16),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
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
                full: Some(SemanticTokensFullOptions::Bool(true)),
                range: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            },
        )),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_owned()]),
            resolve_provider: Some(false),
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
        // Diagnostics are pushed via `client.publish_diagnostics(...)` from
        // `trigger_compile`. The pull endpoint `textDocument/diagnostic`
        // (LSP 3.17) is intentionally not advertised because no `diagnostic()`
        // handler is implemented; advertising the capability made LSP 3.17
        // clients log `-32601 Method not found` errors on every document open.
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
}
