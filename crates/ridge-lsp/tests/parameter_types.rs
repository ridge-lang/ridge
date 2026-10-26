//! Parameter declarations expose their own inferred type, not a container's.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::{cancel::Cancel, index::WorkspaceIndex};
use ridge_resolve::ModuleId;
use tower_lsp::lsp_types::Url;

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url, ModuleId) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"parameters\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
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
    let state =
        check_workspace_incremental(CheckOptions::new(root.clone()).with_retain_indices(true))
            .unwrap();
    let uri = Url::from_file_path(root.join("lib/src/Main.ridge")).unwrap();
    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    (dir, state, uri, module)
}

fn position(source: &str, offset: usize) -> (u32, u32) {
    let prefix = &source[..offset];
    (
        u32::try_from(prefix.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

fn assert_checks(state: &IncrementalState) {
    assert!(
        state.resolved.errors.is_empty(),
        "{:?}",
        state.resolved.errors
    );
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

fn assert_hover(index: &WorkspaceIndex, uri: &Url, source: &str, offset: usize, ty: &str) {
    let (line, column) = position(source, offset);
    let (card, span) = index.hover_at(uri, line, column).unwrap();
    assert_eq!(
        card.lines().nth(1),
        Some(format!("x : {ty}").as_str()),
        "{source}\n{card}"
    );
    let normalized_offset = source[..offset].replace("\r\n", "\n").len();
    assert_eq!(usize::try_from(span.start).unwrap(), normalized_offset);
    assert_eq!(usize::try_from(span.end).unwrap(), normalized_offset + 1);
}

#[test]
fn parameter_declarations_retain_their_own_types() {
    let cases = [
        ("pub fn value (x: Int) -> Int = x\n", "(x:", "Int"),
        ("pub fn value x -> Int = x\n", "value x", "Int"),
        ("pub fn value (x: a) -> a = x\n", "(x:", "a"),
        ("pub fn value (x: Int) -> Int = 7\n", "(x:", "Int"),
        ("type Book = { value: Int }\npub fn value (x: Book) -> Book = x\n", "(x:", "Book"),
        ("pub fn value () -> Int =\n    fn same (x: Int) -> Int = x\n    same 7\n", "(x:", "Int"),
        ("pub fn value () -> Int =\n    fn same x -> Int = x\n    same 7\n", "same x", "Int"),
        ("class Identity a =\n    identity (value: a) -> a\ninstance Identity Int =\n    identity (x: Int) -> Int = x\npub fn value () -> Int = identity 7\n", "(x:", "Int"),
        ("actor Counter =\n    on echo (x: Int) -> Int = x\n", "(x:", "Int"),
        ("actor Counter =\n    state value: Int\n    init (x: Int) =\n        value <- x\n    on get () -> Int = value\n", "(x:", "Int"),
        ("pub fn value () -> Int =\n    let same = fn x -> x + 1\n    same 7\n", "fn x", "Int"),
        ("pub fn value () -> Int =\n    let same = fn (x: Int) -> x\n    same 7\n", "(x:", "Int"),
        ("pub fn value () -> Int =\n    let same = fn (x, y) -> Int = x + y\n    same (3, 4)\n", "(x,", "Int"),
        ("pub fn value (x, y) -> Int = x\n", "(x,", "Int"),
    ];
    for ending in ["\n", "\r\n"] {
        for (source, anchor, ty) in cases {
            let source = format!("-- café 🧭\n{source}").replace('\n', ending);
            let (_dir, state, uri, _) = workspace(&source);
            assert_checks(&state);
            let index =
                WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
            let offset = source.find(anchor).unwrap() + anchor.find('x').unwrap();
            assert_hover(&index, &uri, &source, offset, ty);
            if ty == "Book" {
                let (line, column) = position(&source, offset);
                let definition = index.type_definition_at(&uri, line, column).unwrap();
                assert_eq!(definition.uri, uri);
                assert_eq!(definition.range.start.line, 1);
                assert_eq!(definition.range.start.character, 5);
            }
        }
    }
}

#[test]
fn parameter_type_edits_preserve_binding_identity_and_use_types() {
    let templates = [
        ("pub fn value (x: Int) -> Int = x\n", true),
        (
            "pub fn value () -> Int =\n    fn same (x: Int) -> Int = x\n    same 7\n",
            true,
        ),
        (
            "pub fn value () -> Int =\n    let same = fn (x: Int) -> x\n    same 7\n",
            false,
        ),
        ("actor Counter =\n    on echo (x: Int) -> Int = x\n", true),
    ];
    for ending in ["\n", "\r\n"] {
        for (template, check_completion) in templates {
            let before = template.replace('\n', ending);
            let after = before
                .replace("Int", "Text")
                .replace("same 7", "same \"text\"");
            let (_dir, mut state, uri, module) = workspace(&before);
            for (source, ty) in [(&before, "Int"), (&after, "Text"), (&before, "Int")] {
                state.recompile(module, source);
                assert_checks(&state);
                let index =
                    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
                let declaration = source.find("(x:").unwrap() + 1;
                let use_site = source
                    .find("= x")
                    .map_or_else(|| source.find("-> x").unwrap() + 3, |offset| offset + 2);
                assert_hover(&index, &uri, source, declaration, ty);
                assert_hover(&index, &uri, source, use_site, ty);
                let (line, column) = position(source, use_site);
                let definition = index.definition_at(&uri, line, column).unwrap();
                assert_eq!(definition.uri, uri);
                assert_eq!(
                    (
                        definition.range.start.line,
                        definition.range.start.character
                    ),
                    position(source, declaration)
                );
                assert!(index.prepare_rename_at(&uri, line, column).is_some());
                let refs = index
                    .references_at(&uri, line, column, true, &Cancel::new())
                    .unwrap();
                assert_eq!(refs.len(), 2, "{refs:?}");
                let rename = index
                    .rename_at(&uri, line, column, "renamed", &Cancel::new())
                    .unwrap()
                    .unwrap();
                assert_eq!(rename.changes.unwrap()[&uri].len(), 2);
                assert_eq!(
                    index
                        .document_highlights_at(&uri, line, column)
                        .unwrap()
                        .len(),
                    2
                );
                if check_completion {
                    let completions = index.completions_at(&uri, line, column);
                    assert!(
                        completions.iter().any(|item| item.label == "x"),
                        "{source}\n{line}:{column}"
                    );
                }
                assert!(!index.semantic_tokens(&uri).unwrap().data.is_empty());
            }
        }
    }
}

#[test]
fn generic_parameters_keep_use_site_instantiations_and_shadowed_types() {
    let source = "pub fn keep (x: a) -> a = x\npub fn value () -> Int =\n    let same = keep\n    let x = same 7\n    let ignored = same \"text\"\n    x\npub fn shadow (x: Int) -> Int =\n    let ignored =\n        let x = \"inner\"\n        x\n    x\n";
    let (_dir, state, uri, _) = workspace(source);
    assert_checks(&state);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    assert_hover(&index, &uri, source, source.find("(x:").unwrap() + 1, "a");
    assert_hover(&index, &uri, source, source.find("= x").unwrap() + 2, "a");
    assert_hover(
        &index,
        &uri,
        source,
        source.find("let x = same").unwrap() + 4,
        "Int",
    );
    assert_hover(
        &index,
        &uri,
        source,
        source.find("let x = \"inner\"").unwrap() + 4,
        "Text",
    );
    assert_hover(
        &index,
        &uri,
        source,
        source.find("shadow (x").unwrap() + 8,
        "Int",
    );
    assert_hover(&index, &uri, source, source.rfind('x').unwrap(), "Int");
}
