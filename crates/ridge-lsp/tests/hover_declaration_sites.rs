//! Hovering a name where it is declared says what was declared.
//!
//! Every hover path starts from a binding, and a declaration's own name node
//! carries none — the resolver records uses, and a declaration is not a use of
//! itself. So the one position a reader is most likely to hover, the place they
//! wrote the name to check what they wrote, was the one position that answered
//! nothing.
//!
//! Locals were always the exception and still are: a parameter's own name *is*
//! its binding site, so it has always carded. The tests below cover what did
//! not — top-level declarations and the names written inside them.
//!
//! The assertions that matter most are the identity ones. A declaration-site
//! card and a use-site card are built by the same helpers, so they must come
//! out byte-identical; two renderers describing one declaration would drift.

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

// A doc comment is a `---`-delimited block, not a `--` line; an ordinary
// comment above a declaration is not documentation and must not become one.
const SRC: &str = "---
The catalogue's book.
---
pub type Book = { id: Int, title: Text }

pub type Status = Draft | Live

pub const limit : Int = 10

---
Read the title off a book.
---
pub fn titleOf (b: Book) -> Text = b.title

pub class Tagged a =
    tag (x: a) -> Text

actor Counter =
    state count : Int = 0

    on bump () -> Unit =
        count <- count + 1
";

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

fn index_for(src: &str) -> (WorkspaceIndex, Url) {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"decl\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/proj/src/Main.ridge", src);
    let root = fs::canonicalize(td.path()).expect("canonicalize temp root");
    // The index holds the source text it needs, but the URI must stay
    // resolvable for the length of the test.
    std::mem::forget(td);
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Main.ridge")).unwrap();
    (index, uri)
}

/// Hover `skip` bytes into the first occurrence of `anchor`.
///
/// An anchor plus an explicit offset, rather than the nth occurrence of a bare
/// name: a name also occurs inside comments and inside longer words, and an
/// off-by-one lands on the punctuation beside a one-character name instead of
/// on the name.
fn hover_in(
    index: &WorkspaceIndex,
    uri: &Url,
    src: &str,
    anchor: &str,
    skip: usize,
) -> Option<String> {
    let at = src
        .find(anchor)
        .unwrap_or_else(|| panic!("no `{anchor}` in the fixture"))
        + skip;
    let before = &src[..at];
    let line = before.matches('\n').count() as u32;
    let col = (at - before.rfind('\n').map_or(0, |p| p + 1)) as u32;
    index.hover_at(uri, line, col).map(|(md, _)| md)
}

fn hover(anchor: &str, skip: usize) -> Option<String> {
    let (index, uri) = index_for(SRC);
    hover_in(&index, &uri, SRC, anchor, skip)
}

#[test]
fn a_type_cards_the_same_at_its_declaration_and_at_a_use() {
    let decl = hover("type Book", 6).expect("the declaration should card");
    let usage = hover("b: Book", 4).expect("a use should card");
    assert!(
        decl.contains("pub type Book = { id: Int, title: Text }"),
        "expected the written header, got: {decl}"
    );
    assert!(
        decl.contains("The catalogue's book."),
        "the doc comment above the declaration belongs on the card, got: {decl}"
    );
    assert_eq!(
        decl, usage,
        "one declaration must not have two descriptions"
    );
}

#[test]
fn a_record_field_cards_the_same_at_its_declaration_and_at_a_use() {
    let decl = hover("title: Text", 1).expect("the field declaration should card");
    let usage = hover("b.title", 3).expect("a field use should card");
    assert!(
        decl.contains("title : Text") && decl.contains("field of `Book`"),
        "expected the field card, got: {decl}"
    );
    assert_eq!(decl, usage, "one field must not have two descriptions");
}

#[test]
fn the_other_top_level_declarations_card() {
    let cases = [
        ("type Status", 6, "pub type Status = Draft | Live", "type"),
        ("Draft | Live", 1, "Draft", "constructor of `Status`"),
        ("const limit", 7, "pub const limit: Int", "constant"),
        (
            "fn titleOf",
            4,
            "pub fn titleOf (b: Book) -> Text",
            "function",
        ),
        ("actor Counter", 7, "actor Counter", "actor"),
        ("on bump", 4, "on bump -> Unit", "handler of `Counter`"),
        ("class Tagged", 7, "class Tagged a", "class"),
        ("tag (x: a)", 1, "tag (x: a) -> Text", "method of `Tagged`"),
    ];
    for (anchor, skip, signature, kind) in cases {
        let card = hover(anchor, skip).unwrap_or_else(|| panic!("`{anchor}` should card"));
        assert!(
            card.contains(signature),
            "the `{anchor}` card should carry `{signature}`, got: {card}"
        );
        assert!(
            card.contains(kind),
            "the `{anchor}` card should be kinded `{kind}`, got: {card}"
        );
    }
}

#[test]
fn a_function_carries_its_doc_comment_at_the_declaration() {
    // The reason to hover your own `fn` is usually to reread what it promises.
    let card = hover("fn titleOf", 4).expect("the fn declaration should card");
    assert!(
        card.contains("Read the title off a book."),
        "expected the doc comment, got: {card}"
    );
}

#[test]
fn a_declaration_beats_the_builtin_card_of_the_same_name() {
    // The built-in tier answers when nothing else can. At a declaration site
    // something else can: the reader's own `Set`. Naming the wrong type is
    // worse than the silence this change removed.
    let src = "pub type Set = { tag: Text }\n\npub fn tagOf (s: Set) -> Text = s.tag\n";
    let (index, uri) = index_for(src);
    let decl = hover_in(&index, &uri, src, "type Set", 5).expect("the declaration should card");
    assert!(
        decl.contains("pub type Set = { tag: Text }"),
        "expected the workspace declaration, got: {decl}"
    );
    assert!(
        !decl.contains("immutable set"),
        "the built-in card must not win at a declaration site, got: {decl}"
    );
}

#[test]
fn a_parameter_at_its_binding_site_still_cards() {
    // This one was never broken — a parameter's own name is its binding site —
    // and it is here so a future change to the declaration tier cannot quietly
    // take it over and relabel it.
    let card = hover("(b: Book)", 1).expect("a parameter should card at its binding site");
    assert!(
        card.contains("parameter"),
        "expected the parameter kind, got: {card}"
    );
}

#[test]
fn a_name_that_declares_nothing_still_answers_nothing() {
    // The control. Without it a tier that fired everywhere would satisfy every
    // assertion above and this file would prove nothing.
    let src = "pub fn f (x: Nonesuch) -> Text = \"\"\n";
    let (index, uri) = index_for(src);
    assert!(
        hover_in(&index, &uri, src, "Nonesuch", 2).is_none(),
        "an unresolved type name is not a declaration and should still hover as nothing"
    );

    // A type variable is not a declaration either: `a` in `class Tagged a`
    // names no definition, so there is nothing to put on a card.
    assert!(
        hover("Tagged a =", 7).is_none(),
        "a class type variable should not card"
    );
}
