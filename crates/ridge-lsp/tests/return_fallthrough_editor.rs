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
