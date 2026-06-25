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
    clippy::similar_names,
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

/// The capabilities a modern client advertises: Markdown content, parameter
/// label offsets, a hierarchical outline, and `CodeAction` literals. Used by the
/// fixtures so the default test client exercises the richer response forms, the
/// way mainstream editors do. Degradation tests pass narrower capabilities
/// explicitly.
fn full_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            hover: Some(HoverClientCapabilities {
                content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                ..Default::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    documentation_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            signature_help: Some(SignatureHelpClientCapabilities {
                signature_information: Some(SignatureInformationSettings {
                    parameter_information: Some(ParameterInformationSettings {
                        label_offset_support: Some(true),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            code_action: Some(CodeActionClientCapabilities {
                code_action_literal_support: Some(CodeActionLiteralSupport {
                    code_action_kind: CodeActionKindLiteralSupport {
                        value_set: vec![CodeActionKind::QUICKFIX.as_str().to_owned()],
                    },
                }),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
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

    // This client (default capabilities) advertised neither pull diagnostics nor
    // a refresh, so the server stays on the push model and must NOT advertise a
    // diagnosticProvider. Advertising it to a client without a pull engine is what
    // used to make 3.17 clients log `-32601 Method not found`. The provider is
    // gated on pull support; see the dedicated pull tests for the opt-in path.
    assert!(
        result.capabilities.diagnostic_provider.is_none(),
        "must not advertise diagnosticProvider to a push-only client"
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
    assert!(
        result.capabilities.declaration_provider.is_some(),
        "must advertise declarationProvider"
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
    hover_fixture_with_caps(main_src, full_capabilities()).await
}

/// Like [`hover_fixture`] but with explicit client capabilities, for exercising
/// the capability-degraded response encodings.
async fn hover_fixture_with_caps(
    main_src: &'static str,
    caps: ClientCapabilities,
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
                capabilities: caps,
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

    // Body `x` use-site → the parameter it binds, kinded and typed.
    let h = server.hover(hover_at(&uri, 1, 14)).await.expect("hover ok");
    let md = hover_markdown(h).expect("hover over local returns markup");
    assert!(
        md.contains("x :") && md.contains("*(parameter)*"),
        "param hover should show the type and a parameter kind line, got {md:?}"
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
    // A user-defined class method used by its bare name hovers with its written
    // signature and a "class method" kind line. No stdlib card backs a workspace
    // class, so the signature comes from the workspace `class` declaration.
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
        md.contains("render (x: a) -> Text") && md.contains("*(class method)*"),
        "class-method hover should show the signature and kind line, got {md:?}"
    );
}

#[tokio::test]
async fn test_hover_distinguishes_param_from_local() {
    // A function parameter and a `let` binding hover with different kind lines,
    // even though both are `Binding::Local` under the hood.
    let src = "pub fn f x =\n  let y = x\n  y\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // `x` use on line 1 (`  let y = x`) → a parameter.
    let h = server.hover(hover_at(&uri, 1, 10)).await.expect("hover ok");
    let md = hover_markdown(h).expect("hover over a parameter returns markup");
    assert!(
        md.contains("*(parameter)*"),
        "a parameter should be kinded as such, got {md:?}"
    );

    // `y` use on line 2 → a plain local.
    let h = server.hover(hover_at(&uri, 2, 2)).await.expect("hover ok");
    let md = hover_markdown(h).expect("hover over a local returns markup");
    assert!(
        md.contains("*(local)*"),
        "a let binding should be kinded as a local, got {md:?}"
    );
}

#[tokio::test]
async fn test_hover_stdlib_symbol_shows_header_and_doc() {
    // Hovering a stdlib symbol shows the header and `--` doc lifted from the
    // embedded stdlib source — the same card workspace declarations get.
    let src = "import std.list (length)\npub fn run (xs: List Int) -> Int = length xs\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    // The `length` use on line 1.
    let line1 = "pub fn run (xs: List Int) -> Int = length xs";
    let col =
        u32::try_from(line1.find("length").expect("length use") + 1).expect("offset fits u32");
    let h = server
        .hover(hover_at(&uri, 1, col))
        .await
        .expect("hover ok");
    let md = hover_markdown(h).expect("hover over a stdlib symbol returns markup");
    assert!(
        md.contains("pub fn length"),
        "stdlib hover should show the written header, got {md:?}"
    );
    assert!(
        md.contains("*(stdlib function)*"),
        "stdlib hover should carry a stdlib kind line, got {md:?}"
    );
    assert!(
        md.contains("Return the number of elements"),
        "stdlib hover should include the `--` doc, got {md:?}"
    );
}

#[tokio::test]
async fn test_hover_qualified_stdlib_class_method() {
    // `Repo.filter` is the idiomatic data form: the verb is a method of the
    // `Refinable` class, but it is listed in std.repo's exports, so it resolves to
    // a stdlib symbol rather than a class-method binding. The module-scoped
    // class-method fallback gives the qualified form the same card as the bare one.
    let src = "import std.repo as Repo\npub fn run (q: Int) -> Int = Repo.filter q\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let server = service.inner();

    let line1 = "pub fn run (q: Int) -> Int = Repo.filter q";
    let col =
        u32::try_from(line1.find("filter").expect("filter use") + 1).expect("offset fits u32");
    let h = server
        .hover(hover_at(&uri, 1, col))
        .await
        .expect("hover ok");
    let md = hover_markdown(h).expect("hover over a qualified class method returns markup");
    assert!(
        md.contains("filter"),
        "qualified class-method hover should show the method signature, got {md:?}"
    );
    assert!(
        md.contains("*(stdlib class method)*"),
        "qualified class-method hover should carry the stdlib class-method kind line, got {md:?}"
    );
    assert!(
        md.contains("for both a query and a join"),
        "qualified class-method hover should include the class doc, got {md:?}"
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
    two_member_fixture_with("import lib.Lib as Lib\npub fn run -> Int = Lib.helper\n").await
}

/// [`two_member_fixture`] with a caller-chosen `app` source, so a test can pick
/// the import form — aliased `import lib.Lib as Lib` vs selective
/// `import lib.Lib (helper)` — that exercises the binding it needs.
async fn two_member_fixture_with(
    app_text: &str,
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
async fn test_declaration_jumps_to_import_clause() {
    // A selective import (`import lib.Lib (helper)`) binds `helper` locally, so
    // a use of it has two distinct sites: the clause declares the name here, the
    // `fn helper` in Lib.ridge defines it. Declaration and definition split.
    let import_line = "import lib.Lib (helper)";
    let use_line = "pub fn run -> Int = helper";
    let app_text = format!("{import_line}\n{use_line}\n");
    let (service, _socket, app_uri, lib_uri) = two_member_fixture_with(&app_text).await;
    let server = service.inner();

    let use_col =
        u32::try_from(use_line.find("helper").expect("use of helper") + 1).expect("fits u32");

    // Declaration → the import clause item, in the importing file itself.
    let decl = server
        .goto_declaration(goto_at(&app_uri, 1, use_col))
        .await
        .expect("ok");
    let decl = scalar_location(decl).expect("declaration of imported `helper`");
    assert_eq!(
        decl.uri, app_uri,
        "declaration stays in the importing file's clause"
    );
    assert_eq!(decl.range.start.line, 0, "the import clause is on line 1");
    let clause_col =
        u32::try_from(import_line.find("helper").expect("clause item")).expect("fits u32");
    assert_eq!(
        decl.range.start.character, clause_col,
        "declaration lands on the clause item `helper`"
    );

    // Definition of the same use jumps past the import into Lib.ridge.
    let def = server
        .goto_definition(goto_at(&app_uri, 1, use_col))
        .await
        .expect("ok");
    let def = scalar_location(def).expect("definition of imported `helper`");
    assert_eq!(def.uri, lib_uri, "definition must land in Lib.ridge");
    assert_eq!(def.range.start.line, 0, "helper is on line 1 of Lib.ridge");
}

#[tokio::test]
async fn test_declaration_falls_back_to_definition() {
    // The aliased fixture imports `import lib.Lib as Lib` with no selective
    // clause, so an aliased use (`Lib.helper`) has no separate declaration site;
    // go-to-declaration must mirror go-to-definition into Lib.ridge.
    let (service, _socket, app_uri, lib_uri) = two_member_fixture().await;
    let server = service.inner();

    let decl = server
        .goto_declaration(goto_at(&app_uri, 1, 26))
        .await
        .expect("ok");
    let decl = scalar_location(decl).expect("declaration of aliased `helper`");
    assert_eq!(
        decl.uri, lib_uri,
        "no import clause → declaration follows definition"
    );
    assert_eq!(decl.range.start.line, 0, "helper is on line 1 of Lib.ridge");
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
async fn test_definition_into_qualified_class_method() {
    // `import std.repo as Repo` + a point-free `Repo.filter`: the verb is a method
    // of `Refinable`, but it sits in std.repo's exports, so the qualified name
    // binds as a stdlib symbol. Go-to-def must still land on the method in
    // repo.ridge, through the module-scoped class-method fallback.
    let line1 = "pub fn run = Repo.filter";
    let (service, _socket, uri) =
        hover_fixture("import std.repo as Repo\npub fn run = Repo.filter\n").await;
    let server = service.inner();

    let col =
        u32::try_from(line1.find("Repo.filter").expect("alias use") + 6).expect("offset fits u32");
    let resp = server
        .goto_definition(goto_at(&uri, 1, col))
        .await
        .expect("ok");
    let loc = scalar_location(resp).expect("definition of qualified `Repo.filter`");
    let path = loc
        .uri
        .to_file_path()
        .expect("definition uri is a file path");
    assert!(
        path.ends_with("repo.ridge"),
        "qualified class-method definition must land in repo.ridge, got {path:?}"
    );
    assert!(
        loc.range.start.line > 0 || loc.range.start.character > 0,
        "definition range must not be the file start, got {:?}",
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
async fn test_references_honors_cancellation() {
    use ridge_lsp::cancel::Cancel;

    // Same fixture as the local case: `foo` binds `x`, used twice in the body.
    let src = "pub fn foo x = x + x\n";
    let (service, _socket, uri) = hover_fixture(src).await;
    let index = service
        .inner()
        .workspace_index()
        .await
        .expect("index built");

    // A live token returns the full reference set (binder plus both uses).
    let live = Cancel::new();
    let found = index
        .references_at(&uri, 0, 15, true, &live)
        .expect("references of local `x`");
    assert_eq!(found.len(), 3, "binder and both uses with a live token");

    // A cancelled token short-circuits the per-module scan before it collects
    // anything: the same query yields nothing. This is the cooperative half of
    // `$/cancelRequest` — the handler drops its guard, the flag flips, and the
    // scan bails instead of finishing a result the client has discarded.
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert!(
        index.references_at(&uri, 0, 15, true, &cancelled).is_none(),
        "a cancelled scan yields no references"
    );
}

#[tokio::test]
async fn test_code_lens_run_and_test() {
    use ridge_lsp::index::{CodeLensConfig, RUN_COMMAND, RUN_TEST_COMMAND};

    // An app project earns a Run lens on `fn main` and a Run-test lens on each
    // `@test` function. Both carry a ready client command — no resolve round-trip.
    let app_text = "pub fn main -> Int = 0\n@test \"adds one\"\nfn check_add -> Int = 1\n";
    let (service, _socket, app_uri, _lib_uri) = two_member_fixture_with(app_text).await;
    let index = service
        .inner()
        .workspace_index()
        .await
        .expect("index built");

    let cfg = CodeLensConfig {
        references: false,
        implementations: false,
        run: true,
        run_test: true,
    };
    let lenses = index
        .code_lenses_at(&app_uri, cfg)
        .expect("lenses for the app module");

    let run = lenses
        .iter()
        .find(|l| l.command.as_ref().is_some_and(|c| c.command == RUN_COMMAND))
        .expect("a Run lens on `fn main`");
    assert_eq!(run.range.start.line, 0, "Run lens sits on `fn main`");
    let run_args = run
        .command
        .as_ref()
        .unwrap()
        .arguments
        .as_ref()
        .expect("Run carries arguments");
    assert_eq!(
        run_args[0].as_str(),
        Some("app"),
        "Run targets the app member"
    );

    let test = lenses
        .iter()
        .find(|l| {
            l.command
                .as_ref()
                .is_some_and(|c| c.command == RUN_TEST_COMMAND)
        })
        .expect("a Run-test lens on the `@test` function");
    assert_eq!(
        test.range.start.line, 2,
        "Run-test lens sits on the `@test` fn"
    );
    let test_args = test
        .command
        .as_ref()
        .unwrap()
        .arguments
        .as_ref()
        .expect("Run-test carries arguments");
    assert_eq!(test_args[0].as_str(), Some("app"));
    assert_eq!(
        test_args[1].as_str(),
        Some("adds one"),
        "the test filter uses the @test display name"
    );
}

#[tokio::test]
async fn test_code_lens_references_resolve_counts_uses() {
    use ridge_lsp::cancel::Cancel;
    use ridge_lsp::index::CodeLensConfig;

    // Default fixture: app's `run` calls `Lib.helper`, so `helper` has one use.
    let (service, _socket, _app_uri, lib_uri) = two_member_fixture().await;
    let index = service
        .inner()
        .workspace_index()
        .await
        .expect("index built");

    let cfg = CodeLensConfig {
        references: true,
        implementations: false,
        run: false,
        run_test: false,
    };
    let mut lenses = index
        .code_lenses_at(&lib_uri, cfg)
        .expect("lenses for the lib module");
    // Lib holds a single declaration (`fn helper`), so a single reference lens,
    // and it carries no command until resolve fills in the count.
    assert_eq!(lenses.len(), 1, "one reference lens on `fn helper`");
    let lens = lenses.pop().unwrap();
    assert!(
        lens.command.is_none(),
        "a navigational lens resolves lazily"
    );

    let resolved = index.resolve_code_lens(lens, &Cancel::new());
    let command = resolved.command.expect("resolve fills in the command");
    assert_eq!(command.command, "editor.action.showReferences");
    assert_eq!(command.title, "1 reference", "`helper` is used once");
    assert_eq!(
        command.arguments.as_ref().map(Vec::len),
        Some(3),
        "showReferences takes (uri, position, locations)"
    );
}

#[tokio::test]
async fn test_code_lens_capability_gated_on_opt_in() {
    // Without the opt-in the provider is absent, so a generic client is never
    // served lenses whose commands it can't run.
    let (service_off, _s1) = build_test_service();
    let caps_off = service_off
        .inner()
        .initialize(InitializeParams::default())
        .await
        .expect("initialize")
        .capabilities;
    assert!(
        caps_off.code_lens_provider.is_none(),
        "no codeLens provider until the client opts in"
    );

    // Opting in via initializationOptions advertises the provider, with lazy
    // resolve for the reference/implementation counts.
    let (service_on, _s2) = build_test_service();
    let options: serde_json::Value = serde_json::from_str(
        r#"{"codeLens":{"references":true,"implementations":true,"run":true,"runTest":true}}"#,
    )
    .expect("valid options json");
    let caps_on = service_on
        .inner()
        .initialize(InitializeParams {
            initialization_options: Some(options),
            ..InitializeParams::default()
        })
        .await
        .expect("initialize")
        .capabilities;
    let provider = caps_on
        .code_lens_provider
        .expect("codeLens provider when opted in");
    assert_eq!(
        provider.resolve_provider,
        Some(true),
        "reference/implementation counts resolve lazily"
    );
}

#[tokio::test]
async fn test_code_lens_capability_advertised_when_present_but_all_off() {
    // The provider is gated on the client expressing interest — the `codeLens` key
    // being present — not on a flag being on. That keeps the capability stable so a
    // later `didChangeConfiguration` can turn an individual lens on and have the
    // refresh actually land.
    let (service, _s) = build_test_service();
    let options: serde_json::Value = serde_json::from_str(r#"{"codeLens":{}}"#).expect("json");
    let caps = service
        .inner()
        .initialize(InitializeParams {
            initialization_options: Some(options),
            ..InitializeParams::default()
        })
        .await
        .expect("initialize")
        .capabilities;
    assert!(
        caps.code_lens_provider.is_some(),
        "codeLens provider advertised once the client opts in, even with all flags off"
    );
}

/// A `textDocument/codeLens` request for `uri`.
fn code_lens_params(uri: Url) -> CodeLensParams {
    CodeLensParams {
        text_document: TextDocumentIdentifier { uri },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

/// A `workspace/didChangeConfiguration` carrying the four code-lens flags under a
/// `ridge` section, the shape an editor that namespaces its settings sends.
#[allow(clippy::fn_params_excessive_bools)] // one arg per code-lens flag, by design
fn code_lens_config_change(
    references: bool,
    implementations: bool,
    run: bool,
    run_test: bool,
) -> DidChangeConfigurationParams {
    DidChangeConfigurationParams {
        settings: serde_json::json!({
            "ridge": {
                "codeLens": {
                    "references": references,
                    "implementations": implementations,
                    "run": run,
                    "runTest": run_test,
                }
            }
        }),
    }
}

/// Poll the client log until a `workspace/codeLens/refresh` request arrives, or
/// panic after ~6s.
async fn wait_for_code_lens_refresh(log: &Arc<Mutex<ProgressLog>>) {
    for _ in 0..120 {
        if log.lock().unwrap().code_lens_refreshed > 0 {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    panic!("expected a workspace/codeLens/refresh request");
}

#[tokio::test]
async fn test_did_change_configuration_toggles_code_lenses() {
    // Open with the references lens on, refresh support advertised.
    let init = serde_json::json!({
        "codeLens": { "references": true, "implementations": false, "run": false, "runTest": false }
    });
    let (service, log, root) = init_test_workspace(
        &[("Lib.ridge", "pub fn helper -> Int = 1\n")],
        false,
        Some(init),
        true,
    )
    .await;
    let server = service.inner();
    let uri = Url::from_file_path(root.join("app").join("src").join("Lib.ridge")).expect("uri");

    let before = server
        .code_lens(code_lens_params(uri.clone()))
        .await
        .expect("code_lens");
    assert_eq!(
        before.as_ref().map(Vec::len),
        Some(1),
        "one reference lens on `helper` before the change"
    );

    // Turn every lens off at runtime.
    server
        .did_change_configuration(code_lens_config_change(false, false, false, false))
        .await;

    // The server asks the client to re-query, and the lens is now gone.
    wait_for_code_lens_refresh(&log).await;
    let after = server
        .code_lens(code_lens_params(uri))
        .await
        .expect("code_lens");
    assert!(
        after.is_none(),
        "no lenses once references is disabled, got {after:?}"
    );
}

#[tokio::test]
async fn test_did_change_configuration_ignores_unrelated_settings() {
    let init = serde_json::json!({
        "codeLens": { "references": true, "implementations": false, "run": false, "runTest": false }
    });
    let (service, log, root) = init_test_workspace(
        &[("Lib.ridge", "pub fn helper -> Int = 1\n")],
        false,
        Some(init),
        true,
    )
    .await;
    let server = service.inner();
    let uri = Url::from_file_path(root.join("app").join("src").join("Lib.ridge")).expect("uri");

    // A pull-model nudge (`settings: null`) carries no `codeLens` object and must
    // not be read as "all off".
    server
        .did_change_configuration(DidChangeConfigurationParams {
            settings: serde_json::Value::Null,
        })
        .await;
    // An unrelated section likewise leaves the config untouched.
    server
        .did_change_configuration(DidChangeConfigurationParams {
            settings: serde_json::json!({ "ridge": { "lspPath": "/somewhere" } }),
        })
        .await;
    // Re-sending the same flags is a no-op — no spurious refresh.
    server
        .did_change_configuration(code_lens_config_change(true, false, false, false))
        .await;

    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
    assert_eq!(
        log.lock().unwrap().code_lens_refreshed,
        0,
        "no refresh for a no-op configuration change"
    );

    // The original lens is still served — nothing clobbered the config.
    let lenses = server
        .code_lens(code_lens_params(uri))
        .await
        .expect("code_lens");
    assert_eq!(
        lenses.as_ref().map(Vec::len),
        Some(1),
        "config unchanged, the reference lens is still present"
    );
}

#[tokio::test]
async fn test_did_change_configuration_refresh_gated_on_capability() {
    // Same change, but the client never advertised refresh support.
    let init = serde_json::json!({
        "codeLens": { "references": true, "implementations": false, "run": false, "runTest": false }
    });
    let (service, log, root) = init_test_workspace(
        &[("Lib.ridge", "pub fn helper -> Int = 1\n")],
        false,
        Some(init),
        false,
    )
    .await;
    let server = service.inner();
    let uri = Url::from_file_path(root.join("app").join("src").join("Lib.ridge")).expect("uri");

    server
        .did_change_configuration(code_lens_config_change(false, false, false, false))
        .await;

    // The change still applies internally...
    let after = server
        .code_lens(code_lens_params(uri))
        .await
        .expect("code_lens");
    assert!(
        after.is_none(),
        "the config change applies even without refresh support"
    );

    // ...but the server must not send a refresh the client can't handle.
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
    assert_eq!(
        log.lock().unwrap().code_lens_refreshed,
        0,
        "no refresh request without workspace.codeLens.refreshSupport"
    );
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

// ── Union variant references / highlight / rename ─────────────────────────────

// A union with two variants. `Red` is declared (line 0), used as a constructor
// expression (line 1), and matched as a pattern (line 4); `Green` is declared
// (line 0) and matched (line 5). A rename of `Red` must move its three sites
// and leave `Green` — a sibling variant of the same type — untouched.
const VARIANT_RENAME_SRC: &str = "type Color = Red | Green\npub fn pick -> Color = Red\npub fn name (c: Color) -> Text =\n    match c\n        Red -> \"r\"\n        Green -> \"g\"\n";

/// The `(start.line, start.character)` of every reference that lands in `uri`.
fn ref_spots(locs: &[Location], uri: &Url) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = locs
        .iter()
        .filter(|l| &l.uri == uri)
        .map(|l| (l.range.start.line, l.range.start.character))
        .collect();
    out.sort_unstable();
    out
}

#[tokio::test]
async fn test_references_union_variant() {
    let (service, _socket, uri) = hover_fixture(VARIANT_RENAME_SRC).await;
    let server = service.inner();

    // From the constructor expression `Red` (1,23): the declaration (0,13), the
    // expression, and the pattern (4,8) — never the sibling `Green`.
    let with_decl = server
        .references(references_at(&uri, 1, 23, true))
        .await
        .expect("ok")
        .expect("references of variant `Red`");
    assert_eq!(
        ref_spots(&with_decl, &uri),
        vec![(0, 13), (1, 23), (4, 8)],
        "includeDeclaration=true returns the declaration and both uses"
    );

    // includeDeclaration=false drops the declaration name, leaving the two uses.
    let without_decl = server
        .references(references_at(&uri, 1, 23, false))
        .await
        .expect("ok")
        .expect("references of variant `Red`");
    assert_eq!(
        ref_spots(&without_decl, &uri),
        vec![(1, 23), (4, 8)],
        "includeDeclaration=false returns only the uses"
    );
}

#[tokio::test]
async fn test_highlight_union_variant() {
    let (service, _socket, uri) = hover_fixture(VARIANT_RENAME_SRC).await;
    let server = service.inner();

    let expected = vec![
        (0, 13, DocumentHighlightKind::WRITE),
        (1, 23, DocumentHighlightKind::READ),
        (4, 8, DocumentHighlightKind::READ),
    ];

    // From the constructor expression use.
    let from_use = server
        .document_highlight(highlight_at(&uri, 1, 23))
        .await
        .expect("ok")
        .expect("highlights from a variant use");
    assert_eq!(
        highlight_spots(&from_use),
        expected,
        "the declaration is the write, both uses are reads"
    );

    // From the declaration name — same set.
    let from_decl = server
        .document_highlight(highlight_at(&uri, 0, 13))
        .await
        .expect("ok")
        .expect("highlights from the variant declaration");
    assert_eq!(
        highlight_spots(&from_decl),
        expected,
        "highlighting from the declaration reaches every use"
    );
}

#[tokio::test]
async fn test_rename_union_variant() {
    let sites = vec![(0, 13), (1, 23), (4, 8)];

    // From the pattern use (4,8).
    let (service, _socket, uri) = hover_fixture(VARIANT_RENAME_SRC).await;
    let server = service.inner();
    let edit = server
        .rename(rename_at(&uri, 4, 8, "Crimson"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        sites,
        "the declaration and both uses move; the sibling `Green` does not"
    );
    assert!(
        rename_edits(edit.as_ref(), &uri)
            .iter()
            .all(|(_, t)| t == "Crimson"),
        "every edit rewrites to the new name"
    );

    // From the declaration name (0,13) — same edit set.
    let (service, _socket, uri) = hover_fixture(VARIANT_RENAME_SRC).await;
    let server = service.inner();
    let edit = server
        .rename(rename_at(&uri, 0, 13, "Crimson"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        sites,
        "renaming from the declaration reaches the same sites"
    );
}

#[tokio::test]
async fn test_rename_union_variant_sibling_isolated() {
    let (service, _socket, uri) = hover_fixture(VARIANT_RENAME_SRC).await;
    let server = service.inner();

    // Renaming `Green` from its pattern (5,8) touches only the `Green`
    // declaration (0,19) and that pattern — never any `Red` site.
    let edit = server
        .rename(rename_at(&uri, 5, 8, "Lime"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        vec![(0, 19), (5, 8)],
        "a variant rename is scoped to that one variant"
    );
}

#[tokio::test]
async fn test_prepare_rename_union_variant() {
    let (service, _socket, uri) = hover_fixture(VARIANT_RENAME_SRC).await;
    let server = service.inner();

    for (line, character) in [(1, 23), (0, 13)] {
        let prep = server
            .prepare_rename(prepare_rename_at(&uri, line, character))
            .await
            .expect("ok")
            .expect("a union variant is renameable");
        match prep {
            PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } => {
                assert_eq!(
                    placeholder, "Red",
                    "placeholder is the current variant name"
                );
            }
            other => panic!("expected RangeWithPlaceholder, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn test_rename_union_variant_rejects_lowercase() {
    let (service, _socket, uri) = hover_fixture(VARIANT_RENAME_SRC).await;
    let server = service.inner();

    // A variant is an uppercase name, so a lowercase target is rejected.
    let err = server
        .rename(rename_at(&uri, 4, 8, "crimson"))
        .await
        .expect_err("a lowercase name is not a valid variant");
    assert!(
        err.message.contains("uppercase"),
        "the error explains the variant must stay uppercase, got: {}",
        err.message
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

// ── Class references / highlight / rename ─────────────────────────────────────

// A class `Animal` with a method, a subclass `Pet` that requires it, two
// instances of `Animal` (on `Int` and parametrically on `List a`), an instance
// of `Pet`, and two generic functions constrained by a class. `Animal` is named
// in every class-name position the server resolves — a `class` declaration head
// (0,10), a superclass constraint (2,22), an `instance` head (4,9 and 6,9), an
// instance-context constraint (6,31), and a function `where` clause (10,37) —
// so a rename of `Animal` must move all six and leave `Pet` untouched. `ToText`
// (11,35) is a prelude class with no workspace declaration: it is not renameable.
// Lines (0-indexed):
//   0  pub class Animal a =
//   1    sound (x: a) -> Text
//   2  pub class Pet a where Animal a =
//   3    name (x: a) -> Text
//   4  instance Animal Int =
//   5    sound (x: Int) -> Text = "g"
//   6  instance Animal (List a) where Animal a =
//   7    sound (xs: List a) -> Text = "l"
//   8  instance Pet Int =
//   9    name (x: Int) -> Text = "p"
//  10  pub fn describe (x: a) -> Text where Animal a = "d"
//  11  pub fn render (x: a) -> Text where ToText a = "r"
const CLASS_RENAME_SRC: &str = concat!(
    "pub class Animal a =\n",
    "  sound (x: a) -> Text\n",
    "pub class Pet a where Animal a =\n",
    "  name (x: a) -> Text\n",
    "instance Animal Int =\n",
    "  sound (x: Int) -> Text = \"g\"\n",
    "instance Animal (List a) where Animal a =\n",
    "  sound (xs: List a) -> Text = \"l\"\n",
    "instance Pet Int =\n",
    "  name (x: Int) -> Text = \"p\"\n",
    "pub fn describe (x: a) -> Text where Animal a = \"d\"\n",
    "pub fn render (x: a) -> Text where ToText a = \"r\"\n",
);

#[tokio::test]
async fn test_references_class() {
    let (service, _socket, uri) = hover_fixture(CLASS_RENAME_SRC).await;
    let server = service.inner();

    // From the superclass constraint `Animal` (2,22): the declaration plus every
    // instance head and constraint that names the class — never a `Pet` site.
    let with_decl = server
        .references(references_at(&uri, 2, 22, true))
        .await
        .expect("ok")
        .expect("references of class `Animal`");
    assert_eq!(
        ref_spots(&with_decl, &uri),
        vec![(0, 10), (2, 22), (4, 9), (6, 9), (6, 31), (10, 37)],
        "includeDeclaration=true returns the class head and every reference"
    );
    assert!(
        with_decl.iter().all(|l| l.uri == uri),
        "every reference lands in the source file"
    );

    // includeDeclaration=false drops the class head, leaving the references.
    let without_decl = server
        .references(references_at(&uri, 2, 22, false))
        .await
        .expect("ok")
        .expect("references of class `Animal`");
    assert_eq!(
        ref_spots(&without_decl, &uri),
        vec![(2, 22), (4, 9), (6, 9), (6, 31), (10, 37)],
        "includeDeclaration=false returns only the references"
    );
}

#[tokio::test]
async fn test_highlight_class() {
    let (service, _socket, uri) = hover_fixture(CLASS_RENAME_SRC).await;
    let server = service.inner();

    let expected = vec![
        (0, 10, DocumentHighlightKind::WRITE),
        (2, 22, DocumentHighlightKind::READ),
        (4, 9, DocumentHighlightKind::READ),
        (6, 9, DocumentHighlightKind::READ),
        (6, 31, DocumentHighlightKind::READ),
        (10, 37, DocumentHighlightKind::READ),
    ];

    // From an instance head.
    let from_use = server
        .document_highlight(highlight_at(&uri, 4, 9))
        .await
        .expect("ok")
        .expect("highlights from a class use");
    assert_eq!(
        highlight_spots(&from_use),
        expected,
        "the declaration head is the write, every reference is a read"
    );

    // From the declaration name — same set.
    let from_decl = server
        .document_highlight(highlight_at(&uri, 0, 10))
        .await
        .expect("ok")
        .expect("highlights from the class declaration");
    assert_eq!(
        highlight_spots(&from_decl),
        expected,
        "highlighting from the declaration reaches every reference"
    );
}

#[tokio::test]
async fn test_rename_class() {
    let sites = vec![(0, 10), (2, 22), (4, 9), (6, 9), (6, 31), (10, 37)];

    // From the function `where` clause (10,37).
    let (service, _socket, uri) = hover_fixture(CLASS_RENAME_SRC).await;
    let server = service.inner();
    let edit = server
        .rename(rename_at(&uri, 10, 37, "Creature"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        sites,
        "the declaration and every reference move; `Pet` does not"
    );
    assert!(
        rename_edits(edit.as_ref(), &uri)
            .iter()
            .all(|(_, t)| t == "Creature"),
        "every edit rewrites to the new name"
    );

    // From the declaration name (0,10) — same edit set.
    let (service, _socket, uri) = hover_fixture(CLASS_RENAME_SRC).await;
    let server = service.inner();
    let edit = server
        .rename(rename_at(&uri, 0, 10, "Creature"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        sites,
        "renaming from the declaration reaches the same sites"
    );
}

#[tokio::test]
async fn test_rename_class_sibling_isolated() {
    let (service, _socket, uri) = hover_fixture(CLASS_RENAME_SRC).await;
    let server = service.inner();

    // Renaming `Pet` from its instance head (8,9) touches only the `Pet`
    // declaration (2,10) and that instance head — never any `Animal` site.
    let edit = server
        .rename(rename_at(&uri, 8, 9, "Companion"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &uri),
        vec![(2, 10), (8, 9)],
        "a class rename is scoped to that one class by name"
    );
}

#[tokio::test]
async fn test_prepare_rename_class() {
    let (service, _socket, uri) = hover_fixture(CLASS_RENAME_SRC).await;
    let server = service.inner();

    // Both a use (instance head) and the declaration name offer the same range.
    for (line, character) in [(4, 9), (0, 10)] {
        let prep = server
            .prepare_rename(prepare_rename_at(&uri, line, character))
            .await
            .expect("ok")
            .expect("a workspace class is renameable");
        match prep {
            PrepareRenameResponse::RangeWithPlaceholder { placeholder, .. } => {
                assert_eq!(
                    placeholder, "Animal",
                    "placeholder is the current class name"
                );
            }
            other => panic!("expected RangeWithPlaceholder, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn test_rename_class_rejects_lowercase() {
    let (service, _socket, uri) = hover_fixture(CLASS_RENAME_SRC).await;
    let server = service.inner();

    // A class is an uppercase name, so a lowercase target is rejected.
    let err = server
        .rename(rename_at(&uri, 4, 9, "creature"))
        .await
        .expect_err("a lowercase name is not a valid class");
    assert!(
        err.message.contains("uppercase"),
        "the error explains the class must stay uppercase, got: {}",
        err.message
    );
}

#[tokio::test]
async fn test_class_without_workspace_decl_not_renameable() {
    let (service, _socket, uri) = hover_fixture(CLASS_RENAME_SRC).await;
    let server = service.inner();

    // `ToText` is a prelude class with no `class` declaration in the workspace,
    // so prepareRename declines outright rather than offering a range.
    let prep = server
        .prepare_rename(prepare_rename_at(&uri, 11, 35))
        .await
        .expect("ok");
    assert!(
        prep.is_none(),
        "a prelude class with no workspace declaration is not renameable"
    );

    // A rename attempted directly (a client that skips prepareRename) is refused
    // with a message rather than silently editing prelude references.
    let err = server
        .rename(rename_at(&uri, 11, 35, "Display"))
        .await
        .expect_err("a prelude class cannot be renamed");
    assert!(
        err.message.contains("not declared in this workspace"),
        "the error explains the class is not declared here, got: {}",
        err.message
    );
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

// ── Test 25b: range formatting (textDocument/rangeFormatting) ─────────────────

fn range_format_params(uri: &Url, range: Range) -> DocumentRangeFormattingParams {
    DocumentRangeFormattingParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        range,
        options: FormattingOptions::default(),
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

#[tokio::test]
async fn test_range_formatting_reformats_only_selection() {
    // Both bodies have a tight `+`; the formatter would space both, but the
    // selection covers only the second function's body (line 4).
    let messy = "fn a (x: Int) -> Int =\n    x+1\n\nfn b (y: Int) -> Int =\n    y+2\n";
    let (service, _socket, uri) = open_single_doc(messy).await;
    let server = service.inner();

    let range = Range {
        start: Position::new(4, 0),
        end: Position::new(4, 0),
    };
    let edits = server
        .range_formatting(range_format_params(&uri, range))
        .await
        .expect("range formatting ok")
        .expect("a messy selection yields an edit");

    assert_eq!(edits.len(), 1, "only the selected line is reformatted");
    assert_eq!(edits[0].new_text, "    y + 2\n");
    // The first body (line 1) is left untouched by a selection on line 4.
    assert_eq!(edits[0].range.start, Position::new(4, 0));
}

#[tokio::test]
async fn test_range_formatting_noop_on_already_formatted() {
    let formatted = ridge_fmt::format_source("fn a (x: Int) -> Int =\n    x+1\n").expect("formats");
    let (service, _socket, uri) = open_single_doc(&formatted).await;
    let server = service.inner();

    let range = Range {
        start: Position::new(0, 0),
        end: Position::new(1, 0),
    };
    let edits = server
        .range_formatting(range_format_params(&uri, range))
        .await
        .expect("range formatting ok");
    assert!(
        edits.is_none(),
        "an already-formatted selection yields nothing"
    );
}

#[tokio::test]
async fn test_range_formatting_skips_unparseable() {
    let broken = "fn = = =\n";
    assert!(ridge_fmt::format_source(broken).is_err());
    let (service, _socket, uri) = open_single_doc(broken).await;
    let server = service.inner();

    let range = Range {
        start: Position::new(0, 0),
        end: Position::new(0, 5),
    };
    let edits = server
        .range_formatting(range_format_params(&uri, range))
        .await
        .expect("range formatting ok");
    assert!(
        edits.is_none(),
        "a buffer the parser rejects is left untouched"
    );
}

// ── Test 25c: on-type formatting (textDocument/onTypeFormatting) ──────────────

fn on_type_params(uri: &Url, position: Position, ch: &str) -> DocumentOnTypeFormattingParams {
    DocumentOnTypeFormattingParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        ch: ch.to_owned(),
        options: FormattingOptions::default(),
    }
}

#[tokio::test]
async fn test_on_type_formatting_indents_after_opener() {
    // A newline was just typed under `fn add x y =`, leaving a blank line 1.
    let text = "fn add x y =\n\n";
    let (service, _socket, uri) = open_single_doc(text).await;
    let server = service.inner();

    let edits = server
        .on_type_formatting(on_type_params(&uri, Position::new(1, 0), "\n"))
        .await
        .expect("on-type formatting ok")
        .expect("a newline under an opener indents the fresh line");

    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "  ");
    assert_eq!(edits[0].range.start, Position::new(1, 0));
}

#[tokio::test]
async fn test_on_type_formatting_ignores_other_trigger() {
    let text = "fn add x y =\n\n";
    let (service, _socket, uri) = open_single_doc(text).await;
    let server = service.inner();

    let edits = server
        .on_type_formatting(on_type_params(&uri, Position::new(1, 0), "x"))
        .await
        .expect("on-type formatting ok");
    assert!(edits.is_none(), "only the newline trigger produces edits");
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
    cap_workspace_fixture_with_caps(main_src, full_capabilities()).await
}

/// Like [`cap_workspace_fixture`] but with explicit client capabilities, for
/// exercising the capability-degraded `codeAction` encoding.
async fn cap_workspace_fixture_with_caps(
    main_src: &str,
    caps: ClientCapabilities,
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
                capabilities: caps,
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

fn semantic_delta_params(uri: &Url, previous_result_id: &str) -> SemanticTokensDeltaParams {
    SemanticTokensDeltaParams {
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        previous_result_id: previous_result_id.to_owned(),
    }
}

/// Run a full request and return its `resultId` together with the decoded tokens.
async fn full_with_id(
    server: &RidgeLanguageServer,
    uri: &Url,
) -> (String, Vec<(u32, u32, u32, String, Vec<String>)>) {
    match server
        .semantic_tokens_full(semantic_params(uri))
        .await
        .expect("semantic_tokens ok")
    {
        Some(SemanticTokensResult::Tokens(t)) => (
            t.result_id.expect("a full result carries a resultId"),
            decode_tokens(&t.data),
        ),
        _ => panic!("expected full tokens"),
    }
}

#[tokio::test]
async fn test_semantic_tokens_full_stamps_result_id() {
    let (service, _socket, uri) = hover_fixture(SEMANTIC_SRC).await;
    let server = service.inner();
    let (id, toks) = full_with_id(server, &uri).await;
    assert!(
        !id.is_empty(),
        "the resultId must be non-empty so the client can delta against it"
    );
    assert!(!toks.is_empty(), "the document has tokens");
}

#[tokio::test]
async fn test_semantic_tokens_delta_with_no_edit_has_no_edits() {
    let (service, _socket, uri) = hover_fixture(SEMANTIC_SRC).await;
    let server = service.inner();
    let (id, _) = full_with_id(server, &uri).await;

    // No edit between the full request and the delta: the streams match, so the
    // edit list is empty and the response advances the resultId.
    let result = server
        .semantic_tokens_full_delta(semantic_delta_params(&uri, &id))
        .await
        .expect("delta ok");
    match result {
        Some(SemanticTokensFullDeltaResult::TokensDelta(delta)) => {
            assert!(
                delta.edits.is_empty(),
                "an unedited document yields no edits, got {:?}",
                delta.edits
            );
            assert!(delta.result_id.is_some(), "the delta carries a resultId");
            assert_ne!(
                delta.result_id.as_deref(),
                Some(id.as_str()),
                "the delta stamps a fresh resultId"
            );
        }
        other => panic!("expected a TokensDelta, got {other:?}"),
    }
}

#[tokio::test]
async fn test_semantic_tokens_delta_unknown_id_falls_back_to_full() {
    let (service, _socket, uri) = hover_fixture(SEMANTIC_SRC).await;
    let server = service.inner();
    // The server never served a full result under this id, so it can't diff — it
    // must return the whole stream rather than an edit list.
    let result = server
        .semantic_tokens_full_delta(semantic_delta_params(&uri, "does-not-exist"))
        .await
        .expect("delta ok");
    match result {
        Some(SemanticTokensFullDeltaResult::Tokens(t)) => {
            assert!(
                t.result_id.is_some(),
                "the full fallback carries a resultId"
            );
            assert!(
                !decode_tokens(&t.data).is_empty(),
                "the fallback carries the whole token stream"
            );
        }
        other => panic!("expected a full Tokens fallback, got {other:?}"),
    }
}

#[tokio::test]
async fn test_semantic_tokens_capability_advertises_delta() {
    let (service, _socket) = build_test_service();
    let server = service.inner();
    let result = server
        .initialize(make_init_params("ok_workspace"))
        .await
        .expect("initialize ok");
    match result.capabilities.semantic_tokens_provider {
        Some(SemanticTokensServerCapabilities::SemanticTokensOptions(opts)) => {
            assert_eq!(
                opts.full,
                Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                "full must advertise delta support"
            );
            assert_eq!(opts.range, Some(true), "range support stays advertised");
        }
        other => panic!("expected SemanticTokensOptions, got {other:?}"),
    }
}

// ── Document links (import path → module file) ────────────────────────────────

/// A `documentLink` request for `uri` with default progress fields.
fn document_link_params(uri: &Url) -> DocumentLinkParams {
    DocumentLinkParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams::default(),
    }
}

#[tokio::test]
async fn test_document_link_capability_advertised() {
    let (service, _socket) = build_test_service();
    let server = service.inner();
    let result = server
        .initialize(make_init_params("ok_workspace"))
        .await
        .expect("initialize ok");
    let opts = result
        .capabilities
        .document_link_provider
        .expect("document link provider advertised");
    assert_eq!(
        opts.resolve_provider, None,
        "links resolve their target eagerly, so no resolve step is advertised"
    );
}

#[tokio::test]
async fn test_document_link_points_import_to_module_file() {
    // The link covers the dotted path `lib.Lib` on line 0 and opens Lib.ridge.
    let app_text = "import lib.Lib as Lib\npub fn run -> Int = Lib.helper\n";
    let (service, _socket, app_uri, lib_uri) = two_member_fixture_with(app_text).await;
    let server = service.inner();

    let links = server
        .document_link(document_link_params(&app_uri))
        .await
        .expect("document link ok")
        .expect("a link list for an indexed file");
    assert_eq!(
        links.len(),
        1,
        "one workspace import → one link, got {links:?}"
    );
    let link = &links[0];
    assert_eq!(link.target.as_ref(), Some(&lib_uri), "link opens Lib.ridge");
    assert_eq!(link.range.start.line, 0, "the import is on line 0");
    let path_start = u32::try_from(app_text.find("lib.Lib").expect("path")).expect("fits u32");
    let path_end = path_start + u32::try_from("lib.Lib".len()).expect("fits u32");
    assert_eq!(
        link.range.start.character, path_start,
        "link starts at the dotted path"
    );
    assert_eq!(
        link.range.end.character, path_end,
        "link ends at the dotted path, before ` as Lib`"
    );
    // The default fixture client advertises no tooltipSupport, so it is stripped.
    assert_eq!(
        link.tooltip, None,
        "tooltip is withheld without client support"
    );
}

#[tokio::test]
async fn test_document_link_index_builds_tooltip() {
    // The index always builds the tooltip; gating it on tooltipSupport is the
    // server's job (verified stripped in the handler test above).
    let (service, _socket, app_uri, _lib_uri) = two_member_fixture().await;
    let server = service.inner();
    let index = server.workspace_index().await.expect("index installed");
    let links = index
        .document_links_at(&app_uri)
        .expect("a link list for an indexed file");
    assert_eq!(links.len(), 1, "one workspace import → one link");
    assert!(
        links[0]
            .tooltip
            .as_deref()
            .is_some_and(|t| t.contains("module")),
        "the index attaches a module tooltip, got {:?}",
        links[0].tooltip
    );
}

#[tokio::test]
async fn test_document_link_skips_stdlib_imports() {
    // A stdlib import has no materialized source file, so it produces no link.
    let (service, _socket, uri) = hover_fixture("import std.list as L\npub fn run = L.map\n").await;
    let server = service.inner();
    let links = server
        .document_link(document_link_params(&uri))
        .await
        .expect("document link ok")
        .expect("an empty link list for an indexed file");
    assert!(
        links.is_empty(),
        "stdlib imports are not linked, got {links:?}"
    );
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

#[tokio::test]
async fn test_completion_resolve_fills_stdlib_member_signature_and_doc() {
    // A stdlib member completion (`L.map` after `import std.list as L`) carries a
    // resolve payload pointing at the builtin module, so resolve fills the written
    // signature and the `--` doc lifted from the stdlib source — the same material
    // hover shows for the symbol.
    let line1 = "pub fn run = L.map";
    let (service, _socket, uri) = hover_fixture("import std.list as L\npub fn run = L.map\n").await;
    let server = service.inner();

    let col = u32::try_from(line1.find("L.").expect("alias use") + 2).expect("offset fits u32");
    let items = completion_items(
        server
            .completion(complete_at(&uri, 1, col))
            .await
            .expect("ok"),
    );
    let map = items
        .into_iter()
        .find(|i| i.label == "map")
        .expect("std.list member access offers `map`");

    // The list item carries a payload but stays cheap until the editor resolves it.
    assert!(map.data.is_some(), "a stdlib member carries resolve data");
    assert!(map.detail.is_none(), "detail is filled only on resolve");
    assert!(map.documentation.is_none(), "doc is filled only on resolve");

    let resolved = server.completion_resolve(map).await.expect("resolve ok");
    let detail = resolved.detail.expect("resolve fills the stdlib signature");
    assert!(
        detail.contains("pub fn map") && detail.contains("-> List b"),
        "detail should be the written signature, got {detail:?}"
    );
    let doc = match resolved.documentation {
        Some(Documentation::MarkupContent(m)) => m.value,
        other => panic!("expected a markdown doc, got {other:?}"),
    };
    assert!(
        doc.contains("Apply a function to each element"),
        "resolve fills the stdlib doc, got {doc}"
    );
}

#[tokio::test]
async fn test_completion_resolve_fills_qualified_class_method() {
    // `Repo.` lists the data verbs from std.repo's exports, `filter` among them.
    // Its resolve payload points at the builtin module; resolve fills the method
    // signature and the class doc through the module-scoped class-method fallback,
    // even though `filter` is a `Refinable` method, not a top-level declaration.
    let line1 = "pub fn run = Repo.filter";
    let (service, _socket, uri) =
        hover_fixture("import std.repo as Repo\npub fn run = Repo.filter\n").await;
    let server = service.inner();

    let col = u32::try_from(line1.find("Repo.").expect("alias use") + 5).expect("offset fits u32");
    let items = completion_items(
        server
            .completion(complete_at(&uri, 1, col))
            .await
            .expect("ok"),
    );
    let filter = items
        .into_iter()
        .find(|i| i.label == "filter")
        .expect("std.repo member access offers `filter`");
    assert!(
        filter.data.is_some(),
        "a stdlib member carries resolve data"
    );

    let resolved = server.completion_resolve(filter).await.expect("resolve ok");
    let detail = resolved
        .detail
        .expect("resolve fills the class-method signature");
    assert!(
        detail.contains("filter"),
        "detail should be the method signature, got {detail:?}"
    );
    let doc = match resolved.documentation {
        Some(Documentation::MarkupContent(m)) => m.value,
        other => panic!("expected a markdown doc, got {other:?}"),
    };
    assert!(
        doc.contains("for both a query and a join"),
        "resolve fills the class doc, got {doc}"
    );
}

// ── client-capability degradation ─────────────────────────────────────────────

/// A client that advertises only plain text for hover and completion-doc content,
/// so Markdown must be downgraded.
fn plaintext_only_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            hover: Some(HoverClientCapabilities {
                content_format: Some(vec![MarkupKind::PlainText]),
                ..Default::default()
            }),
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    documentation_format: Some(vec![MarkupKind::PlainText]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_signature_help_degrades_to_simple_labels() {
    // Without `labelOffsetSupport`, each parameter must be the substring it
    // covers, not a `[start, end)` offset pair the client couldn't map.
    let src = "import std.list as L\npub fn run f xs = L.map f xs\n";
    let (service, _socket, uri) = hover_fixture_with_caps(src, ClientCapabilities::default()).await;
    let server = service.inner();

    let help = server
        .signature_help(signature_at(&uri, 1, 24))
        .await
        .expect("signature_help ok")
        .expect("a signature for `L.map`");

    // The label itself is unchanged; only the parameter encoding degrades.
    assert_eq!(
        help.signatures[0].label,
        "map (f: fn a -> b) (xs: List a) -> List b"
    );
    let labels: Vec<String> = help.signatures[0]
        .parameters
        .as_ref()
        .expect("parameters present")
        .iter()
        .map(|p| match &p.label {
            ParameterLabel::Simple(s) => s.clone(),
            ParameterLabel::LabelOffsets(_) => {
                panic!("expected substring labels when offsets are unsupported")
            }
        })
        .collect();
    assert_eq!(labels, ["(f: fn a -> b)", "(xs: List a)"]);
}

#[tokio::test]
async fn test_document_symbol_flattens_without_hierarchical_support() {
    let (service, _socket, uri) =
        hover_fixture_with_caps(SYMBOL_SRC, ClientCapabilities::default()).await;
    let server = service.inner();

    let resp = server
        .document_symbol(doc_symbol_params(&uri))
        .await
        .expect("documentSymbol ok")
        .expect("an outline for a non-empty module");
    let DocumentSymbolResponse::Flat(symbols) = resp else {
        panic!("expected a flat outline without hierarchical support");
    };

    // Pre-order over the same tree the nested form returns: declarations with
    // their members inlined right after them.
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Color", "Red", "Green", "Blue", "User", "name", "age", "maxAge", "greet", "Counter",
            "count", "bump"
        ]
    );

    // Members carry their parent as the container; top-level decls have none.
    let by_name = |n: &str| symbols.iter().find(|s| s.name == n).expect("present");
    assert_eq!(by_name("Color").container_name, None);
    assert_eq!(by_name("Red").container_name.as_deref(), Some("Color"));
    assert_eq!(by_name("age").container_name.as_deref(), Some("User"));
    assert_eq!(by_name("bump").container_name.as_deref(), Some("Counter"));
}

#[tokio::test]
async fn test_code_action_uses_command_bridge_without_literal_support() {
    // Without `codeActionLiteralSupport` the quick-fix can't be an inline
    // `CodeAction`; it's delivered as a `Command` carrying the edit, which the
    // client runs through `workspace/executeCommand`.
    let src = "import std.io as Io\n\npub fn greet () -> Unit =\n    Io.println \"hi\"\n";
    let (service, _socket, uri) =
        cap_workspace_fixture_with_caps(src, ClientCapabilities::default()).await;
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
    let CodeActionOrCommand::Command(cmd) = &resp[0] else {
        panic!("expected a Command bridge, got {:?}", resp[0]);
    };
    assert_eq!(cmd.title, "Add capability `io` to `greet`");
    assert_eq!(cmd.command, "ridge.applyWorkspaceEdit");

    // The command's sole argument is the same edit the literal form would inline.
    let arg = cmd
        .arguments
        .as_ref()
        .and_then(|a| a.first())
        .expect("the command carries the edit");
    let edit: WorkspaceEdit =
        serde_json::from_value(arg.clone()).expect("the argument is a WorkspaceEdit");
    let edits = edit
        .changes
        .as_ref()
        .and_then(|c| c.get(&uri))
        .expect("an edit for this document");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "io ");
    assert_eq!(edits[0].range.start, Position::new(2, 7));
}

#[tokio::test]
async fn test_execute_command_guards_unknown_and_malformed() {
    let (service, _socket, _uri) = hover_fixture("pub fn run -> Int = 1\n").await;
    let server = service.inner();

    let run = |command: &str, arguments: Vec<serde_json::Value>| {
        let command = command.to_owned();
        async move {
            server
                .execute_command(ExecuteCommandParams {
                    command,
                    arguments,
                    work_done_progress_params: WorkDoneProgressParams::default(),
                })
                .await
                .expect("execute_command ok")
        }
    };

    // An unknown command is ignored; the apply bridge with no/garbage argument
    // applies nothing. None of these reaches `workspace/applyEdit`, so the test
    // needs no client pump (the bridge payload itself is covered above).
    assert!(run("something.else", vec![]).await.is_none());
    assert!(run("ridge.applyWorkspaceEdit", vec![]).await.is_none());
    assert!(run(
        "ridge.applyWorkspaceEdit",
        vec![serde_json::Value::String("nope".to_owned())]
    )
    .await
    .is_none());
}

#[tokio::test]
async fn test_hover_degrades_to_plaintext() {
    // `maxAge` is a documented-by-type constant; hover normally fences it as
    // Markdown. A plain-text-only client gets the same content without fences.
    let src = "const maxAge: Int = 120\npub fn run -> Int = maxAge\n";
    let (service, _socket, uri) = hover_fixture_with_caps(src, plaintext_only_capabilities()).await;
    let server = service.inner();

    let hover = server
        .hover(hover_at(&uri, 1, 20))
        .await
        .expect("hover ok")
        .expect("a hover on the `maxAge` reference");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup hover contents");
    };
    assert_eq!(markup.kind, MarkupKind::PlainText);
    assert!(
        !markup.value.contains("```"),
        "plain-text hover must not contain code fences, got {:?}",
        markup.value
    );
}

#[tokio::test]
async fn test_completion_doc_degrades_to_plaintext() {
    let src = "---\nGreets a person by name.\n---\npub fn greet (name: Text) -> Text = name\npub fn run -> Text = gr\n";
    let (service, _socket, uri) = hover_fixture_with_caps(src, plaintext_only_capabilities()).await;
    let server = service.inner();

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
    let resolved = server.completion_resolve(greet).await.expect("resolve ok");

    let doc = match resolved.documentation {
        Some(Documentation::MarkupContent(m)) => {
            assert_eq!(m.kind, MarkupKind::PlainText);
            m.value
        }
        other => panic!("expected plain-text documentation, got {other:?}"),
    };
    assert!(
        doc.contains("Greets a person by name."),
        "the doc text survives the downgrade, got {doc}"
    );
    assert!(!doc.contains("```"), "plain text must carry no code fences");
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

/// Open the `rename_folder_workspace` fixture — module `app.main` imports
/// `app.geo.shapes`, which lives in the `app/src/geo/` subfolder — and return
/// the live service once the workspace compile has installed an index.
async fn rename_folder_workspace_service() -> (LspService<RidgeLanguageServer>, ClientSocket) {
    let (service, socket) = build_test_service();
    {
        let server = service.inner();
        server
            .initialize(make_init_params("rename_folder_workspace"))
            .await
            .expect("initialize");

        let root = fixtures_dir().join("rename_folder_workspace");
        for rel in &["main.ridge", "geo/shapes.ridge"] {
            let path = root.join("app").join("src").join(rel);
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

/// The directory portion of `uri` — everything up to its final `/` segment.
fn parent_uri(uri: &Url) -> String {
    let s = uri.to_string();
    let cut = s.rfind('/').expect("a path separator in the URI");
    s[..cut].to_owned()
}

/// Replace the final segment of a folder URI string with `new_name`.
fn rename_last_segment(folder: &str, new_name: &str) -> String {
    let cut = folder
        .rfind('/')
        .expect("a path separator in the folder URI");
    format!("{}/{new_name}", &folder[..cut])
}

#[tokio::test]
async fn test_will_rename_folder_rewrites_child_imports() {
    let (service, _socket) = rename_folder_workspace_service().await;
    let server = service.inner();
    let shapes = module_uri_ending(server, "geo/shapes.ridge").await;
    let geo_dir = parent_uri(&shapes);
    let geometry_dir = rename_last_segment(&geo_dir, "geometry");

    // Renaming the folder `geo` to `geometry` moves `app.geo.shapes` to
    // `app.geometry.shapes`; the importer's path must follow.
    let edit = server
        .will_rename_files(RenameFilesParams {
            files: vec![FileRename {
                old_uri: geo_dir,
                new_uri: geometry_dir,
            }],
        })
        .await
        .expect("will_rename ok")
        .expect("an edit when a folder holds an imported module");

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
    // Only the dotted path changes; the `(area)` item list is preserved.
    assert_eq!(edits[0].new_text, "app.geometry.shapes");
    // The edit lands on `app.geo.shapes`, the path right after `import ` on line 0.
    assert_eq!(edits[0].range.start.line, 0);
    assert_eq!(
        edits[0].range.start.character, 7,
        "edit should start at `app.geo.shapes` (col 7), got {:?}",
        edits[0].range
    );
}

#[tokio::test]
async fn test_will_rename_folder_without_modules_no_edit() {
    let (service, _socket) = rename_folder_workspace_service().await;
    let server = service.inner();
    let shapes = module_uri_ending(server, "geo/shapes.ridge").await;
    let geo_dir = parent_uri(&shapes);
    // A sibling folder that holds no `.ridge` module under it.
    let empty_dir = rename_last_segment(&geo_dir, "assets");
    let renamed = rename_last_segment(&geo_dir, "static");

    let edit = server
        .will_rename_files(RenameFilesParams {
            files: vec![FileRename {
                old_uri: empty_dir,
                new_uri: renamed,
            }],
        })
        .await
        .expect("will_rename ok");
    assert!(
        edit.is_none(),
        "a folder with no modules beneath it yields no edit, got {edit:?}"
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
    // Two filters: `.ridge` files, and any folder (folder renames carry the
    // modules beneath them, so imports must be fixed up there too).
    assert_eq!(will_rename.filters.len(), 2);
    assert_eq!(will_rename.filters[0].pattern.glob, "**/*.ridge");
    assert_eq!(
        will_rename.filters[0].pattern.matches,
        Some(FileOperationPatternKind::File)
    );
    assert_eq!(will_rename.filters[1].pattern.glob, "**");
    assert_eq!(
        will_rename.filters[1].pattern.matches,
        Some(FileOperationPatternKind::Folder)
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

// ── $/progress (work-done) during indexing ───────────────────────────────────

use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use tower::{Service, ServiceExt};
use tower_lsp::jsonrpc::{Request, Response};

/// The server-to-client work-done progress lifecycle observed by the test client.
#[derive(Default)]
struct ProgressLog {
    /// Tokens from `window/workDoneProgress/create` requests.
    created: Vec<ProgressToken>,
    /// Tokens carried by `$/progress` `begin` notifications.
    begun: Vec<ProgressToken>,
    /// Tokens carried by `$/progress` `end` notifications.
    ended: Vec<ProgressToken>,
    /// Count of `workspace/diagnostic/refresh` requests — the pull model's nudge
    /// to re-pull after a compile.
    refreshed: usize,
    /// Count of `workspace/codeLens/refresh` requests — the nudge to re-query
    /// lenses after a `didChangeConfiguration` flips a code-lens flag.
    code_lens_refreshed: usize,
    /// Partial-result `$/progress` notifications, in arrival order. Each carries
    /// the request's `partialResultToken` and the raw result array the client
    /// appends to what it already has.
    partial_chunks: Vec<(ProgressToken, serde_json::Value)>,
}

/// Drive the client half of the in-process loopback.
///
/// Spawns a task that answers every server-to-client request (so the server
/// never blocks waiting on a response) and records the work-done progress
/// lifecycle for later assertions. Returns the shared log.
fn drive_client(socket: ClientSocket) -> Arc<Mutex<ProgressLog>> {
    let log = Arc::new(Mutex::new(ProgressLog::default()));
    let sink = Arc::clone(&log);
    let (mut requests, mut responses) = socket.split();
    tokio::spawn(async move {
        while let Some(req) = requests.next().await {
            if req.method() == "window/workDoneProgress/create" {
                if let Some(params) = req.params() {
                    if let Ok(p) =
                        serde_json::from_value::<WorkDoneProgressCreateParams>(params.clone())
                    {
                        sink.lock().unwrap().created.push(p.token);
                    }
                }
            } else if req.method() == "$/progress" {
                if let Some(params) = req.params() {
                    if let Ok(p) = serde_json::from_value::<ProgressParams>(params.clone()) {
                        let ProgressParamsValue::WorkDone(work) = p.value;
                        match work {
                            WorkDoneProgress::Begin(_) => sink.lock().unwrap().begun.push(p.token),
                            WorkDoneProgress::End(_) => sink.lock().unwrap().ended.push(p.token),
                            WorkDoneProgress::Report(_) => {}
                        }
                    } else if let (Some(token), Some(value)) =
                        (params.get("token"), params.get("value"))
                    {
                        // A partial-result `$/progress` carries a raw array, which
                        // the typed `ProgressParams` (work-done only) rejects.
                        if let Ok(tok) = serde_json::from_value::<ProgressToken>(token.clone()) {
                            sink.lock()
                                .unwrap()
                                .partial_chunks
                                .push((tok, value.clone()));
                        }
                    }
                }
            } else if req.method() == "workspace/diagnostic/refresh" {
                sink.lock().unwrap().refreshed += 1;
            } else if req.method() == "workspace/codeLens/refresh" {
                sink.lock().unwrap().code_lens_refreshed += 1;
            }
            // Answer anything that expects a reply so the server never blocks;
            // notifications carry no id and need none.
            if let Some(id) = req.id().cloned() {
                let _ = responses
                    .send(Response::from_parts(id, Ok(serde_json::Value::Null)))
                    .await;
            }
        }
    });
    log
}

/// Build a hermetic temp workspace, drive the client loopback, initialize with
/// the given `window.workDoneProgress` support, and open the first file (which
/// triggers the initial reseed compile). Returns the service, the progress log,
/// and the workspace root (the `TempDir` is leaked so the path stays valid).
async fn progress_workspace(
    files: &[(&str, &str)],
    advertise_progress: bool,
) -> (
    LspService<RidgeLanguageServer>,
    Arc<Mutex<ProgressLog>>,
    PathBuf,
) {
    init_test_workspace(files, advertise_progress, None, false).await
}

/// Like [`progress_workspace`], but lets a test pass `initializationOptions` (the
/// `codeLens` opt-in) and advertise `workspace.codeLens.refreshSupport`, so the
/// `workspace/didChangeConfiguration` path can be exercised end to end.
async fn init_test_workspace(
    files: &[(&str, &str)],
    advertise_progress: bool,
    init_options: Option<serde_json::Value>,
    code_lens_refresh: bool,
) -> (
    LspService<RidgeLanguageServer>,
    Arc<Mutex<ProgressLog>>,
    PathBuf,
) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    // Canonicalise so a query URI built from the returned root matches the module
    // URIs the index derives: discovery canonicalises the workspace root, which
    // expands Windows 8.3 short names (`RUNNER~1` → `runneradmin`) and resolves
    // the macOS `/var` → `/private/var` symlink. Without this a find-references
    // query off this root misses the index off-Linux.
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize temp root");
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"progress-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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

    let (mut service, socket) = build_test_service();
    let log = drive_client(socket);
    {
        let root_uri = Url::from_file_path(&root).expect("root URI");
        let window = advertise_progress.then(|| WindowClientCapabilities {
            work_done_progress: Some(true),
            ..WindowClientCapabilities::default()
        });
        let workspace = code_lens_refresh.then(|| WorkspaceClientCapabilities {
            code_lens: Some(CodeLensWorkspaceClientCapabilities {
                refresh_support: Some(true),
            }),
            ..WorkspaceClientCapabilities::default()
        });
        let init_params = InitializeParams {
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: "progress-ws".to_owned(),
            }]),
            capabilities: ClientCapabilities {
                window,
                workspace,
                ..ClientCapabilities::default()
            },
            initialization_options: init_options,
            ..InitializeParams::default()
        };
        // Drive `initialize` through the service rather than the inner server so
        // the framework flips its state to `Initialized`. Otherwise the client
        // suppresses every server-to-client request and notification — including
        // work-done progress — and the test would observe nothing.
        let init_req = Request::build("initialize")
            .id(1_i64)
            .params(serde_json::to_value(init_params).expect("serialize init params"))
            .finish();
        {
            let ready = ServiceExt::ready(&mut service)
                .await
                .expect("service ready");
            ready.call(init_req).await.expect("initialize call");
        }

        let server = service.inner();
        server.initialized(InitializedParams {}).await;

        // Opening the first file drives the initial reseed compile.
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
    (service, log, root)
}

/// Poll the progress log until an `end` notification arrives, or panic after ~6s.
async fn wait_for_progress_end(log: &Arc<Mutex<ProgressLog>>) {
    for _ in 0..120 {
        if !log.lock().unwrap().ended.is_empty() {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    panic!("expected a work-done progress end notification");
}

#[tokio::test]
async fn test_work_done_progress_reported_on_reseed() {
    let (_service, log, _root) =
        progress_workspace(&[("main.ridge", "pub fn a -> Int = 1\n")], true).await;
    wait_for_progress_end(&log).await;

    let (created, begun, ended) = {
        let log = log.lock().unwrap();
        (log.created.clone(), log.begun.clone(), log.ended.clone())
    };
    assert_eq!(
        created.len(),
        1,
        "one progress token created for the reseed, got {created:?}"
    );
    assert_eq!(begun.len(), 1, "one begin, got {begun:?}");
    assert_eq!(ended.len(), 1, "one end, got {ended:?}");
    // begin and end ride the token the server asked the client to create.
    assert_eq!(created[0], begun[0], "begin uses the created token");
    assert_eq!(begun[0], ended[0], "end matches begin");
}

#[tokio::test]
async fn test_no_progress_when_capability_absent() {
    // The helper already waits for the reseed to finish, so any progress would
    // have been emitted by now.
    let (_service, log, _root) =
        progress_workspace(&[("main.ridge", "pub fn a -> Int = 1\n")], false).await;
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let (created, begun, ended) = {
        let log = log.lock().unwrap();
        (log.created.clone(), log.begun.clone(), log.ended.clone())
    };
    assert!(
        created.is_empty(),
        "no token is created without window.workDoneProgress, got {created:?}"
    );
    assert!(begun.is_empty(), "no begin without the capability");
    assert!(ended.is_empty(), "no end without the capability");
}

#[tokio::test]
async fn test_no_progress_for_incremental_compile() {
    let (service, log, root) =
        progress_workspace(&[("main.ridge", "pub fn a -> Int = 1\n")], true).await;
    let server = service.inner();

    // The opening reseed reports progress; wait for it, then snapshot the count.
    wait_for_progress_end(&log).await;
    let created_after_open = log.lock().unwrap().created.len();

    // A didChange schedules a debounced *incremental* recompile (reseed = false),
    // which must stay silent — no spinner flicker on every keystroke.
    let main = root.join("app").join("src").join("main.ridge");
    server
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: Url::from_file_path(&main).expect("uri"),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None, // full replacement
                range_length: None,
                text: "pub fn a -> Int = 2\n".to_owned(),
            }],
        })
        .await;

    // Wait past the 250 ms debounce plus the incremental compile.
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    assert_eq!(
        log.lock().unwrap().created.len(),
        created_after_open,
        "an incremental recompile must not create new progress"
    );
}

// ── Partial-result streaming ($/progress) ─────────────────────────────────────

/// A `references` request that opts into partial results under `token`.
fn references_at_token(
    uri: &Url,
    line: u32,
    character: u32,
    include_declaration: bool,
    token: ProgressToken,
) -> ReferenceParams {
    let mut params = references_at(uri, line, character, include_declaration);
    params.partial_result_params.partial_result_token = Some(token);
    params
}

/// A `workspace/symbol` request, optionally opting into partial results.
fn workspace_symbol_params(query: &str, token: Option<ProgressToken>) -> WorkspaceSymbolParams {
    WorkspaceSymbolParams {
        query: query.to_owned(),
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: PartialResultParams {
            partial_result_token: token,
        },
    }
}

/// Poll the progress log until the streamed chunks carry at least `expected`
/// items in total, or panic after ~6s. Waiting on the item count rather than a
/// chunk count avoids racing the tail chunk. Returns a snapshot in arrival order.
async fn wait_for_partial_chunks(
    log: &Arc<Mutex<ProgressLog>>,
    expected: usize,
) -> Vec<(ProgressToken, serde_json::Value)> {
    for _ in 0..120 {
        {
            let guard = log.lock().unwrap();
            let total: usize = guard
                .partial_chunks
                .iter()
                .filter_map(|(_, value)| value.as_array().map(Vec::len))
                .sum();
            if total >= expected {
                return guard.partial_chunks.clone();
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    panic!("expected streamed chunks totalling {expected} items");
}

/// Concatenate the locations carried by streamed `$/progress` chunks, in order.
fn collect_streamed_locations(chunks: &[(ProgressToken, serde_json::Value)]) -> Vec<Location> {
    let mut out = Vec::new();
    for (_, value) in chunks {
        let locs: Vec<Location> =
            serde_json::from_value(value.clone()).expect("partial chunk is a Location array");
        out.extend(locs);
    }
    out
}

/// The `(name, start line)` of each symbol, for order-preserving comparison that
/// doesn't lean on `SymbolInformation`'s equality.
fn symbol_keys(syms: &[SymbolInformation]) -> Vec<(String, u32)> {
    syms.iter()
        .map(|s| (s.name.clone(), s.location.range.start.line))
        .collect()
}

/// A single module that defines `helper` and uses it `uses` times, enough to push
/// a references/symbols result past one `$/progress` chunk.
fn many_uses_source(uses: usize) -> String {
    let mut src = String::from("pub fn helper -> Int = 1\n");
    src.extend((0..uses).map(|i| format!("pub fn u{i} -> Int = helper\n")));
    src
}

#[tokio::test]
async fn test_references_streams_partial_results() {
    // 150 use-sites plus the declaration is comfortably more than one chunk.
    let src = many_uses_source(150);
    let (service, log, root) = progress_workspace(&[("main.ridge", &src)], false).await;
    let server = service.inner();
    let uri = Url::from_file_path(root.join("app").join("src").join("main.ridge")).expect("uri");

    // Cursor on the first `helper` use-site (line 1: `pub fn u0 -> Int = helper`).
    let use_line = "pub fn u0 -> Int = helper";
    let col = u32::try_from(use_line.find("helper").expect("use of helper") + 1)
        .expect("offset fits u32");

    // Baseline: without a token the whole list comes back in the response.
    let full = server
        .references(references_at(&uri, 1, col, true))
        .await
        .expect("ok")
        .expect("references");
    assert!(
        full.len() > 64,
        "fixture should exceed one chunk, got {} references",
        full.len()
    );

    // With a token the response is empty and every location arrives via $/progress.
    let token = ProgressToken::String("ridge/test/refs".to_owned());
    let streamed = server
        .references(references_at_token(&uri, 1, col, true, token.clone()))
        .await
        .expect("ok")
        .expect("final response present");
    assert!(
        streamed.is_empty(),
        "the final response is empty once results stream, got {streamed:?}"
    );

    let chunks = wait_for_partial_chunks(&log, full.len()).await;
    assert!(
        chunks.len() >= 2,
        "a result past one chunk streams in several notifications, got {}",
        chunks.len()
    );
    assert!(
        chunks.iter().all(|(t, _)| *t == token),
        "every partial result carries the request token"
    );
    assert_eq!(
        collect_streamed_locations(&chunks),
        full,
        "streamed chunks concatenate to the same locations as the direct response"
    );
}

#[tokio::test]
async fn test_workspace_symbol_streams_partial_results() {
    let src = many_uses_source(150);
    let (service, log, _root) = progress_workspace(&[("main.ridge", &src)], false).await;
    let server = service.inner();

    // Baseline: an empty query returns every symbol in the response.
    let full = server
        .symbol(workspace_symbol_params("", None))
        .await
        .expect("ok")
        .expect("symbols");
    assert!(
        full.len() > 64,
        "fixture should exceed one chunk, got {} symbols",
        full.len()
    );

    let token = ProgressToken::String("ridge/test/symbols".to_owned());
    let streamed = server
        .symbol(workspace_symbol_params("", Some(token.clone())))
        .await
        .expect("ok")
        .expect("final response present");
    assert!(
        streamed.is_empty(),
        "the final response is empty once results stream, got {streamed:?}"
    );

    let chunks = wait_for_partial_chunks(&log, full.len()).await;
    assert!(
        chunks.len() >= 2,
        "a result past one chunk streams in several notifications, got {}",
        chunks.len()
    );
    assert!(
        chunks.iter().all(|(t, _)| *t == token),
        "every partial result carries the request token"
    );
    let streamed_syms: Vec<SymbolInformation> = chunks
        .iter()
        .flat_map(|(_, value)| {
            serde_json::from_value::<Vec<SymbolInformation>>(value.clone())
                .expect("partial chunk is a SymbolInformation array")
        })
        .collect();
    assert_eq!(
        symbol_keys(&streamed_syms),
        symbol_keys(&full),
        "streamed chunks concatenate to the same symbols as the direct response"
    );
}

// ── Multi-root workspaces ─────────────────────────────────────────────────────

/// Write a minimal single-module Ridge workspace into a fresh temp dir and
/// return its root path plus the module's file URI. The dir is leaked so the
/// files outlive the helper, matching the other on-disk workspace fixtures.
fn write_mini_workspace(ws_name: &str, module: &str, src: &str) -> (PathBuf, Url) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    // Canonicalise so the test's query URIs match the module URIs the index
    // builds: discovery canonicalises the workspace root, which expands Windows
    // 8.3 short names (`RUNNER~1` → `runneradmin`) and resolves the macOS
    // `/var` → `/private/var` symlink. `uri_key` normalises drive case and colon
    // encoding but not those, so without this the query would miss off-Linux.
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize temp root");
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        format!("[workspace]\nname = \"{ws_name}\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n"),
    )
    .expect("workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("project manifest");
    let file = app_src.join(module);
    std::fs::write(&file, src).expect("write source");
    let uri = Url::from_file_path(&file).expect("file URI");
    std::mem::forget(dir);
    (root, uri)
}

const WIDGET_A_SRC: &str = "pub fn widget_a -> Int = 1\n";
const WIDGET_B_SRC: &str = "pub fn widget_b -> Int = 2\n";

/// Initialise a server over two independent Ridge workspaces, each its own
/// `[workspace]` manifest root, both passed as `workspaceFolders` the way a
/// multi-folder editor window does. Opens one module from each and waits until
/// both are indexed. Returns the service plus each module's file URI.
async fn multi_root_workspace() -> (LspService<RidgeLanguageServer>, ClientSocket, Url, Url) {
    let (root_a, uri_a) = write_mini_workspace("ws-a", "widget_a.ridge", WIDGET_A_SRC);
    let (root_b, uri_b) = write_mini_workspace("ws-b", "widget_b.ridge", WIDGET_B_SRC);

    let (service, socket) = build_test_service();
    {
        let server = service.inner();
        let uri_root_a = Url::from_file_path(&root_a).expect("root A URI");
        let uri_root_b = Url::from_file_path(&root_b).expect("root B URI");
        server
            .initialize(InitializeParams {
                // A real client sets rootUri to the first folder and lists every
                // opened folder in workspaceFolders; the duplicate dedups away.
                root_uri: Some(uri_root_a.clone()),
                workspace_folders: Some(vec![
                    WorkspaceFolder {
                        uri: uri_root_a,
                        name: "ws-a".to_owned(),
                    },
                    WorkspaceFolder {
                        uri: uri_root_b,
                        name: "ws-b".to_owned(),
                    },
                ]),
                capabilities: ClientCapabilities::default(),
                ..InitializeParams::default()
            })
            .await
            .expect("initialize");

        for (uri, src) in [(&uri_a, WIDGET_A_SRC), (&uri_b, WIDGET_B_SRC)] {
            server
                .did_open(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: uri.clone(),
                        language_id: "ridge".to_owned(),
                        version: 1,
                        text: src.to_owned(),
                    },
                })
                .await;
        }
        wait_for_uri_indexed(server, &uri_a).await;
        wait_for_uri_indexed(server, &uri_b).await;
    }
    (service, socket, uri_a, uri_b)
}

/// Poll until `uri` belongs to some workspace's index, or panic after ~6s.
async fn wait_for_uri_indexed(server: &RidgeLanguageServer, uri: &Url) {
    for _ in 0..120 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        if server.index_for_uri(uri).await.is_some() {
            return;
        }
    }
    panic!("uri never indexed: {uri}");
}

/// Poll until `uri`'s index has a generation past `baseline`, returning it, or
/// panic after ~6s.
async fn wait_for_index_generation_above(
    server: &RidgeLanguageServer,
    uri: &Url,
    baseline: u64,
) -> u64 {
    for _ in 0..120 {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        if let Some(idx) = server.index_for_uri(uri).await {
            if idx.generation > baseline {
                return idx.generation;
            }
        }
    }
    panic!("index for {uri} never recompiled past generation {baseline}");
}

#[tokio::test]
async fn test_multi_root_routes_each_file_to_its_own_workspace() {
    let (service, _socket, uri_a, uri_b) = multi_root_workspace().await;
    let server = service.inner();

    let idx_a = server
        .index_for_uri(&uri_a)
        .await
        .expect("workspace A indexed");
    let idx_b = server
        .index_for_uri(&uri_b)
        .await
        .expect("workspace B indexed");

    // Each file routes to the workspace that owns it, and to no other — names
    // never leak between the two unrelated projects.
    assert!(idx_a.contains_uri(&uri_a));
    assert!(
        !idx_a.contains_uri(&uri_b),
        "workspace A must not own B's file"
    );
    assert!(idx_b.contains_uri(&uri_b));
    assert!(
        !idx_b.contains_uri(&uri_a),
        "workspace B must not own A's file"
    );

    // Two independent indices, not one merged graph.
    assert!(
        !Arc::ptr_eq(&idx_a, &idx_b),
        "the two workspaces share a single index"
    );
    assert_eq!(idx_a.uri_to_module.len(), 1);
    assert_eq!(idx_b.uri_to_module.len(), 1);
}

#[tokio::test]
async fn test_multi_root_workspace_symbol_spans_all_projects() {
    let (service, _socket, _uri_a, _uri_b) = multi_root_workspace().await;
    let server = service.inner();

    // `Ctrl-T` reaches into every open project at once.
    let symbols = server
        .symbol(WorkspaceSymbolParams {
            query: "widget".to_owned(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("symbol ok")
        .unwrap_or_default();

    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"widget_a"),
        "missing workspace A's symbol, got {names:?}"
    );
    assert!(
        names.contains(&"widget_b"),
        "missing workspace B's symbol, got {names:?}"
    );
}

#[tokio::test]
async fn test_multi_root_edit_recompiles_only_owning_workspace() {
    let (service, _socket, uri_a, uri_b) = multi_root_workspace().await;
    let server = service.inner();

    let gen_a0 = server
        .index_for_uri(&uri_a)
        .await
        .expect("a indexed")
        .generation;
    let gen_b0 = server
        .index_for_uri(&uri_b)
        .await
        .expect("b indexed")
        .generation;

    // Typing in workspace A schedules a debounced incremental recompile, which
    // must touch only A — workspace B is left untouched.
    server
        .did_change(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri_a.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "pub fn widget_a -> Int = 42\n".to_owned(),
            }],
        })
        .await;

    let gen_a1 = wait_for_index_generation_above(server, &uri_a, gen_a0).await;
    let gen_b1 = server
        .index_for_uri(&uri_b)
        .await
        .expect("b still indexed")
        .generation;

    assert!(gen_a1 > gen_a0, "workspace A should have recompiled");
    assert_eq!(gen_b1, gen_b0, "editing A must not recompile B");
}

// ── pull diagnostics (LSP 3.17) ───────────────────────────────────────────────

/// Capabilities that opt into pull diagnostics. `pull` advertises
/// `textDocument.diagnostic`; `refresh` advertises
/// `workspace.diagnostics.refreshSupport`. The server enters pull mode only when
/// both are present.
fn pull_caps(pull: bool, refresh: bool) -> ClientCapabilities {
    ClientCapabilities {
        text_document: Some(TextDocumentClientCapabilities {
            diagnostic: pull.then(DiagnosticClientCapabilities::default),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            diagnostic: Some(DiagnosticWorkspaceClientCapabilities {
                refresh_support: Some(refresh),
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_diagnostic_provider_gated_on_pull_and_refresh() {
    async fn provider_for(caps: ClientCapabilities) -> Option<DiagnosticServerCapabilities> {
        let (service, _socket) = build_test_service();
        let mut params = make_init_params("ok_workspace");
        params.capabilities = caps;
        service
            .inner()
            .initialize(params)
            .await
            .expect("initialize ok")
            .capabilities
            .diagnostic_provider
    }

    // Both halves present → pull mode, provider advertised with Ridge's options.
    match provider_for(pull_caps(true, true)).await {
        Some(DiagnosticServerCapabilities::Options(opts)) => {
            assert!(
                opts.inter_file_dependencies,
                "Ridge has cross-module diagnostics"
            );
            assert!(opts.workspace_diagnostics, "workspace pull is served");
            assert_eq!(opts.identifier.as_deref(), Some("ridge"));
        }
        other => panic!("expected DiagnosticOptions, got {other:?}"),
    }

    // Missing either half → push model, no provider.
    assert!(
        provider_for(pull_caps(true, false)).await.is_none(),
        "pull without refresh stays on the push model"
    );
    assert!(
        provider_for(pull_caps(false, true)).await.is_none(),
        "refresh without pull stays on the push model"
    );
}

/// Build a hermetic temp workspace, drive the client loopback, initialize as a
/// pull-diagnostics client (pull + refresh), and open the first file (triggering
/// the reseed compile). Returns the service and the client log; queries read the
/// real document URIs back from the index so they match the compile's own keys.
async fn pull_workspace(
    files: &[(&str, &str)],
) -> (LspService<RidgeLanguageServer>, Arc<Mutex<ProgressLog>>) {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = dir.path().to_path_buf();
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create temp workspace");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"pull-ws\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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

    let (mut service, socket) = build_test_service();
    let log = drive_client(socket);
    {
        let root_uri = Url::from_file_path(&root).expect("root URI");
        let init_params = InitializeParams {
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: "pull-ws".to_owned(),
            }]),
            capabilities: pull_caps(true, true),
            ..InitializeParams::default()
        };
        let init_req = Request::build("initialize")
            .id(1_i64)
            .params(serde_json::to_value(init_params).expect("serialize init params"))
            .finish();
        {
            let ready = ServiceExt::ready(&mut service)
                .await
                .expect("service ready");
            ready.call(init_req).await.expect("initialize call");
        }
        let server = service.inner();
        server.initialized(InitializedParams {}).await;
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
    (service, log)
}

/// Poll the client log until a `workspace/diagnostic/refresh` arrives (the pull
/// model's post-compile nudge), or panic after ~6s. Returning guarantees the
/// compile delivered, so the diagnostics cache is populated.
async fn wait_for_refresh(log: &Arc<Mutex<ProgressLog>>) {
    for _ in 0..120 {
        if log.lock().unwrap().refreshed > 0 {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    panic!("expected a workspace/diagnostic/refresh request");
}

#[tokio::test]
async fn test_pull_diagnostics_served_from_cache() {
    // An unresolved name is guaranteed to produce a diagnostic.
    let (service, log) = pull_workspace(&[("main.ridge", "pub fn a -> Int = nope\n")]).await;
    let server = service.inner();
    // The refresh fires only on the pull path, so observing it proves the server
    // delivered via pull (not push) and the cache is ready to query.
    wait_for_refresh(&log).await;

    // Read the document URI back from the index so it matches the compile's keys.
    let uri = {
        let idx = server.workspace_index().await.expect("indexed");
        idx.uri_to_module
            .keys()
            .next()
            .expect("a module uri")
            .clone()
    };

    // textDocument/diagnostic returns a full report carrying the file's errors.
    let report = server
        .diagnostic(DocumentDiagnosticParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            identifier: None,
            previous_result_id: None,
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("diagnostic ok");
    let items = match report {
        DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(full)) => {
            full.full_document_diagnostic_report.items
        }
        other => panic!("expected a full report, got {other:?}"),
    };
    assert!(
        !items.is_empty(),
        "the unresolved-name error must be reported via pull"
    );

    // workspace/diagnostic reports the same file among its items.
    let ws_report = server
        .workspace_diagnostic(WorkspaceDiagnosticParams {
            identifier: None,
            previous_result_ids: Vec::new(),
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .expect("workspace_diagnostic ok");
    let ws_items = match ws_report {
        WorkspaceDiagnosticReportResult::Report(r) => r.items,
        other => panic!("expected a workspace report, got {other:?}"),
    };
    let found = ws_items.iter().any(|item| match item {
        WorkspaceDocumentDiagnosticReport::Full(f) => {
            f.uri == uri && !f.full_document_diagnostic_report.items.is_empty()
        }
        WorkspaceDocumentDiagnosticReport::Unchanged(_) => false,
    });
    assert!(found, "workspace pull must include the erroring file");
}

// ── Module alias references / highlight / rename ──────────────────────────────

/// An app module that aliases a workspace module (`import lib.Lib as M`) and
/// uses it qualified twice. The unrelated `Thing` type exists only for the
/// rename-collision check.
const ALIAS_APP: &str =
    "import lib.Lib as M\npub type Thing = Int\npub fn one -> Int = M.helper\npub fn two -> Int = M.helper\n";

#[tokio::test]
async fn test_references_module_alias() {
    let (service, _socket, app_uri, _lib_uri) = two_member_fixture_with(ALIAS_APP).await;
    let server = service.inner();

    // From a use head (`M` on line 2), includeDeclaration finds the `as M`
    // declaration (line 0) and both `M.helper` heads (lines 2 and 3).
    let with_decl = server
        .references(references_at(&app_uri, 2, 20, true))
        .await
        .expect("ok")
        .expect("references of module alias `M`");
    assert_eq!(
        ref_lines(&with_decl, &app_uri),
        vec![0, 2, 3],
        "the declaration plus both qualified uses"
    );

    // Without the declaration, only the use heads remain.
    let no_decl = server
        .references(references_at(&app_uri, 2, 20, false))
        .await
        .expect("ok")
        .expect("references of module alias `M`");
    assert_eq!(
        ref_lines(&no_decl, &app_uri),
        vec![2, 3],
        "uses only when the declaration is excluded"
    );

    // The same set resolves from the `as M` declaration token (line 0, col 18).
    let from_decl = server
        .references(references_at(&app_uri, 0, 18, true))
        .await
        .expect("ok")
        .expect("references from the alias declaration");
    assert_eq!(ref_lines(&from_decl, &app_uri), vec![0, 2, 3]);
}

#[tokio::test]
async fn test_highlight_module_alias() {
    let (service, _socket, app_uri, _lib_uri) = two_member_fixture_with(ALIAS_APP).await;
    let server = service.inner();

    // From a use head: the declaration is the write, each `M.helper` head a read.
    let hs = server
        .document_highlight(highlight_at(&app_uri, 2, 20))
        .await
        .expect("ok")
        .expect("highlights of module alias `M`");
    assert_eq!(
        highlight_spots(&hs),
        vec![
            (0, 18, DocumentHighlightKind::WRITE),
            (2, 20, DocumentHighlightKind::READ),
            (3, 20, DocumentHighlightKind::READ),
        ],
        "the `as M` declaration is the write, both heads are reads"
    );
}

#[tokio::test]
async fn test_rename_module_alias() {
    let (service, _socket, app_uri, _lib_uri) = two_member_fixture_with(ALIAS_APP).await;
    let server = service.inner();

    // prepareRename on a use head underlines just the `M` token and offers it.
    let prep = server
        .prepare_rename(prepare_rename_at(&app_uri, 2, 20))
        .await
        .expect("ok")
        .expect("a module alias is renameable");
    match prep {
        PrepareRenameResponse::RangeWithPlaceholder { range, placeholder } => {
            assert_eq!(
                (range.start.line, range.start.character),
                (2, 20),
                "underlines the alias head under the cursor"
            );
            assert_eq!(placeholder, "M", "placeholder is the alias name");
        }
        other => panic!("expected RangeWithPlaceholder, got {other:?}"),
    }

    // Renaming `M` rewrites the declaration (line 0) and every qualified head
    // (lines 2 and 3) together.
    let edit = server
        .rename(rename_at(&app_uri, 2, 20, "Mod"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(edit.as_ref(), &app_uri),
        vec![(0, 18), (2, 20), (3, 20)],
        "the `as M` declaration and both `M.helper` heads move together"
    );
    if let Some((_, text)) = rename_edits(edit.as_ref(), &app_uri).first() {
        assert_eq!(text, "Mod", "edits carry the new name");
    }

    // The same rename works from the declaration token (line 0, col 18).
    let from_decl = server
        .rename(rename_at(&app_uri, 0, 18, "Mod"))
        .await
        .expect("ok");
    assert_eq!(
        rename_sites(from_decl.as_ref(), &app_uri),
        vec![(0, 18), (2, 20), (3, 20)],
        "a rename from the declaration covers the same sites"
    );
}

#[tokio::test]
async fn test_rename_module_alias_rejects_invalid_and_collision() {
    let (service, _socket, app_uri, _lib_uri) = two_member_fixture_with(ALIAS_APP).await;
    let server = service.inner();

    // A reserved keyword and a lowercase name are both invalid: an alias is an
    // UPPER_IDENT.
    let kw = server.rename(rename_at(&app_uri, 2, 20, "if")).await;
    assert!(kw.is_err(), "an alias cannot be renamed to a keyword");
    let lower = server.rename(rename_at(&app_uri, 2, 20, "mod")).await;
    assert!(lower.is_err(), "an alias must stay an uppercase identifier");

    // `Thing` is already a type in this module, so renaming `M` onto it would
    // shadow an existing name — reject it.
    let collision = server.rename(rename_at(&app_uri, 2, 20, "Thing")).await;
    assert!(
        collision.is_err(),
        "renaming an alias onto an existing name is rejected"
    );
}

#[tokio::test]
async fn test_bare_import_alias_reads_but_does_not_rename() {
    // A bare `import lib.Lib` binds the implicit alias `Lib` (the last path
    // segment). Its references and highlights work, but it is not renameable —
    // renaming would mean rewriting the path or adding an `as` clause.
    let (service, _socket, app_uri, _lib_uri) =
        two_member_fixture_with("import lib.Lib\npub fn run -> Int = Lib.helper\n").await;
    let server = service.inner();

    // References resolve through the implicit alias: the `import lib.Lib` segment
    // (line 0) and the `Lib.helper` head (line 1).
    let refs = server
        .references(references_at(&app_uri, 1, 20, true))
        .await
        .expect("ok")
        .expect("references of the implicit alias `Lib`");
    assert_eq!(
        ref_lines(&refs, &app_uri),
        vec![0, 1],
        "the bare import's path segment and the qualified head"
    );

    // prepareRename declines outright rather than falling through to rename the
    // qualified member.
    let prep = server
        .prepare_rename(prepare_rename_at(&app_uri, 1, 20))
        .await
        .expect("ok");
    assert!(
        prep.is_none(),
        "a bare import's implicit alias is not renameable"
    );
}
