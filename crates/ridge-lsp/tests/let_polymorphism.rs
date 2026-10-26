//! Generic declarations and independent uses stay consistent after edits.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::{Position, Range, Url};

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    workspace_with_library(source, None)
}

fn workspace_with_library(
    source: &str,
    library: Option<&str>,
) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"let-editor\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
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
    if let Some(library) = library {
        std::fs::write(root.join("lib/src/Lib.ridge"), library).unwrap();
    }
    let uri = Url::from_file_path(path).unwrap();
    let state =
        check_workspace_incremental(CheckOptions::new(root).with_retain_indices(true)).unwrap();
    (dir, state, uri)
}

fn clean(state: &IncrementalState) {
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

fn position(source: &str, offset: usize) -> Position {
    let before = &source[..offset];
    Position::new(
        u32::try_from(before.bytes().filter(|b| *b == b'\n').count()).unwrap(),
        u32::try_from(before.rsplit('\n').next().unwrap().encode_utf16().count()).unwrap(),
    )
}

#[test]
fn inferred_record_types_and_contradictions_follow_edits() {
    let source = "fn read record = record.value\npub fn value () -> Int =\n    let text = read ({ value = \"ok\" })\n    let number = read ({ value = 7 })\n    number\n";
    for ending in ["\n", "\r\n"] {
        let source = source.replace('\n', ending);
        let (_dir, mut state, uri) = workspace(&source);
        clean(&state);
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        for _ in 0..2 {
            let index =
                WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
            for (needle, expected) in [("text =", "text : Text"), ("number =", "number : Int")] {
                let offset = source.find(needle).unwrap();
                let pos = position(&source, offset);
                let (card, _) = index.hover_at(&uri, pos.line, pos.character).unwrap();
                assert!(card.contains(expected), "{card}");
            }
            let invalid = source.replace("value = 7", "other = 7");
            state.recompile(mid, &invalid);
            assert!(
                state.type_errors.iter().any(|(_, error)| matches!(error,
                ridge_typecheck::TypeError::UnknownField { field, .. } if field == "value")),
                "{:?}",
                state.type_errors
            );
            state.recompile(mid, &source);
            clean(&state);
            let invalid = "pub fn value record -> Int =\n    let text: Text = record.value\n    let number: Int = record.value\n    number\n".replace('\n', ending);
            state.recompile(mid, &invalid);
            assert!(
                state.type_errors.iter().any(|(_, error)| matches!(
                    error,
                    ridge_typecheck::TypeError::TypeMismatch { .. }
                )),
                "{:?}",
                state.type_errors
            );
            state.recompile(mid, &source);
            clean(&state);
        }
    }
}

#[test]
fn imported_field_contract_edits_recheck_consumers() {
    let main = "import lib.Lib (read)\npub fn value () -> Int = read ({ value = 7 })\n";
    let library = "pub fn read record -> Int = record.value\n";
    for ending in ["\n", "\r\n"] {
        let library = library.replace('\n', ending);
        let (_dir, mut state, _uri) =
            workspace_with_library(&main.replace('\n', ending), Some(&library));
        clean(&state);
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|module| module.file_path.ends_with("Lib.ridge"))
            .unwrap()
            .id;
        for _ in 0..2 {
            state.recompile(mid, &library.replace("record.value", "record.other"));
            assert!(
                state.type_errors.iter().any(|(_, error)| matches!(error,
                ridge_typecheck::TypeError::UnknownField { field, .. } if field == "other")),
                "{:?}",
                state.type_errors
            );
            state.recompile(mid, &library);
            clean(&state);
        }
    }
}

#[test]
fn constrained_alias_types_and_missing_instances_follow_edits() {
    let initial = "-- café 🚀\nimport std.list as List\ntype Item = | MkItem Int\npub fn value () -> List Int =\n    let sort = List.sort\n    sort [3, 1, 2]\n";
    for ending in ["\n", "\r\n"] {
        let initial = initial.replace('\n', ending);
        let (_dir, mut state, uri) = workspace(&initial);
        clean(&state);
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        let text = initial
            .replace("-> List Int", "-> List Text")
            .replace("[3, 1, 2]", "[\"b\", \"a\"]");
        for (source, ty) in [(&initial, "Int"), (&text, "Text"), (&initial, "Int")] {
            state.recompile(mid, source);
            clean(&state);
            let index =
                WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
            let declaration = source.find("sort =").unwrap();
            let expected_range = Range::new(
                position(source, declaration),
                position(source, declaration + 4),
            );
            for offset in [declaration, source.rfind("sort [").unwrap()] {
                let pos = position(source, offset);
                let (card, _) = index.hover_at(&uri, pos.line, pos.character).unwrap();
                assert!(
                    card.contains(&format!("\nsort : (List {ty}) -> List {ty}\n")),
                    "{card}"
                );
                let target = index.definition_at(&uri, pos.line, pos.character).unwrap();
                assert_eq!(target.uri, uri);
                assert_eq!(target.range, expected_range);
            }
        }
        let invalid = initial
            .replace("-> List Int", "-> List Item")
            .replace("[3, 1, 2]", "[MkItem 1]");
        state.recompile(mid, &invalid);
        assert!(
            state
                .type_errors
                .iter()
                .any(|(_, error)| matches!(error, ridge_typecheck::TypeError::NoInstance { .. })),
            "{:?}",
            state.type_errors
        );
        state.recompile(mid, &initial);
        clean(&state);
    }
}

