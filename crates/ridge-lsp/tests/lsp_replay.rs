//! LSP replay integration tests for `ridge-lsp`.
//!
//! These tests use a hand-rolled JSON-RPC replay framework — no
//! `tower-lsp`-specific test framework.  Each test scripts a sequence of
//! LSP messages assembled as `serde_json::Value` objects and asserts on
//! responses or published diagnostics.
//!
//! # Test organisation
//!
//! - **Protocol-replay** — `initialize`, `didOpen`, `didChange`, `didSave`,
//!   type-error diagnostics, capability-error diagnostics, multi-file workspace
//!   cross-module reference, `shutdown`+`exit`.
//! - **Span recovery** — synthesised `ToText` node, stdlib `IrExpr::Call`
//!   synthesis, fully-synthetic prelude node, span attribution without an open doc.
//! - **Debounce / cancellation** — rapid `didChange` flurries collapse to one
//!   compile; a mid-compile `didChange` cancels the in-flight compile.
//! - **Editor queries** — retained index, scope tree, hover, definition,
//!   completion, find-references, and rename / prepareRename.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items,
    clippy::match_wildcard_for_single_variants,
    dead_code
)]

use std::path::PathBuf;

use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService};

use ridge_lsp::RidgeLanguageServer;

// ── Test helper: replay harness ───────────────────────────────────────────────

/// Fixture root relative to the crate manifest.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Create an `InitializeParams` pointing at the given fixture workspace.
fn make_init_params(workspace_name: &str) -> InitializeParams {
    let root = fixtures_dir().join(workspace_name);
    let uri = Url::from_file_path(&root).expect("fixture path to URL");
    InitializeParams {
        root_uri: Some(uri.clone()),
        workspace_folders: Some(vec![WorkspaceFolder {
            uri,
            name: workspace_name.to_owned(),
        }]),
        capabilities: ClientCapabilities::default(),
        ..InitializeParams::default()
    }
}

// ── In-process service builder ────────────────────────────────────────────────

use tower_lsp::ClientSocket;

/// Build an in-process `LspService` with a test client.
///
/// Returns `(service, socket)` — socket is kept alive to prevent the client
/// from being dropped during test execution.
fn build_test_service() -> (tower_lsp::LspService<RidgeLanguageServer>, ClientSocket) {
    LspService::new(RidgeLanguageServer::new)
}

// ── Protocol-replay test 1: initialize / initialized round-trip ───────────────

#[tokio::test]
async fn test_initialize_initialized_roundtrip() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    let params = make_init_params("ok_workspace");
    let result = server.initialize(params).await.expect("initialize ok");

    // Verify the advertised capabilities match §3.10 exactly.
    let sync = result
        .capabilities
        .text_document_sync
        .expect("textDocumentSync present");
    match sync {
        TextDocumentSyncCapability::Options(opts) => {
            assert_eq!(opts.open_close, Some(true), "openClose must be true");
            assert_eq!(
                opts.change,
                Some(TextDocumentSyncKind::INCREMENTAL),
                "change must be Incremental (2)"
            );
            match opts.save {
                Some(TextDocumentSyncSaveOptions::SaveOptions(save_opts)) => {
                    assert_eq!(
                        save_opts.include_text,
                        Some(false),
                        "save.includeText must be false"
                    );
                }
                _ => panic!("save must be SaveOptions"),
            }
        }
        _ => panic!("textDocumentSync must be Options"),
    }

    // diagnosticProvider must NOT be advertised: the server publishes
    // diagnostics via `client.publish_diagnostics`, and no `diagnostic()`
    // handler implements the LSP 3.17 pull endpoint. Advertising it caused
    // 3.17 clients (vscode-languageclient 9+) to issue
    // `textDocument/diagnostic` requests on every document open and log
    // `-32601 Method not found` errors.
    assert!(
        result.capabilities.diagnostic_provider.is_none(),
        "must not advertise diagnosticProvider — server is push-only"
    );

    // completionProvider, hoverProvider, definitionProvider are all advertised.
    assert!(
        result.capabilities.completion_provider.is_some(),
        "must advertise completionProvider"
    );
    assert!(
        result.capabilities.hover_provider.is_some(),
        "must advertise hoverProvider"
    );
    assert!(
        result.capabilities.definition_provider.is_some(),
        "must advertise definitionProvider"
    );

    // initialized() must not panic.
    server.initialized(InitializedParams {}).await;
}

// ── Protocol-replay test 2: textDocument/didOpen for a well-formed file ───────

#[tokio::test]
async fn test_did_open_well_formed_file() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    let root = fixtures_dir().join("ok_workspace");
    let root_uri = Url::from_file_path(&root).expect("fixture root URI");
    server
        .initialize(InitializeParams {
            root_uri: Some(root_uri.clone()),
            capabilities: ClientCapabilities::default(),
            ..InitializeParams::default()
        })
        .await
        .expect("initialize");

    let file_path = root.join("hello").join("src").join("main.ridge");
    let file_uri = Url::from_file_path(&file_path).expect("file URI");
    let src = std::fs::read_to_string(&file_path).expect("read fixture");

    // didOpen should not panic; we don't assert diagnostics here (compile
    // may or may not finish in the async window of this test).
    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri,
                language_id: "ridge".to_owned(),
                version: 1,
                text: src,
            },
        })
        .await;
    // Give the compile task a moment, then assert server is alive (no panic).
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}

// ── Protocol-replay test 3: textDocument/didChange triggering diagnostics ─────

#[tokio::test]
async fn test_did_change_triggers_compile() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    let root = fixtures_dir().join("ok_workspace");
    server
        .initialize(make_init_params("ok_workspace"))
        .await
        .expect("initialize");

    let file_path = root.join("hello").join("src").join("main.ridge");
    let file_uri = Url::from_file_path(&file_path).expect("file URI");

    // Open the file.
    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri.clone(),
                language_id: "ridge".to_owned(),
                version: 1,
                text: "pub fn greet name -> Text = \"Hello, world!\"".to_owned(),
            },
        })
        .await;

    // Send a change.
    server
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: file_uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None, // full replacement
                range_length: None,
                text: "pub fn greet name -> Text = \"Hi #{name}\"".to_owned(),
            }],
        })
        .await;

    // Wait for the debounce (250 ms) + compile time.
    tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
    // Server still alive — no panics.
}

// ── Protocol-replay test 4: textDocument/didSave re-publishes ─────────────────

#[tokio::test]
async fn test_did_save_unconditional() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    server
        .initialize(make_init_params("ok_workspace"))
        .await
        .expect("initialize");

    let root = fixtures_dir().join("ok_workspace");
    let file_path = root.join("hello").join("src").join("main.ridge");
    let file_uri = Url::from_file_path(&file_path).expect("file URI");

    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri.clone(),
                language_id: "ridge".to_owned(),
                version: 1,
                text: "pub fn greet name -> Text = \"Hello, world!\"".to_owned(),
            },
        })
        .await;

    // didSave must not panic (no text included since includeText=false).
    server
        .did_save(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: file_uri },
            text: None,
        })
        .await;

    // Wait for compile.
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
}

// ── Protocol-replay test 5: type-error fixture publishes T001 ─────────────────

#[tokio::test]
async fn test_type_error_fixture_publishes_diagnostics() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    // Initialize against the type_error_workspace (has a T001 TypeMismatch).
    server
        .initialize(make_init_params("type_error_workspace"))
        .await
        .expect("initialize");

    let root = fixtures_dir().join("type_error_workspace");
    let file_path = root.join("app").join("src").join("main.ridge");
    let file_uri = Url::from_file_path(&file_path).expect("file URI");
    let src = std::fs::read_to_string(&file_path).expect("read fixture");

    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri.clone(),
                language_id: "ridge".to_owned(),
                version: 1,
                text: src,
            },
        })
        .await;

    // Wait for compile to complete.
    tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
    // Diagnostics are published via client.publish_diagnostics; since we can't
    // intercept them in a pure-server test, we verify the server didn't panic
    // and the compile ran without fatal error.
    //
    // Note: the type_error_workspace fixture contains `pub fn wrong -> Text = 42`
    // which should produce a T001 TypeMismatch diagnostic (Int found, Text expected).
    // The assertion is that the server remained alive and processed the compile.
}

// ── Protocol-replay test 6: capability-error fixture ─────────────────────────

#[tokio::test]
async fn test_capability_error_fixture() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    // The capability_error_workspace has an import of std.io but the function
    // only does text manipulation — so this actually compiles cleanly.
    // We verify the server handles it without panic.
    server
        .initialize(make_init_params("capability_error_workspace"))
        .await
        .expect("initialize");

    let root = fixtures_dir().join("capability_error_workspace");
    let file_path = root.join("app").join("src").join("main.ridge");
    let file_uri = Url::from_file_path(&file_path).expect("file URI");
    let src = std::fs::read_to_string(&file_path).expect("read fixture");

    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri,
                language_id: "ridge".to_owned(),
                version: 1,
                text: src,
            },
        })
        .await;

    tokio::time::sleep(tokio::time::Duration::from_millis(600)).await;
    // Server still alive.
}

// ── Protocol-replay test 7: multi-file workspace cross-module reference ───────

#[tokio::test]
async fn test_multi_file_workspace() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    server
        .initialize(make_init_params("multi_file_workspace"))
        .await
        .expect("initialize");

    let root = fixtures_dir().join("multi_file_workspace");

    // Open both files.
    for (member, file) in &[("lib", "math.ridge"), ("app", "main.ridge")] {
        let file_path = root.join(member).join("src").join(file);
        let file_uri = Url::from_file_path(&file_path).expect("file URI");
        let src = std::fs::read_to_string(&file_path).expect("read fixture");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: file_uri,
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: src,
                },
            })
            .await;
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
    // Both modules compiled; server still alive.
}

// ── Protocol-replay test 8: shutdown + exit ───────────────────────────────────

#[tokio::test]
async fn test_shutdown_and_exit() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    server
        .initialize(make_init_params("ok_workspace"))
        .await
        .expect("initialize");

    // shutdown() must return Ok(()).
    server.shutdown().await.expect("shutdown ok");

    // A second shutdown is also valid (idempotent).
    server.shutdown().await.expect("double shutdown ok");
}

// ── Span-recovery test 1: synthesised ToText node ────────────────────────────

#[tokio::test]
async fn test_d087_synthesised_totext_span_recovery() {
    use ridge_lexer::Span;
    use ridge_lsp::span_recovery::resolve_span_to_lsp;

    // A synthesised ToText node from string interpolation uses Span::point(0).
    let src = "pub fn greet name -> Text = \"Hello #{name}!\"";
    let span = Span::point(0);
    let range = resolve_span_to_lsp(span, src);

    // Must recover to file-line-1 (LSP: line 0, character 0).
    assert_eq!(range.start.line, 0);
    assert_eq!(range.start.character, 0);
    // end.character = 1 per the file_line1_range sentinel.
    assert_eq!(range.end.character, 1);
}

// ── Span-recovery test 2: IrExpr::Call with stdlib synthesis ──────────────────

#[tokio::test]
async fn test_d087_ir_call_stdlib_synthesis_span_recovery() {
    use ridge_lexer::Span;
    use ridge_lsp::span_recovery::resolve_span_to_lsp;

    // Simulates an IrExpr::Call synthesised from `a ++ b` on text.
    // The driver propagates the call-site span (byte 22..23 on line 2).
    let src = "pub fn concat a b =\n  a ++ b";
    let span = Span::new(22, 23); // 'a' on line 2
    let range = resolve_span_to_lsp(span, src);

    assert_eq!(
        range.start.line, 1,
        "should be on line 2 (0-indexed line 1)"
    );
    assert_eq!(range.start.character, 2, "col 3 (0-indexed col 2)");
}

// ── Span-recovery test 3: fully-synthetic prelude node ───────────────────────

#[tokio::test]
async fn test_d087_fully_synthetic_prelude_node() {
    use ridge_lexer::Span;
    use ridge_lsp::span_recovery::resolve_span_to_lsp;

    // Fully-synthetic prelude nodes use Span::point(0) even on non-empty source.
    let src = "";
    let span = Span::point(0);
    let range = resolve_span_to_lsp(span, src);

    assert_eq!(range.start.line, 0);
    assert_eq!(range.start.character, 0);
    assert_eq!(range.end.character, 1);
}

// ── Span-attribution test: diagnostic lands on its real line and file ─────────

/// Regression for the `<unknown> 1:1` defect: a diagnostic for a file that is
/// not open in the editor must still resolve to the correct document URI and
/// the correct line/column, not collapse to the workspace `<unknown>` file at
/// line 1.
///
/// The fix derives the URI from the workspace-relative `source_id` and resolves
/// the span against the on-disk text the compiler read (`artefacts.sources`),
/// so no open document is required.  This test deliberately never calls
/// `did_open` — it drives the driver directly, the way the publish loop does.
#[tokio::test]
async fn test_diagnostic_resolves_to_real_span_without_open_doc() {
    use ridge_driver::{check_workspace, CheckOptions};
    use ridge_lsp::diagnostics::{source_id_to_uri, to_lsp_diagnostic};

    // Build a hermetic single-member workspace whose only module has a T001 on
    // line 2. Driving the driver directly (no `did_open`) is exactly what the
    // publish loop does for a file the editor has not opened.
    let root = std::env::temp_dir().join(format!("ridge_lsp_span_{}", std::process::id()));
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"span-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(
        app_src.join("Main.ridge"),
        "-- intentional type error: Int where Text is expected\npub fn wrong -> Text = 42\n",
    )
    .expect("write source");

    let artefacts =
        check_workspace(CheckOptions::new(root.clone())).expect("workspace checks without fatal");

    // The fixture has exactly one error (a T001 on `42`, which sits on line 2).
    let diag = artefacts
        .diagnostics
        .iter()
        .find(|d| d.code == "T001")
        .expect("type_error_workspace must emit T001");

    let source_key = diag.source_id.as_str();
    assert_ne!(
        source_key, "<unknown>",
        "a type error must carry its module source id, not the unknown placeholder"
    );

    let uri = source_id_to_uri(&root, source_key);
    let expected_uri = Url::from_file_path(root.join("app").join("src").join("Main.ridge"))
        .expect("fixture file URI");
    assert_eq!(
        uri, expected_uri,
        "diagnostic must attribute to the real file"
    );

    let src_text = artefacts
        .sources
        .text(source_key)
        .expect("source cache holds the on-disk text");
    let lsp_diag = to_lsp_diagnostic(diag, &uri, Some(src_text));

    // `pub fn wrong -> Text = 42` is the second line; the `42` token must land
    // on 0-indexed line 1, never the line-1 (`0:0`) fallback.
    assert_eq!(
        lsp_diag.range.start.line, 1,
        "T001 must point at line 2, not the `<unknown> 1:1` fallback"
    );
    assert!(
        lsp_diag.range.start.character > 0,
        "the span must carry a real column, not character 0"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// ── Debounce test: rapid didChange flurries trigger exactly one compile ────────

#[tokio::test]
async fn test_debounce_rapid_changes() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    server
        .initialize(make_init_params("ok_workspace"))
        .await
        .expect("initialize");

    let root = fixtures_dir().join("ok_workspace");
    let file_path = root.join("hello").join("src").join("main.ridge");
    let file_uri = Url::from_file_path(&file_path).expect("file URI");

    // Open file.
    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri.clone(),
                language_id: "ridge".to_owned(),
                version: 1,
                text: "pub fn greet -> Text = \"Hello\"".to_owned(),
            },
        })
        .await;

    // Simulate 5 rapid changes within < 250 ms each.
    for i in 2_i32..=6 {
        server
            .did_change(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: file_uri.clone(),
                    version: i,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: format!("pub fn greet -> Text = \"Hello {i}\""),
                }],
            })
            .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }

    // Wait for the debounce window to pass + compile time.
    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

    // The server must be alive and not have stacked up multiple simultaneous
    // compiles (the abort-on-new-change policy ensures at most one runs at end).
    // We can't count compile invocations without instrumenting the server, but
    // the liveness assertion is the observable invariant here.
}

// ── Cancellation test: mid-compile didChange cancels in-flight compile ────────

#[tokio::test]
async fn test_cancellation_discards_stale_results() {
    let (service, _socket) = build_test_service();
    let server = service.inner();

    server
        .initialize(make_init_params("type_error_workspace"))
        .await
        .expect("initialize");

    let root = fixtures_dir().join("type_error_workspace");
    let file_path = root.join("app").join("src").join("main.ridge");
    let file_uri = Url::from_file_path(&file_path).expect("file URI");
    let src = std::fs::read_to_string(&file_path).expect("read fixture");

    // Open the file with error content — triggers a compile.
    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri.clone(),
                language_id: "ridge".to_owned(),
                version: 1,
                text: src,
            },
        })
        .await;

    // Immediately send a didChange with corrected content before the first
    // compile completes.  The in-flight compile should be cancelled (aborted).
    server
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: file_uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                // Now the file is well-formed.
                text: "pub fn correct -> Text = \"ok\"".to_owned(),
            }],
        })
        .await;

    // Wait for debounce + compile.
    tokio::time::sleep(tokio::time::Duration::from_millis(1200)).await;

    // Server must be alive.  The cancellation ensures that only the latest
    // compile (the well-formed content) ran to completion.  We cannot inspect
    // what was published without a test client, but the liveness + no-panic
    // assertion is the observable invariant.
    //
    // The abort() call on the JoinHandle is the cancellation mechanism.
    // The aborted blocking thread may finish but its result is discarded.
}

// ── Test 14: retained analysis index is queryable after a compile ─────────────

