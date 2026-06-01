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
//! 2. `textDocument/didChange`: debounce 250 ms; on trigger, cancel any in-flight
//!    compile (by aborting the tokio task), then spawn a fresh `check_workspace` call.
//! 3. `textDocument/didSave`: unconditional compile (no debounce).
//! 4. Diagnostics published via `client.publish_diagnostics(...)`.
//!
//! # Cancellation
//!
//! The `check_workspace` driver function is synchronous.  We run it inside
//! `tokio::task::spawn_blocking`.  Cancellation is achieved by calling
//! `JoinHandle::abort()` on the running task — this is the minimal correct
//! approach given that `check_workspace` has no cooperative cancellation hook.
//! The aborted blocking thread may run briefly past the abort signal (tokio
//! does not forcibly kill blocking threads), but it will not publish diagnostics
//! because the result is discarded when a new compile is queued.

// LSP server module-local stylistic allow: `significant_drop_tightening`
// (nursery) — the suggested rewrites push lock acquisitions into single
// expressions and lose visual clarity around "snapshot then act on snapshot"
// patterns; the lock holds are short.
#![allow(clippy::significant_drop_tightening)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tower_lsp::jsonrpc::Result as LspResult;
// LSP server uses 25+ types from `tower_lsp::lsp_types`; an explicit `use`
// list churns on every protocol revision.  Wildcard import is the idiomatic
// pattern in `tower-lsp`-based servers.
#[allow(clippy::wildcard_imports)]
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use ridge_driver::{check_workspace_typed, CheckOptions};
use ridge_lexer::LineIndex;
use ridge_manifest::find_workspace_root;

