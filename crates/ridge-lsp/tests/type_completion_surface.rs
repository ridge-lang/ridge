//! What a type position offers is the set of names a reader can write.
//!
//! The arena the list is drawn from holds more than that. It also holds the
//! sixteen per-arity dispatch keys behind function types and the projection
//! shapes the query builder threads through a chain — names the compiler writes
//! for itself. And it can still hold one name twice: `Parsed` is registered
//! along two paths, so the list keeps the first row for a repeated name and
//! drops the rest, since two rows spelled the same give a reader nothing to
//! choose between.
//!
//! Hover keeps carding those names, and that asymmetry is the point: hover
//! explains what a reader has already run into, completion proposes what they
//! should type next. The set you must explain is larger than the set you should
//! propose.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::TempDir;

use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState};
use ridge_lsp::completion::CompletionItemData;
use ridge_lsp::index::WorkspaceIndex;
use ridge_types::{fn_tycon_name, FN_ARITY_COUNT};
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
        "[workspace]\nname = \"tcs\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/proj/src/Main.ridge", src);
    td
}

/// Every candidate offered where `Map Text Book` is written, with no prefix
/// typed — the widest the list ever gets.
fn type_position_candidates() -> Vec<CompletionItemData> {
    let td = build_ws(SRC);
    let root = ridge_manifest::canonicalize(td.path()).expect("canonicalize temp root");
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Main.ridge")).unwrap();

    let line = SRC.lines().position(|l| l.contains("pub fn find")).unwrap() as u32;
    let col = SRC
        .lines()
        .nth(line as usize)
        .unwrap()
        .find("Map Text Book")
        .unwrap() as u32;
    index.completions_at(&uri, line, col)
}

fn labels() -> Vec<String> {
    type_position_candidates()
        .into_iter()
        .map(|i| i.label)
        .collect()
}

#[test]
fn the_list_still_holds_the_names_a_reader_wants() {
    // The control. A filter that emptied the list would satisfy every exclusion
    // below, so the exclusions prove nothing without this.
    let labels = labels();
    for wanted in [
        "Int", "Text", "Map", "Result", "Option", "List", "Book", "Query", "Joined", "Decimal",
    ] {
        assert!(
            labels.iter().any(|l| l == wanted),
            "{wanted} should still be offered in a type position, got {labels:?}"
        );
    }
}

#[test]
fn no_function_dispatch_key_is_offered() {
    // Derived from the function that mints the names, not from a list written
    // here or from the predicate the filter itself consults. That is what makes
    // this a check rather than the same claim twice: the day the arity ceiling
    // moves, the new keys are covered here and the filter's own list is not.
    let labels = labels();
    let leaked: Vec<String> = (0..FN_ARITY_COUNT)
        .map(fn_tycon_name)
        .filter(|n| labels.contains(n))
        .collect();
    assert!(
        leaked.is_empty(),
        "a function type is written `fn a -> b`, so no per-arity dispatch key belongs in the list; leaked {leaked:?}"
    );
}

#[test]
fn no_name_the_compiler_writes_for_itself_is_offered() {
    // Named literally rather than derived from the predicate the filter uses,
    // so this asserts something the implementation does not already say.
    let labels = labels();
    for internal in [
        "Ret",
        "Rows",
        "JoinCond",
        "JoinResult",
        "LeftJoinResult",
        "RightJoinResult",
        "FullJoinResult",
        "InsertShape",
    ] {
        assert!(
            !labels.iter().any(|l| l == internal),
            "{internal} is threaded through a query chain by the compiler, never written; it should not be offered"
        );
    }
}

#[test]
fn a_name_is_offered_once() {
    let labels = labels();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for l in &labels {
        *counts.entry(l.as_str()).or_default() += 1;
    }
    let repeated: Vec<(&str, usize)> = counts.into_iter().filter(|(_, n)| *n > 1).collect();
    assert!(
        repeated.is_empty(),
        "one name, one entry — a reader has no way to choose between two rows spelled the same; repeated {repeated:?}"
    );
}

#[test]
fn the_two_column_types_are_two_rows() {
    // `Column` used to name both the typed column reference behind
    // `deriving (Table)` and the opaque column of a migration. One row survived
    // the dedupe above and described whichever the arena happened to hold
    // first, so a reader who meant the other could not ask for it and was not
    // told it existed. They are two names now, and this is what says so.
    let items = type_position_candidates();
    let offered = |label: &str| -> Vec<&CompletionItemData> {
        items.iter().filter(|i| i.label == label).collect()
    };

    let column = offered("Column");
    assert_eq!(column.len(), 1, "`Column` should be offered once");
    let column_ref = offered("ColumnRef");
    assert_eq!(column_ref.len(), 1, "`ColumnRef` should be offered once");

    // The card's own signature, so the row says what the two parameters are for
    // rather than leaving a reader to guess at `ColumnRef e a`.
    assert_eq!(
        column_ref[0].detail.as_deref(),
        Some("ColumnRef entity value"),
        "the built-in row should carry its parameter-named signature"
    );
    // `Column` is a reconciled `std.migrate` type, and none of those carry a
    // completion detail — `Repo`, `Query`, `Migration` and `SortOrder` are all
    // bare rows too. Pinned so this reads as the shape of that whole family
    // rather than as something the rename took away.
    assert_eq!(
        column[0].detail.as_deref(),
        None,
        "a reconciled stdlib type is a bare row here; hover is where its          declaration is read"
    );
}

#[test]
fn a_name_that_is_not_offered_is_still_explained() {
    // The asymmetry, pinned. `Rows` is dropped from the list above because a
    // reader never writes it — but it does appear in inferred signatures and in
    // messages, and hovering it there has to answer. Without this, a later
    // reader could "fix" the inconsistency by dropping the card and leave the
    // name unexplained wherever the compiler puts it on screen.
    let src = "pub fn f (x: Rows) -> Int = 1\n";
    let td = build_ws(src);
    let root = ridge_manifest::canonicalize(td.path()).expect("canonicalize temp root");
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let state: IncrementalState = check_workspace_incremental(opts).expect("seed");
    let index = WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache());
    let uri = Url::from_file_path(root.join("libs/proj/src/Main.ridge")).unwrap();
    let col = src.lines().next().unwrap().find("Rows").unwrap() as u32 + 1;

    let card = index
        .hover_at(&uri, 0, col)
        .map(|(text, _span)| text)
        .expect("a name the compiler puts on screen must still hover");
    assert!(
        card.contains("Rows"),
        "the card should describe the name under the cursor, got {card:?}"
    );
    assert!(
        !labels().iter().any(|l| l == "Rows"),
        "and it should still not be proposed"
    );
}
