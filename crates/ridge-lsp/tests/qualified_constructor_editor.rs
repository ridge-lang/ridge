//! A constructor written through its module alias answers in the editor.
//!
//! `Actor.Timeout` used to be a name the compiler had no path for, and in a
//! pattern the spelling did not parse at all, so there was no node to hover.
//! Now that both positions resolve, the qualified spelling has to answer with
//! whatever the bare spelling answers *in the same position* — the editor is
//! where a reader finds out that the two spellings mean one thing.
//!
//! Every assertion pairs the two spellings rather than pinning a string, and
//! two of the things they are paired against are known to be wrong for both:
//! hovering a constructor in a *pattern* reports the match arm's result type
//! rather than the constructor's, and go-to-definition on a standard-library
//! constructor lands nowhere because its declaration has no workspace URI.
//! Both are older than this change and filed separately. Pinning a literal
//! here would either bake one in or fail for a reason that has nothing to do
//! with the spelling — so the go-to-definition test uses a workspace module,
//! where a definition genuinely exists and the assertion can fail.

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
use ridge_lsp::cancel::Cancel;
use ridge_lsp::index::WorkspaceIndex;
use tower_lsp::lsp_types::Url;

const LIB: &str = "pub type Colour = Red | Green | Blue Int
";

const SRC: &str = "import std.actor as Actor
import std.actor (Timeout)
import proj.Lib as L
import proj.Lib (Colour, Red)

pub fn qualified () -> AskError = Actor.Timeout

pub fn bare () -> AskError = Timeout

pub fn matchedQualified (e: AskError) -> Text =
    match e
        Actor.Timeout -> \"timed out\"
        _ -> \"other\"

pub fn matchedBare (e: AskError) -> Text =
    match e
        Timeout -> \"timed out\"
        _ -> \"other\"

pub fn colourQualified () -> Colour = L.Red

pub fn colourBare () -> Colour = Red
";

/// Whole-word occurrences of `Timeout` in `SRC`, in source order.
const IMPORT_ITEM: usize = 0;
const EXPR_QUALIFIED: usize = 1;
const EXPR_BARE: usize = 2;
const PATTERN_QUALIFIED: usize = 3;
const PATTERN_BARE: usize = 4;

/// Whole-word occurrences of `Red`: 0 is the import item, 1 is `L.Red`, 2 is
/// the bare use.
const RED_QUALIFIED: usize = 1;
const RED_BARE: usize = 2;

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

fn build_index() -> (TempDir, WorkspaceIndex, Url) {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"qc\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/proj/src/Lib.ridge", LIB);
    write_file(td.path(), "libs/proj/src/Main.ridge", SRC);
    let root = ridge_manifest::canonicalize(td.path()).expect("canonicalize temp root");
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Main.ridge")).unwrap();
    (td, index, uri)
}

/// The zero-based line and one-based column of the `nth` whole-word occurrence
/// of `name` in `SRC`.
fn pos_nth(name: &str, nth: usize) -> (u32, u32) {
    let at = SRC
        .match_indices(name)
        .filter(|(i, _)| {
            let before_ok = *i == 0 || !SRC.as_bytes()[i - 1].is_ascii_alphanumeric();
            let after = i + name.len();
            let after_ok = after >= SRC.len() || !SRC.as_bytes()[after].is_ascii_alphanumeric();
            before_ok && after_ok
        })
        .map(|(i, _)| i)
        .nth(nth)
        .unwrap_or_else(|| panic!("no occurrence {nth} of `{name}`"));
    let before = &SRC[..at];
    let line = before.matches('\n').count() as u32;
    let col = (at - before.rfind('\n').map_or(0, |p| p + 1)) as u32 + 1;
    (line, col)
}

/// Hover the `nth` whole-word occurrence of `name`.
fn hover_nth(index: &WorkspaceIndex, uri: &Url, name: &str, nth: usize) -> Option<String> {
    let (line, col) = pos_nth(name, nth);
    index.hover_at(uri, line, col).map(|(md, _)| md)
}