/// After a successful compile, the server retains a queryable analysis index:
/// the opened file maps to a module, an offset inside an identifier resolves to
/// a node, and whitespace / unknown URIs resolve to nothing.
///
/// Uses a hermetic temp workspace with complete manifests so the compile
/// actually succeeds (the committed `tests/fixtures` manifests omit required
/// fields and so never get past discovery — they back the smoke tests only).
#[tokio::test]
async fn test_workspace_index_populated_after_compile() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"idx-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    let main_src = "pub fn greet -> Text = \"hi\"\n";
    std::fs::write(app_src.join("Main.ridge"), main_src).expect("write source");

    let (service, _socket) = build_test_service();
    let server = service.inner();

    let root_uri = Url::from_file_path(&root).expect("root URI");
    server
        .initialize(InitializeParams {
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: "idx-ws".to_owned(),
            }]),
            capabilities: ClientCapabilities::default(),
            ..InitializeParams::default()
        })
        .await
        .expect("initialize");

    let file_path = app_src.join("Main.ridge");
    let file_uri = Url::from_file_path(&file_path).expect("file URI");

    // No index exists before the first compile completes. Reading the snapshot
    // here must not block or deadlock.
    assert!(
        server.workspace_index().await.is_none(),
        "no index should exist before any compile"
    );

    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri.clone(),
                language_id: "ridge".to_owned(),
                version: 1,
                text: main_src.to_owned(),
            },
        })
        .await;

    // did_open triggers an immediate compile (no debounce). Poll for the index
    // rather than sleeping a fixed amount: under parallel test-suite load a
    // single compile can take longer than a fixed window, and reading the
    // snapshot repeatedly also exercises the lock discipline.
    let mut index = None;
    for _ in 0..120 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        if let Some(idx) = server.workspace_index().await {
            index = Some(idx);
            break;
        }
    }
    let index = index.expect("index installed after a successful compile");

    // Exactly one module was compiled. Query against the URI the index actually
    // holds: discovery canonicalizes file paths, so the key may differ
    // textually from the tempdir path built above (on macOS `/var` resolves to
    // `/private/var`, on Windows a verbatim prefix appears). Reconciling an
    // editor-sent URI with the canonical key belongs where hover and
    // go-to-definition consume `textDocument.uri`.
    let (module_uri, _mid) = index
        .uri_to_module
        .iter()
        .next()
        .expect("the compiled workspace contributes one module");
    assert!(
        module_uri.path().ends_with("Main.ridge"),
        "module URI must point at the source file, got {module_uri}"
    );

    // The source is `pub fn greet -> Text = "hi"`; `greet` starts at byte
    // offset 7, so offset 8 falls inside it and resolves to a node.
    let hit = index.node_at(module_uri, 8, &[]);
    assert!(
        hit.is_some(),
        "node_at inside `greet` must hit, got {hit:?}"
    );

    // Offset 3 is the space in `pub fn`, covered by no stamped node.
    let miss = index.node_at(module_uri, 3, &[]);
    assert!(
        miss.is_none(),
        "node_at in the `pub fn` prefix must miss, got {miss:?}"
    );

    // An unknown URI resolves to nothing.
    let other = Url::parse("file:///not/in/workspace.ridge").expect("url");
    assert!(
        index.node_at(&other, 0, &[]).is_none(),
        "node_at on a non-workspace URI must be None"
    );
}

// ── Test 15: scope tree is retained and queryable after a compile ─────────────

/// With `retain_indices` enabled (the LSP path), the resolved workspace carries
/// a populated scope tree, so the locals visible at a body offset can be
/// enumerated.
#[tokio::test]
async fn test_scope_tree_retained_on_didopen() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"scope-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    // `greet` binds the parameter `name`, used again in the body.
    let main_src = "pub fn greet name -> Text = name\n";
    std::fs::write(app_src.join("Main.ridge"), main_src).expect("write source");

    let (service, _socket) = build_test_service();
    let server = service.inner();

    let root_uri = Url::from_file_path(&root).expect("root URI");
    server
        .initialize(InitializeParams {
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: "scope-ws".to_owned(),
            }]),
            capabilities: ClientCapabilities::default(),
            ..InitializeParams::default()
        })
        .await
        .expect("initialize");

    let file_uri = Url::from_file_path(app_src.join("Main.ridge")).expect("file URI");
    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri,
                language_id: "ridge".to_owned(),
                version: 1,
                text: main_src.to_owned(),
            },
        })
        .await;

    let mut index = None;
    for _ in 0..120 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        if let Some(idx) = server.workspace_index().await {
            index = Some(idx);
            break;
        }
    }
    let index = index.expect("index installed after a successful compile");

    // The parameter `name` is in scope in the body. Completing there must offer
    // it, which only works when the scope tree was retained (retain_indices set).
    let uri = index
        .uri_to_module
        .keys()
        .next()
        .expect("the workspace contributes one module")
        .clone();
    // A column inside the body's `name` use-site, where the parameter is in scope.
    let col =
        u32::try_from(main_src.rfind("name").expect("body name") + 1).expect("offset fits u32");
    let labels: Vec<String> = index
        .completions_at(&uri, 0, col)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(
        labels.contains(&"name".to_owned()),
        "parameter `name` must be in scope in the body, got {labels:?}"
    );
}

// ── Test 16: textDocument/hover ───────────────────────────────────────────────

/// Build a hermetic workspace with `main_src`, open it, and return the server,
/// the file URI, and the kept-alive temp dir once a compile has produced an
/// analysis index.
async fn hover_fixture(
    main_src: &'static str,
) -> (
    tower_lsp::LspService<RidgeLanguageServer>,
    tower_lsp::ClientSocket,
    Url,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"hover-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), main_src).expect("write source");

    let (service, socket) = build_test_service();
    let mut file_uri = Url::from_file_path(app_src.join("Main.ridge")).expect("file URI");
    {
        let server = service.inner();
        let root_uri = Url::from_file_path(&root).expect("root URI");
        server
            .initialize(InitializeParams {
                root_uri: Some(root_uri.clone()),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri,
                    name: "hover-ws".to_owned(),
                }]),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: file_uri.clone(),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: main_src.to_owned(),
                },
            })
            .await;
        let mut index = None;
        for _ in 0..120 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            if let Some(idx) = server.workspace_index().await {
                index = Some(idx);
                break;
            }
        }
        let index = index.expect("index must be installed before hovering");
        // Hover against the URI the index actually holds — the same scheme
        // diagnostics are published with, which is what an editor sends. A
        // freshly built path can differ when a temp dir is symlinked.
        file_uri = index
            .uri_to_module
            .keys()
            .next()
            .expect("one module in index")
            .clone();
    }
    // Leak the temp dir for the duration of the test process; the OS reclaims it.
    std::mem::forget(dir);
    (service, socket, file_uri)
}

fn hover_at(uri: &Url, line: u32, character: u32) -> HoverParams {
    HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

fn hover_markdown(h: Option<Hover>) -> Option<String> {
    match h?.contents {
        HoverContents::Markup(MarkupContent { value, .. }) => Some(value),
        _ => None,
    }
}

#[tokio::test]
async fn test_hover_literal_and_local_and_misses() {
    // Line 0: `42` literal at character 23. Line 1: body `x` at character 14.
    let src = "pub fn answer -> Int = 42\npub fn id x = x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Literal `42` → its primitive type.
    let h = server.hover(hover_at(&uri, 0, 23)).await.expect("hover ok");
    let md = hover_markdown(h).expect("hover over literal returns markup");
    assert!(
        md.contains("Int"),
        "literal hover should mention Int, got {md:?}"
    );

    // Body `x` use-site → a local with its type.
    let h = server.hover(hover_at(&uri, 1, 14)).await.expect("hover ok");
    let md = hover_markdown(h).expect("hover over local returns markup");
    assert!(
        md.contains("(local) x"),
        "local hover should label the binding, got {md:?}"
    );

    // Whitespace between tokens → no hover.
    let h = server.hover(hover_at(&uri, 0, 6)).await.expect("hover ok");
    assert!(h.is_none(), "hover over whitespace must be null");

    // Far past end of line → no hover.
    let h = server
        .hover(hover_at(&uri, 0, 9999))
        .await
        .expect("hover ok");
    assert!(h.is_none(), "hover past end-of-line must be null");
}

#[tokio::test]
async fn test_hover_labels_class_method() {
    // A user-defined class method used by its bare name hovers with the
    // "(class method)" role label — the same way locals get "(local)".
    let src = "pub class Show a =\n  render (x: a) -> Text\npub type T = { n: Int }\ninstance Show T =\n  render (x: T) -> Text = \"t\"\npub fn run -> Text = render (T { n = 1 })\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // `render` on the last line (line 5) → labelled as a class method.
    let line5 = "pub fn run -> Text = render (T { n = 1 })";
    let col =
        u32::try_from(line5.find("render").expect("render use") + 1).expect("offset fits u32");
    let h = server
        .hover(hover_at(&uri, 5, col))
        .await
        .expect("hover ok");
    let md = hover_markdown(h).expect("hover over a class method returns markup");
    assert!(
        md.contains("(class method) render"),
        "class-method hover should label the binding, got {md:?}"
    );
}

#[tokio::test]
async fn test_hover_enriches_function_signature_and_doc() {
    // Hovering a function use-site shows its written header — visibility, named
    // parameters, return type — inside a `ridge` code fence, plus its doc.
    let src = "---\nGreets a person by name.\n---\npub fn greet (name: Text) -> Text = name\npub fn run -> Text = greet \"x\"\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // `greet` on line 4 (`pub fn run -> Text = greet "x"`).
    let line4 = "pub fn run -> Text = greet \"x\"";
    let col = u32::try_from(line4.find("greet").expect("greet use") + 1).expect("offset fits u32");
    let h = server
        .hover(hover_at(&uri, 4, col))
        .await
        .expect("hover ok");
    let md = hover_markdown(h).expect("hover over a function returns markup");

    assert!(
        md.contains("```ridge"),
        "function hover should be a ridge code fence, got {md:?}"
    );
    assert!(
        md.contains("pub fn greet (name: Text) -> Text"),
        "function hover should show the written signature, got {md:?}"
    );
    assert!(
        md.contains("Greets a person by name."),
        "function hover should include the doc comment, got {md:?}"
    );
}

#[tokio::test]
async fn test_hover_record_field_names_owner() {
    // Hovering a record-field use shows its type and the record it belongs to.
    let src = "pub type User = { age: Int, name: Text }\npub fn ageOf (u: User) -> Int = u.age\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // `age` in `u.age` on line 1.
    let line1 = "pub fn ageOf (u: User) -> Int = u.age";
    let col = u32::try_from(line1.rfind("age").expect("field use") + 1).expect("offset fits u32");
    let h = server
        .hover(hover_at(&uri, 1, col))
        .await
        .expect("hover ok");
    let md = hover_markdown(h).expect("hover over a record field returns markup");

    assert!(
        md.contains("age : Int"),
        "field hover should show the field type, got {md:?}"
    );
    assert!(
        md.contains("field of `User`"),
        "field hover should name the owning record, got {md:?}"
    );
}

// ── Test 17: textDocument/definition ──────────────────────────────────────────

fn goto_at(uri: &Url, line: u32, character: u32) -> GotoDefinitionParams {
    GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

fn scalar_location(resp: Option<GotoDefinitionResponse>) -> Option<Location> {
    match resp? {
        GotoDefinitionResponse::Scalar(loc) => Some(loc),
        _ => None,
    }
}

#[tokio::test]
async fn test_definition_local_and_nulls() {
    // `foo` binds `x` at character 11; the body uses it at 15 and 19.
    let src = "pub fn foo x = x + x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Body `x` → its parameter definition, same file.
    let resp = server
        .goto_definition(goto_at(&uri, 0, 15))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of local `x`");
    assert_eq!(loc.uri, uri, "local definition is in the same file");
    assert_eq!(
        loc.range.start.character, 11,
        "must point at the parameter `x`"
    );

    // Keyword and whitespace have no definition.
    let kw = server
        .goto_definition(goto_at(&uri, 0, 4))
        .await
        .expect("ok");
    assert!(scalar_location(kw).is_none(), "keyword has no definition");
    let ws = server
        .goto_definition(goto_at(&uri, 0, 3))
        .await
        .expect("ok");
    assert!(
        scalar_location(ws).is_none(),
        "whitespace has no definition"
    );
}

// ── Test 18: two-member fixture (cross-module definition + completion) ─────────

/// Build a two-member workspace (a `library` `lib` and an `app` that imports it),
/// open the app file, and return the service plus the index's app and lib URIs.
async fn two_member_fixture() -> (
    tower_lsp::LspService<RidgeLanguageServer>,
    tower_lsp::ClientSocket,
    Url,
    Url,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("lib").join("src")).expect("lib src");
    std::fs::create_dir_all(root.join("app").join("src")).expect("app src");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"two-ws\"\nversion = \"0.1.0\"\nmembers = [\"lib\", \"app\"]\n",
    )
    .expect("workspace manifest");
    std::fs::write(
        root.join("lib").join("ridge.toml"),
        "[project]\nname = \"lib\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("lib manifest");
    std::fs::write(
        root.join("lib").join("src").join("Lib.ridge"),
        "pub fn helper -> Int = 1\n",
    )
    .expect("lib source");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("app manifest");
    let app_text = "import lib.Lib as Lib\npub fn run -> Int = Lib.helper\n";
    std::fs::write(root.join("app").join("src").join("Main.ridge"), app_text).expect("app source");

    let (service, socket) = build_test_service();
    let app_uri;
    let lib_uri;
    {
        let server = service.inner();
        let root_uri = Url::from_file_path(&root).expect("root URI");
        server
            .initialize(InitializeParams {
                root_uri: Some(root_uri.clone()),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri,
                    name: "two-ws".to_owned(),
                }]),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Url::from_file_path(root.join("app").join("src").join("Main.ridge"))
                        .expect("app URI"),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: app_text.to_owned(),
                },
            })
            .await;
        let mut index = None;
        for _ in 0..120 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            if let Some(idx) = server.workspace_index().await {
                index = Some(idx);
                break;
            }
        }
        let index = index.expect("index installed");
        app_uri = index
            .uri_to_module
            .keys()
            .find(|u| u.path().ends_with("Main.ridge"))
            .expect("app module")
            .clone();
        lib_uri = index
            .uri_to_module
            .keys()
            .find(|u| u.path().ends_with("Lib.ridge"))
            .expect("lib module — multi-member discovery")
            .clone();
    }
    std::mem::forget(dir);
    (service, socket, app_uri, lib_uri)
}

#[tokio::test]
async fn test_definition_cross_module() {
    let (service, _socket, app_uri, lib_uri) = two_member_fixture().await;
    let server = service.inner();

    // Go-to-def on `Lib.helper` (line 1, inside `helper`) → Lib.ridge.
    let resp = server
        .goto_definition(goto_at(&app_uri, 1, 26))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("cross-module definition");
    assert_eq!(loc.uri, lib_uri, "definition must land in Lib.ridge");
    assert_eq!(loc.range.start.line, 0, "helper is on line 1 of Lib.ridge");
}

#[tokio::test]
async fn test_definition_into_stdlib_symbol() {
    // `import std.list as L` plus a point-free use of `L.map` so the workspace
    // compiles and the index carries the qualified-name binding. Go-to-def on
    // `map` must land in the materialised stdlib source for `std.list`.
    let line1 = "pub fn run = L.map";
    let (service, _socket, uri) = hover_fixture("import std.list as L\npub fn run = L.map\n").await;
    let server = service.inner();

    // Cursor inside `map` (one char past the start of `map`).
    let col = u32::try_from(line1.find("L.map").expect("alias use") + 3).expect("offset fits u32");
    let resp = server
        .goto_definition(goto_at(&uri, 1, col))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of stdlib `map`");
    let path = loc
        .uri
        .to_file_path()
        .expect("definition uri is a file path");
    assert!(
        path.ends_with("list.ridge"),
        "stdlib definition must land in list.ridge, got {path:?}"
    );
    // `map` is declared well past the start of the file, so the range is real.
    assert!(
        loc.range.start.line > 0 || loc.range.start.character > 0,
        "stdlib definition range must not be the file start, got {:?}",
        loc.range.start
    );
}

#[tokio::test]
async fn test_definition_into_stdlib_module_alias() {
    // A bare reference to the alias `L` in value position carries the
    // `ModuleAlias` binding (the qualified `L.map` form binds the whole name as a
    // stdlib symbol instead). Go-to-def on that bare `L` resolves to the stdlib
    // module file at its start. The body has a type error — a module is not a
    // value — but the retained index still stamps the resolved binding.
    let line1 = "pub fn run = L";
    let (service, _socket, uri) = hover_fixture("import std.list as L\npub fn run = L\n").await;
    let server = service.inner();

    let col = u32::try_from(line1.rfind('L').expect("alias use")).expect("offset fits u32");
    let resp = server
        .goto_definition(goto_at(&uri, 1, col))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of stdlib module alias");
    let path = loc
        .uri
        .to_file_path()
        .expect("definition uri is a file path");
    assert!(
        path.ends_with("list.ridge"),
        "module-alias definition must land in list.ridge, got {path:?}"
    );
    assert_eq!(loc.range.start.line, 0, "module alias points at file start");
    assert_eq!(
        loc.range.start.character, 0,
        "module alias points at file start"
    );
}

#[tokio::test]
async fn test_definition_into_stdlib_class_method() {
    // A bare use of the fundep verb `filter` carries a `ClassMethod` binding
    // naming the `Refinable` class. The class is redeclared in the workspace so
    // the resolver's class-method index stamps the binding (the same trick the
    // deriving e2e tests use for `encode`/`decode`) without the full ridge.data
    // setup; go-to-def then resolves the verb to the canonical signature in the
    // materialised stdlib `repo.ridge`, not the workspace redeclaration.
    let src = concat!(
        "pub class Refinable q p | q -> p =\n",
        "  filter (pred: p) (x: q) -> q\n",
        "pub fn run q p -> q = filter p q\n",
    );
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Cursor inside the bare `filter` use on the last line (line 2).
    let line2 = "pub fn run q p -> q = filter p q";
    let col =
        u32::try_from(line2.find("filter").expect("filter use") + 1).expect("offset fits u32");
    let resp = server
        .goto_definition(goto_at(&uri, 2, col))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of stdlib class method `filter`");
    let path = loc
        .uri
        .to_file_path()
        .expect("definition uri is a file path");
    assert!(
        path.ends_with("repo.ridge"),
        "class-method definition must land in repo.ridge, got {path:?}"
    );
    // `filter` is declared well past the start of the file, so the range points
    // at the method signature rather than the file start.
    assert!(
        loc.range.start.line > 0 || loc.range.start.character > 0,
        "class-method definition range must not be the file start, got {:?}",
        loc.range.start
    );
}

