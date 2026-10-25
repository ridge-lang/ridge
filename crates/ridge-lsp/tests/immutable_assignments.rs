//! Immutable-write diagnostics preserve declaration identity across edits.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_diagnostics::{Diagnostic, SourceId};
use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::diagnostics::to_lsp_diagnostic;
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Position, Range, Url};

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"immutable-editor\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let file = root.join("lib/src/Main.ridge");
    std::fs::write(&file, source).unwrap();
    let uri = Url::from_file_path(file).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    (dir, state, uri)
}

fn range(source: &str, offset: usize) -> Range {
    let before = &source[..offset];
    let start = Position::new(
        u32::try_from(before.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(before.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    );
    Range::new(start, Position::new(start.line, start.character + 1))
}

fn assert_diagnostic(state: &IncrementalState, source: &str, uri: &Url, declaration: &str) {
    assert!(state.resolved.lex_errors.is_empty());
    assert!(
        state.resolved.parse_errors.is_empty(),
        "{:?}",
        state.resolved.parse_errors
    );
    let errors: Vec<_> = state
        .resolved
        .errors
        .iter()
        .filter(|(_, e)| matches!(e, ridge_resolve::ResolveError::ImmutableAssignment { .. }))
        .collect();
    assert_eq!(errors.len(), 1, "{:?}", state.resolved.errors);
    let diagnostic = Diagnostic::from_resolve(&errors[0].1, SourceId::new("lib/src/Main.ridge"));
    let lsp = to_lsp_diagnostic(&diagnostic, uri, Some(source));
    assert_eq!(lsp.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(lsp.code, Some(NumberOrString::String("R031".to_owned())));
    assert_eq!(lsp.range, range(source, source.find("n <-").unwrap()));
    assert!(
        lsp.message
            .contains("cannot assign to immutable binding `n`"),
        "{}",
        lsp.message
    );
    assert!(
        lsp.message.contains("use `var` when mutation is intended"),
        "{}",
        lsp.message
    );
    assert!(lsp
        .code_description
        .unwrap()
        .href
        .as_str()
        .ends_with("#r031"));
    let related = lsp.related_information.unwrap();
    let binding = related
        .iter()
        .find(|note| note.message == "immutable binding declared here")
        .unwrap();
    assert_eq!(binding.location.uri, *uri);
    assert_eq!(
        binding.location.range,
        range(source, source.find(declaration).unwrap() + 4)
    );
}

#[test]
fn immutable_assignment_diagnostics_follow_line_endings_and_edits() {
    let invalid = "-- café 🚀\npub fn value () -> Int =\n    let n = 1\n    n <- 7\n    n\n";
    for ending in ["\n", "\r\n"] {
        let invalid = invalid.replace('\n', ending);
        let (_dir, mut state, uri) = workspace(&invalid);
        assert_diagnostic(&state, &invalid, &uri, "let n = 1");
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for _ in 0..2 {
            let valid = invalid.replace("let n", "var n");
            state.recompile(mid, &valid);
            assert!(
                state.resolved.errors.is_empty(),
                "{:?}",
                state.resolved.errors
            );
            assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
            assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            state.recompile(mid, &invalid);
            assert_diagnostic(&state, &invalid, &uri, "let n = 1");
        }
    }
}

#[test]
fn immutable_assignment_related_information_follows_shadowed_binding() {
    for ending in ["\n", "\r\n"] {
        let valid = "-- café 🚀\npub fn value () -> Int =\n    let n = 1\n    let chosen =\n        var n = 2\n        n <- 7\n        n\n    n + chosen\n".replace('\n', ending);
        let (_dir, mut state, uri) = workspace(&valid);
        assert!(
            state.resolved.errors.is_empty(),
            "{:?}",
            state.resolved.errors
        );
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        let invalid = valid
            .replace("let n = 1", "var n = 1")
            .replace("var n = 2", "let n = 2");
        state.recompile(mid, &invalid);
        assert_diagnostic(&state, &invalid, &uri, "let n = 2");
        state.recompile(mid, &valid);
        assert!(
            state.resolved.errors.is_empty(),
            "{:?}",
            state.resolved.errors
        );
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    }
}
