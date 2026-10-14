//! JSON construction has one surface: the prelude constructors.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]

use ridge_driver::{
    check_workspace_incremental, collect_diagnostics, CheckOptions, IncrementalState,
};
use ridge_lsp::diagnostics::to_lsp_diagnostic;
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

const SHIMS: &[&str] = &[
    "jNull", "jBool", "jInt", "jFloat", "jText", "jList", "jObject",
];
const SOURCE: &str = "import std.json as Json\n\
pub fn encoded () -> Text = Json.encode (JInt 3)\n\
pub fn inspected () -> Option Int = Json.asInt (JInt 3)\n\
pub fn nullValue () -> JsonValue = JNull\n\
pub fn boolValue () -> JsonValue = JBool true\n\
pub fn floatValue () -> JsonValue = JFloat 1.25\n\
pub fn textValue () -> JsonValue = JText \"x\"\n\
pub fn listValue () -> JsonValue = JList [JInt 3]\n\
pub fn objectValue () -> JsonValue = JObject Map.empty\n";

fn workspace(source: &str) -> (tempfile::TempDir, IncrementalState, Url) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"json-surface\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
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

fn position(source: &str, marker: &str, offset: usize) -> (u32, u32) {
    let at = source.find(marker).unwrap() + offset;
    (
        source[..at].matches('\n').count() as u32,
        (at - source[..at].rfind('\n').map_or(0, |p| p + 1)) as u32,
    )
}

#[test]
fn module_completion_keeps_accessors_and_removes_shims() {
    let (_dir, state, uri) = workspace(SOURCE);
    assert!(
        state.resolved.errors.is_empty(),
        "{:?}",
        state.resolved.errors
    );
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let (line, col) = position(SOURCE, "Json.encode", 5);
    let items = index.completions_at(&uri, line, col);
    assert!(items.iter().all(|i| !SHIMS.contains(&i.label.as_str())));
    for name in [
        "encode", "decode", "asInt", "asFloat", "asBool", "asText", "asList", "asObject", "isNull",
    ] {
        let item = items
            .iter()
            .find(|i| i.label == name)
            .expect("JSON API remains available");
        let (card, _) = index
            .resolve_completion(item.data.as_ref().expect("resolve payload"))
            .unwrap();
        assert!(card.contains(name), "{card}");
        assert!(!card.contains("Cross-module pattern matching"), "{card}");
    }
    let (line, col) = position(SOURCE, "Json.asInt", 6);
    let (card, _) = index.hover_at(&uri, line, col).unwrap();
    assert!(card.contains("asInt") && card.contains("Option"), "{card}");
}

#[test]
fn prelude_constructors_still_hover_and_complete() {
    let (_dir, state, uri) = workspace(SOURCE);
    assert!(state.resolved.errors.is_empty() && state.type_errors.is_empty());
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    for name in [
        "JNull", "JBool", "JInt", "JFloat", "JText", "JList", "JObject",
    ] {
        let (line, col) = position(SOURCE, name, 1);
        let (card, _) = index
            .hover_at(&uri, line, col)
            .expect("prelude constructor hover");
        assert!(card.contains(name) && card.contains("JsonValue"), "{card}");
        let items = index.completions_at(&uri, line, col);
        let item = items
            .iter()
            .find(|i| i.label == name)
            .expect("prelude constructor completion");
        let (card, doc) = index
            .resolve_completion(item.data.as_ref().unwrap())
            .unwrap();
        assert!(card.contains(name), "{card}");
        assert!(doc.unwrap().contains("constructor of `JsonValue`"));
    }
}

