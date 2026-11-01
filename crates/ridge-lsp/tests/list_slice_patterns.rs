//! Slice bindings and diagnostics survive unsaved edits and both line endings.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

#[test]
fn slice_binding_navigation_and_diagnostic_recovery() {
    for ending in ["\n", "\r\n"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("app/src")).unwrap();
        std::fs::write(
            root.join("ridge.toml"),
            "[workspace]\nname=\"slices\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("app/ridge.toml"),
            "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
        )
        .unwrap();
        let valid = "pub fn ends (xs: List Int) -> (Text, Int) =\n    match xs\n        ([first, .., last]) when first == last -> (\"🌲\", last)\n        _ -> (\"other\", 0)\npub fn nested (xs: List (List Int)) -> Int =\n    match xs\n        [[.., _]] -> 1\n        _ -> 0\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        std::fs::write(&file, &valid).unwrap();
        let uri = Url::from_file_path(&file).unwrap();
        let mut state = check_workspace_incremental(
            CheckOptions::new(root.to_path_buf()).with_retain_indices(true),
        )
        .unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path == file)
            .unwrap()
            .id;
        for _ in 0..2 {
            state.recompile(module, &valid);
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
            assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
            assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            let index =
                WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
            let line = valid.lines().nth(2).unwrap();
            let body_col =
                u32::try_from(line[..line.rfind("last").unwrap()].encode_utf16().count()).unwrap();
            let binding_col =
                u32::try_from(line[..line.find("last").unwrap()].encode_utf16().count()).unwrap();
            let definition = index.definition_at(&uri, 2, body_col).unwrap();
            assert_eq!(definition.uri, uri);
            assert_eq!(definition.range.start.line, 2);
            assert_eq!(definition.range.start.character, binding_col);
            assert!(index.hover_at(&uri, 2, body_col).is_some());
            let invalid = valid.replace("[[.., _]]", "[[.., 1]]");
            state.recompile(module, &invalid);
            assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
            assert!(
                state
                    .lower_errors
                    .iter()
                    .any(|(_, error)| error.code() == "L109"),
                "{:?}",
                state.lower_errors
            );
        }
        state.recompile(module, &valid);
        assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
    }
}
