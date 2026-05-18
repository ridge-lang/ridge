//! `ridge-lsp` binary entry point.
//!
//! Wires [`RidgeLanguageServer`] to the stdio LSP transport via
//! `tower_lsp::Server`.  Transport is stdio only (no `--tcp`,
//! no named pipe, no Unix socket).
//!
//! # Usage
//!
//! ```text
//! ridge-lsp              # start the LSP server (stdio transport)
//! ridge-lsp --version    # print version and exit
//! ```
//!
//! The server reads JSON-RPC messages from stdin and writes responses to stdout.
//! Logs (via `tracing`) go to stderr at the level set by `RIDGE_LSP_LOG`
//! (default: `info`).

use tower_lsp::{LspService, Server};

use ridge_lsp::RidgeLanguageServer;

#[tokio::main]
async fn main() {
    // Handle --version / -V before starting the LSP loop.  Useful for
    // installers that want to verify both binaries are at the expected
    // version after extracting.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("ridge-lsp {}", env!("CARGO_PKG_VERSION"));
        return;
    }

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
