//! A constructor cards from its own declaration, wherever it is written.
//!
//! Nothing about a name depends on whether the value is being built or taken
//! apart, so the editor cannot say two things about one constructor. It used to:
//! no node inside a pattern carries a type, so the narrowest node that did was
//! the enclosing `match`, and the arm's result type was attributed to the
//! constructor's name. `Timeout` in a pattern carded as `Text` because the arm
//! returned `Text`, and would have carded as anything at all somewhere else.
//!
//! A constructor can be declared in three places, and the card has to come out
//! the same shape from all three:
//!
//! - a workspace module, where the variant's own source text is what a reader
//!   wrote and what the declaration site itself shows;
//! - the standard library, compiled ahead of the workspace;
//! - the prelude and the built-in unions — `Option`, `Result`, `JsonValue` —
//!   which are registered in Rust and have no Ridge source anywhere.
//!
//! Most assertions pair the two positions rather than pinning a string, so a
//! change to how a card is rendered fails in the few tests that pin the shape
//! rather than in every test at once.

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
use ridge_types::{TyConKind, UnionVariant};
use tower_lsp::lsp_types::Url;

const LIB: &str = "pub type Colour = Red | Green | Blue Int

pub type Point = { px: Int, py: Int }
";

const SRC: &str = r#"import std.actor (Timeout)
import proj.Lib (Colour, Point, Red, Green, Blue)

pub fn builtWorkspace () -> Colour = Red

pub fn matchedWorkspace (c: Colour) -> Text =
    match c
        Red -> "red"
        Green -> "green"
        Blue n -> "blue"

pub fn builtPayload () -> Colour = Blue 3

pub fn builtStdlib () -> AskError = Timeout

pub fn matchedStdlib (e: AskError) -> Text =
    match e
        Timeout -> "timed out"
        _ -> "other"

pub fn builtPrelude () -> Option Int = Some 1

pub fn matchedPrelude (o: Option Colour) -> Text =
    match o
        Some Red -> "red"
        Some _ -> "other"
        None -> "none"

pub fn builtResult () -> Result Int Text = Ok 1

pub fn matchedResult (r: Result Int Text) -> Int =
    match r
        Ok n -> n
        Err _ -> 0

pub fn matchedJson (j: JsonValue) -> Int =
    match j
        JList _ -> 1
        JObject _ -> 2
        JObjectFields _ -> 3
        _ -> 0

pub fn builtOrdered () -> JsonValue = JObjectFields [("z", JInt 1)]

pub fn builtRecord () -> Point = Point { px = 1, py = 2 }

pub fn matchedRecord (p: Point) -> Int =
    match p
        Point { px, py } -> px + py
"#;

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

fn seed_workspace(main: &str, lib: Option<&str>) -> (std::path::PathBuf, IncrementalState) {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ctor\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    if let Some(lib) = lib {
        write_file(td.path(), "libs/proj/src/Lib.ridge", lib);
    }
    write_file(td.path(), "libs/proj/src/Main.ridge", main);
    let root = ridge_manifest::canonicalize(td.path()).expect("canonicalize temp root");
    // The index holds the source it needs, but the URI must stay resolvable for
    // the length of the test.
    std::mem::forget(td);
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    (root, state)
}

/// Build the index, and fail loudly if the fixture does not compile.
///
/// A name that does not resolve still hovers — it just answers from an empty
/// inferred type — so a fixture with one missing import reads as a passing test
/// while proving nothing about the name it forgot. The error sets are checked
/// here so that cannot happen quietly.
fn build_index() -> (WorkspaceIndex, Url) {
    let (root, state) = seed_workspace(SRC, Some(LIB));
    assert!(
        state.resolved.errors.is_empty()
            && state.resolved.parse_errors.is_empty()
            && state.type_errors.is_empty(),
        "the fixture must compile: {:?} / {:?} / {:?}",
        state.resolved.errors,
        state.resolved.parse_errors,
        state.type_errors
    );
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Main.ridge")).unwrap();
    (index, uri)
}