// ── Test 19: textDocument/completion ──────────────────────────────────────────

fn complete_at(uri: &Url, line: u32, character: u32) -> CompletionParams {
    CompletionParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: None,
    }
}

fn completion_items(resp: Option<CompletionResponse>) -> Vec<CompletionItem> {
    match resp {
        Some(CompletionResponse::Array(items)) => items,
        Some(CompletionResponse::List(list)) => list.items,
        None => Vec::new(),
    }
}

#[tokio::test]
async fn test_completion_locals_module_and_misses() {
    // `counter` is a top-level fn; `foo` binds `count`, used in its body.
    let src = "pub fn counter = 1\npub fn foo count = count\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Inside the body `count` after typing `co` (line 1, char 21): the local
    // `count` (sort 0) ranks before the module fn `counter` (sort 1).
    let items = completion_items(
        server
            .completion(complete_at(&uri, 1, 21))
            .await
            .expect("ok"),
    );
    let count = items
        .iter()
        .find(|i| i.label == "count")
        .expect("local count");
    let counter = items
        .iter()
        .find(|i| i.label == "counter")
        .expect("module counter");
    assert_eq!(count.kind, Some(CompletionItemKind::VARIABLE));
    assert!(
        count.sort_text < counter.sort_text,
        "local must sort before module symbol: {:?} vs {:?}",
        count.sort_text,
        counter.sort_text
    );

    // Inside a comment → nothing.
    let comment_src = "-- todo: write foo\n";
    let (service, _socket, uri) = hover_fixture(comment_src).await;
    let server = service.inner();
    let items = completion_items(
        server
            .completion(complete_at(&uri, 0, 14))
            .await
            .expect("ok"),
    );
    assert!(
        items.is_empty(),
        "no completion inside a comment, got {items:?}"
    );
}

#[tokio::test]
async fn test_completion_member_access() {
    let (service, _socket, app_uri, _lib_uri) = two_member_fixture().await;
    let server = service.inner();

    // Right after `Lib.` (line 1, char 24) → the library's exported symbols.
    let items = completion_items(
        server
            .completion(complete_at(&app_uri, 1, 24))
            .await
            .expect("ok"),
    );
    assert!(
        items.iter().any(|i| i.label == "helper"),
        "member access should offer `helper`, got {:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_completion_stdlib_member_access() {
    // `import std.list as L` plus a point-free reference to a builtin function so
    // the workspace compiles and the retained index carries the `L.` line. The
    // member completion for a stdlib alias must offer that module's exports, the
    // way it already does for a workspace-module alias.
    let line1 = "pub fn run = L.map";
    let (service, _socket, uri) = hover_fixture("import std.list as L\npub fn run = L.map\n").await;
    let server = service.inner();

    // Right after `L.` on line 1 → std.list's exported names.
    let col = u32::try_from(line1.find("L.").expect("alias use") + 2).expect("offset fits u32");
    let items = completion_items(
        server
            .completion(complete_at(&uri, 1, col))
            .await
            .expect("ok"),
    );
    let labels: Vec<String> = items.into_iter().map(|i| i.label).collect();
    assert!(
        labels.iter().any(|l| l == "map"),
        "stdlib member access should offer `map`, got {labels:?}"
    );
}

#[tokio::test]
async fn test_completion_record_fields() {
    // A parameter of record type; `p.` should offer the record's field names,
    // resolved from the value's type rather than any module alias.
    let line1 = "pub fn run (p: P) -> Int = p.x";
    let (service, _socket, uri) =
        hover_fixture("pub type P = { x: Int, y: Int }\npub fn run (p: P) -> Int = p.x\n").await;
    let server = service.inner();

    // Right after `p.` on line 1 → the record's fields.
    let col = u32::try_from(line1.rfind("p.").expect("field access") + 2).expect("offset fits u32");
    let items = completion_items(
        server
            .completion(complete_at(&uri, 1, col))
            .await
            .expect("ok"),
    );
    let labels: Vec<String> = items.into_iter().map(|i| i.label).collect();
    assert!(
        labels.iter().any(|l| l == "x"),
        "record member access should offer field `x`, got {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == "y"),
        "record member access should offer field `y`, got {labels:?}"
    );
}

// ── Incremental: a didChange recompile reflects the buffer, not disk ───────────

/// After `didOpen`, a `didChange` that replaces the body must update the retained
/// index from the editor buffer — not the unchanged on-disk file.
#[tokio::test]
async fn test_didchange_incremental_reflects_buffer() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"inc-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    // On disk the function returns an Int.
    std::fs::write(app_src.join("Main.ridge"), "pub fn f = 1\n").expect("write source");

    let (service, _socket) = build_test_service();
    let server = service.inner();

    let root_uri = Url::from_file_path(&root).expect("root URI");
    server
        .initialize(InitializeParams {
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: "inc-ws".to_owned(),
            }]),
            capabilities: ClientCapabilities::default(),
            ..InitializeParams::default()
        })
        .await
        .expect("initialize");

    let file_uri = Url::from_file_path(app_src.join("Main.ridge")).expect("file URI");
    server
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: file_uri,
                language_id: "ridge".to_owned(),
                version: 1,
                text: "pub fn f = 1\n".to_owned(),
            },
        })
        .await;

    // Wait for the open compile, then take the canonical URI the index uses.
    let mut uri = None;
    for _ in 0..120 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        if let Some(idx) = server.workspace_index().await {
            if let Some(u) = idx.uri_to_module.keys().next() {
                uri = Some(u.clone());
                break;
            }
        }
    }
    let uri = uri.expect("index installed after didOpen");

    // Edit the buffer (not disk): the function now returns Text.
    let v2 = "pub fn f = \"hello\"\n";
    server
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: v2.to_owned(),
            }],
        })
        .await;

    // After the debounced incremental compile, hover on the new literal must
    // report Text (the buffer), not Int (the unchanged disk file).
    let col = u32::try_from(v2.find('"').expect("string literal") + 1).expect("offset fits u32");
    let mut hover = None;
    for _ in 0..120 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        if let Some(idx) = server.workspace_index().await {
            if let Some((markdown, _)) = idx.hover_at(&uri, 0, col) {
                if markdown.contains("Text") {
                    hover = Some(markdown);
                    break;
                }
            }
        }
    }
    assert!(
        hover.is_some(),
        "hover after the edit must report the buffer's Text type, not disk's Int"
    );
}

// ── Test 20: textDocument/references ───────────────────────────────────────────

fn references_at(
    uri: &Url,
    line: u32,
    character: u32,
    include_declaration: bool,
) -> ReferenceParams {
    ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        context: ReferenceContext {
            include_declaration,
        },
    }
}

/// The start lines (sorted) of the references that land in `uri`.
fn ref_lines(locs: &[Location], uri: &Url) -> Vec<u32> {
    let mut lines: Vec<u32> = locs
        .iter()
        .filter(|l| &l.uri == uri)
        .map(|l| l.range.start.line)
        .collect();
    lines.sort_unstable();
    lines
}

#[tokio::test]
async fn test_references_local() {
    // `foo` binds `x` at character 11; the body uses it at 15 and 19.
    let src = "pub fn foo x = x + x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // From a body use, includeDeclaration=true returns the parameter (col 11)
    // plus both body uses (cols 15 and 19), all on line 0.
    let with_decl = server
        .references(references_at(&uri, 0, 15, true))
        .await
        .expect("ok")
        .expect("references of local `x`");
    let cols: Vec<u32> = with_decl
        .iter()
        .filter(|l| l.uri == uri)
        .map(|l| l.range.start.character)
        .collect();
    assert_eq!(
        cols,
        vec![11, 15, 19],
        "includeDeclaration=true returns the binder and both uses"
    );

    // includeDeclaration=false drops the parameter, leaving the two body uses.
    let without_decl = server
        .references(references_at(&uri, 0, 15, false))
        .await
        .expect("ok")
        .expect("references of local `x`");
    let cols: Vec<u32> = without_decl
        .iter()
        .filter(|l| l.uri == uri)
        .map(|l| l.range.start.character)
        .collect();
    assert_eq!(
        cols,
        vec![15, 19],
        "includeDeclaration=false drops the binder"
    );

    // A keyword has no referent.
    let kw = server
        .references(references_at(&uri, 0, 4, true))
        .await
        .expect("ok");
    assert!(kw.is_none(), "a keyword has no references");
}

#[tokio::test]
async fn test_references_cross_module() {
    // `helper` is defined in Lib.ridge (line 0) and used as `Lib.helper` in the
    // app (line 1). A references query from the use-site must reach both files.
    let (service, _socket, app_uri, lib_uri) = two_member_fixture().await;
    let server = service.inner();

    // Cursor inside `helper` of `Lib.helper` (line 1, char 26).
    let with_decl = server
        .references(references_at(&app_uri, 1, 26, true))
        .await
        .expect("ok")
        .expect("references of `helper`");
    assert_eq!(
        ref_lines(&with_decl, &lib_uri),
        vec![0],
        "includeDeclaration=true reaches the definition in Lib.ridge"
    );
    assert_eq!(
        ref_lines(&with_decl, &app_uri),
        vec![1],
        "the app use-site is reported"
    );

    // includeDeclaration=false drops the Lib.ridge declaration, keeping the use.
    let without_decl = server
        .references(references_at(&app_uri, 1, 26, false))
        .await
        .expect("ok")
        .expect("references of `helper`");
    assert!(
        ref_lines(&without_decl, &lib_uri).is_empty(),
        "includeDeclaration=false drops the declaration"
    );
    assert_eq!(
        ref_lines(&without_decl, &app_uri),
        vec![1],
        "the app use-site survives"
    );
}

#[tokio::test]
async fn test_references_stdlib_symbol() {
    // Two point-free uses of `L.map`; references on one must find both. The
    // definition lives in the stdlib (outside the workspace), so the result is
    // the workspace use-sites regardless of the includeDeclaration flag.
    let line1 = "pub fn a = L.map";
    let (service, _socket, uri) =
        hover_fixture("import std.list as L\npub fn a = L.map\npub fn b = L.map\n").await;
    let server = service.inner();

    let col = u32::try_from(line1.find("L.map").expect("alias use") + 3).expect("offset fits u32");
    let refs = server
        .references(references_at(&uri, 1, col, true))
        .await
        .expect("ok")
        .expect("references of stdlib `map`");
    assert_eq!(
        ref_lines(&refs, &uri),
        vec![1, 2],
        "both `L.map` use-sites are found"
    );
}

#[tokio::test]
async fn test_references_class_method() {
    // The fundep verb `filter` is used on two lines. References on one use must
    // find the other; the class method itself lives in the stdlib, so the result
    // is the workspace use-sites. The class is redeclared so the resolver's
    // class-method index stamps the bindings (the same trick the goto test uses).
    let src = concat!(
        "pub class Refinable q p | q -> p =\n",
        "  filter (pred: p) (x: q) -> q\n",
        "pub fn run1 q p -> q = filter p q\n",
        "pub fn run2 q p -> q = filter p q\n",
    );
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    let line2 = "pub fn run1 q p -> q = filter p q";
    let col =
        u32::try_from(line2.find("filter").expect("filter use") + 1).expect("offset fits u32");
    let refs = server
        .references(references_at(&uri, 2, col, true))
        .await
        .expect("ok")
        .expect("references of class method `filter`");
    let hits = ref_lines(&refs, &uri);
    assert!(
        hits.contains(&2) && hits.contains(&3),
        "both bare `filter` use-sites are found, got {hits:?}"
    );
}

// ── Test 21: textDocument/rename + prepareRename ──────────────────────────────

fn prepare_rename_at(uri: &Url, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position { line, character },
    }
}

