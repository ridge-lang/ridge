//! Constant metadata changes retain the editor's type and navigation contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

#[test]
fn constant_types_and_definitions_follow_an_incremental_edit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"constants\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let source = "const sizes: List Int = [1, 2]\npub fn values () -> List Int = sizes\n";
    std::fs::write(dir.path().join("lib/src/Constants.ridge"), source).unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let mut state =
        check_workspace_incremental(CheckOptions::new(root.clone()).with_retain_indices(true))
            .unwrap();
    let uri = Url::from_file_path(root.join("lib/src/Constants.ridge")).unwrap();
    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Constants.ridge"))
        .unwrap()
        .id;
    let edited = source
        .replace("List Int", "List Text")
        .replace("[1, 2]", "[\"a\"]");
    for (text, expected) in [(source, "List Int"), (edited.as_str(), "List Text")] {
        state.recompile(module, text);
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let column = u32::try_from(text.lines().nth(1).unwrap().rfind("sizes").unwrap()).unwrap();
        let (hover, _) = index.hover_at(&uri, 1, column).unwrap();
        assert!(hover.contains(expected), "{hover}");
        let definition = index.definition_at(&uri, 1, column).unwrap();
        assert_eq!(definition.uri, uri);
        assert_eq!(definition.range.start.line, 0);
    }
}