use crate::diagnostics::{source_id_to_uri, to_lsp_diagnostic};
use crate::index::WorkspaceIndex;

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

    /// Run a type-check of the workspace and publish diagnostics.
    ///
    /// Cancels any currently-running compile by aborting its task.
    /// Then spawns a new `tokio::task::spawn_blocking` call to `check_workspace`.
    async fn trigger_compile(&self) {
        let state_arc = Arc::clone(&self.state);
        let client = self.client.clone();
        let compile_handle_arc = Arc::clone(&self.compile_handle);

        // Cancel any existing in-flight compile.
        {
            let mut ch = compile_handle_arc.lock().await;
            if let Some(handle) = ch.take() {
                handle.abort();
            }
        }

        // Snapshot the workspace root and open docs.
        let (workspace_root, docs_snapshot) = {
            let snap = state_arc.lock().await;
            if snap.missing_workspace {
                // Already published L801; nothing to compile.
                return;
            }
            match snap.workspace_root.clone() {
                Some(root) => (root, snap.open_docs.clone()),
                None => return,
            }
        };

        // Claim a generation for this compile and capture a state handle so the
        // task can install its index without clobbering a newer one.
        let generation = self.compile_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let state_for_install = Arc::clone(&self.state);

        let handle = tokio::spawn(async move {
            let opts = CheckOptions::new(workspace_root.clone()).with_retain_indices(true);

            // Run the synchronous check in a blocking thread pool thread.
            let result = tokio::task::spawn_blocking(move || check_workspace_typed(opts)).await;

            match result {
                Err(_join_err) => {
                    // Task was aborted or panicked; discard silently.
                }
                Ok(Err(check_err)) => {
                    // Fatal driver error (e.g. workspace not found).
                    tracing::error!("L804 LspInternal: driver fatal error: {check_err}");
                    // The static URL `file:///unknown` is hard-coded; `Url::parse`
                    // on it cannot fail.  `expect` is the right tool here — the
                    // lib-level `expect_used` deny is for user-reachable inputs,
                    // not for compile-time-known constants.
                    #[allow(clippy::expect_used)]
                    let uri = Url::from_file_path(&workspace_root).unwrap_or_else(|()| {
                        Url::parse("file:///unknown").expect("static URL is valid")
                    });
                    let lsp_diag = Diagnostic {
                        range: Range::default(),
                        severity: Some(DiagnosticSeverity::ERROR),
                        code: Some(NumberOrString::String("L804".to_owned())),
                        code_description: None,
                        source: Some("ridge".to_owned()),
                        message: format!("L804 LspInternal: {check_err}"),
                        related_information: None,
                        tags: None,
                        data: None,
                    };
                    client.publish_diagnostics(uri, vec![lsp_diag], None).await;
                }
                Ok(Ok(artefacts)) => {
                    // Bucket diagnostics by source file.
                    let mut by_file: std::collections::HashMap<String, Vec<Diagnostic>> =
                        std::collections::HashMap::new();

                    // Pre-populate with all open docs so we clear stale diagnostics.
                    for uri in docs_snapshot.keys() {
                        by_file.entry(uri.to_string()).or_default();
                    }

                    for diag in &artefacts.diagnostics {
                        let source_key = diag.source_id.as_str();

                        // Derive the document URI from the workspace-relative
                        // source id instead of suffix-matching open-doc paths.
                        // The old `ends_with` match failed whenever the file was
                        // not open, anchoring the diagnostic to `<unknown>`.
                        let uri = source_id_to_uri(&workspace_root, source_key);

                        // Resolve spans against the exact on-disk text the
                        // compiler read — `check_workspace` compiles disk state,
                        // so a diagnostic's byte offsets index that text, not the
                        // editor buffer. Fall back to the open-doc text only when
                        // the cache has no entry for this source id.
                        let src_text = artefacts
                            .sources
                            .text(source_key)
                            .or_else(|| docs_snapshot.get(&uri).map(String::as_str));

                        let lsp_diag = to_lsp_diagnostic(diag, &uri, src_text);
                        by_file.entry(uri.to_string()).or_default().push(lsp_diag);
                    }

                    // Publish (or clear) diagnostics for every file.
                    for (uri_str, diags) in by_file {
                        if let Ok(uri) = Url::parse(&uri_str) {
                            client.publish_diagnostics(uri, diags, None).await;
                        }
                    }

                    // Build the retained analysis index off-lock, then install
                    // it under a short lock. The previous index stays queryable
                    // until this fully-built one replaces it, and the generation
                    // guard prevents a slow aborted compile from overwriting a
                    // newer result.
                    let typed = artefacts.typed;
                    let resolved = artefacts.resolved;
                    let new_index = Arc::new(WorkspaceIndex::build(generation, typed, resolved));
                    let mut snap = state_for_install.lock().await;
                    if snap
                        .index
                        .as_ref()
                        .is_none_or(|existing| generation > existing.generation)
                    {
                        snap.index = Some(new_index);
                    }
                }
            }
        });

        // Store the new handle.
        let mut ch = compile_handle_arc.lock().await;
        *ch = Some(handle);
    }

    /// Schedule a debounced compile (250 ms delay).
    ///
    /// Cancels any pending debounce timer and restarts it.  If a new
    /// `didChange` arrives before the 250 ms elapses, the previous timer
    /// is cancelled and a new one starts.
    async fn schedule_debounced_compile(&self) {
        let debounce_arc = Arc::clone(&self.debounce_handle);
        let server_state = Arc::clone(&self.state);
        let server_compile = Arc::clone(&self.compile_handle);
        let client = self.client.clone();

        // Cancel any pending debounce timer.
        {
            let mut dh = debounce_arc.lock().await;
            if let Some(handle) = dh.take() {
                handle.abort();
            }
        }

        let self_clone = Self {
            client,
            state: server_state,
            compile_handle: server_compile,
            debounce_handle: Arc::clone(&debounce_arc),
            compile_generation: Arc::clone(&self.compile_generation),
        };

        let handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
            self_clone.trigger_compile().await;
        });

        let mut dh = debounce_arc.lock().await;
        *dh = Some(handle);
    }
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
            capabilities: ServerCapabilities {
                // Positions are exchanged as UTF-16 code-unit offsets, the LSP
                // default. Advertising it explicitly documents the contract; the
                // server converts via `ridge_lexer::LineIndex`.
                position_encoding: Some(PositionEncodingKind::UTF16),
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
                // Diagnostics are pushed via `client.publish_diagnostics(...)`
                // from `trigger_compile`. The pull endpoint
                // `textDocument/diagnostic` (LSP 3.17) is intentionally not
                // advertised because no `diagnostic()` handler is implemented;
                // advertising the capability made LSP 3.17 clients log
                // `-32601 Method not found` errors on every document open.
                ..ServerCapabilities::default()
            },
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
        // Debounced compile — 250 ms.
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
