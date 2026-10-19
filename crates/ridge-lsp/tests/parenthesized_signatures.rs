//! Parentheses preserve signature promises and their editor diagnostics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{
    check_workspace_incremental, collect_diagnostics, CheckOptions, IncrementalState,
};
use ridge_lsp::{diagnostics::to_lsp_diagnostic, index::WorkspaceIndex};
use tower_lsp::lsp_types::{NumberOrString, Url};

fn position(source: &str, offset: usize) -> (u32, u32) {
    let prefix = &source[..offset];
    (
        u32::try_from(prefix.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

fn assert_clean(state: &IncrementalState) {
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
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

fn workspace(
    source: &str,
) -> (
    tempfile::TempDir,
    IncrementalState,
    ridge_resolve::ModuleId,
    Url,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"signatures\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let path = root.join("app/src/Main.ridge");
    std::fs::write(&path, source).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    assert_clean(&state);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path == path)
        .unwrap()
        .id;
    (dir, state, mid, uri)
}

#[test]
fn parenthesized_signature_diagnostics_repair_with_utf16_ranges_and_typed_hovers() {
    let valid = "pub fn choose (tag: Text) (value: Text) -> Text = value\npub fn bad (f: (fn Int -> Text)) -> Text = choose \"caf\u{e9} \u{1f9ed}\" (f 1)\n";
    let (_dir, mut state, mid, uri) = workspace(valid);
    let broken = valid.replace("(fn Int -> Text)", "((fn a -> a))");
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
            .find(|d| {
                d.code == Some(NumberOrString::String("T002".into())) && d.message.contains("`f`")
            })
            .expect("the function must preserve its generic promise");
        let argument = broken.rfind('1').unwrap();
        assert_ne!(
            usize::try_from(position(&broken, argument).1).unwrap(),
            broken[..argument].rsplit('\n').next().unwrap().len(),
            "the prefix must distinguish UTF-16 columns from bytes"
        );
        assert_eq!(
            (
                diagnostic.range.start.line,
                diagnostic.range.start.character
            ),
            position(&broken, argument)
        );
        assert_eq!(
            (diagnostic.range.end.line, diagnostic.range.end.character),
            position(&broken, argument + 1)
        );
        assert!(diagnostic.message.contains("expected a"), "{diagnostic:?}");
        state.recompile(mid, valid);
        assert_clean(&state);
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        let (line, column) = position(valid, valid.rfind("(f 1)").unwrap() + 1);
        let (hover, _) = index
            .hover_at(&uri, line, column)
            .expect("repaired parameter hover");
        assert!(hover.contains("Int") && hover.contains("Text"), "{hover}");
    }
    let generic = "pub fn same (x: (a)) -> (a) = x\n";
    state.recompile(mid, generic);
    assert_clean(&state);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let (line, column) = position(generic, generic.rfind('x').unwrap());
    let (hover, _) = index
        .hover_at(&uri, line, column)
        .expect("generic parameter hover");
    assert!(
        hover
            .lines()
            .any(|line| line.trim() == "a" || line.contains(": a")),
        "{hover}"
    );
}
