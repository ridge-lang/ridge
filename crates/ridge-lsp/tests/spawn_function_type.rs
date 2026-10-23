//! Spawn callback types retain permissions and editor identity through edits.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::{
    cancel::Cancel,
    index::{CodeLensConfig, WorkspaceIndex},
};
use tower_lsp::lsp_types::Url;

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"callbacks\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/ridge.toml"), "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"spawn\",\"io\"]\n").unwrap();
    std::fs::write(dir.path().join("lib/src/Main.ridge"), source).unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let uri = Url::from_file_path(root.join("lib/src/Main.ridge")).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    (dir, state, uri)
}

fn clean_frontend(state: &IncrementalState) {
    assert!(state.resolved.lex_errors.is_empty());
    assert!(
        state.resolved.parse_errors.is_empty(),
        "{:?}",
        state.resolved.parse_errors
    );
    assert!(
        state.resolved.errors.is_empty(),
        "{:?}",
        state.resolved.errors
    );
}

fn clean(state: &IncrementalState) {
    clean_frontend(state);
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

#[test]
fn spawn_callback_calls_require_the_declared_capability_after_edits() {
    let source = "pub fn spawn invoke (callback: fn spawn Unit -> Unit) -> Unit = callback ()\n";
    let (_dir, mut state, _uri) = workspace(source);
    clean(&state);
    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    let forbidden = source.replacen("fn spawn invoke", "fn invoke", 1);
    let (_control_dir, control, _) = workspace(&forbidden);
    clean_frontend(&control);
    assert!(
        !control.type_errors.is_empty(),
        "a fresh pure caller cannot invoke a spawn callback"
    );
    for _ in 0..2 {
        state.recompile(module, &forbidden);
        clean_frontend(&state);
        assert!(
            !state.type_errors.is_empty(),
            "a pure caller cannot invoke a spawn callback"
        );
        assert!(
            state.type_errors.iter().any(|(_, error)| matches!(
                error,
                ridge_typecheck::TypeError::CapabilityNotDeclared { .. }
                    | ridge_typecheck::TypeError::CallerCapabilityInsufficient { .. }
            )),
            "{:?}",
            state.type_errors
        );
        state.recompile(module, source);
        clean(&state);
    }
}

#[test]
fn spawn_callback_types_reach_editor_signatures_and_navigation() {
    let source = "pub fn spawn invoke (callback: fn spawn Unit -> Unit) -> Unit =\n    callback ()\npub fn spawn run (callback: fn spawn Unit -> Unit) -> Unit =\n    invoke callback\n";
    let (_dir, state, uri) = workspace(source);
    clean(&state);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let (hover, _) = index.hover_at(&uri, 3, 5).unwrap();
    assert!(hover.contains("fn spawn Unit -> Unit"), "{hover}");
    let item = index
        .completions_at(&uri, 3, 4)
        .into_iter()
        .find(|i| i.label == "invoke")
        .unwrap();
    let (card, _) = index
        .resolve_completion(item.data.as_ref().unwrap())
        .unwrap();
    assert!(card.contains("fn spawn Unit -> Unit"), "{card}");
    let signature = index.signature_help_at(&uri, 3, 11, false).unwrap();
    assert!(
        signature.signatures[0]
            .label
            .contains("fn spawn Unit -> Unit"),
        "{signature:?}"
    );
    assert_eq!(index.definition_at(&uri, 3, 5).unwrap().range.start.line, 0);
    assert_eq!(
        index.declaration_at(&uri, 3, 5).unwrap().range.start.line,
        0
    );
    assert_eq!(
        index
            .references_at(&uri, 3, 5, true, &Cancel::new())
            .unwrap()
            .len(),
        2
    );
    assert!(index.prepare_rename_at(&uri, 3, 5).is_some());
    assert!(index
        .rename_at(&uri, 3, 5, "execute", &Cancel::new())
        .unwrap()
        .is_some());
    assert_eq!(index.document_highlights_at(&uri, 3, 5).unwrap().len(), 2);
    assert!(!index.semantic_tokens(&uri).unwrap().data.is_empty());
    assert!(!index.folding_ranges_at(&uri).unwrap().is_empty());
    let hierarchy = index.prepare_call_hierarchy_at(&uri, 3, 5).unwrap();
    let incoming = index
        .incoming_calls(hierarchy[0].data.as_ref().unwrap(), &Cancel::new())
        .unwrap();
    assert!(incoming.iter().any(|call| call.from.name == "run"));
    let run = index.prepare_call_hierarchy_at(&uri, 2, 13).unwrap();
    let outgoing = index.outgoing_calls(run[0].data.as_ref().unwrap()).unwrap();
    assert!(outgoing.iter().any(|call| call.to.name == "invoke"));
    let lenses = index
        .code_lenses_at(
            &uri,
            CodeLensConfig {
                references: true,
                implementations: true,
                run: false,
                run_test: false,
            },
        )
        .unwrap();
    let lens = lenses
        .into_iter()
        .find(|lens| lens.range.start.line == 0)
        .unwrap();
    assert!(index
        .resolve_code_lens(lens, &Cancel::new())
        .command
        .is_some());
    // Function values have no nominal type or class implementation hierarchy.
    assert!(index.type_definition_at(&uri, 3, 5).is_none());
    assert!(index.implementations_at(&uri, 3, 5).is_none());
    assert!(index.prepare_type_hierarchy_at(&uri, 3, 5).is_none());
}

#[test]
fn spawn_callback_permission_cannot_be_erased_by_a_pure_parameter() {
    let source = "fn pure (callback: Unit -> Unit) -> Unit = callback ()\npub fn spawn invoke (callback: fn spawn Unit -> Unit) -> Unit = pure callback\n";
    let (_dir, state, _uri) = workspace(source);
    clean_frontend(&state);
    assert!(
        !state.type_errors.is_empty(),
        "passing a spawn callback to a pure function type must fail"
    );
}

#[test]
fn callback_capabilities_follow_local_aliases_and_shadowed_names() {
    for capability in ["io", "spawn"] {
        for body in ["callback ()", "let alias = callback\n    alias ()"] {
            let source = format!("pub fn {capability} invoke (callback: fn {capability} Unit -> Unit) -> Unit =\n    {body}\n");
            let (_dir, state, _) = workspace(&source);
            clean(&state);
            let forbidden = source.replacen(&format!("fn {capability} invoke"), "fn invoke", 1);
            let (_dir, state, _) = workspace(&forbidden);
            clean_frontend(&state);
            assert!(!state.type_errors.is_empty(), "{forbidden}");
        }
        let source = format!("fn callback (value: Unit) -> Unit = value\npub fn invoke (callback: fn {capability} Unit -> Unit) -> Unit = callback ()\n");
        let (_dir, state, _) = workspace(&source);
        clean_frontend(&state);
        assert!(
            !state.type_errors.is_empty(),
            "a pure global name cannot hide the local callback's permission"
        );
    }
}