/// Hover `skip` bytes into the one occurrence of `anchor`.
///
/// An anchor with an explicit offset rather than the nth occurrence of a bare
/// name: a constructor's name also occurs in its declaration, in an import item
/// and inside longer words, and counting occurrences is how a test ends up
/// comparing one position with itself. The uniqueness check is what makes the
/// anchor an address rather than a guess.
fn hover(index: &WorkspaceIndex, uri: &Url, anchor: &str, skip: usize) -> Option<String> {
    assert_eq!(
        SRC.match_indices(anchor).count(),
        1,
        "`{anchor}` must address one position in the fixture"
    );
    let at = SRC.find(anchor).expect("checked just above") + skip;
    let before = &SRC[..at];
    let line = before.matches('\n').count() as u32;
    let col = (at - before.rfind('\n').map_or(0, |p| p + 1)) as u32;
    index.hover_at(uri, line, col).map(|(md, _)| md)
}

/// Assert that a constructor cards identically where it is built and where it is
/// matched, and that the card names the type it belongs to.
fn agrees(built: (&str, usize), matched: (&str, usize), owner: &str) {
    let (index, uri) = build_index();
    let built_card = hover(&index, &uri, built.0, built.1)
        .unwrap_or_else(|| panic!("`{}` should card", built.0));
    let matched_card = hover(&index, &uri, matched.0, matched.1)
        .unwrap_or_else(|| panic!("`{}` should card", matched.0));
    assert_eq!(
        built_card, matched_card,
        "one constructor, two positions, one card — `{}` vs `{}`",
        built.0, matched.0
    );
    assert!(
        built_card.contains(&format!("constructor of `{owner}`")),
        "the card should name the type the constructor belongs to, got: {built_card}"
    );
}

#[test]
fn a_workspace_constructor_cards_the_same_built_and_matched() {
    // Four spaces of the arm's indent, because a bare `Red -> ` also matches
    // the nested `Some Red -> ` further down.
    agrees(("= Red", 2), ("    Red -> ", 4), "Colour");
}

#[test]
fn a_workspace_constructor_carries_its_payload() {
    agrees(("Blue 3", 0), ("Blue n", 0), "Colour");
    let (index, uri) = build_index();
    let card = hover(&index, &uri, "Blue 3", 0).expect("should card");
    assert!(
        card.contains("Blue Int"),
        "what a constructor carries is part of what it is, got: {card}"
    );
}

#[test]
fn a_stdlib_constructor_cards_the_same_built_and_matched() {
    // The reported case: this pattern used to report the arm's `Text`.
    agrees(("= Timeout", 2), ("Timeout -> ", 0), "AskError");
}

#[test]
fn a_prelude_constructor_cards_the_same_built_and_matched() {
    agrees(("Some 1", 0), ("Some Red", 0), "Option");
}

#[test]
fn a_record_constructor_cards_as_the_type_it_constructs() {
    // A record's auto-constructor is not a separate declaration: it is spelled
    // like the type and declared by writing the type, so it cards as the type.
    let (index, uri) = build_index();
    let built = hover(&index, &uri, "Point { px = 1", 0).expect("construction should card");
    let matched = hover(&index, &uri, "Point { px,", 0).expect("pattern should card");
    assert_eq!(built, matched, "one record constructor, one card");
    assert!(
        built.contains("pub type Point = { px: Int, py: Int }"),
        "expected the type's written header, got: {built}"
    );
}

#[test]
fn a_constructor_nested_inside_another_pattern_cards_as_itself() {
    let (index, uri) = build_index();
    let nested = hover(&index, &uri, "Some Red", 5).expect("the nested `Red` should card");
    let built = hover(&index, &uri, "= Red", 2).expect("`Red` as a value should card");
    assert_eq!(
        nested, built,
        "how deep in a pattern a constructor sits is not something its card can depend on"
    );
}

#[test]
fn the_two_parameters_of_result_do_not_print_the_same_letter() {
    // Rendering each payload on its own letters both from `a`, so `Ok` and `Err`
    // would card identically for two different parameters of `Result` — one more
    // card that is confidently wrong, which is the shape this change is about.
    let (index, uri) = build_index();
    let ok = hover(&index, &uri, "Ok n", 0).expect("`Ok` should card");
    let err = hover(&index, &uri, "Err _", 0).expect("`Err` should card");
    assert!(
        ok.contains("Ok a"),
        "`Ok` carries the first parameter, got: {ok}"
    );
    assert!(
        err.contains("Err b"),
        "`Err` carries the second parameter, got: {err}"
    );
}

