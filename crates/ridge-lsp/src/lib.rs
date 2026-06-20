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
//! - Missing `ridge.toml` → `L801 LspWorkspaceMissing` workspace-level diagnostic.
//! - Multi-root workspace → one-time `L802 LspMultiRootUnsupported` warning, single-root used.
//! - File outside any workspace member → one-time `L803 LspFileOrphan` warning, skipped.
//! - Driver internal error → `tracing::error!` + `L804 LspInternal` surfaced as LSP error.
//!
//! # Limitations
//!
//! Single-root workspaces only; a multi-root window falls back to the first
//! root with the `L802` warning above.  Recompilation is incremental and
//! debounced (see the `didChange`/`didSave` notes), so edit-to-diagnostic
//! latency stays flat as a workspace grows.  See `README.md` for documented
//! behaviour and known limitations.

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

pub mod completion;
pub mod diagnostics;
pub mod index;
pub mod server;
pub mod span_recovery;

pub use server::RidgeLanguageServer;
