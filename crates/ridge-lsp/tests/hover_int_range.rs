//! Hovering integer arithmetic says what happens at the boundary.
//!
//! `Int` is 64-bit and arithmetic past the ends raises, so the two things a
//! reader wants from an editor are the range itself and the fact that `+` is
//! partial. Neither is visible from the source of a call.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

const SRC: &str = "import std.int as Int\n\npub fn bump (n: Int) -> Int = Int.add n 1\n";

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

fn build_ws() -> TempDir {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"hov\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/proj/src/Main.ridge", SRC);
    td
}

fn card_at(anchor: &str, skip: usize) -> Option<String> {
    let td = build_ws();
    let root = fs::canonicalize(td.path()).expect("canonicalize temp root");
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Main.ridge")).unwrap();
    let at = SRC.find(anchor).unwrap_or_else(|| panic!("no `{anchor}`")) + skip;
    let before = &SRC[..at];
    let line = before.matches('\n').count() as u32;
    let col = (at - before.rfind('\n').map_or(0, |p| p + 1)) as u32 + 1;
    index.hover_at(&uri, line, col).map(|(md, _)| md)
}

#[test]
fn hovering_integer_arithmetic_says_it_can_raise() {
    // The card is lifted from the `--` block above the declaration, so this is
    // really asserting that the declaration says it. Worth a test anyway: the
    // fact that `+` is partial is invisible at a call site, and an editor is
    // where a reader would find out.
    let card = card_at("Int.add n 1", 5).expect("Int.add should card");
    assert!(
        card.contains("pub fn add (a: Int) (b: Int) -> Int"),
        "card should carry the signature, got: {card}"
    );
    assert!(
        card.contains("Raises") && card.contains("range of `Int`"),
        "card should say what happens at the boundary, got: {card}"
    );
}

#[test]
fn hovering_the_opt_out_says_what_it_answers_instead() {
    // `wrappingAdd` only makes sense once the default raises, so its card is
    // the other half of the same explanation.
    let card = card_at("Int.add n 1", 5);
    assert!(card.is_some(), "sanity: the harness resolves a stdlib card");

    let td = build_ws();
    let root = fs::canonicalize(td.path()).expect("canonicalize temp root");
    write_file(
        &root,
        "libs/proj/src/Wrap.ridge",
        "import std.int as Int

pub fn w (a: Int) -> Int = Int.wrappingAdd a 1
",
    );
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Wrap.ridge")).unwrap();
    let card = index
        .hover_at(&uri, 2, 32)
        .map(|(md, _)| md)
        .expect("Int.wrappingAdd should card");
    assert!(
        card.contains("wrappingAdd"),
        "expected the wrappingAdd card, got: {card}"
    );
    assert!(
        card.contains("opt-out") || card.contains("wrap"),
        "card should explain what it answers instead of raising, got: {card}"
    );
}