#[test]
fn removed_names_are_rejected_as_members_and_import_items() {
    for name in SHIMS {
        for source in [
            format!("import std.json as Json\npub fn removed () = Json.{name}\n"),
            format!("import std.json ({name})\npub fn valid () -> JsonValue = JNull\n"),
        ] {
            let (_dir, state, uri) = workspace(&source);
            let diagnostics = collect_diagnostics(
                &state.disc_resolve_errors,
                &state.resolved,
                &state.type_errors,
                &state.lower_errors,
                &state.source_cache(),
                &state.typed.tycons,
            );
            assert!(
                diagnostics.iter().any(|d| d.primary_message.contains(name)),
                "{diagnostics:?}"
            );
            let diagnostic = diagnostics
                .iter()
                .find(|d| d.primary_message.contains(name))
                .unwrap();
            let constructor = format!("J{}", &name[1..]);
            assert_eq!(diagnostic.notes.len(), 1);
            let note = &diagnostic.notes[0].message;
            assert!(
                note.contains(&format!("prelude constructor `{constructor}`")),
                "{note}"
            );
            assert!(!note.contains("did you mean") && !note.contains("already in scope"));
            assert_eq!(note.contains("remove the `()` argument"), *name == "jNull");
            let editor = to_lsp_diagnostic(diagnostic, &uri, Some(&source));
            assert_eq!(editor.related_information.unwrap()[0].message, *note);
            let index =
                WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
            let (line, col) = position(&source, name, 1);
            assert!(index.definition_at(&uri, line, col).is_none());
        }
    }
}

#[test]
fn migration_repairs_all_constructor_calls() {
    for (old, new) in [
        ("jNull ()", "JNull"),
        ("jBool true", "JBool true"),
        ("jInt 3", "JInt 3"),
        ("jFloat 1.25", "JFloat 1.25"),
        ("jText \"x\"", "JText \"x\""),
        ("jList [JInt 3]", "JList [JInt 3]"),
        ("jObject Map.empty", "JObject Map.empty"),
    ] {
        let source = format!("import std.json as Json\npub fn f () -> JsonValue = Json.{old}\n");
        let (_dir, mut state, _) = workspace(&source);
        assert!(!state.resolved.errors.is_empty());
        let mid = state
            .resolved
            .graph
            .modules
            .iter()
            .find(|m| m.file_path.ends_with("Main.ridge"))
            .unwrap()
            .id;
        state.recompile(mid, &source.replace(&format!("Json.{old}"), new));
        let diagnostics = collect_diagnostics(
            &state.disc_resolve_errors,
            &state.resolved,
            &state.type_errors,
            &state.lower_errors,
            &state.source_cache(),
            &state.typed.tycons,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }
}

#[test]
fn migration_distinguishes_shadowing_and_genuine_typos() {
    for source in [
        "import std.json as Json\nimport std.text as JInt\npub fn f () = Json.jInt 3\n",
        "import std.json as Json\ntype Local = JInt Int\npub fn f () = Json.jInt 3\n",
    ] {
        let (_dir, state, _) = workspace(source);
        let diagnostics = collect_diagnostics(
            &state.disc_resolve_errors,
            &state.resolved,
            &state.type_errors,
            &state.lower_errors,
            &state.source_cache(),
            &state.typed.tycons,
        );
        let diag = diagnostics
            .iter()
            .find(|d| d.primary_message.contains("jInt"))
            .unwrap();
        let note = &diag.notes[0].message;
        assert!(
            note.contains("rename any binding that shadows `JInt` first"),
            "{note}"
        );
        assert!(!note.contains("already in scope") && !note.contains("asInt"));
    }
    for (module, name, migration, guess) in [
        ("std.json", "asIn", false, Some("asInt")),
        ("std.text", "jInt", false, None),
    ] {
        let source = format!("import {module} as M\npub fn f () = M.{name}\n");
        let (_dir, state, _) = workspace(&source);
        let diagnostics = collect_diagnostics(
            &state.disc_resolve_errors,
            &state.resolved,
            &state.type_errors,
            &state.lower_errors,
            &state.source_cache(),
            &state.typed.tycons,
        );
        let diag = diagnostics
            .iter()
            .find(|d| d.primary_message.contains(name))
            .unwrap();
        assert_eq!(
            diag.notes.iter().any(|n| n.message.contains("was removed")),
            migration
        );
        if let Some(guess) = guess {
            assert!(
                diag.notes.iter().any(|n| n.message.contains(guess)),
                "{diag:?}"
            );
        }
    }
}