#[test]
fn a_payload_of_more_than_one_word_is_parenthesised() {
    let (index, uri) = build_index();
    let list = hover(&index, &uri, "JList _", 0).expect("`JList` should card");
    assert!(
        list.contains("JList (List JsonValue)"),
        "an applied payload is one argument, not two, got: {list}"
    );
    let object = hover(&index, &uri, "JObject _", 0).expect("`JObject` should card");
    assert!(
        object.contains("JObject (Map Text JsonValue)"),
        "got: {object}"
    );
}

#[test]
fn ordered_object_constructor_cards_in_values_and_patterns() {
    let (index, uri) = build_index();
    let value = hover(&index, &uri, "JObjectFields [(", 0).expect("ordered constructor in value");
    let pattern =
        hover(&index, &uri, "JObjectFields _", 0).expect("ordered constructor in pattern");
    assert_eq!(value, pattern);
    assert!(
        value.contains("JObjectFields")
            && value.contains("List")
            && value.contains("Text")
            && value.contains("JsonValue"),
        "{value}"
    );
    let at = SRC.find("JObjectFields [(").unwrap();
    let line = SRC[..at].matches('\n').count() as u32;
    let col = (at - SRC[..at].rfind('\n').map_or(0, |p| p + 1)) as u32;
    let items = index.completions_at(&uri, line, col + 7);
    assert!(
        items.iter().any(|item| item.label == "JObjectFields"),
        "ordered constructor must complete"
    );
    let ordered = items
        .iter()
        .find(|item| item.label == "JObjectFields")
        .unwrap();
    let data = ordered
        .data
        .as_ref()
        .expect("constructor completion resolve payload");
    let (resolved, _) = index
        .resolve_completion(data)
        .expect("constructor completion resolves");
    assert!(resolved.contains("JObjectFields") && resolved.contains("JsonValue"));
    let signature = index
        .signature_help_at(&uri, line, col + 14, false)
        .expect("ordered constructor signature");
    assert!(signature.signatures[0].label.contains("JObjectFields"));
    let at = SRC.find("Ok 1").unwrap();
    let line = SRC[..at].matches('\n').count() as u32;
    let col = (at - SRC[..at].rfind('\n').map_or(0, |p| p + 1)) as u32;
    let signature = index
        .signature_help_at(&uri, line, col + 3, false)
        .expect("generic constructor signature");
    assert!(signature.signatures[0].label.contains("arg0: a"));
    assert!(signature.signatures[0].label.ends_with("Result a b"));
}

#[test]
fn builtin_variant_names_are_unambiguous() {
    // The card for a constructor with no Ridge source is found by name across
    // the built-in unions. That is sound exactly while no two of them share a
    // variant name, so this asserts the property instead of trusting it: a new
    // built-in union that reuses a name fails here, next to the code that
    // depends on it, rather than silently carding the wrong owner.
    let (_root, state) = seed_workspace("pub fn main () -> Int = 1\n", None);

    let mut owners: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
    for decl in &state.typed.tycons {
        // The `u32::MAX` module sentinel means "declared somewhere no user
        // module can be", which is the same answer as no module at all.
        if decl.def_module_raw.filter(|m| *m != u32::MAX).is_some() {
            continue;
        }
        let TyConKind::Union(schema) = &decl.kind else {
            continue;
        };
        for UnionVariant { name, .. } in &schema.variants {
            owners.entry(name).or_default().push(&decl.name);
        }
    }
    assert!(
        owners.len() > 20,
        "the sweep found almost no built-in variants — it is broken, not the arena"
    );
    let clashes: Vec<_> = owners.iter().filter(|(_, o)| o.len() > 1).collect();
    assert!(
        clashes.is_empty(),
        "two built-in unions share a variant name, so a lookup by name cannot tell them apart: {clashes:?}"
    );
}