fn rename_at(uri: &Url, line: u32, character: u32, new_name: &str) -> RenameParams {
    RenameParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        new_name: new_name.to_owned(),
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

/// The `(start.character, new_text)` of every edit a rename produced for `uri`,
/// sorted by start character.
fn rename_edits(edit: Option<&WorkspaceEdit>, uri: &Url) -> Vec<(u32, String)> {
    let Some(edit) = edit else {
        return Vec::new();
    };
    let Some(changes) = &edit.changes else {
        return Vec::new();
    };
    let mut out: Vec<(u32, String)> = changes
        .get(uri)
        .map(|edits| {
            edits
                .iter()
                .map(|e| (e.range.start.character, e.new_text.clone()))
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|(col, _)| *col);
    out
}

#[tokio::test]
async fn test_rename_local() {
    // `foo` binds `x` at character 11; the body uses it at 15 and 19. Renaming
    // from a body use rewrites the binder and both uses to the new name.
    let src = "pub fn foo x = x + x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // prepareRename underlines the identifier and offers its current text.
    let prep = server
        .prepare_rename(prepare_rename_at(&uri, 0, 15))
        .await
        .expect("ok")
        .expect("a local is renameable");
    match prep {
        PrepareRenameResponse::RangeWithPlaceholder { range, placeholder } => {
            assert_eq!(range.start.character, 15, "underlines the use under cursor");
            assert_eq!(placeholder, "x", "placeholder is the current name");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }

    let edit = server
        .rename(rename_at(&uri, 0, 15, "y"))
        .await
        .expect("ok");
    let edits = rename_edits(edit.as_ref(), &uri);
    assert_eq!(
        edits,
        vec![
            (11, "y".to_owned()),
            (15, "y".to_owned()),
            (19, "y".to_owned()),
        ],
        "rename rewrites the binder and both uses"
    );
}

#[tokio::test]
async fn test_rename_cross_module_fn_preserves_qualifier() {
    // `helper` is defined in Lib.ridge and used as `Lib.helper` in the app.
    // Renaming it must rewrite the declaration and only the `helper` segment of
    // the qualified use — the `Lib.` qualifier must survive.
    let (service, _socket, app_uri, lib_uri) = two_member_fixture().await;
    let server = service.inner();

    let edit = server
        .rename(rename_at(&app_uri, 1, 26, "helper2"))
        .await
        .expect("ok");

    // The app edit lands on `helper` (char 24), NOT on `Lib.helper` (char 20).
    assert_eq!(
        rename_edits(edit.as_ref(), &app_uri),
        vec![(24, "helper2".to_owned())],
        "only the final segment of `Lib.helper` is rewritten"
    );
    // The declaration in Lib.ridge is rewritten at the name token (char 7).
    assert_eq!(
        rename_edits(edit.as_ref(), &lib_uri),
        vec![(7, "helper2".to_owned())],
        "the declaration name is rewritten"
    );
}

#[tokio::test]
async fn test_rename_from_declaration() {
    // Renaming from the declaration name itself (which carries no binding) is
    // resolved through the symbol table.
    let src = "pub fn foo x = x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    let prep = server
        .prepare_rename(prepare_rename_at(&uri, 0, 7))
        .await
        .expect("ok")
        .expect("a declaration name is renameable");
    match prep {
        PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } => {
            assert_eq!(placeholder, "foo");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }

    let edit = server
        .rename(rename_at(&uri, 0, 7, "bar"))
        .await
        .expect("ok");
    assert_eq!(
        rename_edits(edit.as_ref(), &uri),
        vec![(7, "bar".to_owned())],
        "the declaration name is rewritten"
    );
}

#[tokio::test]
async fn test_rename_rejects_invalid_name_and_non_renameable() {
    let src = "pub fn foo x = x + x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // A reserved keyword is rejected with an error.
    let err = server.rename(rename_at(&uri, 0, 15, "if")).await;
    assert!(err.is_err(), "renaming to a keyword must error");

    // An empty name is rejected.
    let err = server.rename(rename_at(&uri, 0, 15, "")).await;
    assert!(err.is_err(), "renaming to an empty name must error");

    // A capitalised name is invalid for a local.
    let err = server.rename(rename_at(&uri, 0, 15, "X")).await;
    assert!(err.is_err(), "a local must stay a lowercase identifier");

    // A keyword has no renameable referent.
    let prep = server
        .prepare_rename(prepare_rename_at(&uri, 0, 4))
        .await
        .expect("ok");
    assert!(prep.is_none(), "a keyword cannot be renamed");
}

#[tokio::test]
async fn test_rename_stdlib_symbol_is_not_renameable() {
    // A stdlib symbol is defined outside the workspace, so it cannot be renamed
    // (its use-sites would drift from the unchanged stdlib definition).
    let line1 = "pub fn a = L.map";
    let (service, _socket, uri) = hover_fixture("import std.list as L\npub fn a = L.map\n").await;
    let server = service.inner();

    let col = u32::try_from(line1.find("L.map").expect("alias use") + 3).expect("offset fits u32");
    let prep = server
        .prepare_rename(prepare_rename_at(&uri, 1, col))
        .await
        .expect("ok");
    assert!(prep.is_none(), "a stdlib symbol must not be renameable");

    let edit = server
        .rename(rename_at(&uri, 1, col, "myMap"))
        .await
        .expect("ok");
    assert!(edit.is_none(), "rename of a stdlib symbol yields no edit");
}

// ── Test 22: textDocument/documentHighlight ───────────────────────────────────

fn highlight_at(uri: &Url, line: u32, character: u32) -> DocumentHighlightParams {
    DocumentHighlightParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

/// The `(start.line, start.character, kind)` of every highlight, sorted.
fn highlight_spots(hs: &[DocumentHighlight]) -> Vec<(u32, u32, DocumentHighlightKind)> {
    let mut out: Vec<(u32, u32, DocumentHighlightKind)> = hs
        .iter()
        .map(|h| {
            (
                h.range.start.line,
                h.range.start.character,
                h.kind.expect("a highlight carries a kind"),
            )
        })
        .collect();
    out.sort_by_key(|(line, ch, _)| (*line, *ch));
    out
}

#[tokio::test]
async fn test_highlight_local() {
    // `foo` binds `x` at character 11; the body uses it at 15 and 19. A highlight
    // from a body use marks the binder as a write and both uses as reads.
    let src = "pub fn foo x = x + x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    let hs = server
        .document_highlight(highlight_at(&uri, 0, 15))
        .await
        .expect("ok")
        .expect("highlights of local `x`");
    assert_eq!(
        highlight_spots(&hs),
        vec![
            (0, 11, DocumentHighlightKind::WRITE),
            (0, 15, DocumentHighlightKind::READ),
            (0, 19, DocumentHighlightKind::READ),
        ],
        "the binder is a write, both uses are reads"
    );
}

#[tokio::test]
async fn test_highlight_is_same_file_only() {
    // `helper` is declared in Lib.ridge and used as `Lib.helper` in the app.
    // documentHighlight never leaves the cursor's file: from the app use it marks
    // only the `helper` segment there; from the Lib declaration it marks only the
    // declaration name.
    let (service, _socket, app_uri, lib_uri) = two_member_fixture().await;
    let server = service.inner();

    // From the app use-site: only `helper` (char 24), not `Lib.helper` (char 20),
    // and nothing from Lib.ridge — same file only.
    let app = server
        .document_highlight(highlight_at(&app_uri, 1, 26))
        .await
        .expect("ok")
        .expect("highlights of `helper` in the app");
    assert_eq!(
        highlight_spots(&app),
        vec![(1, 24, DocumentHighlightKind::READ)],
        "only the final segment of `Lib.helper`, this file only"
    );

    // From the Lib declaration name: the declaration is the write site, with no
    // uses inside Lib.ridge.
    let lib = server
        .document_highlight(highlight_at(&lib_uri, 0, 7))
        .await
        .expect("ok")
        .expect("highlights of `helper` in Lib.ridge");
    assert_eq!(
        highlight_spots(&lib),
        vec![(0, 7, DocumentHighlightKind::WRITE)],
        "the declaration name is the write site"
    );
}

#[tokio::test]
async fn test_highlight_stdlib_symbol_same_file() {
    // Two point-free uses of `L.map` in one file; a highlight on one marks both
    // (reads), narrowed to the `map` segment. The definition lives in the stdlib,
    // so there is no write site.
    let line1 = "pub fn a = L.map";
    let (service, _socket, uri) =
        hover_fixture("import std.list as L\npub fn a = L.map\npub fn b = L.map\n").await;
    let server = service.inner();

    let cursor = u32::try_from(line1.find("L.map").expect("alias use") + 3).expect("fits u32");
    let map_col = u32::try_from(line1.find("L.map").expect("alias use") + 2).expect("fits u32");
    let hs = server
        .document_highlight(highlight_at(&uri, 1, cursor))
        .await
        .expect("ok")
        .expect("highlights of stdlib `map`");
    assert_eq!(
        highlight_spots(&hs),
        vec![
            (1, map_col, DocumentHighlightKind::READ),
            (2, map_col, DocumentHighlightKind::READ),
        ],
        "both `map` uses are reads, narrowed to the final segment"
    );
}

#[tokio::test]
async fn test_highlight_none_on_keyword() {
    // A keyword is not a name; documentHighlight yields nothing.
    let src = "pub fn foo x = x + x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    let hs = server
        .document_highlight(highlight_at(&uri, 0, 4))
        .await
        .expect("ok");
    assert!(hs.is_none(), "a keyword has no highlights");
}

// ── Test 23: type references (go-to-definition, references, highlight) ─────────

// A `type` and two uses of it in annotations: the parameter type at column 16
// and the return type at column 26 of line 1.
const TYPE_REF_SRC: &str = "type Color = Red | Green\npub fn pick (c: Color) -> Color = c\n";

#[tokio::test]
async fn test_definition_on_type_reference() {
    let (service, _socket, uri) = hover_fixture(TYPE_REF_SRC).await;
    let server = service.inner();

    // From the parameter annotation `Color`.
    let resp = server
        .goto_definition(goto_at(&uri, 1, 16))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of the type `Color`");
    assert_eq!(loc.uri, uri, "the type is declared in the same file");
    assert_eq!(loc.range.start.line, 0, "`Color` is declared on line 0");

    // From the return-type `Color` — the same definition.
    let resp = server
        .goto_definition(goto_at(&uri, 1, 26))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition from the return type");
    assert_eq!(
        loc.range.start.line, 0,
        "both annotations resolve to the type"
    );
}

#[tokio::test]
async fn test_references_on_type() {
    let (service, _socket, uri) = hover_fixture(TYPE_REF_SRC).await;
    let server = service.inner();

    // From a use, with the declaration: the decl (line 0) plus both
    // type-position uses (line 1).
    let with_decl = server
        .references(references_at(&uri, 1, 16, true))
        .await
        .expect("ok")
        .expect("references of `Color`");
    assert_eq!(
        ref_lines(&with_decl, &uri),
        vec![0, 1, 1],
        "the declaration plus both annotation uses"
    );

    // Without the declaration: only the two uses.
    let without = server
        .references(references_at(&uri, 1, 16, false))
        .await
        .expect("ok")
        .expect("references of `Color`");
    assert_eq!(
        ref_lines(&without, &uri),
        vec![1, 1],
        "both uses, declaration dropped"
    );
}

#[tokio::test]
async fn test_highlight_on_type() {
    let (service, _socket, uri) = hover_fixture(TYPE_REF_SRC).await;
    let server = service.inner();

    let expected = vec![
        (0, 5, DocumentHighlightKind::WRITE),
        (1, 16, DocumentHighlightKind::READ),
        (1, 26, DocumentHighlightKind::READ),
    ];

    // From a use: the declaration name is the write, both annotations are reads.
    let from_use = server
        .document_highlight(highlight_at(&uri, 1, 16))
        .await
        .expect("ok")
        .expect("highlights of `Color`");
    assert_eq!(
        highlight_spots(&from_use),
        expected,
        "the declaration name is the write site, the annotations are reads"
    );

    // From the declaration name itself: the same highlight set (cursor-on-decl
    // is resolved through the symbol table).
    let from_decl = server
        .document_highlight(highlight_at(&uri, 0, 5))
        .await
        .expect("ok")
        .expect("highlights from the type declaration");
    assert_eq!(
        highlight_spots(&from_decl),
        expected,
        "cursor on the type declaration highlights it and all its uses"
    );
}

/// Build a two-member workspace whose library exports a `pub type` and whose
/// app selectively imports it and uses it in an annotation. Returns the service
/// plus the app and lib URIs.
async fn imported_type_fixture() -> (
    tower_lsp::LspService<RidgeLanguageServer>,
    tower_lsp::ClientSocket,
    Url,
    Url,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("lib").join("src")).expect("lib src");
    std::fs::create_dir_all(root.join("app").join("src")).expect("app src");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"type-ws\"\nversion = \"0.1.0\"\nmembers = [\"lib\", \"app\"]\n",
    )
    .expect("workspace manifest");
    std::fs::write(
        root.join("lib").join("ridge.toml"),
        "[project]\nname = \"lib\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("lib manifest");
    std::fs::write(
        root.join("lib").join("src").join("Lib.ridge"),
        "pub type Color = Red | Green\n",
    )
    .expect("lib source");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("app manifest");
    let app_text = "import lib.Lib (Color)\npub fn pick (c: Color) -> Color = c\n";
    std::fs::write(root.join("app").join("src").join("Main.ridge"), app_text).expect("app source");

    let (service, socket) = build_test_service();
    let app_uri;
    let lib_uri;
    {
        let server = service.inner();
        let root_uri = Url::from_file_path(&root).expect("root URI");
        server
            .initialize(InitializeParams {
                root_uri: Some(root_uri.clone()),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri,
                    name: "type-ws".to_owned(),
                }]),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Url::from_file_path(root.join("app").join("src").join("Main.ridge"))
                        .expect("app URI"),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: app_text.to_owned(),
                },
            })
            .await;
        let mut index = None;
        for _ in 0..120 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            if let Some(idx) = server.workspace_index().await {
                index = Some(idx);
                break;
            }
        }
        let index = index.expect("index installed");
        app_uri = index
            .uri_to_module
            .keys()
            .find(|u| u.path().ends_with("Main.ridge"))
            .expect("app module")
            .clone();
        lib_uri = index
            .uri_to_module
            .keys()
            .find(|u| u.path().ends_with("Lib.ridge"))
            .expect("lib module — multi-member discovery")
            .clone();
    }
    std::mem::forget(dir);
    (service, socket, app_uri, lib_uri)
}

#[tokio::test]
async fn test_definition_on_imported_type() {
    let (service, _socket, app_uri, lib_uri) = imported_type_fixture().await;
    let server = service.inner();

    // Go-to-definition on the parameter annotation `Color` jumps across files to
    // the `pub type Color` declaration in Lib.ridge.
    let resp = server
        .goto_definition(goto_at(&app_uri, 1, 16))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of the imported type");
    assert_eq!(loc.uri, lib_uri, "the type is declared in Lib.ridge");
    assert_eq!(loc.range.start.line, 0, "`Color` is on line 0 of Lib.ridge");
}

// ── Test 24: type rename ──────────────────────────────────────────────────────

// A union type and two type-position uses; the constructor `Red` shares no name
// with the type, so renaming `Color` must leave it untouched.
const UNION_RENAME_SRC: &str = "type Color = Red | Green\npub fn pick (c: Color) -> Color = Red\n";

// A record type used in a construction (line 1), an annotation (line 2), and a
// pattern (line 4). The construction and annotation key to the type symbol; the
// pattern keys to the auto-constructor, so a complete rename needs both.
const RECORD_RENAME_SRC: &str = "type User = { name: Text }\npub fn make = User { name = \"a\" }\npub fn greet (u: User) -> Text =\n    match u\n        User { name } -> name\n";

/// The `(start.line, start.character)` of every edit a rename produced for `uri`.
fn rename_sites(edit: Option<&WorkspaceEdit>, uri: &Url) -> Vec<(u32, u32)> {
    let Some(changes) = edit.and_then(|e| e.changes.as_ref()) else {
        return Vec::new();
    };
    let mut out: Vec<(u32, u32)> = changes
        .get(uri)
        .map(|edits| {
            edits
                .iter()
                .map(|e| (e.range.start.line, e.range.start.character))
                .collect()
        })
        .unwrap_or_default();
    out.sort_unstable();
    out
}

#[tokio::test]
async fn test_prepare_rename_on_type() {
    let (service, _socket, uri) = hover_fixture(UNION_RENAME_SRC).await;
    let server = service.inner();

    // From an annotation use.
    let prep = server
        .prepare_rename(prepare_rename_at(&uri, 1, 16))
        .await
        .expect("ok")
        .expect("a type is renameable from a use");
    match prep {
        PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } => {
            assert_eq!(placeholder, "Color", "placeholder is the current type name");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }

    // From the declaration name.
    let prep = server
        .prepare_rename(prepare_rename_at(&uri, 0, 5))
        .await
        .expect("ok")
        .expect("a type is renameable from its declaration");
    match prep {
        PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } => {
            assert_eq!(placeholder, "Color");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }
}

#[tokio::test]
async fn test_rename_union_type_leaves_constructors() {
    let (service, _socket, uri) = hover_fixture(UNION_RENAME_SRC).await;
    let server = service.inner();

    let edit = server
        .rename(rename_at(&uri, 1, 16, "Hue"))
        .await
        .expect("ok");
    // The declaration (line 0) and both annotations (line 1) are rewritten; the
    // constructor `Red` is a different name and is left alone.
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        vec![(0, 5), (1, 16), (1, 26)],
        "declaration plus both type-position uses, not the constructor"
    );
    assert!(
        rename_edits(edit.as_ref(), &uri)
            .iter()
            .all(|(_, t)| t == "Hue"),
        "every edit rewrites to the new name"
    );
}

#[tokio::test]
async fn test_rename_record_type_includes_pattern_and_construction() {
    let (service, _socket, uri) = hover_fixture(RECORD_RENAME_SRC).await;
    let server = service.inner();

    // Rename from the parameter annotation (line 2).
    let edit = server
        .rename(rename_at(&uri, 2, 17, "Person"))
        .await
        .expect("ok");
    // Declaration (0,5), construction (1,14), annotation (2,17), and pattern
    // (4,8) all move together — the pattern only because the constructor key is
    // renamed alongside the type symbol.
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        vec![(0, 5), (1, 14), (2, 17), (4, 8)],
        "a record rename covers the declaration, construction, annotation, and pattern"
    );
}

#[tokio::test]
async fn test_rename_record_type_from_pattern() {
    let (service, _socket, uri) = hover_fixture(RECORD_RENAME_SRC).await;
    let server = service.inner();

    // Rename starting from the pattern `User { name }` (line 4) — the same
    // complete edit set as starting from the annotation.
    let edit = server
        .rename(rename_at(&uri, 4, 8, "Person"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        vec![(0, 5), (1, 14), (2, 17), (4, 8)],
        "renaming from the pattern reaches the type and every use"
    );
}

#[tokio::test]
async fn test_rename_type_rejects_lowercase() {
    let (service, _socket, uri) = hover_fixture(UNION_RENAME_SRC).await;
    let server = service.inner();

    // A type must stay an uppercase identifier.
    let err = server.rename(rename_at(&uri, 1, 16, "hue")).await;
    assert!(err.is_err(), "a type cannot be renamed to a lowercase name");
}

/// Build a two-member workspace from explicit library and app sources. Returns
/// the service plus the app and lib URIs.
async fn two_member_ws(
    lib_src: &str,
    app_src: &str,
) -> (
    tower_lsp::LspService<RidgeLanguageServer>,
    tower_lsp::ClientSocket,
    Url,
    Url,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("lib").join("src")).expect("lib src");
    std::fs::create_dir_all(root.join("app").join("src")).expect("app src");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"rename-ws\"\nversion = \"0.1.0\"\nmembers = [\"lib\", \"app\"]\n",
    )
    .expect("workspace manifest");
    std::fs::write(
        root.join("lib").join("ridge.toml"),
        "[project]\nname = \"lib\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("lib manifest");
    std::fs::write(root.join("lib").join("src").join("Lib.ridge"), lib_src).expect("lib source");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("app manifest");
    std::fs::write(root.join("app").join("src").join("Main.ridge"), app_src).expect("app source");

    let (service, socket) = build_test_service();
    let app_uri;
    let lib_uri;
    {
        let server = service.inner();
        let root_uri = Url::from_file_path(&root).expect("root URI");
        server
            .initialize(InitializeParams {
                root_uri: Some(root_uri.clone()),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri,
                    name: "rename-ws".to_owned(),
                }]),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Url::from_file_path(root.join("app").join("src").join("Main.ridge"))
                        .expect("app URI"),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: app_src.to_owned(),
                },
            })
            .await;
        let mut index = None;
        for _ in 0..120 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            if let Some(idx) = server.workspace_index().await {
                index = Some(idx);
                break;
            }
        }
        let index = index.expect("index installed");
        app_uri = index
            .uri_to_module
            .keys()
            .find(|u| u.path().ends_with("Main.ridge"))
            .expect("app module")
            .clone();
        lib_uri = index
            .uri_to_module
            .keys()
            .find(|u| u.path().ends_with("Lib.ridge"))
            .expect("lib module — multi-member discovery")
            .clone();
    }
    std::mem::forget(dir);
    (service, socket, app_uri, lib_uri)
}

#[tokio::test]
async fn test_rename_imported_type_across_files() {
    // A `pub type` in the library, selectively imported and used in two
    // annotations in the app. Renaming it rewrites the declaration in the
    // library, both annotations, and the import clause that names it.
    let lib = "pub type User = { name: Text }\n";
    let app = "import lib.Lib (User)\npub fn greet (u: User) -> User = u\n";
    let (service, _socket, app_uri, lib_uri) = two_member_ws(lib, app).await;
    let server = service.inner();

    let edit = server
        .rename(rename_at(&app_uri, 1, 17, "Person"))
        .await
        .expect("ok");

    // The library declaration name (`pub type ` is 9 columns).
    assert_eq!(
        rename_sites(edit.as_ref(), &lib_uri),
        vec![(0, 9)],
        "the declaration in the library is rewritten"
    );
    // The import clause (line 0) plus both annotations (line 1) in the app.
    assert_eq!(
        rename_sites(edit.as_ref(), &app_uri),
        vec![(0, 16), (1, 17), (1, 26)],
        "the import clause and both annotations are rewritten"
    );
}

// ── Test 25: document formatting (textDocument/formatting) ─────────────────────

/// Open a single in-memory document under a throwaway workspace root and return
/// the service plus the document URI. Formatting reads the raw buffer, so no
/// compile needs to finish first.
async fn open_single_doc(
    text: &str,
) -> (
    tower_lsp::LspService<RidgeLanguageServer>,
    tower_lsp::ClientSocket,
    Url,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let root_uri = Url::from_file_path(&root).expect("root URI");
    let file_uri = Url::from_file_path(root.join("main.ridge")).expect("file URI");
    let (service, socket) = build_test_service();
    {
        let server = service.inner();
        server
            .initialize(InitializeParams {
                root_uri: Some(root_uri),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: file_uri.clone(),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: text.to_owned(),
                },
            })
            .await;
    }
    std::mem::forget(dir);
    (service, socket, file_uri)
}

