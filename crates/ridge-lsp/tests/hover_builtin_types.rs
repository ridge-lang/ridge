//! Hovering a built-in type name says what the type is.
//!
//! A built-in has no declaration to lift a card from, so hover used to return
//! nothing for `Int`, `Text`, `List` and the rest — and for the handful that a
//! stdlib module happens to export it returned the bare word under a "stdlib"
//! label, which is worse: it looks like the editor answered.
//!
//! The two shapes need different nodes and both are covered here. The eleven
//! spellings the parser turns into a primitive type carry no name node at all,
//! so their card has to come off the type node; everything else has an ident
//! node whose span is the name, sitting inside a type node that spans the whole
//! application.

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

const SRC: &str = "pub type Book = { id: Int, title: Text }

pub fn find (key: Text) (rows: Map Text Book) -> Result (Option Book) Error =
    Ok None
";

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

fn build_ws(src: &str) -> TempDir {
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
    write_file(td.path(), "libs/proj/src/Main.ridge", src);
    td
}

/// Hover one column inside the `nth` occurrence of `name` in `src`, matching how
/// the other hover tests aim: on the first column the cursor is still on the
/// boundary before the word.
fn hover(src: &str, name: &str, nth: usize) -> Option<String> {
    let td = build_ws(src);
    let root = fs::canonicalize(td.path()).expect("canonicalize temp root");
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Main.ridge")).unwrap();

    let at = src
        .match_indices(name)
        .filter(|(i, _)| {
            let before_ok = *i == 0 || !src.as_bytes()[i - 1].is_ascii_alphanumeric();
            let after = i + name.len();
            let after_ok = after >= src.len() || !src.as_bytes()[after].is_ascii_alphanumeric();
            before_ok && after_ok
        })
        .map(|(i, _)| i)
        .nth(nth)
        .unwrap_or_else(|| panic!("no occurrence {nth} of `{name}`"));
    let before = &src[..at];
    let line = before.matches('\n').count() as u32;
    let col = (at - before.rfind('\n').map_or(0, |p| p + 1)) as u32 + 1;
    index.hover_at(&uri, line, col).map(|(md, _)| md)
}

#[test]
fn a_primitive_cards_even_though_it_has_no_name_node() {
    // `Int` inside the record declaration: the parser produced a primitive type
    // here, so there is no ident node to hang the lookup on.
    let card = hover(SRC, "Int", 0).expect("Int should card");
    assert!(
        card.contains("built-in type"),
        "expected the built-in kind line, got: {card}"
    );
    assert!(
        card.contains("64-bit signed integer"),
        "expected the summary, got: {card}"
    );
    assert!(
        card.contains("9223372036854775807"),
        "the Int card is where the range belongs, got: {card}"
    );
}

#[test]
fn a_parameterised_builtin_names_its_parameters() {
    // The point of the card for these: `Result a e` is what the reader can
    // already see, and it is not an answer to the question they hovered to ask.
    let result = hover(SRC, "Result", 0).expect("Result should card");
    assert!(
        result.contains("Result value error"),
        "expected named parameters, got: {result}"
    );

    let map = hover(SRC, "Map", 0).expect("Map should card");
    assert!(
        map.contains("Map key value"),
        "expected named parameters, got: {map}"
    );
}

#[test]
fn a_stdlib_exported_builtin_stops_showing_the_bare_word() {
    // `Option` resolves to a stdlib symbol, so it always reached the fallback
    // tier and rendered as its own name under a "stdlib" label — an answer that
    // carried nothing. It now takes the built-in card like the rest.
    let card = hover(SRC, "Option", 0).expect("Option should card");
    assert!(
        card.contains("Option value") && card.contains("Some value"),
        "expected the built-in card, got: {card}"
    );
    assert!(
        !card.contains("*(stdlib)*"),
        "the bare stdlib fallback should no longer win here, got: {card}"
    );
}

#[test]
fn a_workspace_type_still_beats_the_builtin_card() {
    // The built-in tier sits below the declaration tier on purpose. A reader who
    // declared their own `Set` is asking about theirs, and the card that names
    // the wrong type is worse than the silence this change removed.
    let src = "pub type Set = { tag: Text }

pub fn tagOf (s: Set) -> Text = s.tag
";
    let card = hover(src, "Set", 1).expect("a shadowing workspace type should card");
    assert!(
        card.contains("type Set = { tag: Text }"),
        "expected the workspace declaration, got: {card}"
    );
    assert!(
        !card.contains("immutable set"),
        "the built-in card must not win over a declaration, got: {card}"
    );
}

#[test]
fn an_unknown_type_name_still_answers_nothing() {
    // The control. Without it, a card tier that fired for everything would pass
    // every assertion above and this file would prove nothing.
    let src = "pub fn f (x: Nonesuch) -> Text = \"\"\n";
    assert!(
        hover(src, "Nonesuch", 0).is_none(),
        "a name that resolves to nothing should still hover as nothing"
    );
}

#[test]
fn type_completion_carries_the_builtin_card() {
    // The other half of the same knowledge. Hover and completion read one table,
    // so they cannot drift into describing `Result` differently.
    let td = build_ws(SRC);
    let root = fs::canonicalize(td.path()).expect("canonicalize temp root");
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Main.ridge")).unwrap();

    // Column 20 on the signature line sits just after the `:` of `(rows: `.
    let line = SRC.lines().position(|l| l.contains("pub fn find")).unwrap() as u32;
    let col = SRC
        .lines()
        .nth(line as usize)
        .unwrap()
        .find("Map Text Book")
        .unwrap() as u32;
    let items = index.completions_at(&uri, line, col);

    let map = items
        .iter()
        .find(|i| i.label == "Map")
        .expect("Map should be offered in a type position");
    assert_eq!(
        map.detail.as_deref(),
        Some("Map key value"),
        "the list should show what the type takes"
    );

    let payload = map.data.clone().expect("a built-in should be resolvable");
    let (detail, doc) = index
        .resolve_completion(&payload)
        .expect("the built-in payload should resolve");
    assert_eq!(detail, "Map key value");
    assert!(
        doc.unwrap_or_default().contains("immutable map"),
        "resolve should fill in the prose"
    );

    // An arity-0 built-in has nothing to add beside its own name, so the detail
    // column stays empty rather than repeating the label.
    let text = items
        .iter()
        .find(|i| i.label == "Text")
        .expect("Text should be offered");
    assert_eq!(text.detail, None, "detail should not echo the label");
}