#[test]
fn let_declarations_and_specialized_uses_follow_incremental_changes() {
    let initial = "pub fn value () -> (Text, Int) =\n    let identity = fn x -> x\n    let text: Text = identity \"hello\"\n    let number: Int = identity 7\n    (text, number)\n";
    let (_dir, mut state, uri) = workspace(initial);
    clean(&state);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    for (ty, source) in [
        ("Text", initial.to_owned()),
        (
            "Bool",
            initial.replace("Text", "Bool").replace("\"hello\"", "true"),
        ),
        ("Text", initial.to_owned()),
    ] {
        for ending in ["\n", "\r\n"] {
            state.recompile(mid, &source.replace('\n', ending));
            clean(&state);
            let index =
                WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
            let declaration_offset = source.find("identity").unwrap();
            let declaration = Range::new(
                position(&source, declaration_offset),
                position(&source, declaration_offset + 8),
            );
            for (offset, expected) in [
                (declaration_offset, "(a) -> a".to_owned()),
                (
                    source.find("= identity").unwrap() + 2,
                    format!("({ty}) -> {ty}"),
                ),
                (source.rfind("identity").unwrap(), "(Int) -> Int".to_owned()),
            ] {
                let pos = position(&source, offset);
                let (card, span) = index.hover_at(&uri, pos.line, pos.character).unwrap();
                assert!(
                    card.contains(&format!("\nidentity : {expected}\n")),
                    "{card}"
                );
                assert!(card.contains("*(local)*"), "{card}");
                assert_eq!(
                    index.span_to_range(&uri, span).unwrap(),
                    Range::new(pos, position(&source, offset + 8))
                );
                let target = index.definition_at(&uri, pos.line, pos.character).unwrap();
                assert_eq!(target.uri, uri);
                assert_eq!(target.range, declaration);
            }
        }
    }
}

#[test]
fn writes_and_recovery_remain_checked_after_edits() {
    let valid = "pub fn value () -> Int =\n    let identity = fn x -> x\n    let first: Text = identity \"hello\"\n    identity 7\n";
    let (_dir, mut state, _uri) = workspace(valid);
    clean(&state);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    for (invalid, immutable_write) in [
        ("pub fn value () -> Text =\n    let identity = fn x -> x\n    let call = fn x -> identity x\n    identity <- fn _ -> 7\n    call \"hello\"\n", true),
        ("pub fn value () -> Int =\n    let read = fn record -> record.value\n    let first: Int = read ({ value = 1 })\n    read 7\n", false),
    ] {
        for ending in ["\n", "\r\n"] {
            state.recompile(mid, &invalid.replace('\n', ending));
            if immutable_write {
                assert!(state.resolved.errors.iter().any(|(_, error)| matches!(error, ridge_resolve::ResolveError::ImmutableAssignment { .. })), "{:?}", state.resolved.errors);
            } else {
                assert!(state.type_errors.iter().any(|(_, error)| matches!(error, ridge_typecheck::TypeError::FieldAccessOnNonRecord { .. })), "{:?}", state.type_errors);
            }
            state.recompile(mid, &valid.replace('\n', ending));
            clean(&state);
        }
    }
}

#[test]
fn let_aliases_keep_capability_errors_after_rechecks() {
    let source = "import std.io as Io\npub fn value () -> Unit =\n    let print = Io.println\n    print \"hello\"\n";
    let (_dir, mut state, _uri) = workspace(source);
    let mid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    for ending in ["\n", "\r\n"] {
        state.recompile(mid, &source.replace('\n', ending));
        assert!(
            state.type_errors.iter().any(|(_, e)| matches!(
                e,
                ridge_typecheck::TypeError::CapabilityNotDeclared { .. }
                    | ridge_typecheck::TypeError::CallerCapabilityInsufficient { .. }
            )),
            "{:?}",
            state.type_errors
        );
        state.recompile(
            mid,
            &source
                .replace("pub fn value", "pub fn io value")
                .replace('\n', ending),
        );
        clean(&state);
    }
}
