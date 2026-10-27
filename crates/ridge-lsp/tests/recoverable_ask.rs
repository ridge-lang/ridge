//! Recoverable asks have a typed hover and a propagation repair that compiles.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{check_workspace_incremental, CheckOptions};
use ridge_lsp::{ask_migration::collect_ask_fixes, index::WorkspaceIndex};

const PREFIX: &str = "import std.actor (AskError)\nactor Worker =\n    on value () -> Int = 7\n\n";

fn workspace(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname = \"asks\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/ridge.toml"), "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n[capabilities]\nallow = [\"time\", \"spawn\"]\n").unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), source).unwrap();
    dir
}

#[test]
fn propagation_repairs_annotations_calls_and_converted_errors() {
    let source = format!("{PREFIX}type LocalError = | Failed AskError\ninstance ErrorFrom AskError LocalError =\n    errorFrom (e: AskError) -> LocalError = Failed e\n\nfn plus (n: Int) -> Int = n + 1\n\npub fn time read (h: Handle Worker) -> Result Int AskError =\n    let n: Int = h ?> value timeout never\n    Ok (n + 1)\n\npub fn time converted (h: Handle Worker) -> Result Int LocalError =\n    let n = plus (h ?> value timeout 1000)\n    Ok n\n");
    let source = source
        + "\npub fn time discarded (h: Handle Worker) -> Result Unit AskError =\n    h ?> value\n    ((h ?> value))\n    Ok ()\n";
    let dir = workspace(&source);
    let mut state = check_workspace_incremental(
        CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
    )
    .unwrap();
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
    assert!(!state.type_errors.is_empty());
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let fixes = collect_ask_fixes(&index, &state.typed, &state.resolved, &state.type_errors);
    assert_eq!(
        fixes.len(),
        4,
        "errors: {:?}; fixes: {fixes:?}",
        state.type_errors
    );
    assert!(fixes.iter().any(|fix| fix.code == "T022"));
    let li = ridge_lexer::LineIndex::new(&source);
    let mut edits: Vec<_> = fixes
        .iter()
        .flat_map(|fix| {
            std::iter::once((fix.edit_range, fix.new_text.as_str())).chain(
                fix.extra_edits
                    .iter()
                    .map(|(range, text)| (*range, text.as_str())),
            )
        })
        .map(|(range, text)| {
            (
                li.utf16_to_byte(range.start.line, range.start.character) as usize,
                text,
            )
        })
        .collect();
    edits.sort_unstable_by(|a, b| b.0.cmp(&a.0));
    let mut repaired = source;
    for (byte, text) in edits {
        repaired.insert_str(byte, text);
    }
    assert!(repaired.contains("timeout never ?"));
    assert!(repaired.contains("timeout 1000 ?)"));
    assert!(repaired.contains("let _ = h ?> value ?"));
    assert!(repaired.contains("let _ = ((h ?> value ?))"));
    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    state.recompile(module, &repaired);
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
    let index = WorkspaceIndex::build(1, &state.typed, &state.resolved, &state.source_cache());
    assert!(
        collect_ask_fixes(&index, &state.typed, &state.resolved, &state.type_errors).is_empty()
    );
    let uri =
        tower_lsp::lsp_types::Url::from_file_path(dir.path().join("app/src/Main.ridge")).unwrap();
    let offset = repaired.find("h ?> value").unwrap() + 2;
    let (line, col) =
        ridge_lexer::LineIndex::new(&repaired).byte_to_utf16(u32::try_from(offset).unwrap());
    let (hover, _) = index.hover_at(&uri, line, col).expect("ask has a hover");
    assert!(hover.contains("Result Int AskError"), "{hover}");
}

