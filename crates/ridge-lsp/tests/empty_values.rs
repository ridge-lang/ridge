//! Collection empty values stay consistent through incremental editor updates.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"empty-values\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/src/Main.ridge"), source).unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let uri = Url::from_file_path(root.join("lib/src/Main.ridge")).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    (dir, state, uri)
}

#[test]
fn collection_empty_completion_and_hover_show_values() {
    for module in ["Map", "List", "Set", "Bytes"] {
        let source = format!(
            "import std.{} as {module}\npub fn size () -> Int = {module}.length {module}.empty\n",
            module.to_lowercase()
        );
        let (_dir, state, uri) = workspace(&source);
        assert!(
            state.resolved.errors.is_empty(),
            "{:?}",
            state.resolved.errors
        );
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let at = source.rfind("empty").unwrap();
        let line = source[..at].matches('\n').count() as u32;
        let col = (at - source[..at].rfind('\n').unwrap() - 1) as u32;
        let (hover, _) = index.hover_at(&uri, line, col + 1).unwrap();
        let header = format!("pub fn empty -> {module}");
        assert!(hover.contains(&header), "{hover}");
        let items = index.completions_at(&uri, line, col);
        let item = items.iter().find(|item| item.label == "empty").unwrap();
        let (card, _) = index
            .resolve_completion(item.data.as_ref().unwrap())
            .unwrap();
        assert!(card.contains(&header), "{card}");
    }
}

#[test]
fn removing_empty_argument_repairs_alias_and_selective_import_incrementally() {
    for (import, expression) in [
        ("import std.bytes as Binary", "Binary.empty"),
        ("import std.bytes (empty)", "empty"),
    ] {
        let source = format!("{import}\npub fn value () -> Bytes = {expression} ()\n");
        let (_dir, mut state, _) = workspace(&source);
        assert!(state.resolved.errors.is_empty());
        assert!(
            !state.type_errors.is_empty(),
            "a byte string cannot be called"
        );
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        let repaired = source.replace(&format!("{expression} ()"), expression);
        state.recompile(mid, &repaired);
        assert!(
            state.resolved.errors.is_empty(),
            "{:?}",
            state.resolved.errors
        );
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
    }
}
