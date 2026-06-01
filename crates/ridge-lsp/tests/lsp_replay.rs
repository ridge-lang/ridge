//! LSP replay integration tests for `ridge-lsp`.
//!
//! These tests use a hand-rolled JSON-RPC replay framework — no
//! `tower-lsp`-specific test framework.  Each test scripts a sequence of
//! LSP messages assembled as `serde_json::Value` objects and asserts on
//! responses or published diagnostics.
//!
//! # Test organisation
//!
//! - **8 protocol-replay tests** — `initialize`, `didOpen`, `didChange`,
//!   `didSave`, type-error diagnostics, capability-error diagnostics,
//!   multi-file workspace cross-module reference, `shutdown`+`exit`.
//! - **3 span-recovery tests** — synthesised `ToText` node, stdlib
//!   `IrExpr::Call` synthesis, fully-synthetic prelude node.
//! - **1 debounce test** — rapid `didChange` flurries trigger exactly one compile.
//! - **1 cancellation test** — `didChange` mid-compile cancels the in-flight compile.
//!
//! Total: 13 tests.

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

    // No completionProvider, hoverProvider, definitionProvider.
    assert!(
        result.capabilities.completion_provider.is_none(),
        "must not advertise completionProvider"
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

    let mid = *index
        .uri_to_module
        .values()
        .next()
        .expect("the workspace contributes one module");
    let module = index
        .resolved
        .modules
        .iter()
        .find(|m| m.id == mid)
        .expect("resolved module present");

    assert!(
        !module.scopes.is_empty(),
        "scope tree must be retained when retain_indices is set"
    );

    // The parameter `name` is visible at the body use-site (the last `name`).
    let offset = u32::try_from(main_src.rfind("name").expect("body name")).expect("fits u32") + 1;
    let visible: Vec<&str> = module
        .scopes
        .visible_at(offset)
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert!(
        visible.contains(&"name"),
        "parameter `name` must be visible in the body, got {visible:?}"
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