#[test]
fn a_qualified_constructor_cards_like_the_bare_one_in_an_expression() {
    let (_td, index, uri) = build_index();
    let qualified =
        hover_nth(&index, &uri, "Timeout", EXPR_QUALIFIED).expect("`Actor.Timeout` should card");
    let bare = hover_nth(&index, &uri, "Timeout", EXPR_BARE).expect("bare `Timeout` should card");

    assert_eq!(
        qualified, bare,
        "the two spellings name one constructor, so the editor has to say one thing about them"
    );
    assert!(
        qualified.contains("AskError"),
        "and the thing it says is the constructor's own type, got: {qualified}"
    );
}

#[test]
fn a_qualified_constructor_cards_like_the_bare_one_in_a_pattern() {
    let (_td, index, uri) = build_index();
    // Before this, the qualified spelling had no node here at all: it stopped
    // at a parse error on the dot, so hover had nothing to answer from.
    let qualified = hover_nth(&index, &uri, "Timeout", PATTERN_QUALIFIED)
        .expect("`Actor.Timeout` in a match arm should card");
    let bare = hover_nth(&index, &uri, "Timeout", PATTERN_BARE)
        .expect("bare `Timeout` in a match arm should card");

    assert_eq!(
        qualified, bare,
        "matching a constructor through an alias and matching it bare are one thing"
    );
}

#[test]
fn the_alias_segment_resolves_as_part_of_the_name() {
    let (_td, index, uri) = build_index();
    // The binding is stamped over the whole dotted name as well as over the
    // constructor segment, so a cursor anywhere in `Actor.Timeout` resolves.
    // Occurrence 1 of `Actor` is the one in the expression; occurrence 0 is the
    // alias in its own import line, a different node and not this change's
    // business.
    let on_alias = hover_nth(&index, &uri, "Actor", 1);
    assert!(
        on_alias.is_some_and(|card| card.contains("AskError")),
        "a cursor on the alias half of `Actor.Timeout` must resolve the name it is part of"
    );
}

#[test]
fn the_import_item_is_a_binding_site_and_stays_quiet() {
    let (_td, index, uri) = build_index();
    // The guard against a stamp that leaked over every occurrence of the name:
    // the item list in `import std.actor (Timeout)` is where the name is bound,
    // not used, and it answered nothing before this change either.
    assert!(
        hover_nth(&index, &uri, "Timeout", IMPORT_ITEM).is_none(),
        "the import item is a binding site, not a reference"
    );
}

#[test]
fn go_to_definition_lands_from_the_qualified_spelling_too() {
    let (_td, index, uri) = build_index();
    // A workspace module, so the constructor has a declaration with a URI and
    // the assertion can fail. The bare spelling is the control: if it stops
    // landing, the qualified one agreeing with it means nothing.
    let (bl, bc) = pos_nth("Red", RED_BARE);
    let bare = index.definition_at(&uri, bl, bc);
    assert!(
        bare.is_some(),
        "the control: go-to-definition on the bare `Red` must land"
    );

    let (ql, qc) = pos_nth("Red", RED_QUALIFIED);
    let qualified = index.definition_at(&uri, ql, qc);
    assert_eq!(
        qualified.map(|l| l.range),
        bare.map(|l| l.range),
        "`L.Red` and `Red` are one constructor and must lead to one declaration"
    );
}

#[test]
fn find_references_from_the_bare_spelling_turns_up_the_qualified_uses() {
    let (_td, index, uri) = build_index();
    // The two spellings are one constructor, so a rename driven off references
    // has to move both. Asking from the bare use must reach past it.
    let (line, col) = pos_nth("Timeout", EXPR_BARE);
    let found = index
        .references_at(&uri, line, col, true, &Cancel::default())
        .unwrap_or_default();
    assert!(
        found.len() >= 2,
        "expected the bare use and at least one qualified one; got {} location(s)",
        found.len()
    );
}
