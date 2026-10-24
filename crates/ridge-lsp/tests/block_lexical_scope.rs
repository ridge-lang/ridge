//! Block-local declarations and resolved identities agree with inferred types.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::{cancel::Cancel, index::WorkspaceIndex};
use tower_lsp::lsp_types::{Position, Range, TextEdit, Url};

const SHADOW: &str = "pub fn value () -> Int =\n    let (n, text) = (42, \"hello\")\n    let chosen =\n        let n = \"inner\"\n        n\n    n\n";

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"scopes\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    let root = ridge_manifest::canonicalize(dir.path()).unwrap();
    let path = root.join("lib/src/Main.ridge");
    std::fs::write(&path, source).unwrap();
    let uri = Url::from_file_path(path).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
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
    (dir, state, uri)
}

fn clean(state: &IncrementalState) {
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

fn position(source: &str, offset: usize) -> Position {
    let before = &source[..offset];
    Position::new(
        u32::try_from(before.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(before.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

fn range(source: &str, offset: usize, name: &str) -> Range {
    Range::new(
        position(source, offset),
        position(source, offset + name.len()),
    )
}

fn byte_offset(source: &str, position: Position) -> usize {
    let start = source
        .split_inclusive('\n')
        .take(position.line as usize)
        .map(str::len)
        .sum::<usize>();
    let line = source[start..].split('\n').next().unwrap();
    let mut utf16 = 0;
    for (byte, ch) in line.char_indices() {
        if utf16 == position.character {
            return start + byte;
        }
        utf16 += u32::try_from(ch.len_utf16()).unwrap();
    }
    assert_eq!(utf16, position.character);
    start + line.len()
}

fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut offsets: Vec<_> = edits
        .iter()
        .map(|edit| {
            let start = byte_offset(source, edit.range.start);
            let end = byte_offset(source, edit.range.end);
            assert_eq!(&source[start..end], "n");
            (start, end, &edit.new_text)
        })
        .collect();
    offsets.sort_unstable_by(|a, b| b.cmp(a));
    let mut renamed = source.to_owned();
    for (start, end, new_text) in offsets {
        renamed.replace_range(start..end, new_text);
    }
    renamed
}

#[test]
fn nested_block_types_match_lexical_bindings() {
    for source in [
        SHADOW.to_owned(),
        SHADOW.replace("let n = \"inner\"", "let inner = \"inner\"")
            .replace("        n\n", "        inner\n"),
        "pub fn value () -> Text =\n    let n = \"outer\"\n    let first =\n        let n = 7\n        n\n    let second: Text =\n        let nested =\n            let n = true\n            n\n        n\n    n\n".to_owned(),
        "pub fn value () -> Int =\n    var n = 1\n    let chosen =\n        var n = \"inner\"\n        n <- \"changed\"\n        n\n    let update =\n        n <- 7\n        ()\n    n\n".to_owned(),
        "pub fn value () -> Int =\n    fn local (x: Int) -> Int = x\n    let chosen =\n        fn local (x: Text) -> Text = x\n        local \"inner\"\n    local 42\n".to_owned(),
        "pub fn value () -> Int =\n    let n = 42\n    let chosen: Result Text Text = try\n        let n = \"inner\"\n        n\n    n\n".to_owned(),
        "pub fn value () -> Int =\n    fn identity x = x\n    let chosen =\n        let text: Text = identity \"inner\"\n        let n: Int = identity 7\n        text\n    identity 42\n".to_owned(),
        "pub fn value () -> Int =\n    let chosen =\n        return 42\n    \"unreachable\"\n".to_owned(),
    ] {
        for ending in ["\n", "\r\n"] {
            let text = source.replace('\n', ending);
            let (_dir, state, _uri) = workspace(&text);
            clean(&state);
        }
    }
}

#[test]
fn block_scope_preserves_outer_constraints_and_rejects_wrong_returns() {
    for source in [
        SHADOW.replace("() -> Int", "() -> Text"),
        "pub fn value (x) -> Text =\n    let chosen =\n        let n: Int = x\n        n\n    x\n".to_owned(),
        "pub fn value () -> Int =\n    var n = 1\n    let chosen =\n        n <- \"wrong\"\n        ()\n    n\n".to_owned(),
        "pub fn value () -> Int =\n    let chosen =\n        return \"wrong\"\n    42\n".to_owned(),
    ] {
        let (_dir, state, _uri) = workspace(&source);
        assert!(
            state.type_errors.iter().any(|(_, e)| matches!(
                e, ridge_typecheck::TypeError::TypeMismatch { .. }
            )),
            "expected a type mismatch for {source}: {:?}", state.type_errors
        );
    }
}

#[test]
fn popped_block_bindings_do_not_hide_required_capabilities() {
    let source = "import std.io as Io\npub fn value () -> Unit =\n    let action = Io.println\n    let chosen =\n        let action = fn x -> ()\n        action \"pure\"\n    action \"effectful\"\n";
    let (_dir, state, _uri) = workspace(source);
    assert!(
        state.type_errors.iter().any(|(_, error)| matches!(
            error,
            ridge_typecheck::TypeError::CapabilityNotDeclared { .. }
                | ridge_typecheck::TypeError::CallerCapabilityInsufficient { .. }
        )),
        "{:?}",
        state.type_errors
    );
    let allowed = source.replace("pub fn value", "pub fn io value");
    let (_dir, state, _uri) = workspace(&allowed);
    clean(&state);
    // Also retain effects of a local alias that expires before the caps pass.
    let nested = "import std.io as Io\npub fn value () -> Unit =\n    let chosen =\n        let action = Io.println\n        action \"effectful\"\n    ()\n";
    let (_dir, state, _uri) = workspace(nested);
    assert!(
        state.type_errors.iter().any(|(_, error)| matches!(
            error,
            ridge_typecheck::TypeError::CapabilityNotDeclared { .. }
                | ridge_typecheck::TypeError::CallerCapabilityInsufficient { .. }
        )),
        "{:?}",
        state.type_errors
    );
    let (_dir, state, _uri) = workspace(&nested.replace("pub fn value", "pub fn io value"));
    clean(&state);
}

#[test]
fn shadowed_hover_navigation_references_and_rename_survive_edits() {
    let (_dir, mut state, uri) = workspace(SHADOW);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    for ending in ["\n", "\r\n", "\n"] {
        let text = SHADOW.replace('\n', ending);
        state.recompile(mid, &text);
        clean(&state);
        let index = WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
        let outer = text.find("(n,").unwrap() + 1;
        let inner = text.find("let n").unwrap() + 4;
        let inner_use = text.find("        n").unwrap() + 8;
        let outer_use = text.rfind('n').unwrap();
        for (decl, usage, ty, replacement) in [
            (outer, outer_use, "Int", "outer"),
            (inner, inner_use, "Text", "inner"),
        ] {
            for at in [decl, usage] {
                let pos = position(&text, at);
                let (card, span) = index.hover_at(&uri, pos.line, pos.character).unwrap();
                assert!(card.contains(&format!("\nn : {ty}\n")), "{card}");
                assert_eq!(
                    index.span_to_range(&uri, span).unwrap(),
                    range(&text, at, "n")
                );
            }
            let pos = position(&text, usage);
            let definition = index.definition_at(&uri, pos.line, pos.character).unwrap();
            assert_eq!(definition.uri, uri);
            assert_eq!(definition.range, range(&text, decl, "n"));
            let refs = index
                .references_at(&uri, pos.line, pos.character, true, &Cancel::new())
                .unwrap();
            let mut actual: Vec<_> = refs
                .iter()
                .map(|loc| {
                    assert_eq!(loc.uri, uri);
                    (loc.range.start.line, loc.range.start.character)
                })
                .collect();
            actual.sort_unstable();
            let mut expected: Vec<_> = [decl, usage]
                .map(|at| {
                    let p = position(&text, at);
                    (p.line, p.character)
                })
                .to_vec();
            expected.sort_unstable();
            assert_eq!(actual, expected);
            let edit = index
                .rename_at(&uri, pos.line, pos.character, replacement, &Cancel::new())
                .unwrap()
                .unwrap();
            let changes = edit.changes.unwrap();
            let edits = changes.get(&uri).unwrap();
            let mut actual: Vec<_> = edits
                .iter()
                .map(|edit| {
                    assert_eq!(edit.new_text, replacement);
                    edit.range.start
                })
                .collect();
            actual.sort_by_key(|p| (p.line, p.character));
            assert_eq!(
                actual,
                expected
                    .iter()
                    .map(|&(l, c)| Position::new(l, c))
                    .collect::<Vec<_>>()
            );
            // Apply the returned ranges, then type-check the renamed program.
            let renamed = apply_edits(&text, edits);
            let (_renamed_dir, renamed_state, _renamed_uri) = workspace(&renamed);
            clean(&renamed_state);
        }
    }
}
