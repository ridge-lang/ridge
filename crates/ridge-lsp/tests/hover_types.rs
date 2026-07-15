//! Hover over a type name resolves to the type's declaration — for a locally
//! declared type, an imported one, and across an incremental recheck.
//!
//! Hovering a value has always worked; a type-position reference carries no
//! inferred type, so `hover_at` used to bail before reaching the binding path
//! that go-to-definition already resolves. These guard that it now cards the
//! type from its declaration instead of returning nothing.

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

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

const ENTITIES: &str = "import std.repo as Repo\n\npub type Book = { id: Int, title: Text } deriving (Row, Schema)\n\nfn titleOf (x: Book) -> Text = x.title\n";

const QUERIES: &str = "import std.data (Sqlite)\nimport std.repo as Repo\nimport std.query (Asc)\nimport proj.Entities (Book)\n\npub fn db allBooks (books: Repo Book Sqlite) -> Result (List Book) Error =\n    books |> Repo.query |> Repo.orderBy Asc (fn (b: Book) -> b.id) |> Repo.toList\n";

fn build_ws() -> TempDir {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"inc-ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n[capabilities]\nallow = [\"db\"]\n",
    );
    write_file(td.path(), "libs/proj/src/Entities.ridge", ENTITIES);
    write_file(td.path(), "libs/proj/src/Queries.ridge", QUERIES);
    td
}

fn uri_for(root: &Path, rel: &str) -> Url {
    Url::from_file_path(root.join(rel)).unwrap()
}

/// The (line, utf16 col) just inside the type name that follows `anchor`.
fn type_pos(src: &str, anchor: &str, skip: &str) -> (u32, u32) {
    let at = src.find(anchor).unwrap_or_else(|| panic!("no `{anchor}`")) + skip.len();
    let before = &src[..at];
    let line = before.matches('\n').count() as u32;
    // +1 to land inside the name rather than on its first column boundary.
    let col = (at - before.rfind('\n').map_or(0, |p| p + 1)) as u32 + 1;
    (line, col)
}

fn index_of(state: &IncrementalState) -> WorkspaceIndex {
    WorkspaceIndex::build(0, &state.typed, &state.resolved, &state.source_cache())
}

#[test]
fn hover_cards_imported_and_local_type_references() {
    let td = build_ws();
    // Discovery canonicalizes on-disk paths (resolving the macOS `/var` -> `/private/var`
    // symlink and Windows 8.3 short names), and the index keys each module by the
    // canonical root joined with its source id. Build the query URIs from that same
    // canonical root so they match on every platform; a raw `TempDir` path diverges
    // from the canonical form on macOS and Windows, and the key lookup would miss.
    let root = std::fs::canonicalize(td.path()).expect("canonicalize temp root");
    let opts = CheckOptions::new(root.clone()).with_retain_indices(true);
    let mut state = check_workspace_incremental(opts).expect("seed");
    let idx = index_of(&state);

    let queries = uri_for(&root, "libs/proj/src/Queries.ridge");
    let entities = uri_for(&root, "libs/proj/src/Entities.ridge");

    // Imported type `Book` in `Repo Book Sqlite`.
    let (l, c) = type_pos(QUERIES, "Repo Book", "Repo ");
    let imported = idx.hover_at(&queries, l, c);
    assert!(
        imported
            .as_ref()
            .is_some_and(|(md, _)| md.contains("type Book")),
        "hovering an imported type should card its declaration, got: {imported:?}"
    );

    // Local type `Book` in `titleOf (x: Book)`.
    let (l, c) = type_pos(ENTITIES, "x: Book", "x: ");
    let local = idx.hover_at(&entities, l, c);
    assert!(
        local
            .as_ref()
            .is_some_and(|(md, _)| md.contains("type Book")),
        "hovering a local type should card its declaration, got: {local:?}"
    );

    // The same imported hover must survive a surface-preserving incremental edit.
    let qid = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.fully_qualified_name.ends_with(".Queries"))
        .map(|m| m.id)
        .expect("queries module");
    let v2 = QUERIES.replace("fn (b: Book) -> b.id", "fn (bk: Book) -> bk.id");
    state.recompile(qid, &v2);
    let idx2 = index_of(&state);
    let (l, c) = type_pos(&v2, "Repo Book", "Repo ");
    let after = idx2.hover_at(&queries, l, c);
    assert!(
        after
            .as_ref()
            .is_some_and(|(md, _)| md.contains("type Book")),
        "hovering an imported type after an incremental edit should still card it, got: {after:?}"
    );
}
