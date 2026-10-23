//! Editor signatures and incremental diagnostics agree on splitAny's list input.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{
    check_workspace_incremental, collect_diagnostics, CheckOptions, IncrementalState,
};
use ridge_lsp::{diagnostics::to_lsp_diagnostic, index::WorkspaceIndex};
use tower_lsp::lsp_types::Url;

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"split-any\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
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

fn position(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset];
    (
        u32::try_from(before.matches('\n').count()).unwrap(),
        u32::try_from(before.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

fn assert_clean(state: &IncrementalState) {
    assert!(state.resolved.lex_errors.is_empty());
    assert!(state.resolved.parse_errors.is_empty());
    assert!(
        state.resolved.errors.is_empty(),
        "{:?}",
        state.resolved.errors
    );
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

#[test]
fn text_split_any_editor_signatures_and_incremental_diagnostics_agree() {
    for (import, name) in [
        ("import std.text as Text", "Text.splitAny"),
        ("import std.text (splitAny, split)", "splitAny"),
    ] {
        let source =
            format!("{import}\npub fn parts () -> List Text = {name} [\",\", \";\"] \"a,b;c\"\n");
        let (_dir, mut state, uri) = workspace(&source);
        assert_clean(&state);
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let (line, col) = position(&source, source.rfind("splitAny").unwrap());
        let header = "pub fn splitAny (seps: List Text) (s: Text) -> List Text";
        let (hover, _) = index.hover_at(&uri, line, col + 1).unwrap();
        assert!(hover.contains(header), "{hover}");
        let items = index.completions_at(&uri, line, col);
        let item = items
            .iter()
            .find(|item| item.label == "splitAny")
            .unwrap_or_else(|| {
                let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
                panic!("{import}: completion labels {labels:?}")
            });
        let (card, _) = index
            .resolve_completion(item.data.as_ref().unwrap())
            .unwrap();
        assert!(card.contains(header), "{card}");
        if name == "splitAny" {
            let control = items.iter().find(|item| item.label == "split").unwrap();
            let (control_card, _) = index
                .resolve_completion(control.data.as_ref().unwrap())
                .unwrap();
            assert!(control_card.contains("pub fn split (sep: Text) (s: Text) -> List Text"));
            assert!(!items.iter().any(|item| item.label == "trim"));
        }
        let (arg_line, arg_col) = position(&source, source.find("[\",\"").unwrap() + 1);
        let signature = index
            .signature_help_at(&uri, arg_line, arg_col, false)
            .unwrap();
        assert!(signature.signatures[0].label.contains("seps: List Text"));

        let broken = source.replace("[\",\", \";\"]", "\",\"");
        for _ in 0..2 {
            state.recompile(mid, &broken);
            assert!(state.resolved.parse_errors.is_empty());
            assert!(state.resolved.errors.is_empty());
            let diagnostics = collect_diagnostics(
                &state.disc_resolve_errors,
                &state.resolved,
                &state.type_errors,
                &state.lower_errors,
                &state.source_cache(),
                &state.typed.tycons,
            );
            let diagnostic = diagnostics
                .iter()
                .map(|d| to_lsp_diagnostic(d, &uri, Some(&broken)))
                .find(|d| d.message.contains("splitAny") && d.message.contains("List Text"))
                .expect("the editor rejects a text separator");
            let start = broken.rfind("\",\"").unwrap();
            let (line, character) = position(&broken, start);
            assert_eq!(diagnostic.range.start.line, line);
            assert_eq!(diagnostic.range.start.character, character);
            state.recompile(mid, &source);
            assert_clean(&state);
        }
    }
}

#[test]
fn selective_function_completions_preserve_prefix_and_local_shadowing() {
    let source =
        "import std.text (splitAny, split)\npub fn value (splitAny: Int) -> Int = splitAny\n";
    let (_dir, state, uri) = workspace(source);
    assert_clean(&state);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let (line, col) = position(source, source.rfind("splitAny").unwrap() + 3);
    let items = index.completions_at(&uri, line, col);
    let local: Vec<_> = items
        .iter()
        .filter(|item| item.label == "splitAny")
        .collect();
    assert_eq!(
        local.len(),
        1,
        "the local must shadow the imported function"
    );
    assert_eq!(
        local[0].kind,
        tower_lsp::lsp_types::CompletionItemKind::VARIABLE
    );
    assert!(local[0].data.is_none());
    let imported = items.iter().find(|item| item.label == "split").unwrap();
    assert!(local[0].sort_text < imported.sort_text);
    assert!(items.iter().all(|item| item.label.starts_with("spl")));
}
