//! Valid mutable initializers keep frontend identities and permission checks.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::{Position, Range, Url};

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"initializer-editor\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let path = root.join("lib/src/Main.ridge");
    std::fs::write(&path, source).unwrap();
    let uri = Url::from_file_path(path).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    assert!(state.resolved.lex_errors.is_empty());
    assert!(
        state.resolved.parse_errors.is_empty(),
        "{:?}",
        state.resolved.parse_errors
    );
    (dir, state, uri)
}

fn clean(state: &IncrementalState) {
    assert!(
        state.resolved.errors.is_empty(),
        "{:?}",
        state.resolved.errors
    );
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

#[test]
fn initializer_types_and_navigation_follow_incremental_changes() {
    let initial = "pub fn value () -> Int =\n    var n = 1\n    let chosen =\n        n <- 7\n        n\n    n\n";
    let (_dir, mut state, uri) = workspace(initial);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    for (ty, source) in [
        ("Int", initial.to_owned()),
        (
            "Text",
            initial
                .replace("Int", "Text")
                .replace("= 1", "= \"old\"")
                .replace("<- 7", "<- \"new\""),
        ),
        ("Int", initial.to_owned()),
    ] {
        for ending in ["\n", "\r\n"] {
            state.recompile(mid, &source.replace('\n', ending));
            clean(&state);
            let index =
                WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
            let declaration = Range::new(Position::new(1, 8), Position::new(1, 9));
            for (line, column) in [(1, 8), (3, 8), (4, 8), (5, 4)] {
                let (card, span) = index.hover_at(&uri, line, column).unwrap();
                assert!(card.contains(&format!("\nn : {ty}\n")), "{card}");
                assert!(card.contains("*(mutable variable)*"), "{card}");
                assert_eq!(
                    index.span_to_range(&uri, span).unwrap(),
                    Range::new(Position::new(line, column), Position::new(line, column + 1))
                );
                let definition = index.definition_at(&uri, line, column).unwrap();
                assert_eq!(definition.uri, uri);
                assert_eq!(definition.range, declaration);
            }
        }
    }
}

#[test]
fn mutable_initializers_do_not_bypass_assignment_types() {
    let wrong_type = "pub fn value () -> Int =\n    var n = 1\n    let chosen =\n        n <- \"bad\"\n        n\n    n\n";
    let (_dir, state, _uri) = workspace(wrong_type);
    assert!(
        state
            .type_errors
            .iter()
            .any(|(_, e)| matches!(e, ridge_typecheck::TypeError::TypeMismatch { .. })),
        "{:?}",
        state.type_errors
    );
}

#[test]
fn initializer_effects_still_require_the_declared_capability() {
    let source = "import std.io as Io\npub fn value () -> Unit =\n    var n = 1\n    let chosen =\n        n <- 7\n        Io.println \"effect\"\n    ()\n";
    let (_dir, state, _uri) = workspace(source);
    assert!(
        state.type_errors.iter().any(|(_, e)| matches!(
            e,
            ridge_typecheck::TypeError::CapabilityNotDeclared { .. }
                | ridge_typecheck::TypeError::CallerCapabilityInsufficient { .. }
        )),
        "{:?}",
        state.type_errors
    );
    let (_dir, state, _uri) = workspace(&source.replace("pub fn value", "pub fn io value"));
    clean(&state);
}