fn format_params(uri: &Url) -> DocumentFormattingParams {
    DocumentFormattingParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        options: FormattingOptions::default(),
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

#[tokio::test]
async fn test_formatting_reformats_messy_source() {
    // Parseable but mis-spaced and over-indented, so the formatter has work.
    let messy = "fn  add (x: Int) (y: Int) -> Int =\n        x+y\n";
    let (service, _socket, uri) = open_single_doc(messy).await;
    let server = service.inner();

    let edits = server
        .formatting(format_params(&uri))
        .await
        .expect("formatting ok")
        .expect("a messy buffer yields an edit");

    // A whole-document replacement is a single edit from the origin to the end
    // of the buffer (the input has two lines plus a trailing newline).
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].range.start, Position::new(0, 0));
    assert_eq!(edits[0].range.end, Position::new(2, 0));
    // The replacement is exactly what `ridge-fmt` produces — the server is a
    // thin wrapper over the engine the CLI's `ridge fmt` already uses.
    let expected = ridge_fmt::format_source(messy).expect("formats");
    assert_eq!(edits[0].new_text, expected);
    assert_ne!(edits[0].new_text, messy);
}

#[tokio::test]
async fn test_formatting_noop_on_already_formatted() {
    // Feed the formatter's own output back in: nothing is left to change.
    let formatted = ridge_fmt::format_source("fn  add (x: Int) (y: Int) -> Int =\n        x+y\n")
        .expect("formats");
    let (service, _socket, uri) = open_single_doc(&formatted).await;
    let server = service.inner();

    let edits = server
        .formatting(format_params(&uri))
        .await
        .expect("formatting ok");
    assert!(
        edits.is_none(),
        "an already-formatted buffer yields no edits"
    );
}

#[tokio::test]
async fn test_formatting_skips_unparseable() {
    let broken = "fn = = =\n";
    // Precondition: the formatter rejects this buffer.
    assert!(ridge_fmt::format_source(broken).is_err());
    let (service, _socket, uri) = open_single_doc(broken).await;
    let server = service.inner();

    let edits = server
        .formatting(format_params(&uri))
        .await
        .expect("formatting ok");
    assert!(
        edits.is_none(),
        "a buffer the parser rejects is left untouched"
    );
}

// ── Test 26: document & workspace symbols ─────────────────────────────────────

const SYMBOL_SRC: &str = r"type Color = Red | Green | Blue
type User = { name: Text, age: Int }
const maxAge: Int = 120
pub fn greet (u: User) -> Text = u.name
actor Counter =
    state count: Int = 0

    on bump () -> Unit =
        count <- count + 1
";

fn doc_symbol_params(uri: &Url) -> DocumentSymbolParams {
    DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

fn child_names(sym: &DocumentSymbol) -> Vec<String> {
    sym.children
        .as_ref()
        .map(|cs| cs.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn test_document_symbol_outline() {
    let (service, _socket, uri) = hover_fixture(SYMBOL_SRC).await;
    let server = service.inner();

    let resp = server
        .document_symbol(doc_symbol_params(&uri))
        .await
        .expect("documentSymbol ok")
        .expect("an outline for a non-empty module");
    let DocumentSymbolResponse::Nested(symbols) = resp else {
        panic!("expected a nested outline");
    };

    // Top-level declarations, in source order.
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Color", "User", "maxAge", "greet", "Counter"]);

    // A union is an enum whose variants are its members.
    let color = &symbols[0];
    assert_eq!(color.kind, SymbolKind::ENUM);
    assert_eq!(child_names(color), ["Red", "Green", "Blue"]);

    // A record is a struct whose fields are its members.
    let user = &symbols[1];
    assert_eq!(user.kind, SymbolKind::STRUCT);
    assert_eq!(child_names(user), ["name", "age"]);

    assert_eq!(symbols[2].kind, SymbolKind::CONSTANT);
    assert_eq!(symbols[3].kind, SymbolKind::FUNCTION);

    // An actor is a class holding its state fields and message handlers.
    let counter = &symbols[4];
    assert_eq!(counter.kind, SymbolKind::CLASS);
    assert_eq!(child_names(counter), ["count", "bump"]);

    // The selection range (the name) sits inside the full declaration range.
    assert!(color.selection_range.start >= color.range.start);
    assert!(color.selection_range.end <= color.range.end);
}

#[tokio::test]
async fn test_workspace_symbol_query() {
    let (service, _socket, _uri) = hover_fixture(SYMBOL_SRC).await;
    let server = service.inner();

    let query = |q: &str| {
        let q = q.to_owned();
        async {
            server
                .symbol(WorkspaceSymbolParams {
                    query: q,
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                })
                .await
                .expect("symbol ok")
                .unwrap_or_default()
        }
    };

    // An empty query returns every top-level declaration plus union variants,
    // but no record auto-constructor or field accessor.
    let all = query("").await;
    let mut names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["Blue", "Color", "Counter", "Green", "Red", "User", "greet", "maxAge"]
    );

    // A substring query is case-insensitive.
    let greet = query("GREET").await;
    assert_eq!(greet.len(), 1);
    assert_eq!(greet[0].name, "greet");
    assert_eq!(greet[0].kind, SymbolKind::FUNCTION);

    // A union variant resolves to an enum member.
    let red = query("Red").await;
    assert_eq!(red.len(), 1);
    assert_eq!(red[0].kind, SymbolKind::ENUM_MEMBER);
}

// ── Test 27: inlay hints (textDocument/inlayHint) ─────────────────────────────

const INLAY_SRC: &str = r#"pub fn demo () -> Int =
    let count = 5
    let label = "hi"
    let annotated: Int = 9
    count + annotated
"#;

fn inlay_label(h: &InlayHint) -> String {
    match &h.label {
        InlayHintLabel::String(s) => s.clone(),
        InlayHintLabel::LabelParts(_) => String::new(),
    }
}

#[tokio::test]
async fn test_inlay_hints_for_unannotated_bindings() {
    let (service, _socket, uri) = hover_fixture(INLAY_SRC).await;
    let server = service.inner();

    let hints = server
        .inlay_hint(InlayHintParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range {
                start: Position::new(0, 0),
                end: Position::new(100, 0),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("inlayHint ok")
        .expect("a module with bindings yields hints");

    // Two un-annotated lets get a hint; `annotated: Int` does not.
    let rendered: Vec<(u32, u32, String)> = hints
        .iter()
        .map(|h| (h.position.line, h.position.character, inlay_label(h)))
        .collect();
    assert_eq!(
        rendered,
        vec![(1, 13, ": Int".to_owned()), (2, 13, ": Text".to_owned())],
        "hints sit after the binder name and skip the annotated binding"
    );
    assert_eq!(hints[0].kind, Some(InlayHintKind::TYPE));
}

#[tokio::test]
async fn test_inlay_hints_clip_to_range() {
    let (service, _socket, uri) = hover_fixture(INLAY_SRC).await;
    let server = service.inner();

    // Ask only for line 2, so the line-1 binding's hint is clipped out.
    let hints = server
        .inlay_hint(InlayHintParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range {
                start: Position::new(2, 0),
                end: Position::new(2, 100),
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
        })
        .await
        .expect("inlayHint ok")
        .expect("hints present");

    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].position, Position::new(2, 13));
    assert_eq!(inlay_label(&hints[0]), ": Text");
}

// ── Test 28: capability code action (textDocument/codeAction) ─────────────────

/// Build a hermetic single-file workspace whose manifest allows the `io`
/// capability, open `main_src`, and return the service plus the index-held URI
/// once a compile has produced an analysis index.
async fn cap_workspace_fixture(
    main_src: &str,
) -> (
    tower_lsp::LspService<RidgeLanguageServer>,
    tower_lsp::ClientSocket,
    Url,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"cap-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"io\"]\n",
    )
    .expect("project manifest");
    std::fs::write(app_src.join("Main.ridge"), main_src).expect("write source");

    let (service, socket) = build_test_service();
    let file_uri;
    {
        let server = service.inner();
        let root_uri = Url::from_file_path(&root).expect("root URI");
        server
            .initialize(InitializeParams {
                root_uri: Some(root_uri.clone()),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri,
                    name: "cap-ws".to_owned(),
                }]),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Url::from_file_path(app_src.join("Main.ridge")).expect("file URI"),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: main_src.to_owned(),
                },
            })
            .await;
        let mut index = None;
        for _ in 0..120 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            if let Some(idx) = server.workspace_index().await {
                index = Some(idx);
                break;
            }
        }
        let index = index.expect("an index after compile");
        file_uri = index
            .uri_to_module
            .keys()
            .next()
            .expect("one module in index")
            .clone();
    }
    std::mem::forget(dir);
    (service, socket, file_uri)
}

#[tokio::test]
async fn test_code_action_adds_missing_capability() {
    // `greet` calls `Io.println` (needs `io`) but declares no capabilities, so
    // the type checker raises T014. A quick-fix offers to add `io`.
    let src = "import std.io as Io\n\npub fn greet () -> Unit =\n    Io.println \"hi\"\n";
    let (service, _socket, uri) = cap_workspace_fixture(src).await;
    let server = service.inner();

    let resp = server
        .code_action(CodeActionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range {
                start: Position::new(2, 8),
                end: Position::new(2, 8),
            },
            context: CodeActionContext::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("code_action ok")
        .expect("a quick-fix is offered on the flagged function");

    assert_eq!(resp.len(), 1);
    let CodeActionOrCommand::CodeAction(action) = &resp[0] else {
        panic!("expected a CodeAction, got {:?}", resp[0]);
    };
    assert_eq!(action.title, "Add capability `io` to `greet`");
    assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));

    // The edit inserts `io ` immediately before the function name (`pub fn ` is
    // seven columns).
    let edits = action
        .edit
        .as_ref()
        .and_then(|e| e.changes.as_ref())
        .and_then(|c| c.get(&uri))
        .expect("an edit for this document");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "io ");
    assert_eq!(edits[0].range.start, Position::new(2, 7));
    assert_eq!(edits[0].range.end, Position::new(2, 7));
}

