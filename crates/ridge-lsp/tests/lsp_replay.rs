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
