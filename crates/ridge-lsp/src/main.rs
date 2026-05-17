//! `ridge-lsp` binary entry point.
//!
//! Wires [`RidgeLanguageServer`] to the stdio LSP transport via
//! `tower_lsp::Server`.  Transport is stdio only (OQ-C001 — no `--tcp`,
//! no named pipe, no Unix socket).
//!
//! # Usage
//!
//! ```text
//! ridge-lsp
//! ```
//!
//! The server reads JSON-RPC messages from stdin and writes responses to stdout.
//! Logs (via `tracing`) go to stderr at the level set by `RIDGE_LSP_LOG`
//! (default: `info`).

use tower_lsp::{LspService, Server};

use ridge_lsp::RidgeLanguageServer;

#[tokio::main]
async fn main() {
    // Initialise tracing to stderr (never stdout — that carries LSP messages).
    let log_level = std::env::var("RIDGE_LSP_LOG").unwrap_or_else(|_| "info".to_owned());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&log_level)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(RidgeLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