#[tokio::test]
async fn test_code_action_none_when_clean() {
    // A function that declares the capability it uses raises no T014, so there
    // is no quick-fix.
    let src = "import std.io as Io\n\npub fn io greet () -> Unit =\n    Io.println \"hi\"\n";
    let (service, _socket, uri) = cap_workspace_fixture(src).await;
    let server = service.inner();

    let resp = server
        .code_action(CodeActionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range {
                start: Position::new(2, 8),
                end: Position::new(2, 8),
            },
            context: CodeActionContext::default(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("code_action ok");

    assert!(
        resp.is_none(),
        "no quick-fix on a correctly-annotated function"
    );
}

// ── Test 29: signature help (textDocument/signatureHelp) ──────────────────────

fn signature_at(uri: &Url, line: u32, character: u32) -> SignatureHelpParams {
    SignatureHelpParams {
        context: None,
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

/// The single signature's label and active-parameter index.
fn sig_label_active(help: Option<SignatureHelp>) -> Option<(String, u32)> {
    let help = help?;
    let active = help.active_parameter?;
    let sig = help.signatures.into_iter().next()?;
    Some((sig.label, active))
}

/// The `[start, end)` UTF-16 offsets of each parameter in the first signature.
fn param_offsets(help: &SignatureHelp) -> Vec<(u32, u32)> {
    help.signatures[0]
        .parameters
        .as_ref()
        .expect("parameters present")
        .iter()
        .map(|p| match &p.label {
            ParameterLabel::LabelOffsets([s, e]) => (*s, *e),
            ParameterLabel::Simple(_) => panic!("expected label offsets, not a string label"),
        })
        .collect()
}

#[tokio::test]
async fn test_signature_help_stdlib_fn() {
    // `L.map` is a plain stdlib `fn`; its signature is read from the materialised
    // `list.ridge`. The call has two argument atoms, so the active parameter
    // follows which one the cursor sits in.
    let src = "import std.list as L\npub fn run f xs = L.map f xs\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Line 1: `pub fn run f xs = L.map f xs`. The call starts at column 18; its
    // arguments `f` and `xs` are at columns 24 and 26.
    let help = server
        .signature_help(signature_at(&uri, 1, 24))
        .await
        .expect("signature_help ok")
        .expect("a signature for `L.map`");
    assert_eq!(
        help.signatures[0].label,
        "map (f: fn a -> b) (xs: List a) -> List b"
    );
    // The parameter offsets bracket each parameter exactly inside the label.
    assert_eq!(param_offsets(&help), vec![(4, 18), (19, 31)]);
    let label = &help.signatures[0].label;
    assert_eq!(&label[4..18], "(f: fn a -> b)");
    assert_eq!(&label[19..31], "(xs: List a)");
    assert_eq!(
        help.active_parameter,
        Some(0),
        "cursor on the first argument"
    );

    // Cursor on the second argument `xs` (column 26).
    let (_, active) = sig_label_active(
        server
            .signature_help(signature_at(&uri, 1, 26))
            .await
            .expect("ok"),
    )
    .expect("signature");
    assert_eq!(active, 1, "cursor on the second argument");

    // Cursor in the whitespace right after `f` (column 25): the next parameter
    // is active.
    let (_, active) = sig_label_active(
        server
            .signature_help(signature_at(&uri, 1, 25))
            .await
            .expect("ok"),
    )
    .expect("signature");
    assert_eq!(active, 1, "the gap after an argument advances to the next");

    // Cursor on the callee name `map` itself (column 21): the signature shows
    // with the first parameter active, no argument attributed yet.
    let (label, active) = sig_label_active(
        server
            .signature_help(signature_at(&uri, 1, 21))
            .await
            .expect("ok"),
    )
    .expect("a signature on the callee");
    assert_eq!(label, "map (f: fn a -> b) (xs: List a) -> List b");
    assert_eq!(active, 0);
}

#[tokio::test]
async fn test_signature_help_stdlib_class_method() {
    // A bare `filter` carries a `ClassMethod` binding for the stdlib `Refinable`
    // class (redeclared so the resolver stamps it, the same trick the goto test
    // uses). The signature is the verb's canonical one from `repo.ridge`, not the
    // workspace redeclaration.
    let src = concat!(
        "pub class Refinable q p | q -> p =\n",
        "  filter (pred: p) (x: q) -> q\n",
        "pub fn run q p -> q = filter p q\n",
    );
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Line 2: `pub fn run q p -> q = filter p q`. The call `filter p q` starts at
    // column 22; the arguments `p` and `q` are at columns 29 and 31.
    let (label, active) = sig_label_active(
        server
            .signature_help(signature_at(&uri, 2, 29))
            .await
            .expect("ok"),
    )
    .expect("a stdlib class-method signature");
    assert_eq!(label, "filter (pred: Quote p) (x: q) -> q");
    assert_eq!(active, 0, "cursor on the first argument");

    let (_, active) = sig_label_active(
        server
            .signature_help(signature_at(&uri, 2, 31))
            .await
            .expect("ok"),
    )
    .expect("signature");
    assert_eq!(active, 1, "cursor on the second argument");
}

#[tokio::test]
async fn test_signature_help_and_goto_workspace_class_method() {
    // A class declared in the workspace with no stdlib counterpart. Its method
    // call gets both a signature (read from the declaration) and go-to-definition
    // onto the method-name signature — the workspace class-method path.
    let src = concat!(
        "pub class Greeter a =\n",
        "  greetWith (greeting: Text) (subject: a) -> Text\n",
        "pub fn run = greetWith \"hi\" 3\n",
    );
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Line 2: `pub fn run = greetWith "hi" 3`. `greetWith` spans columns 13..22;
    // the arguments `"hi"` and `3` are at columns 23 and 28.
    let (label, active) = sig_label_active(
        server
            .signature_help(signature_at(&uri, 2, 15))
            .await
            .expect("ok"),
    )
    .expect("a workspace class-method signature");
    assert_eq!(label, "greetWith (greeting: Text) (subject: a) -> Text");
    assert_eq!(active, 0, "cursor on the callee shows the first parameter");

    let (_, active) = sig_label_active(
        server
            .signature_help(signature_at(&uri, 2, 28))
            .await
            .expect("ok"),
    )
    .expect("signature");
    assert_eq!(active, 1, "cursor on the second argument");

    // Go-to-definition on the same `greetWith` use lands on the method-name
    // signature in the workspace class declaration (line 1, column 2).
    let goto = server
        .goto_definition(goto_at(&uri, 2, 15))
        .await
        .expect("ok");
    let loc = scalar_location(goto).expect("workspace class-method definition");
    assert_eq!(loc.uri, uri, "definition is in the same workspace file");
    assert_eq!(
        loc.range.start.line, 1,
        "lands on the method signature line"
    );
    assert_eq!(loc.range.start.character, 2, "at the method name");
}

#[tokio::test]
async fn test_signature_help_none_off_call() {
    // Away from any call there is no signature to show.
    let src = "import std.list as L\npub fn run f xs = L.map f xs\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // The `fn` keyword on line 1 (column 4) is inside no call and is no name.
    let off = server
        .signature_help(signature_at(&uri, 1, 4))
        .await
        .expect("signature_help ok");
    assert!(off.is_none(), "no signature help on a keyword");
}

// ── Test 30: semantic tokens (textDocument/semanticTokens) ────────────────────

const SEMANTIC_SRC: &str = concat!(
    "import std.list as L\n",              // line 0
    "type Color = Red | Green\n",          // line 1
    "const maxAge: Int = 120\n",           // line 2
    "pub fn io greet (n: Int) -> Int =\n", // line 3
    "    L.map greet Red\n",               // line 4
);

fn semantic_params(uri: &Url) -> SemanticTokensParams {
    SemanticTokensParams {
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        text_document: TextDocumentIdentifier { uri: uri.clone() },
    }
}

fn semantic_range_params(uri: &Url, range: Range) -> SemanticTokensRangeParams {
    SemanticTokensRangeParams {
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        range,
    }
}

/// Decode the relative token stream into `(line, char, len, type, [modifiers])`.
fn decode_tokens(data: &[SemanticToken]) -> Vec<(u32, u32, u32, String, Vec<String>)> {
    let types = ridge_lsp::index::SEMANTIC_TOKEN_TYPES;
    let modifiers = ridge_lsp::index::SEMANTIC_TOKEN_MODIFIERS;
    let mut out = Vec::new();
    let mut line = 0u32;
    let mut start = 0u32;
    for t in data {
        if t.delta_line == 0 {
            start += t.delta_start;
        } else {
            line += t.delta_line;
            start = t.delta_start;
        }
        let ty = types[t.token_type as usize].as_str().to_owned();
        let mods = modifiers
            .iter()
            .enumerate()
            .filter(|(i, _)| t.token_modifiers_bitset & (1u32 << i) != 0)
            .map(|(_, m)| m.as_str().to_owned())
            .collect();
        out.push((line, start, t.length, ty, mods));
    }
    out
}

fn tokens_of(result: Option<SemanticTokensResult>) -> Vec<(u32, u32, u32, String, Vec<String>)> {
    match result {
        Some(SemanticTokensResult::Tokens(t)) => decode_tokens(&t.data),
        _ => Vec::new(),
    }
}

/// The (type, modifiers) of the token starting at `(line, char)`, if any.
fn token_at(
    toks: &[(u32, u32, u32, String, Vec<String>)],
    line: u32,
    char: u32,
) -> Option<(String, Vec<String>)> {
    toks.iter()
        .find(|(l, c, _, _, _)| *l == line && *c == char)
        .map(|(_, _, _, ty, mods)| (ty.clone(), mods.clone()))
}

#[tokio::test]
async fn test_semantic_tokens_classifies_names_and_capabilities() {
    let (service, _socket, uri) = hover_fixture(SEMANTIC_SRC).await;
    let server = service.inner();

    let toks = tokens_of(
        server
            .semantic_tokens_full(semantic_params(&uri))
            .await
            .expect("semantic_tokens ok"),
    );

    let expect = |line: u32, char: u32, ty: &str, mods: &[&str]| {
        let got = token_at(&toks, line, char)
            .unwrap_or_else(|| panic!("expected a token at ({line}, {char})"));
        assert_eq!(got.0, ty, "type at ({line}, {char})");
        assert_eq!(got.1, mods, "modifiers at ({line}, {char})");
    };

    // Declarations — their name nodes carry no binding, so the declaration pass
    // is their only source.
    expect(1, 5, "type", &["declaration"]); // type Color
    expect(1, 13, "enumMember", &["declaration"]); // variant Red
    expect(1, 19, "enumMember", &["declaration"]); // variant Green
    expect(2, 6, "variable", &["declaration", "readonly"]); // const maxAge
    expect(3, 10, "function", &["declaration"]); // fn greet
                                                 // The capability annotation — the security-visible token type.
    expect(3, 7, "capability", &[]); // io
                                     // A parameter binder.
    expect(3, 17, "parameter", &["declaration"]); // (n: Int)
                                                  // A qualified stdlib call, coloured per segment.
    expect(4, 4, "namespace", &["defaultLibrary"]); // L
    expect(4, 6, "function", &["defaultLibrary"]); // map
                                                   // Use sites carry no declaration modifier.
    expect(4, 10, "function", &[]); // greet
    expect(4, 16, "enumMember", &[]); // Red
}

#[tokio::test]
async fn test_semantic_tokens_range_limits_to_region() {
    let (service, _socket, uri) = hover_fixture(SEMANTIC_SRC).await;
    let server = service.inner();

    // Restrict to line 4 (the `L.map greet Red` call).
    let range = Range {
        start: Position::new(4, 0),
        end: Position::new(4, 20),
    };
    let result = server
        .semantic_tokens_range(semantic_range_params(&uri, range))
        .await
        .expect("semantic_tokens_range ok");
    let toks = match result {
        Some(SemanticTokensRangeResult::Tokens(t)) => decode_tokens(&t.data),
        _ => Vec::new(),
    };

    assert!(!toks.is_empty(), "the range has tokens");
    assert!(
        toks.iter().all(|(line, ..)| *line == 4),
        "only line-4 tokens are returned, got {toks:?}"
    );
    // The qualified call is still split into namespace + function.
    assert_eq!(
        token_at(&toks, 4, 4).map(|t| t.0),
        Some("namespace".to_owned())
    );
    assert_eq!(
        token_at(&toks, 4, 6).map(|t| t.0),
        Some("function".to_owned())
    );
}

#[tokio::test]
async fn test_semantic_tokens_no_overlap() {
    // Whatever the document, the emitted tokens must be strictly ordered and
    // pairwise disjoint — the encoding the client decodes depends on it.
    let (service, _socket, uri) = hover_fixture(SEMANTIC_SRC).await;
    let server = service.inner();
    let toks = tokens_of(
        server
            .semantic_tokens_full(semantic_params(&uri))
            .await
            .expect("ok"),
    );
    for win in toks.windows(2) {
        let a = &win[0];
        let b = &win[1];
        assert!((a.0, a.1) < (b.0, b.1), "ordered: {a:?} then {b:?}");
        if a.0 == b.0 {
            assert!(a.1 + a.2 <= b.1, "no overlap on a line: {a:?} then {b:?}");
        }
    }
}

// ── Record-field navigation + go-to-type-definition ───────────────────────────

// Two records sharing the field name `age`, so a correct field query must key
// on the owner type, not the bare name. Field-name columns:
//   line 0  `User.age` decl  → col 18      `User.name` decl → col 28
//   line 1  `Pet.age` decl   → col 17
//   line 2  `userAge` uses `u.age`         → field at col 36
//   line 3  `petAge` uses `p.age`          → field at col 34
//   line 4  `twice` uses `u.age` twice     → fields at cols 34 and 42
const FIELD_SRC: &str = "pub type User = { age: Int, name: Text }\n\
pub type Pet = { age: Int }\n\
pub fn userAge (u: User) -> Int = u.age\n\
pub fn petAge (p: Pet) -> Int = p.age\n\
pub fn twice (u: User) -> Int = u.age + u.age\n\
pub fn one -> Int = 1\n";

/// The `(line, character)` of every reference, sorted.
fn ref_pairs(locs: &[Location], uri: &Url) -> Vec<(u32, u32)> {
    let mut pairs: Vec<(u32, u32)> = locs
        .iter()
        .filter(|l| &l.uri == uri)
        .map(|l| (l.range.start.line, l.range.start.character))
        .collect();
    pairs.sort_unstable();
    pairs
}

#[tokio::test]
async fn test_field_definition() {
    let (service, _socket, uri) = hover_fixture(FIELD_SRC).await;
    let server = service.inner();

    // `u.age` in `userAge` (line 2, on the field) → the `age` field of `User`
    // (line 0, col 18) — not the `age` field of `Pet`.
    let resp = server
        .goto_definition(goto_at(&uri, 2, 37))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of field `age`");
    assert_eq!(loc.uri, uri);
    assert_eq!(
        (loc.range.start.line, loc.range.start.character),
        (0, 18),
        "must point at User.age, the owner resolved through the base type"
    );

    // `p.age` in `petAge` (line 3) → the `age` field of `Pet` (line 1, col 17),
    // proving the same field name resolves by owner type.
    let resp = server
        .goto_definition(goto_at(&uri, 3, 35))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of field `age` on Pet");
    assert_eq!(
        (loc.range.start.line, loc.range.start.character),
        (1, 17),
        "Pet.age must resolve to its own declaration"
    );
}

#[tokio::test]
async fn test_field_references() {
    let (service, _socket, uri) = hover_fixture(FIELD_SRC).await;
    let server = service.inner();

    // From a `User.age` use (line 2), with the declaration: the decl (line 0)
    // plus all three `User.age` uses (lines 2 and 4×2) — never the `Pet.age`
    // use on line 3.
    let with_decl = server
        .references(references_at(&uri, 2, 37, true))
        .await
        .expect("ok")
        .expect("references of User.age");
    assert_eq!(
        ref_pairs(&with_decl, &uri),
        vec![(0, 18), (2, 36), (4, 34), (4, 42)],
        "User.age: declaration plus every use, excluding Pet.age"
    );

    // includeDeclaration=false drops the field declaration.
    let without_decl = server
        .references(references_at(&uri, 2, 37, false))
        .await
        .expect("ok")
        .expect("references of User.age");
    assert_eq!(
        ref_pairs(&without_decl, &uri),
        vec![(2, 36), (4, 34), (4, 42)],
        "includeDeclaration=false drops the User.age declaration"
    );

    // The query also works from the field declaration itself (line 0, col 19).
    let from_decl = server
        .references(references_at(&uri, 0, 19, true))
        .await
        .expect("ok")
        .expect("references from the User.age declaration");
    assert_eq!(
        ref_pairs(&from_decl, &uri),
        vec![(0, 18), (2, 36), (4, 34), (4, 42)],
        "a field declaration cursor finds the same set"
    );

    // `Pet.age` is a disjoint set: its declaration (line 1) plus its single use
    // (line 3).
    let pet = server
        .references(references_at(&uri, 3, 35, true))
        .await
        .expect("ok")
        .expect("references of Pet.age");
    assert_eq!(
        ref_pairs(&pet, &uri),
        vec![(1, 17), (3, 34)],
        "Pet.age must not collect User.age sites"
    );
}

#[tokio::test]
async fn test_type_definition() {
    let (service, _socket, uri) = hover_fixture(FIELD_SRC).await;
    let server = service.inner();

    // The value `u` in `userAge` (line 2, col 34) has type `User`; jumping to
    // its type lands on the `User` declaration name (line 0, col 9).
    let resp = server
        .goto_type_definition(goto_at(&uri, 2, 34))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("type definition of `u`");
    assert_eq!(loc.uri, uri);
    assert_eq!(
        (loc.range.start.line, loc.range.start.character),
        (0, 9),
        "type definition must point at the `User` declaration name"
    );

    // A built-in type has no source declaration: the literal `1` (line 5, col
    // 20) is an `Int`, so there is nothing to jump to.
    let builtin = server
        .goto_type_definition(goto_at(&uri, 5, 20))
        .await
        .expect("ok");
    assert!(
        scalar_location(builtin).is_none(),
        "a built-in type yields no type definition"
    );
}

/// A two-member workspace whose `lib` declares a record type used across the
/// module boundary by `app`. Mirrors [`two_member_fixture`] with record-typed
/// sources so field navigation and go-to-type-definition can be exercised
/// cross-file.
async fn record_ws_fixture() -> (
    tower_lsp::LspService<RidgeLanguageServer>,
    tower_lsp::ClientSocket,
    Url,
    Url,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("lib").join("src")).expect("lib src");
    std::fs::create_dir_all(root.join("app").join("src")).expect("app src");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"rec-ws\"\nversion = \"0.1.0\"\nmembers = [\"lib\", \"app\"]\n",
    )
    .expect("workspace manifest");
    std::fs::write(
        root.join("lib").join("ridge.toml"),
        "[project]\nname = \"lib\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("lib manifest");
    std::fs::write(
        root.join("lib").join("src").join("Lib.ridge"),
        "pub type User = { age: Int, name: Text }\n",
    )
    .expect("lib source");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("app manifest");
    let app_text = "import lib.Lib (User)\npub fn ageOf (u: User) -> Int = u.age\n";
    std::fs::write(root.join("app").join("src").join("Main.ridge"), app_text).expect("app source");

    let (service, socket) = build_test_service();
    let app_uri;
    let lib_uri;
    {
        let server = service.inner();
        let root_uri = Url::from_file_path(&root).expect("root URI");
        server
            .initialize(InitializeParams {
                root_uri: Some(root_uri.clone()),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri,
                    name: "rec-ws".to_owned(),
                }]),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Url::from_file_path(root.join("app").join("src").join("Main.ridge"))
                        .expect("app URI"),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: app_text.to_owned(),
                },
            })
            .await;
        let mut index = None;
        for _ in 0..120 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            if let Some(idx) = server.workspace_index().await {
                index = Some(idx);
                break;
            }
        }
        let index = index.expect("index installed");
        app_uri = index
            .uri_to_module
            .keys()
            .find(|u| u.path().ends_with("Main.ridge"))
            .expect("app module")
            .clone();
        lib_uri = index
            .uri_to_module
            .keys()
            .find(|u| u.path().ends_with("Lib.ridge"))
            .expect("lib module — multi-member discovery")
            .clone();
    }
    std::mem::forget(dir);
    (service, socket, app_uri, lib_uri)
}

#[tokio::test]
async fn test_field_navigation_cross_module() {
    let (service, _socket, app_uri, lib_uri) = record_ws_fixture().await;
    let server = service.inner();

    // Go-to-def on `u.age` in the app (line 1, col 35) → the `age` field of
    // `User` declared in Lib.ridge (line 0, col 18).
    let resp = server
        .goto_definition(goto_at(&app_uri, 1, 35))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("cross-module field definition");
    assert_eq!(loc.uri, lib_uri, "field decl lives in Lib.ridge");
    assert_eq!((loc.range.start.line, loc.range.start.character), (0, 18));

    // Find-references on the same field gathers the lib declaration and the app
    // use across the two files.
    let refs = server
        .references(references_at(&app_uri, 1, 35, true))
        .await
        .expect("ok")
        .expect("cross-module field references");
    assert_eq!(
        ref_pairs(&refs, &lib_uri),
        vec![(0, 18)],
        "the declaration in Lib.ridge"
    );
    assert_eq!(
        ref_pairs(&refs, &app_uri),
        vec![(1, 34)],
        "the use in Main.ridge"
    );

    // Go-to-type-definition on the value `u` (line 1, col 32) → the `User`
    // declaration name in Lib.ridge (line 0, col 9).
    let ty = server
        .goto_type_definition(goto_at(&app_uri, 1, 32))
        .await
        .expect("ok");
    let loc = scalar_location(ty).expect("cross-module type definition");
    assert_eq!(loc.uri, lib_uri, "type decl lives in Lib.ridge");
    assert_eq!((loc.range.start.line, loc.range.start.character), (0, 9));
}

