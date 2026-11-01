//! Predicate editor support through valid/error/restored unsaved buffers.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ridge_driver::{check_workspace_incremental, CheckOptions};
use ridge_lsp::{cancel::Cancel, index::WorkspaceIndex};
use tower_lsp::lsp_types::Url;

#[test]
fn predicate_hover_navigation_completion_and_edit_recovery() {
    for ending in ["\n", "\r\n"] {
        let (_dir, root) = editor_workspace();
        let valid = "type Shape = | Square Int | Circle Int\npub fn probe (shape: Shape) -> (Text, Bool) = (\"🧭 café\", shape is Square)\n".replace('\n', ending);
        let file = root.join("app/src/Main.ridge");
        let other = root.join("app/src/Other.ridge");
        std::fs::write(
            &other,
            "pub type Shape = | Square Bool | Circle\npub fn make () -> Shape = Square true\n",
        )
        .unwrap();
        std::fs::write(&file, &valid).unwrap();
        let uri = Url::from_file_path(&file).unwrap();
        let mut state =
            check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
        let module = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path == file)
            .unwrap()
            .id;
        let changed = valid.replace("🧭 café", "🧭 🧭 café");
        for buffer in [&valid, &changed, &valid] {
            state.recompile(module, buffer);
            assert!(
                state.resolved.parse_errors.is_empty(),
                "{:?}",
                state.resolved.parse_errors
            );
            assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
            assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
            let index =
                WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
            let line = buffer.lines().nth(1).unwrap();
            let col = |needle: &str| {
                u32::try_from(line[..line.rfind(needle).unwrap()].encode_utf16().count()).unwrap()
            };
            let (hover, span) = index.hover_at(&uri, 1, col("is Square")).unwrap();
            assert!(
                hover.contains("independently of `Eq`") && hover.contains("once"),
                "{hover}"
            );
            assert_eq!(span.end - span.start, 2);
            let ctor_col = col("Square");
            assert!(index.hover_at(&uri, 1, ctor_col).is_some());
            let definition = index.definition_at(&uri, 1, ctor_col).unwrap();
            assert_eq!(definition.range.start.line, 0);
            assert_eq!(definition.uri, uri);
            assert_eq!(
                index.type_definition_at(&uri, 1, ctor_col),
                Some(definition)
            );
            assert!(index
                .completions_at(&uri, 1, ctor_col)
                .iter()
                .any(|item| item.label == "Square"));
            assert!(index
                .rename_at(&uri, 1, col("shape is"), "is", &Cancel::new())
                .is_err());
            let invalid = buffer.replace("is Square)", "is Square captured)");
            state.recompile(module, &invalid);
            assert!(state
                .resolved
                .parse_errors
                .iter()
                .any(|(_, error)| error.to_string().contains("use `match`")));
            let invalid_index =
                WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
            assert!(!invalid_index
                .completions_at(&uri, 1, ctor_col)
                .iter()
                .any(|item| item.label == "captured"));
            state.recompile(module, buffer);
            let invalid_type = buffer.replace("is Square)", "is Square true)");
            state.recompile(module, &invalid_type);
            assert!(!state.type_errors.is_empty());
            let wildcard = buffer.replace("is Square)", "is _)");
            state.recompile(module, &wildcard);
            let index =
                WorkspaceIndex::build(2, &state.typed, &state.resolved, &state.source_cache());
            let (hover, _) = index.hover_at(&uri, 1, col("is Square")).unwrap();
            assert!(hover.contains("always succeeds"), "{hover}");
        }
        state.recompile(module, &valid);
        assert!(state.type_errors.is_empty());
        assert!(state.lower_errors.is_empty());
        assert_imported_navigation(&mut state, module, ending, &uri, &other);
    }
}

fn editor_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join("app/src")).unwrap();
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname=\"predicate-editor\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    (dir, root)
}

fn assert_imported_navigation(
    state: &mut ridge_driver::IncrementalState,
    module: ridge_resolve::ModuleId,
    ending: &str,
    uri: &Url,
    other: &std::path::Path,
) {
    let foreign = "import app.Other as Other\npub fn probe () -> (Text, Bool) = (\"🧭 café\", Other.make () is Other.Square)\n".replace('\n', ending);
    state.recompile(module, &foreign);
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
    let index = WorkspaceIndex::build(3, &state.typed, &state.resolved, &state.source_cache());
    let line = foreign.lines().nth(1).unwrap();
    let column =
        u32::try_from(line[..line.rfind("Square").unwrap()].encode_utf16().count()).unwrap();
    let target = index.definition_at(uri, 1, column).unwrap();
    assert_eq!(target.uri, Url::from_file_path(other).unwrap());
    assert_eq!(index.type_definition_at(uri, 1, column), Some(target));
}