#[test]
fn propagation_is_not_offered_across_a_callback_boundary_or_without_a_conversion() {
    let source = format!("{PREFIX}fn invoke (f: fn Unit -> Int) -> Int = f ()\n\npub fn time plain (h: Handle Worker) -> Int = h ?> value\n\npub fn time wrong (h: Handle Worker) -> Result Int Text =\n    let n: Int = h ?> value\n    Ok n\n\npub fn time callback (h: Handle Worker) -> Result Int AskError =\n    let n = invoke (fn _ -> h ?> value)\n    Ok n\n");
    let dir = workspace(&source);
    let state = check_workspace_incremental(
        CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
    )
    .unwrap();
    assert!(
        state.resolved.parse_errors.is_empty(),
        "{:?}",
        state.resolved.parse_errors
    );
    assert!(!state.type_errors.is_empty());
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let fixes = collect_ask_fixes(&index, &state.typed, &state.resolved, &state.type_errors);
    assert!(fixes.is_empty(), "{fixes:?}");
}

#[test]
fn an_already_consumed_result_is_not_rewritten_for_an_outer_discard() {
    let source = format!("{PREFIX}fn consume (reply: Result Int AskError) -> Int =\n    match reply\n        Ok n -> n\n        Err _ -> 0\n\npub fn time used (h: Handle Worker) -> Result Unit AskError =\n    consume (h ?> value)\n    Ok ()\n");
    let dir = workspace(&source);
    let state = check_workspace_incremental(
        CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
    )
    .unwrap();
    assert!(state.resolved.parse_errors.is_empty());
    assert!(state.resolved.errors.is_empty());
    assert!(!state.type_errors.is_empty());
    assert!(state
        .type_errors
        .iter()
        .all(|(_, error)| error.code() == "T022"));
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    assert!(
        collect_ask_fixes(&index, &state.typed, &state.resolved, &state.type_errors).is_empty()
    );
}

#[test]
fn propagation_repairs_nested_result_boundaries_with_utf16_positions() {
    let source = format!("{PREFIX}fn time invoke (f: fn time Unit -> Result Int AskError) -> Result Int AskError = f ()\n\npub fn time callback (h: Handle Worker) -> Result Int AskError =\n    invoke (fn _ ->\n        -- café 🧭\n        let n: Int = h ?> value\n        Ok n)\n\npub fn time scoped (h: Handle Worker) -> Result Int AskError =\n    try\n        let n: Int = h ?> value timeout never\n        n\n").replace('\n', "\r\n");
    let dir = workspace(&source);
    let mut state = check_workspace_incremental(
        CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
    )
    .unwrap();
    assert!(
        state.resolved.parse_errors.is_empty(),
        "{:?}",
        state.resolved.parse_errors
    );
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let fixes = collect_ask_fixes(&index, &state.typed, &state.resolved, &state.type_errors);
    assert_eq!(
        fixes.len(),
        2,
        "errors: {:?}; fixes: {fixes:?}",
        state.type_errors
    );
    let li = ridge_lexer::LineIndex::new(&source);
    let mut offsets: Vec<_> = fixes
        .iter()
        .map(|f| li.utf16_to_byte(f.edit_range.start.line, f.edit_range.start.character) as usize)
        .collect();
    offsets.sort_unstable_by(|a, b| b.cmp(a));
    let mut repaired = source;
    for offset in offsets {
        repaired.insert_str(offset, " ?");
    }
    let module = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.file_path.ends_with("Main.ridge"))
        .unwrap()
        .id;
    state.recompile(module, &repaired);
    assert!(
        state.resolved.parse_errors.is_empty(),
        "{:?}",
        state.resolved.parse_errors
    );
    assert!(state.type_errors.is_empty(), "{:?}", state.type_errors);
    assert!(state.lower_errors.is_empty(), "{:?}", state.lower_errors);
}

#[test]
fn handler_annotation_alone_does_not_offer_a_function_propagation_edit() {
    let source = format!("{PREFIX}actor Forwarder =\n    on time read (h: Handle Worker) -> Result Int AskError =\n        let n: Int = h ?> value\n        Ok n\n");
    let dir = workspace(&source);
    let state = check_workspace_incremental(
        CheckOptions::new(dir.path().to_path_buf()).with_retain_indices(true),
    )
    .unwrap();
    assert!(state
        .type_errors
        .iter()
        .any(|(_, error)| error.code() == "T001"));
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    assert!(
        collect_ask_fixes(&index, &state.typed, &state.resolved, &state.type_errors).is_empty()
    );
}