#[tokio::test]
async fn test_field_rename() {
    let (service, _socket, uri) = hover_fixture(FIELD_SRC).await;
    let server = service.inner();

    // prepareRename on the `u.age` use (line 2) underlines just the field token
    // (col 36) and offers its current name.
    let prep = server
        .prepare_rename(prepare_rename_at(&uri, 2, 37))
        .await
        .expect("ok")
        .expect("a record field is renameable");
    match prep {
        PrepareRenameResponse::RangeWithPlaceholder { range, placeholder } => {
            assert_eq!(
                (range.start.line, range.start.character),
                (2, 36),
                "underlines the field token under the cursor"
            );
            assert_eq!(placeholder, "age", "placeholder is the field name");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }

    // Renaming `User.age` from a use rewrites the declaration (line 0) and every
    // `User.age` use (lines 2 and 4×2) — never the `Pet.age` use on line 3.
    let edit = server
        .rename(rename_at(&uri, 2, 37, "years"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        vec![(0, 18), (2, 36), (4, 34), (4, 42)],
        "the declaration and every User.age use move together"
    );
    if let Some((_, text)) = rename_edits(edit.as_ref(), &uri).first() {
        assert_eq!(text, "years", "edits carry the new name");
    }

    // The same rename works from the field declaration itself (line 0, col 19).
    let from_decl = server
        .rename(rename_at(&uri, 0, 19, "years"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(from_decl.as_ref(), &uri),
        vec![(0, 18), (2, 36), (4, 34), (4, 42)],
        "a rename from the declaration covers the same sites"
    );

    // `Pet.age` is a disjoint rename: its declaration (line 1) and its one use
    // (line 3), never a User.age site.
    let pet = server
        .rename(rename_at(&uri, 3, 35, "years"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(pet.as_ref(), &uri),
        vec![(1, 17), (3, 34)],
        "Pet.age renames independently of User.age"
    );
}

#[tokio::test]
async fn test_field_rename_rejects_invalid_and_collision() {
    let (service, _socket, uri) = hover_fixture(FIELD_SRC).await;
    let server = service.inner();

    // A reserved keyword and a capitalised name are both invalid for a field.
    let kw = server.rename(rename_at(&uri, 2, 37, "if")).await;
    assert!(kw.is_err(), "a field cannot be renamed to a keyword");
    let upper = server.rename(rename_at(&uri, 2, 37, "Age")).await;
    assert!(upper.is_err(), "a field must stay a lowercase identifier");

    // `User` already has a `name` field, so renaming `age` onto it would create
    // a duplicate — reject it.
    let collision = server.rename(rename_at(&uri, 2, 37, "name")).await;
    assert!(
        collision.is_err(),
        "renaming onto an existing field name is rejected"
    );
}

#[tokio::test]
async fn test_field_document_highlight() {
    let (service, _socket, uri) = hover_fixture(FIELD_SRC).await;
    let server = service.inner();

    // From a `User.age` use (line 2): the declaration is the write, every
    // User.age use is a read, and the Pet.age use on line 3 is excluded.
    let from_use = server
        .document_highlight(highlight_at(&uri, 2, 37))
        .await
        .expect("ok")
        .expect("highlights of User.age");
    assert_eq!(
        highlight_spots(&from_use),
        vec![
            (0, 18, DocumentHighlightKind::WRITE),
            (2, 36, DocumentHighlightKind::READ),
            (4, 34, DocumentHighlightKind::READ),
            (4, 42, DocumentHighlightKind::READ),
        ],
        "declaration writes, every User.age use reads"
    );

    // The same set from the declaration name itself (line 0, col 19).
    let from_decl = server
        .document_highlight(highlight_at(&uri, 0, 19))
        .await
        .expect("ok")
        .expect("highlights from the User.age declaration");
    assert_eq!(highlight_spots(&from_decl), highlight_spots(&from_use));

    // `Pet.age` is a disjoint set: its declaration (write) and its one use.
    let pet = server
        .document_highlight(highlight_at(&uri, 3, 35))
        .await
        .expect("ok")
        .expect("highlights of Pet.age");
    assert_eq!(
        highlight_spots(&pet),
        vec![
            (1, 17, DocumentHighlightKind::WRITE),
            (3, 34, DocumentHighlightKind::READ),
        ],
        "Pet.age highlights independently of User.age"
    );
}

#[tokio::test]
async fn test_field_rename_and_highlight_cross_module() {
    let (service, _socket, app_uri, lib_uri) = record_ws_fixture().await;
    let server = service.inner();

    // Renaming `u.age` from the app (line 1, col 35) rewrites both the use in
    // Main.ridge and the declaration in Lib.ridge.
    let edit = server
        .rename(rename_at(&app_uri, 1, 35, "years"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &lib_uri),
        vec![(0, 18)],
        "the field declaration in Lib.ridge is rewritten"
    );
    assert_eq!(
        rename_sites(edit.as_ref(), &app_uri),
        vec![(1, 34)],
        "the use in Main.ridge is rewritten"
    );

    // documentHighlight never leaves the cursor's file: from the app use it
    // marks only the use there; the declaration lives in Lib.ridge, so no write.
    let app = server
        .document_highlight(highlight_at(&app_uri, 1, 35))
        .await
        .expect("ok")
        .expect("field highlights in the app");
    assert_eq!(
        highlight_spots(&app),
        vec![(1, 34, DocumentHighlightKind::READ)],
        "the app use only, no cross-file write"
    );

    // From the Lib declaration: the declaration is the write, with no app reads.
    let lib = server
        .document_highlight(highlight_at(&lib_uri, 0, 19))
        .await
        .expect("ok")
        .expect("field highlights at the declaration");
    assert_eq!(
        highlight_spots(&lib),
        vec![(0, 18, DocumentHighlightKind::WRITE)],
        "the declaration write only, same file"
    );
}

// ── Standalone-file mode (L34) ────────────────────────────────────────────────

/// Initialize the server, open a single `.ridge` file that has no workspace
/// manifest, and return it once a compile has produced an index.
///
/// `with_root` selects which broken case is exercised: `true` passes a `rootUri`
/// for a folder that holds no `[workspace]` manifest, `false` passes no root at
/// all (a truly loose file). Both must fall back to standalone analysis.
async fn standalone_fixture(
    src: &'static str,
    with_root: bool,
) -> (
    tower_lsp::LspService<RidgeLanguageServer>,
    tower_lsp::ClientSocket,
    Url,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let file = root.join("scratch.ridge");
    std::fs::write(&file, src).expect("write standalone file");

    let (service, socket) = build_test_service();
    let mut file_uri = Url::from_file_path(&file).expect("file URI");
    {
        let server = service.inner();
        let root_uri = with_root.then(|| Url::from_file_path(&root).expect("root URI"));
        server
            .initialize(InitializeParams {
                root_uri,
                workspace_folders: None,
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: file_uri.clone(),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: src.to_owned(),
                },
            })
            .await;
        let mut index = None;
        for _ in 0..120 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            if let Some(idx) = server.workspace_index().await {
                index = Some(idx);
                break;
            }
        }
        let index = index.expect("an index must be installed in standalone mode");
        // Use the URI the index actually holds (same scheme diagnostics use).
        file_uri = index
            .uri_to_module
            .keys()
            .next()
            .expect("the standalone file is indexed as one module")
            .clone();
    }
    std::mem::forget(dir);
    (service, socket, file_uri)
}

#[tokio::test]
async fn test_standalone_file_without_root_uri_is_analysed() {
    // A loose file opened with no folder (rootUri = null) still type-checks and
    // serves hover, rather than going dark for want of a workspace manifest. Same
    // source as the workspace hover test, so the only variable is standalone mode.
    let src = "---\nGreets a person by name.\n---\npub fn greet (name: Text) -> Text = name\npub fn run -> Text = greet \"x\"\n";
    let (service, _socket, uri) = standalone_fixture(src, false).await;
    let server = service.inner();

    // Hover the `greet` use on line 4 — the enriched signature + doc proves the
    // full pipeline ran on a file with no project on disk.
    let line4 = "pub fn run -> Text = greet \"x\"";
    let col = u32::try_from(line4.find("greet").expect("greet use") + 1).expect("u32");
    let md = hover_markdown(
        server
            .hover(hover_at(&uri, 4, col))
            .await
            .expect("hover ok"),
    )
    .expect("hover returns markdown in standalone mode");
    assert!(
        md.contains("pub fn greet (name: Text) -> Text"),
        "enriched signature, got: {md}"
    );
    assert!(
        md.contains("Greets a person by name."),
        "doc shown, got: {md}"
    );
}

#[tokio::test]
async fn test_standalone_folder_without_workspace_manifest_is_analysed() {
    // A folder opened as the root but holding no `[workspace]` manifest also
    // falls back to standalone mode — previously this published L801 and indexed
    // nothing.
    let src = "pub fn double (n: Int) -> Int = n\npub fn run -> Int = double 21\n";
    let (service, _socket, uri) = standalone_fixture(src, true).await;
    let server = service.inner();

    // Hover the `double` use on line 1.
    let line1 = "pub fn run -> Int = double 21";
    let col = u32::try_from(line1.find("double").expect("double use") + 1).expect("u32");
    let md = hover_markdown(
        server
            .hover(hover_at(&uri, 1, col))
            .await
            .expect("hover ok"),
    )
    .expect("hover works under a manifest-less folder");
    assert!(
        md.contains("pub fn double (n: Int) -> Int"),
        "enriched signature, got: {md}"
    );
}

// ── completionItem/resolve (L17) ──────────────────────────────────────────────

#[tokio::test]
async fn test_completion_resolve_fills_signature_and_doc() {
    // A documented function, and a second whose body starts referencing it, so
    // the name is offered at an expression position with a `gr` prefix.
    let src = "---\nGreets a person by name.\n---\npub fn greet (name: Text) -> Text = name\npub fn run -> Text = gr\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Complete at the end of `pub fn run -> Text = gr` (line 4, char 23).
    let items = completion_items(
        server
            .completion(complete_at(&uri, 4, 23))
            .await
            .expect("ok"),
    );
    let greet = items
        .into_iter()
        .find(|i| i.label == "greet")
        .expect("greet is offered");

    // The list item carries a resolve payload, but detail and doc stay empty
    // until the editor asks for them.
    assert!(
        greet.data.is_some(),
        "a workspace symbol carries resolve data"
    );
    assert!(greet.detail.is_none(), "detail is filled only on resolve");
    assert!(
        greet.documentation.is_none(),
        "doc is filled only on resolve"
    );

    let resolved = server.completion_resolve(greet).await.expect("resolve ok");
    assert_eq!(
        resolved.detail.as_deref(),
        Some("pub fn greet (name: Text) -> Text"),
        "resolve fills the written signature"
    );
    let doc = match resolved.documentation {
        Some(Documentation::MarkupContent(m)) => m.value,
        other => panic!("expected a markdown doc, got {other:?}"),
    };
    assert!(
        doc.contains("Greets a person by name."),
        "resolve fills the doc comment, got {doc}"
    );
}

#[tokio::test]
async fn test_completion_resolve_passes_through_items_without_data() {
    // A keyword completion has no resolve payload, so resolve returns it as-is.
    let item = CompletionItem {
        label: "match".to_owned(),
        kind: Some(CompletionItemKind::KEYWORD),
        ..CompletionItem::default()
    };
    let (service, _socket, _uri) = hover_fixture("pub fn run -> Int = 1\n").await;
    let server = service.inner();
    let resolved = server.completion_resolve(item).await.expect("resolve ok");
    assert_eq!(resolved.label, "match");
    assert!(resolved.detail.is_none(), "no data means no enrichment");
    assert!(resolved.documentation.is_none());
}

// ── textDocument/foldingRange (L15) ───────────────────────────────────────────

fn folding_at(uri: &Url) -> FoldingRangeParams {
    FoldingRangeParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

#[tokio::test]
async fn test_folding_ranges_imports_and_declarations() {
    // Two consecutive imports, a multi-line record, a multi-line function, and a
    // single-line const. Lines (0-indexed):
    //   0 import · 1 import · 2 blank · 3-6 type · 7 blank · 8-9 fn · 10 blank · 11 const
    let src = "import std.list as L\nimport std.option as O\n\npub type Point = {\n  x: Int,\n  y: Int,\n}\n\npub fn area (p: Point) -> Int =\n  p.x\n\npub const ZERO : Int = 0\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    let folds = server
        .folding_range(folding_at(&uri))
        .await
        .expect("folding ok")
        .expect("folds present");

    // The two consecutive imports collapse into one Imports-kind fold.
    let imports: Vec<&FoldingRange> = folds
        .iter()
        .filter(|f| f.kind == Some(FoldingRangeKind::Imports))
        .collect();
    assert_eq!(imports.len(), 1, "exactly one import block, got {folds:?}");
    assert_eq!(
        (imports[0].start_line, imports[0].end_line),
        (0, 1),
        "import block spans lines 0..1"
    );

    // The multi-line record and function each fold as a region.
    let regions: Vec<(u32, u32)> = folds
        .iter()
        .filter(|f| f.kind == Some(FoldingRangeKind::Region))
        .map(|f| (f.start_line, f.end_line))
        .collect();
    assert!(
        regions.contains(&(3, 6)),
        "type Point folds 3..6, got {regions:?}"
    );
    assert!(
        regions.contains(&(8, 9)),
        "fn area folds 8..9, got {regions:?}"
    );

    // The single-line const on line 11 has nothing to fold.
    assert!(
        !folds.iter().any(|f| f.start_line == 11),
        "single-line const must not fold, got {folds:?}"
    );
}

// ── textDocument/selectionRange ───────────────────────────────────────────────

fn selection_at(uri: &Url, positions: Vec<Position>) -> SelectionRangeParams {
    SelectionRangeParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        positions,
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

/// The selection-range hierarchy flattened innermost → outermost.
fn chain_ranges(sr: &SelectionRange) -> Vec<Range> {
    let mut out = vec![sr.range];
    let mut cur = sr.parent.as_deref();
    while let Some(parent) = cur {
        out.push(parent.range);
        cur = parent.parent.as_deref();
    }
    out
}

#[tokio::test]
async fn test_selection_range_nests_from_token_to_file() {
    // Cursor inside the `21` literal of the call `double 21`.
    //   line 0: pub fn double (n: Int) -> Int = n
    //   line 1: pub fn run -> Int =
    //   line 2: ··double 21
    let src = "pub fn double (n: Int) -> Int = n\npub fn run -> Int =\n  double 21\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    let ranges = server
        .selection_range(selection_at(&uri, vec![Position::new(2, 10)]))
        .await
        .expect("selection ok")
        .expect("a hierarchy is returned");
    assert_eq!(ranges.len(), 1, "one hierarchy per requested position");
    let chain = chain_ranges(&ranges[0]);

    // Expanding steps through at least: the literal, the enclosing call, the
    // declaration, and the whole file.
    assert!(
        chain.len() >= 3,
        "expected a multi-level hierarchy, got {chain:?}"
    );

    // The innermost range starts on the cursor's line and brackets the cursor.
    let inner = chain[0];
    assert_eq!(inner.start.line, 2, "innermost is on the cursor line");
    assert!(
        inner.start <= Position::new(2, 10) && Position::new(2, 10) <= inner.end,
        "innermost brackets the cursor, got {inner:?}"
    );

    // Each parent strictly contains its child.
    for pair in chain.windows(2) {
        let (child, parent) = (pair[0], pair[1]);
        assert!(
            parent.start <= child.start && child.end <= parent.end,
            "parent {parent:?} must contain child {child:?}"
        );
        assert!(
            parent.start < child.start || child.end < parent.end,
            "parent {parent:?} must be strictly larger than child {child:?}"
        );
    }

    // The outermost level is the whole file.
    let outer = *chain.last().expect("non-empty chain");
    assert_eq!(
        outer.start,
        Position::new(0, 0),
        "outermost selection is the whole document"
    );
}

#[tokio::test]
async fn test_selection_range_one_result_per_position() {
    let src = "pub fn double (n: Int) -> Int = n\npub fn run -> Int =\n  double 21\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // Two positions: inside `21` and on the `double` callee.
    let ranges = server
        .selection_range(selection_at(
            &uri,
            vec![Position::new(2, 10), Position::new(2, 4)],
        ))
        .await
        .expect("selection ok")
        .expect("hierarchies returned");
    assert_eq!(
        ranges.len(),
        2,
        "result count and order match the input positions"
    );
    for sr in &ranges {
        assert!(
            chain_ranges(sr).len() >= 2,
            "each position yields its own hierarchy"
        );
    }
}

#[tokio::test]
async fn test_selection_range_outside_nodes_yields_file() {
    // The cursor sits in the leading whitespace before `double`, off every
    // stamped node. The declaration and the whole file still bracket it.
    let src = "pub fn double (n: Int) -> Int = n\npub fn run -> Int =\n  double 21\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    let ranges = server
        .selection_range(selection_at(&uri, vec![Position::new(2, 0)]))
        .await
        .expect("selection ok")
        .expect("a hierarchy is returned");
    let chain = chain_ranges(&ranges[0]);
    let outer = *chain.last().expect("non-empty chain");
    assert_eq!(
        outer.start,
        Position::new(0, 0),
        "expand-selection always reaches the whole document"
    );
}

// ── call hierarchy: prepare / incoming / outgoing ─────────────────────────────

/// Three functions where `helper` is called by both `caller_a` and `caller_b`,
/// and `caller_b` calls both `helper` and `caller_a`.
const CALL_GRAPH_SRC: &str = "pub fn helper (n: Int) -> Int = n\npub fn caller_a (x: Int) -> Int = helper x\npub fn caller_b (y: Int) -> Int = helper (caller_a y)\n";

fn prepare_call_at(uri: &Url, line: u32, character: u32) -> CallHierarchyPrepareParams {
    CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

#[tokio::test]
async fn test_call_hierarchy_incoming_calls() {
    let (service, _socket, uri) = hover_fixture(CALL_GRAPH_SRC).await;
    let server = service.inner();

    // Prepare on the `helper` declaration name (line 0, inside `helper`).
    let items = server
        .prepare_call_hierarchy(prepare_call_at(&uri, 0, 9))
        .await
        .expect("prepare ok")
        .expect("an item under the cursor");
    assert_eq!(items.len(), 1, "one item, got {items:?}");
    assert_eq!(items[0].name, "helper");
    assert_eq!(items[0].kind, SymbolKind::FUNCTION);

    let incoming = server
        .incoming_calls(CallHierarchyIncomingCallsParams {
            item: items[0].clone(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("incoming ok")
        .expect("calls present");

    let callers: Vec<&str> = incoming.iter().map(|c| c.from.name.as_str()).collect();
    assert_eq!(incoming.len(), 2, "exactly two callers, got {callers:?}");
    assert!(callers.contains(&"caller_a"), "got {callers:?}");
    assert!(callers.contains(&"caller_b"), "got {callers:?}");
    for c in &incoming {
        assert_eq!(
            c.from_ranges.len(),
            1,
            "each caller calls helper once, {:?}",
            c.from.name
        );
    }
}

#[tokio::test]
async fn test_call_hierarchy_outgoing_calls() {
    let (service, _socket, uri) = hover_fixture(CALL_GRAPH_SRC).await;
    let server = service.inner();

    // Prepare on the `caller_b` declaration name (line 2, inside `caller_b`).
    let items = server
        .prepare_call_hierarchy(prepare_call_at(&uri, 2, 9))
        .await
        .expect("prepare ok")
        .expect("an item under the cursor");
    assert_eq!(items[0].name, "caller_b");

    let outgoing = server
        .outgoing_calls(CallHierarchyOutgoingCallsParams {
            item: items[0].clone(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("outgoing ok")
        .expect("calls present");

    let callees: Vec<&str> = outgoing.iter().map(|c| c.to.name.as_str()).collect();
    assert_eq!(outgoing.len(), 2, "exactly two callees, got {callees:?}");
    assert!(callees.contains(&"helper"), "got {callees:?}");
    assert!(callees.contains(&"caller_a"), "got {callees:?}");
}

#[tokio::test]
async fn test_call_hierarchy_prepare_none_off_function() {
    let (service, _socket, uri) = hover_fixture(CALL_GRAPH_SRC).await;
    let server = service.inner();

    // The parameter `n` (line 0, col 15) is a local, not a workspace function.
    let none = server
        .prepare_call_hierarchy(prepare_call_at(&uri, 0, 15))
        .await
        .expect("prepare ok");
    assert!(
        none.is_none(),
        "a local parameter is not a call-hierarchy anchor, got {none:?}"
    );
}

// ── textDocument/implementation ───────────────────────────────────────────────

/// A class `Greeter` with one method, implemented by two instances, plus a call
/// site of the method. Lines (0-indexed):
///   0  pub class Greeter a =
///   1    greetWith (greeting: Text) (subject: a) -> Text
///   2  instance Greeter Int =
///   3    greetWith (greeting: Text) (subject: Int) -> Text = greeting
///   4  instance Greeter Text =
///   5    greetWith (greeting: Text) (subject: Text) -> Text = greeting
///   6  pub fn run = greetWith "hi" 3
const IMPL_SRC: &str = concat!(
    "pub class Greeter a =\n",
    "  greetWith (greeting: Text) (subject: a) -> Text\n",
    "instance Greeter Int =\n",
    "  greetWith (greeting: Text) (subject: Int) -> Text = greeting\n",
    "instance Greeter Text =\n",
    "  greetWith (greeting: Text) (subject: Text) -> Text = greeting\n",
    "pub fn run = greetWith \"hi\" 3\n",
);

/// The locations of a `goto_implementation` response, ignoring response shape.
fn impl_locations(resp: Option<GotoDefinitionResponse>) -> Vec<Location> {
    match resp {
        Some(GotoDefinitionResponse::Array(locs)) => locs,
        Some(GotoDefinitionResponse::Scalar(loc)) => vec![loc],
        _ => Vec::new(),
    }
}

/// The `(line, character)` start of each location, sorted — a stable shape to
/// assert against.
fn impl_starts(locs: &[Location]) -> Vec<(u32, u32)> {
    locs.iter()
        .map(|l| (l.range.start.line, l.range.start.character))
        .collect()
}

#[tokio::test]
async fn test_implementation_from_class_method_call_site() {
    let (service, _socket, uri) = hover_fixture(IMPL_SRC).await;
    let server = service.inner();

    // The `greetWith` call on line 6 (columns 13..22): jump to both instance
    // definitions of the method, each at column 2 of its instance body.
    let locs = impl_locations(
        server
            .goto_implementation(goto_at(&uri, 6, 15))
            .await
            .expect("implementation ok"),
    );
    assert_eq!(
        impl_starts(&locs),
        vec![(3, 2), (5, 2)],
        "both instance method definitions, got {locs:?}"
    );
    assert!(locs.iter().all(|l| l.uri == uri), "all in the same file");
}

#[tokio::test]
async fn test_implementation_from_class_name_lists_instances() {
    let (service, _socket, uri) = hover_fixture(IMPL_SRC).await;
    let server = service.inner();

    // The class name `Greeter` on line 0 (columns 10..17): jump to every
    // instance head, each naming the class at column 9.
    let locs = impl_locations(
        server
            .goto_implementation(goto_at(&uri, 0, 12))
            .await
            .expect("implementation ok"),
    );
    assert_eq!(
        impl_starts(&locs),
        vec![(2, 9), (4, 9)],
        "both instance heads, got {locs:?}"
    );
}

#[tokio::test]
async fn test_implementation_from_class_method_signature() {
    let (service, _socket, uri) = hover_fixture(IMPL_SRC).await;
    let server = service.inner();

    // The method signature `greetWith` in the class declaration (line 1, column
    // 2) carries no binding, so the class/method is read from the AST. It still
    // resolves to both instance definitions.
    let locs = impl_locations(
        server
            .goto_implementation(goto_at(&uri, 1, 4))
            .await
            .expect("implementation ok"),
    );
    assert_eq!(
        impl_starts(&locs),
        vec![(3, 2), (5, 2)],
        "both instance method definitions, got {locs:?}"
    );
}

#[tokio::test]
async fn test_implementation_none_off_class_or_instance() {
    let (service, _socket, uri) = hover_fixture(IMPL_SRC).await;
    let server = service.inner();

    // The `run` function name (line 6, column 8) is an ordinary function, not a
    // class, instance, or class method — no implementations to navigate to.
    let none = server
        .goto_implementation(goto_at(&uri, 6, 8))
        .await
        .expect("implementation ok");
    assert!(
        impl_locations(none).is_empty(),
        "a plain function is not an implementation anchor"
    );
}

// ── textDocument/prepareTypeHierarchy + supertypes/subtypes ───────────────────

/// A two-level class hierarchy (`Pet` requires `Animal`) with one instance of
/// each, on `Int`. Lines (0-indexed):
///   0  pub class Animal a =
///   1    sound (x: a) -> Text
///   2  pub class Pet a where Animal a =
///   3    name (x: a) -> Text
///   4  instance Animal Int =
///   5    sound (x: Int) -> Text = "generic"
///   6  instance Pet Int =
///   7    name (x: Int) -> Text = "rex"
const TYPE_HIER_SRC: &str = concat!(
    "pub class Animal a =\n",
    "  sound (x: a) -> Text\n",
    "pub class Pet a where Animal a =\n",
    "  name (x: a) -> Text\n",
    "instance Animal Int =\n",
    "  sound (x: Int) -> Text = \"generic\"\n",
    "instance Pet Int =\n",
    "  name (x: Int) -> Text = \"rex\"\n",
);

fn prepare_type_at(uri: &Url, line: u32, character: u32) -> TypeHierarchyPrepareParams {
    TypeHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position: Position { line, character },
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

#[tokio::test]
async fn test_type_hierarchy_prepare_on_class_name() {
    let (service, _socket, uri) = hover_fixture(TYPE_HIER_SRC).await;
    let server = service.inner();

    // The class name `Pet` on line 2 (columns 10..13).
    let items = server
        .prepare_type_hierarchy(prepare_type_at(&uri, 2, 11))
        .await
        .expect("prepare ok")
        .expect("an item under the cursor");
    assert_eq!(items.len(), 1, "one item, got {items:?}");
    assert_eq!(items[0].name, "Pet");
    assert_eq!(items[0].kind, SymbolKind::INTERFACE);
    assert_eq!(
        items[0].selection_range.start.line, 2,
        "selection range is the class name"
    );
}

#[tokio::test]
async fn test_type_hierarchy_supertypes() {
    let (service, _socket, uri) = hover_fixture(TYPE_HIER_SRC).await;
    let server = service.inner();

    let items = server
        .prepare_type_hierarchy(prepare_type_at(&uri, 2, 11))
        .await
        .expect("prepare ok")
        .expect("item");

    // `Pet` requires `Animal` (line 0).
    let supers = server
        .supertypes(TypeHierarchySupertypesParams {
            item: items[0].clone(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("supertypes ok")
        .expect("supertypes present");
    assert_eq!(supers.len(), 1, "one superclass, got {supers:?}");
    assert_eq!(supers[0].name, "Animal");
    assert_eq!(supers[0].selection_range.start.line, 0);
}

#[tokio::test]
async fn test_type_hierarchy_subtypes_lists_subclasses_and_instances() {
    let (service, _socket, uri) = hover_fixture(TYPE_HIER_SRC).await;
    let server = service.inner();

    // Prepare on `Animal` (line 0, columns 10..16).
    let items = server
        .prepare_type_hierarchy(prepare_type_at(&uri, 0, 12))
        .await
        .expect("prepare ok")
        .expect("item");
    assert_eq!(items[0].name, "Animal");

    // `Animal`'s subtypes: the subclass `Pet` (line 2) and the instance
    // `Animal Int` (line 4).
    let subs = server
        .subtypes(TypeHierarchySubtypesParams {
            item: items[0].clone(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("subtypes ok")
        .expect("subtypes present");
    let names: Vec<&str> = subs.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(subs.len(), 2, "subclass + instance, got {names:?}");
    assert!(
        subs.iter()
            .any(|i| i.name == "Pet" && i.kind == SymbolKind::INTERFACE),
        "the subclass Pet, got {names:?}"
    );
    assert!(
        subs.iter()
            .any(|i| i.name == "Animal Int" && i.kind == SymbolKind::OBJECT),
        "the instance Animal Int, got {names:?}"
    );
}

#[tokio::test]
async fn test_type_hierarchy_prepare_on_instance_head_anchors_class() {
    let (service, _socket, uri) = hover_fixture(TYPE_HIER_SRC).await;
    let server = service.inner();

    // The class name `Pet` inside the instance head on line 6 (columns 9..12).
    let items = server
        .prepare_type_hierarchy(prepare_type_at(&uri, 6, 10))
        .await
        .expect("prepare ok")
        .expect("item");
    assert_eq!(items[0].name, "Pet");
    assert_eq!(
        items[0].selection_range.start.line, 2,
        "an instance-head anchor still points at the class declaration"
    );
}

#[tokio::test]
async fn test_type_hierarchy_prepare_none_off_class() {
    let (service, _socket, uri) = hover_fixture(TYPE_HIER_SRC).await;
    let server = service.inner();

    // The method name `sound` (line 1, column 4) is not a class name.
    let none = server
        .prepare_type_hierarchy(prepare_type_at(&uri, 1, 4))
        .await
        .expect("prepare ok");
    assert!(
        none.is_none(),
        "a method name is not a type-hierarchy anchor, got {none:?}"
    );
}

// ── workspace/willRenameFiles ────────────────────────────────────────────────

/// Open the two-file `rename_workspace` fixture — module `app.main` imports
/// `app.math` — and return the live service once the workspace compile has
/// installed an index. The service and socket must be kept alive by the caller.
async fn rename_workspace_service() -> (LspService<RidgeLanguageServer>, ClientSocket) {
    let (service, socket) = build_test_service();
    {
        let server = service.inner();
        server
            .initialize(make_init_params("rename_workspace"))
            .await
            .expect("initialize");

        let root = fixtures_dir().join("rename_workspace");
        for file in &["math.ridge", "main.ridge"] {
            let path = root.join("app").join("src").join(file);
            let uri = Url::from_file_path(&path).expect("file URI");
            let text = std::fs::read_to_string(&path).expect("read fixture");
            server
                .did_open(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri,
                        language_id: "ridge".to_owned(),
                        version: 1,
                        text,
                    },
                })
                .await;
        }

        let mut ready = false;
        for _ in 0..120 {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            if server.workspace_index().await.is_some() {
                ready = true;
                break;
            }
        }
        assert!(ready, "the workspace compile must install an index");
    }
    (service, socket)
}

/// The `file://` URI the index actually holds for the module whose path ends
/// with `suffix`. Querying against the index's own URI — rather than a freshly
/// built path — keeps the rename requests in the same coordinate system the
/// server keys modules by, regardless of Windows path canonicalization.
async fn module_uri_ending(server: &RidgeLanguageServer, suffix: &str) -> Url {
    let index = server.workspace_index().await.expect("index installed");
    index
        .uri_to_module
        .keys()
        .find(|u| u.path().ends_with(suffix))
        .cloned()
        .unwrap_or_else(|| {
            let keys: Vec<_> = index.uri_to_module.keys().collect();
            panic!("a module whose path ends with {suffix}; keys={keys:?}")
        })
}

/// Replace the final path segment of `uri` with `new_rel` (which may itself
/// contain `/` for a deeper destination), keeping the directory prefix.
fn sibling_uri(uri: &Url, new_rel: &str) -> String {
    let s = uri.to_string();
    let cut = s.rfind('/').expect("a path separator in the URI");
    format!("{}/{new_rel}", &s[..cut])
}

#[tokio::test]
async fn test_will_rename_rewrites_dependent_import() {
    let (service, _socket) = rename_workspace_service().await;
    let server = service.inner();
    let math = module_uri_ending(server, "math.ridge").await;

    let edit = server
        .will_rename_files(RenameFilesParams {
            files: vec![FileRename {
                old_uri: math.to_string(),
                new_uri: sibling_uri(&math, "geometry.ridge"),
            }],
        })
        .await
        .expect("will_rename ok")
        .expect("an edit when a dependent import exists");

    let changes = edit.changes.expect("changes map");
    assert_eq!(
        changes.len(),
        1,
        "only the importer is edited, got {changes:?}"
    );
    let (uri, edits) = changes.iter().next().unwrap();
    assert!(
        uri.path().ends_with("main.ridge"),
        "the importer main.ridge is edited, got {uri}"
    );
    assert_eq!(edits.len(), 1, "one import path rewritten, got {edits:?}");
    // Only the dotted path changes; the `(add)` item list is preserved.
    assert_eq!(edits[0].new_text, "app.geometry");
    // The edit lands on `app.math`, the path right after `import ` on line 0.
    assert_eq!(edits[0].range.start.line, 0);
    assert_eq!(
        edits[0].range.start.character, 7,
        "edit should start at `app.math` (col 7), got {:?}",
        edits[0].range
    );
}

#[tokio::test]
async fn test_will_rename_into_subdir_uses_new_path() {
    let (service, _socket) = rename_workspace_service().await;
    let server = service.inner();
    let math = module_uri_ending(server, "math.ridge").await;

    // Moving the module deeper changes its name to match the new path.
    let edit = server
        .will_rename_files(RenameFilesParams {
            files: vec![FileRename {
                old_uri: math.to_string(),
                new_uri: sibling_uri(&math, "geo/shapes.ridge"),
            }],
        })
        .await
        .expect("will_rename ok")
        .expect("edit");

    let changes = edit.changes.expect("changes map");
    let (_uri, edits) = changes.iter().next().unwrap();
    assert_eq!(edits[0].new_text, "app.geo.shapes");
}

#[tokio::test]
async fn test_will_rename_leaf_module_no_edit() {
    let (service, _socket) = rename_workspace_service().await;
    let server = service.inner();
    let main = module_uri_ending(server, "main.ridge").await;

    // Nothing imports `app.main`, so renaming it touches no other file.
    let edit = server
        .will_rename_files(RenameFilesParams {
            files: vec![FileRename {
                old_uri: main.to_string(),
                new_uri: sibling_uri(&main, "entry.ridge"),
            }],
        })
        .await
        .expect("will_rename ok");
    assert!(edit.is_none(), "no importer, so no edit, got {edit:?}");
}

#[tokio::test]
async fn test_will_rename_unknown_file_no_edit() {
    let (service, _socket) = rename_workspace_service().await;
    let server = service.inner();
    let math = module_uri_ending(server, "math.ridge").await;

    // A path that is not a workspace module yields nothing.
    let edit = server
        .will_rename_files(RenameFilesParams {
            files: vec![FileRename {
                old_uri: sibling_uri(&math, "ghost.ridge"),
                new_uri: sibling_uri(&math, "phantom.ridge"),
            }],
        })
        .await
        .expect("will_rename ok");
    assert!(
        edit.is_none(),
        "an unknown file yields no edit, got {edit:?}"
    );
}

#[tokio::test]
async fn test_capability_advertises_will_rename_files() {
    let (service, _socket) = build_test_service();
    let server = service.inner();
    let result = server
        .initialize(make_init_params("ok_workspace"))
        .await
        .expect("initialize ok");

    let file_ops = result
        .capabilities
        .workspace
        .expect("workspace capabilities")
        .file_operations
        .expect("file-operation capabilities");
    let will_rename = file_ops.will_rename.expect("willRename advertised");
    assert_eq!(will_rename.filters.len(), 1);
    assert_eq!(will_rename.filters[0].pattern.glob, "**/*.ridge");
    assert_eq!(
        will_rename.filters[0].pattern.matches,
        Some(FileOperationPatternKind::File)
    );
}

// ── workspace/didChangeWatchedFiles ──────────────────────────────────────────

/// Build a hermetic temp workspace whose `app/src/` holds the given
/// `(filename, contents)` files, initialize the server, drive the first compile
/// by opening the first file, and wait until the index reflects every on-disk
/// module. Returns the live service plus the workspace root (the `TempDir` is
/// leaked so the path stays valid for the test).
async fn watched_workspace(
    files: &[(&str, &str)],
) -> (LspService<RidgeLanguageServer>, ClientSocket, PathBuf) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"watch-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("project manifest");
    for (name, contents) in files {
        std::fs::write(app_src.join(name), contents).expect("write source");
    }

    let (service, socket) = build_test_service();
    {
        let server = service.inner();
        let root_uri = Url::from_file_path(&root).expect("root URI");
        server
            .initialize(InitializeParams {
                root_uri: Some(root_uri.clone()),
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri,
                    name: "watch-ws".to_owned(),
                }]),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");

        // Opening the first file drives the initial reseed compile, which
        // discovers every on-disk module — opened or not.
        let first = app_src.join(files[0].0);
        server
            .did_open(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: Url::from_file_path(&first).expect("file URI"),
                    language_id: "ridge".to_owned(),
                    version: 1,
                    text: files[0].1.to_owned(),
                },
            })
            .await;
        wait_for_module_count(server, files.len()).await;
    }
    std::mem::forget(dir);
    (service, socket, root)
}

/// Poll the index until it holds exactly `n` modules, or panic after ~6s.
async fn wait_for_module_count(server: &RidgeLanguageServer, n: usize) {
    for _ in 0..120 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        if server
            .workspace_index()
            .await
            .map(|idx| idx.uri_to_module.len())
            == Some(n)
        {
            return;
        }
    }
    let got = server
        .workspace_index()
        .await
        .map(|idx| idx.uri_to_module.len());
    panic!("expected {n} modules, got {got:?}");
}

#[tokio::test]
async fn test_watched_file_created_is_indexed() {
    let (service, _socket, root) =
        watched_workspace(&[("main.ridge", "pub fn a -> Int = 1\n")]).await;
    let server = service.inner();

    // Create a second module on disk without opening it in the editor.
    let extra = root.join("app").join("src").join("extra.ridge");
    std::fs::write(&extra, "pub fn b -> Int = 2\n").expect("write new file");
    server
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: Url::from_file_path(&extra).expect("uri"),
                typ: FileChangeType::CREATED,
            }],
        })
        .await;

    wait_for_module_count(server, 2).await;
    let idx = server.workspace_index().await.expect("index");
    assert!(
        idx.uri_to_module
            .keys()
            .any(|u| u.path().ends_with("extra.ridge")),
        "the newly created module is now indexed"
    );
}

#[tokio::test]
async fn test_watched_file_deleted_drops_module() {
    let (service, _socket, root) = watched_workspace(&[
        ("main.ridge", "pub fn a -> Int = 1\n"),
        ("extra.ridge", "pub fn b -> Int = 2\n"),
    ])
    .await;
    let server = service.inner();

    // Remove one module from disk, then report the deletion.
    let extra = root.join("app").join("src").join("extra.ridge");
    std::fs::remove_file(&extra).expect("delete file");
    server
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: Url::from_file_path(&extra).expect("uri"),
                typ: FileChangeType::DELETED,
            }],
        })
        .await;

    wait_for_module_count(server, 1).await;
    let idx = server.workspace_index().await.expect("index");
    assert!(
        !idx.uri_to_module
            .keys()
            .any(|u| u.path().ends_with("extra.ridge")),
        "the deleted module is gone from the index"
    );
}

#[tokio::test]
async fn test_watched_irrelevant_file_ignored() {
    let (service, _socket, root) =
        watched_workspace(&[("main.ridge", "pub fn a -> Int = 1\n")]).await;
    let server = service.inner();

    let gen_before = server.workspace_index().await.expect("index").generation;

    // A non-Ridge file must not trigger a recompile.
    let txt = root.join("notes.txt");
    std::fs::write(&txt, "scratch").ok();
    server
        .did_change_watched_files(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: Url::from_file_path(&txt).expect("uri"),
                typ: FileChangeType::CHANGED,
            }],
        })
        .await;

    // Give any (erroneous) recompile time to land, then confirm none happened.
    tokio::time::sleep(tokio::time::Duration::from_millis(400)).await;
    let gen_after = server.workspace_index().await.expect("index").generation;
    assert_eq!(
        gen_before, gen_after,
        "an irrelevant watched file must not trigger a recompile"
    );
}
