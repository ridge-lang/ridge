//! Inferred parameter bindings retain editor identity and incremental types.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::{cancel::Cancel, index::WorkspaceIndex};
use ridge_resolve::ModuleId;
use tower_lsp::lsp_types::{TextEdit, Url};

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url, ModuleId) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"patterns\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
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
    let line = u32::try_from(prefix.bytes().filter(|b| *b == b'\n').count()).unwrap();
    let column = u32::try_from(prefix.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap();
    (line, column)
}

fn apply_rename(source: &str, edits: &[TextEdit], name: &str) -> String {
    let mut ordered: Vec<_> = edits.iter().collect();
    ordered
        .sort_by_key(|edit| std::cmp::Reverse((edit.range.start.line, edit.range.start.character)));
    let li = ridge_lexer::LineIndex::new(source);
    let mut updated = source.to_owned();
    for edit in ordered {
        let start =
            usize::try_from(li.utf16_to_byte(edit.range.start.line, edit.range.start.character))
                .unwrap();
        let end = usize::try_from(li.utf16_to_byte(edit.range.end.line, edit.range.end.character))
            .unwrap();
        assert_eq!(&source[start..end], name);
        assert!(edit.new_text == "renamed" || edit.new_text == format!("{name} = renamed"));
        updated.replace_range(start..end, &edit.new_text);
    }
    updated
}

fn assert_program_checks(source: &str) {
    let parsed = ridge_parser::parse_source(source);
    assert!(
        parsed.lex_errors.is_empty(),
        "{source}\n{:?}",
        parsed.lex_errors
    );
    assert!(parsed.errors.is_empty(), "{source}\n{:?}", parsed.errors);
    let (_dir, state, _, _) = workspace(source);
    assert!(
        state.resolved.errors.is_empty(),
        "{source}\n{:?}",
        state.resolved.errors
    );
    assert!(
        state.type_errors.is_empty(),
        "{source}\n{:?}",
        state.type_errors
    );
    assert!(
        state.lower_errors.is_empty(),
        "{source}\n{:?}",
        state.lower_errors
    );
}

fn assert_binding(index: &WorkspaceIndex, uri: &Url, source: &str, name: &str, ty: &str) {
    let (line, column) = position(source, source.rfind(name).unwrap());
    let functions = source.find("pub fn").unwrap();
    let (def_line, def_column) =
        position(source, functions + source[functions..].find(name).unwrap());
    for (hover_line, hover_column) in [(line, column), (def_line, def_column)] {
        let (hover, _) = index.hover_at(uri, hover_line, hover_column).unwrap();
        assert!(hover.contains(ty), "{name}: {hover}");
    }
    let definition = index.definition_at(uri, line, column).unwrap();
    assert_eq!(definition.uri, *uri);
    assert_eq!(
        (
            definition.range.start.line,
            definition.range.start.character
        ),
        (def_line, def_column)
    );
    let references = index
        .references_at(uri, line, column, true, &Cancel::new())
        .unwrap();
    assert_eq!(references.len(), 2, "{references:?}");
    assert!(index.prepare_rename_at(uri, line, column).is_some());
    let highlights = index.document_highlights_at(uri, line, column).unwrap();
    assert_eq!(highlights.len(), 2, "{highlights:?}");
    assert!(!index.semantic_tokens(uri).unwrap().data.is_empty());
    let completion = index.completions_at(uri, line, column);
    assert!(
        completion.iter().any(|item| item.label == name),
        "missing completion {name}"
    );
    let edit = index
        .rename_at(uri, line, column, "renamed", &Cancel::new())
        .unwrap()
        .unwrap();
    let changes = edit.changes.unwrap();
    let edits = &changes[uri];
    assert_eq!(edits.len(), 2, "{edits:?}");
    assert_program_checks(&apply_rename(source, edits, name));
}

#[test]
fn inferred_parameter_bindings_follow_incremental_type_edits() {
    let before = "pub fn pick (value, other) -> Int = value\n";
    let after = "pub fn pick (value, other) -> Text = value\n";
    let (_dir, mut state, uri, module) = workspace(before);
    for (source, ty) in [(before, "Int"), (after, "Text")] {
        state.recompile(module, source);
        assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
        assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
        let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
        assert_binding(&index, &uri, source, "value", ty);
    }
}

#[test]
fn nested_record_and_alias_parameters_have_editor_bindings() {
    let source = "type Point = { x: Int, y: Text }\ntype Labelled = { label: Text }\npub fn show (whole @ (Point { x = first, y = second }, _)) -> Text =\n    let (point, _) = whole\n    second\npub fn keep (entry @ Point { .. }) -> Point = entry\npub fn text (Labelled { label }) -> Text = label\npub fn items ([xs @ ..]) -> List Int = xs\npub fn inner (pair: (Int, Int)) -> Int =\n    fn add (left, right) -> Int = left + right\n    add pair\n";
    let (_dir, mut state, uri, module) = workspace(source);
    state.recompile(module, source);
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    assert_binding(&index, &uri, source, "whole", "Point");
    assert_binding(&index, &uri, source, "second", "Text");
    assert_binding(&index, &uri, source, "entry", "Point");
    assert_binding(&index, &uri, source, "label", "Text");
    assert_binding(&index, &uri, source, "xs", "List Int");
    assert_binding(&index, &uri, source, "left", "Int");
    for offset in [
        source.find("entry").unwrap(),
        source.rfind("entry").unwrap(),
    ] {
        let (line, column) = position(source, offset);
        let definition = index.type_definition_at(&uri, line, column).unwrap();
        assert_eq!(definition.uri, uri);
        assert_eq!(definition.range.start.line, 0);
    }
}

#[test]
fn lifecycle_pattern_bindings_have_fixed_editor_types() {
    let source = "pub fn ready () -> Unit = ()\nactor Watcher =\n    terminate (why) =\n        let _ = why\n        ()\n    onDown (watched) reason =\n        let _ = watched\n        ()\n";
    let (_dir, mut state, uri, module) = workspace(source);
    state.recompile(module, source);
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    assert_binding(&index, &uri, source, "why", "ExitReason");
    assert_binding(&index, &uri, source, "watched", "Monitor");
    state.recompile(module, &source.replace("let _ = why", "let _ = why + 1"));
    assert!(
        !state.type_errors.is_empty(),
        "callback input cannot become numeric after an edit"
    );
}

#[test]
fn parameter_rename_preserves_pattern_and_value_record_puns() {
    let source = "type Labelled = { label: Text }\npub fn copy (Labelled { label }) -> Labelled = Labelled { label }\n";
    let (_dir, mut state, uri, module) = workspace(source);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let (line, column) = position(source, source.find("label }").unwrap());
    let edit = index
        .rename_at(&uri, line, column, "renamed", &Cancel::new())
        .unwrap()
        .unwrap();
    let edits = edit.changes.unwrap().remove(&uri).unwrap();
    let updated = apply_rename(source, &edits, "label");
    assert_program_checks(&updated);
    state.recompile(module, &updated);
    assert!(
        state.resolved.errors.is_empty(),
        "{updated}\n{:?}",
        state.resolved.errors
    );
    assert!(
        state.type_errors.is_empty(),
        "{updated}\n{:?}",
        state.type_errors
    );
    assert!(
        state.lower_errors.is_empty(),
        "{updated}\n{:?}",
        state.lower_errors
    );
    assert!(
        updated.contains("label: Text"),
        "the field declaration must keep its name: {updated}"
    );
    assert!(
        updated.contains("label = renamed"),
        "the variable must change without renaming its field: {updated}"
    );
}
