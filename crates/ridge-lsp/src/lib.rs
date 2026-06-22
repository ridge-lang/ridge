//! Ridge Language Server Protocol implementation.
//!
//! This crate exposes a `tower-lsp`-based LSP server with stdio transport only
//! (no `--tcp`, no named pipe, no Unix socket).
//!
//! # Architecture
//!
//! - [`server::RidgeLanguageServer`] — the core `tower_lsp::LanguageServer` impl.
//! - [`diagnostics`] — `ridge_diagnostics::Diagnostic` → `lsp_types::Diagnostic` conversion.
//! - [`span_recovery`] — fallback walk for synthesised IR nodes.
//!
//! # Edge cases documented
//!
//! - No `[workspace]` manifest at or above any opened folder → standalone mode:
//!   each open `.ridge` file is type-checked on its own, so a loose file still
//!   gets full single-file analysis (no cross-module imports across loose files).
//! - Multi-root window → each opened folder with a `[workspace]` manifest becomes
//!   an independent workspace, analysed and queried on its own; a request routes
//!   to the workspace that owns the document.
//! - File outside any workspace member → one-time `L803 LspFileOrphan` warning, skipped.
//! - Driver internal error → `tracing::error!` + `L804 LspInternal` surfaced as LSP error.
//!
//! # Performance
//!
//! Recompilation is incremental and debounced (see the `didChange`/`didSave`
//! notes), so edit-to-diagnostic latency stays flat as a workspace grows. With
//! several folders open, only the workspace that owns an edited file recompiles.
//! Workspace-scale queries — find-references, rename, `workspace/symbol`, call
//! hierarchy — run on a blocking thread under a cooperative cancellation token
//! (see [`cancel`]), so a `$/cancelRequest` actually stops the scan and a heavy
//! query never stalls the point queries (hover, completion) typed alongside it.
//! See `README.md` for documented behaviour and known limitations.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::missing_docs_in_private_items,
        dead_code
    )
)]

pub mod cancel;
pub mod completion;
pub mod diagnostics;
pub mod index;
pub mod server;
pub mod span_recovery;
pub mod stdlib_defs;

pub use server::RidgeLanguageServer;
