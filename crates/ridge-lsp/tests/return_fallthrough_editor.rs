//! Return flow uses the editor's incremental checking and position index too.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{
    check_workspace_incremental, collect_diagnostics, CheckOptions, IncrementalState,
};
use ridge_lsp::diagnostics::to_lsp_diagnostic;
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::{DiagnosticSeverity, NumberOrString, Url};

const SOURCE: &str =
    "pub fn answer () -> Int =\n    return 5\n\npub fn use () -> Int = answer ()\n";

/// Use exactly the diagnostic collection path the server uses after an edit.
fn diagnostics(
    state: &IncrementalState,
    uri: &Url,
    source: &str,
) -> Vec<tower_lsp::lsp_types::Diagnostic> {
    collect_diagnostics(
        &state.disc_resolve_errors,
        &state.resolved,
        &state.type_errors,
        &state.lower_errors,
        &state.source_cache(),
        &state.typed.tycons,
    )
    .iter()
    .map(|diag| to_lsp_diagnostic(diag, uri, Some(source)))
    .collect()
}

#[test]
fn valid_return_has_a_type_and_definition_and_invalid_edit_has_a_diagnostic() {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"returns\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/src/Returns.ridge"), SOURCE).unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let mut state =
        check_workspace_incremental(CheckOptions::new(root.clone()).with_retain_indices(true))
            .unwrap();
    let uri = Url::from_file_path(root.join("lib/src/Returns.ridge")).unwrap();
    assert!(diagnostics(&state, &uri, SOURCE).is_empty());
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let use_line = SOURCE.lines().nth(3).unwrap();
    let column = u32::try_from(use_line.find("answer").unwrap()).unwrap();
    let (card, _) = index
        .hover_at(&uri, 3, column)
        .expect("hover on answer call");
    assert!(card.contains("Int"), "{card}");
    let definition = index
        .definition_at(&uri, 3, column)
        .expect("definition of answer");
    assert_eq!(definition.uri, uri);
    assert_eq!(definition.range.start.line, 0);

    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Returns.ridge"))
        .unwrap()
        .id;
    let wrong = SOURCE.replace("return 5", "return \"bad\"");
    state.recompile(mid, &wrong);
    let errors = diagnostics(&state, &uri, &wrong);
    assert!(errors.iter().any(
        |d| d.code == Some(NumberOrString::String("T001".to_owned()))
            && d.severity == Some(DiagnosticSeverity::ERROR)
    ));
    state.recompile(mid, SOURCE);
    assert!(
        diagnostics(&state, &uri, SOURCE).is_empty(),
        "repair must clear the editor error"
    );
}

#[test]
fn missing_return_targets_appear_and_clear_across_unsaved_context_edits() {
    fn position(source: &str, offset: usize) -> tower_lsp::lsp_types::Position {
        let prefix = &source[..offset];
        tower_lsp::lsp_types::Position::new(
            u32::try_from(prefix.bytes().filter(|b| *b == b'\n').count()).unwrap(),
            u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
        )
    }
    for ending in ["\n", "\r\n"] {
        let valid =
            "-- café 🧭\nactor Counter =\n    on value () -> Unit = ()\n".replace('\n', ending);
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
        std::fs::write(
            dir.path().join("ridge.toml"),
            "[workspace]\nname=\"returns\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("lib/ridge.toml"),
            "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("lib/src/Returns.ridge"), &valid).unwrap();
        let root = ridge_manifest::canonicalize(dir.path()).unwrap();
        let uri = Url::from_file_path(root.join("lib/src/Returns.ridge")).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Returns.ridge"))
            .unwrap()
            .id;
        for (source, expected) in [
            (valid.clone(), None),
            (valid.replace("= ()", "= if \"🧭\" == \"x\" then return () else ()"), Some("T062")),
            ("const answer: Unit = return ()\npub fn value () -> Unit = answer\n".replace('\n', ending), Some("T062")),
            ("actor Counter =\n    on value () -> Unit =\n        try\n            return ()\n        ()\n".replace('\n', ending), Some("T062")),
            ("actor Counter =\n    on value () -> Int =\n        let f = fn () -> Int = return 7\n        f ()\n".replace('\n', ending), None),
            ("actor Counter =\n    on value () -> Int =\n        let f = fn () -> Int = return \"wrong\"\n        f ()\n".replace('\n', ending), Some("T001")),
            (valid, None),
        ] {
            state.recompile(mid, &source);
            let errors: Vec<_> = diagnostics(&state, &uri, &source).into_iter().filter(|d| d.severity == Some(DiagnosticSeverity::ERROR)).collect();
            if let Some(code) = expected {
                let error = errors.iter().find(|d| d.code == Some(NumberOrString::String(code.to_owned()))).unwrap_or_else(|| panic!("{errors:?}"));
                if code == "T062" {
                    let start = source.find("return ()").unwrap();
                    assert_eq!(error.range.start, position(&source, start));
                    assert_eq!(error.range.end, position(&source, start + "return ()".len()));
                    assert!(error.message.contains("enclosing function or lambda"));
                    assert!(error.code_description.as_ref().unwrap().href.as_str().ends_with("#t062"));
                }
            } else {
                assert!(errors.is_empty(), "{errors:?}");
            }
        }
    }
}
