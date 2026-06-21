//! Wire-level smoke test: drive the built `ridge-lsp` binary over real stdio.
//!
//! `lsp_replay` exercises the request handlers in-process through
//! `service.inner()`. This test is the only one that goes through `main.rs`,
//! the `Content-Length` message framing, the `tower-lsp` stdio transport, and
//! the full `initialize` -> `initialized` -> `didOpen` -> `shutdown` -> `exit`
//! lifecycle the way a real editor does. It catches a class of bugs the
//! in-process tests cannot: framing, transport wiring, and a non-clean exit.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::missing_docs_in_private_items
)]

use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tower_lsp::lsp_types::Url;

/// Read one `Content-Length`-framed message from `reader`, or `None` at EOF.
fn read_message(reader: &mut impl Read) -> Option<Value> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        reader.read_exact(&mut byte).ok()?;
        header.push(byte[0]);
    }
    let header = String::from_utf8_lossy(&header);
    let len: usize = header
        .split("\r\n")
        .find_map(|line| line.strip_prefix("Content-Length:"))
        .and_then(|v| v.trim().parse().ok())?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Pump framed messages off the child's stdout on a background thread so the
/// test can interleave writes and reads without deadlocking.
fn spawn_reader(stdout: ChildStdout) -> Receiver<Value> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Some(msg) = read_message(&mut reader) {
            if tx.send(msg).is_err() {
                return;
            }
        }
    });
    rx
}

/// Frame and send a JSON-RPC message to the server.
fn send(stdin: &mut ChildStdin, msg: &Value) {
    let body = serde_json::to_vec(msg).expect("serialize message");
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).expect("write header");
    stdin.write_all(&body).expect("write body");
    stdin.flush().expect("flush");
}

/// Drain messages until one satisfies `pred` or `timeout` elapses.
fn recv_until(
    rx: &Receiver<Value>,
    timeout: Duration,
    mut pred: impl FnMut(&Value) -> bool,
) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        match rx.recv_timeout(remaining) {
            Ok(msg) if pred(&msg) => return Some(msg),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Wait for the child to exit, killing it if it overruns `timeout`.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
}

#[test]
fn stdio_lifecycle_and_capabilities() {
    // A minimal but real workspace on disk; the server discovers it from the
    // root URI exactly as an editor's launch would.
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path();
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"smoke-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("project manifest");
    let source = "pub fn answer -> Int = 42\n";
    let main = app_src.join("Main.ridge");
    std::fs::write(&main, source).expect("source file");

    let root_uri = Url::from_file_path(root).expect("root URI");
    let file_uri = Url::from_file_path(&main).expect("file URI");

    let mut child = Command::new(env!("CARGO_BIN_EXE_ridge-lsp"))
        .env("RIDGE_LSP_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ridge-lsp binary");
    let mut stdin = child.stdin.take().expect("child stdin");
    let rx = spawn_reader(child.stdout.take().expect("child stdout"));

    // initialize -> the server answers with its advertised capabilities.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {},
                "workspaceFolders": [{ "uri": root_uri, "name": "smoke-ws" }]
            }
        }),
    );
    let init = recv_until(&rx, Duration::from_secs(20), |m| m["id"] == json!(1))
        .expect("initialize response over stdio");
    let caps = &init["result"]["capabilities"];
    assert_eq!(
        caps["positionEncoding"],
        json!("utf-16"),
        "the server must negotiate UTF-16 position encoding"
    );
    assert!(
        !caps["hoverProvider"].is_null(),
        "hover capability must be advertised, got {caps}"
    );
    assert!(
        !caps["renameProvider"].is_null(),
        "rename capability must be advertised, got {caps}"
    );

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    // didOpen -> the server compiles and pushes diagnostics for the document.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": file_uri, "languageId": "ridge", "version": 1, "text": source
            } }
        }),
    );
    let diagnostics = recv_until(&rx, Duration::from_secs(20), |m| {
        m["method"] == json!("textDocument/publishDiagnostics")
    });
    assert!(
        diagnostics.is_some(),
        "the server must push diagnostics after didOpen"
    );

    // A request/response round-trip over the wire.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": file_uri },
                "position": { "line": 0, "character": 23 }
            }
        }),
    );
    let hover = recv_until(&rx, Duration::from_secs(15), |m| m["id"] == json!(2))
        .expect("hover response over stdio");
    assert!(
        hover.get("error").is_none(),
        "hover must not error, got {hover}"
    );

    // shutdown -> exit -> a clean process exit with status 0.
    // `shutdown` and `exit` carry no params; the server rejects an explicit
    // `null` params field, so the key is omitted entirely.
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown" }),
    );
    let shutdown = recv_until(&rx, Duration::from_secs(15), |m| m["id"] == json!(3))
        .expect("shutdown response over stdio");
    assert!(
        shutdown.get("error").is_none(),
        "shutdown must not error, got {shutdown}"
    );

    send(&mut stdin, &json!({ "jsonrpc": "2.0", "method": "exit" }));
    drop(stdin);
    let status = wait_for_exit(&mut child, Duration::from_secs(15))
        .expect("the server process must exit after the exit notification");
    assert!(
        status.success(),
        "a shutdown followed by exit must yield status 0, got {status:?}"
    );

    std::mem::forget(dir);
}
