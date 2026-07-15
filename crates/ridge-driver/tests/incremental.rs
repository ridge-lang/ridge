//! Incremental-vs-full equality and minimal-recompute tests for the
//! [`ridge_driver::IncrementalState`] engine.
//!
//! The contract: after any single-file edit, the incrementally updated caches
//! must hold exactly the same diagnostics and per-node types as a full rebuild
//! of the edited workspace — while recomputing only the modules the edit can
//! actually affect.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use ridge_driver::{
    check_workspace_incremental, collect_diagnostics, CheckOptions, IncrementalState,
};
use ridge_resolve::{discover_workspace, resolve_workspace_with, ModuleId, ResolvedWorkspace};
use ridge_typecheck::{render_type_with, typecheck_workspace, TypedWorkspace};

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// A two-module library: `App` imports `Lib`, so `App` is a reverse-dependency
/// of `Lib`.
fn build_ws(lib_src: &str, app_src: &str) -> TempDir {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"inc-ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_src);
    write_file(td.path(), "libs/proj/src/App.ridge", app_src);
    td
}

fn module_id_by_suffix(ws: &ResolvedWorkspace, suffix: &str) -> ModuleId {
    for m in &ws.graph.modules {
        if m.fully_qualified_name.ends_with(suffix) {
            return m.id;
        }
    }
    panic!("module ending in {suffix} not found");
}

/// Run the full pipeline and seed an incremental cache from it.
fn full_state(root: &Path) -> IncrementalState {
    let mut disc = discover_workspace(root);
    let disc_errs = std::mem::take(&mut disc.resolve_errors);
    let ws = disc.graph.expect("graph");
    let resolved = resolve_workspace_with(ws, true);
    let tc = typecheck_workspace(&resolved);
    IncrementalState::new(resolved, tc, disc_errs)
}

/// Every diagnostic across the workspace, as sorted `Debug` strings. Spans are
/// included; an incremental result and the full rebuild of the same sources must
/// agree on them exactly.
fn all_diags(
    resolved: &ResolvedWorkspace,
    type_errors: &[(ModuleId, ridge_typecheck::TypeError)],
) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for (m, e) in &resolved.lex_errors {
        v.push(format!("L|{}|{e:?}", m.0));
    }
    for (m, e) in &resolved.parse_errors {
        v.push(format!("P|{}|{e:?}", m.0));
    }
    for (m, e) in &resolved.errors {
        v.push(format!("R|{}|{e:?}", m.0));
    }
    for (m, e) in type_errors {
        v.push(format!("T|{}|{e:?}", m.0));
    }
    v.sort();
    v
}

/// Per-module rendered node types. Internal `TyConId`s differ between an
/// incremental update and a full build, so types are compared by their rendered
/// form, module by module.
fn all_rendered(typed: &TypedWorkspace) -> Vec<Vec<Option<String>>> {
    typed
        .modules
        .iter()
        .map(|tm| {
            tm.node_types
                .iter()
                .map(|o| o.as_ref().map(|ty| render_type_with(ty, &typed.tycons)))
                .collect()
        })
        .collect()
}

fn assert_matches_full(state: &IncrementalState, root: &Path) {
    let oracle = full_state(root);
    assert_eq!(
        all_diags(&state.resolved, &state.type_errors),
        all_diags(&oracle.resolved, &oracle.type_errors),
        "incremental diagnostics must match the full rebuild"
    );
    assert_eq!(
        all_rendered(&state.typed),
        all_rendered(&oracle.typed),
        "incremental rendered node types must match the full rebuild"
    );
}

#[test]
fn body_edit_recompiles_only_the_edited_module() {
    let td = build_ws(
        "pub fn helper -> Int = 1\n",
        "import proj.Lib\npub fn use_it -> Int = 2\n",
    );
    let mut state = full_state(td.path());
    let lib = module_id_by_suffix(&state.resolved, ".Lib");

    // Change only Lib's body — its exported surface is untouched.
    let lib_v2 = "pub fn helper -> Int = 5\n";
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_v2);
    let recompiled = state.recompile(lib, lib_v2);

    assert_eq!(
        recompiled,
        vec![lib],
        "a body edit must recompile only the edited module"
    );
    assert_matches_full(&state, td.path());
}

#[test]
fn surface_edit_recompiles_the_reverse_dependency_closure() {
    let td = build_ws(
        "pub fn helper -> Int = 1\n",
        "import proj.Lib\npub fn use_it -> Int = 2\n",
    );
    let mut state = full_state(td.path());
    let lib = module_id_by_suffix(&state.resolved, ".Lib");
    let app = module_id_by_suffix(&state.resolved, ".App");

    // Add a public function to Lib — its surface changes, so every module that
    // imports it must be recomputed too.
    let lib_v2 = "pub fn helper -> Int = 1\npub fn extra -> Int = 9\n";
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_v2);
    let mut recompiled = state.recompile(lib, lib_v2);
    recompiled.sort_by_key(|m| m.0);

    let mut expected = vec![lib, app];
    expected.sort_by_key(|m| m.0);
    assert_eq!(
        recompiled, expected,
        "a surface edit must recompile the edited module and its importers"
    );
    assert_matches_full(&state, td.path());
}

#[test]
fn edit_that_introduces_a_type_error_matches_full() {
    let td = build_ws(
        "pub fn helper -> Int = 1\n",
        "import proj.Lib\npub fn use_it -> Int = 2\n",
    );
    let mut state = full_state(td.path());
    let lib = module_id_by_suffix(&state.resolved, ".Lib");

    // Introduce a type mismatch in Lib's body.
    let lib_v2 = "pub fn helper -> Int = \"oops\"\n";
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_v2);
    state.recompile(lib, lib_v2);

    assert!(
        !state.type_errors.is_empty(),
        "the incremental recompile must surface the new type error"
    );
    assert_matches_full(&state, td.path());
}

// ── Tier-2b: class / instance / deriving changes ──────────────────────────────

const LIB_WITH_CLASS: &str = "class Show a =\n    toText (x: a) -> Text\ntype Color = Red | Green\ninstance Show Color =\n    toText (c: Color) -> Text = \"red\"\n";

#[test]
fn adding_an_instance_deep_recompiles_and_matches_full() {
    let td = build_ws(LIB_WITH_CLASS, "import proj.Lib\npub fn f -> Int = 1\n");
    let mut state = full_state(td.path());
    let lib = module_id_by_suffix(&state.resolved, ".Lib");

    // Add a second type and instance — a change to the class/instance surface.
    let lib_v2 = "class Show a =\n    toText (x: a) -> Text\ntype Color = Red | Green\ninstance Show Color =\n    toText (c: Color) -> Text = \"red\"\ntype Tone = Hi | Lo\ninstance Show Tone =\n    toText (t: Tone) -> Text = \"t\"\n";
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_v2);
    let recompiled = state.recompile(lib, lib_v2);

    assert_eq!(
        recompiled.len(),
        state.resolved.modules.len(),
        "a class/instance change must deep-recompile every module"
    );
    assert_matches_full(&state, td.path());
}

#[test]
fn deriving_change_deep_recompiles_and_matches_full() {
    let td = build_ws(
        "type Color = Red | Green\n",
        "import proj.Lib\npub fn f -> Int = 1\n",
    );
    let mut state = full_state(td.path());
    let lib = module_id_by_suffix(&state.resolved, ".Lib");

    let lib_v2 = "type Color = Red | Green deriving (Eq)\n";
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_v2);
    let recompiled = state.recompile(lib, lib_v2);

    assert_eq!(
        recompiled.len(),
        state.resolved.modules.len(),
        "a deriving change must deep-recompile every module"
    );
    assert_matches_full(&state, td.path());
}

#[test]
fn body_edit_in_a_typeclass_module_stays_incremental() {
    // A module with class/instance declarations plus an ordinary function.
    let lib_v1 = "pub fn greet -> Text = \"hi\"\nclass Show a =\n    toText (x: a) -> Text\ntype Color = Red\ninstance Show Color =\n    toText (c: Color) -> Text = \"red\"\n";
    let td = build_ws(lib_v1, "import proj.Lib\npub fn f -> Int = 1\n");
    let mut state = full_state(td.path());
    let lib = module_id_by_suffix(&state.resolved, ".Lib");

    // Change only the ordinary function's body. The class/instance declarations
    // shift position, but the typeclass surface is unchanged — so this stays a
    // single-module recompile, not a deep one.
    let lib_v2 = "pub fn greet -> Text = \"hello there\"\nclass Show a =\n    toText (x: a) -> Text\ntype Color = Red\ninstance Show Color =\n    toText (c: Color) -> Text = \"red\"\n";
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_v2);
    let recompiled = state.recompile(lib, lib_v2);

    assert_eq!(
        recompiled,
        vec![lib],
        "a body edit must not deep-recompile, even with typeclass declarations present"
    );
    assert_matches_full(&state, td.path());
}

// ── The LSP-facing seeding + source-cache + diagnostics path ──────────────────

// ── Tier-2c: data-layer deriving + quoted predicates on the fast path ──────────
//
// A surface-preserving body edit takes the single-module fast path
// (`typecheck_module_incremental`), which must stay faithful to a full rebuild.
// Two ridge.data shapes used to break it: a module that *defines* a
// `deriving (Row, Schema)` entity and uses its derived `HasSchema` instance (the
// module's own types must keep the ids the instances are keyed by, or the
// instance is orphaned — a spurious `NoInstance HasSchema`), and a module that
// *imports* such an entity and runs a quoted predicate over it (the import's
// real id must be threaded in, or the quote checker can't pin the entity).

const DATA_ENTITIES: &str = "import std.migrate as Migrate\nimport std.schema (schemaOf)\n\npub type Book = { id: Int, title: Text, cents: Int } deriving (Row, Schema)\n\nfn bookWitness () -> Option Book = None\n\npub fn schema () -> List Migration =\n    [ Migrate.migration \"0001\" [ Migrate.createSchema (schemaOf (bookWitness ())) ] ]\n";

const DATA_QUERIES: &str = "import std.data (Sqlite)\nimport std.repo as Repo\nimport std.query (Asc)\nimport proj.Entities (Book)\n\npub fn db allBooks (books: Repo Book Sqlite) -> Result (List Book) Error =\n    books |> Repo.query |> Repo.orderBy Asc (fn (b: Book) -> b.id) |> Repo.toList\n";

fn build_data_ws() -> TempDir {
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
    write_file(td.path(), "libs/proj/src/Entities.ridge", DATA_ENTITIES);
    write_file(td.path(), "libs/proj/src/Queries.ridge", DATA_QUERIES);
    td
}

#[test]
fn body_edit_in_deriving_schema_definer_matches_full() {
    let td = build_data_ws();
    let mut state = full_state(td.path());
    assert!(
        state.type_errors.is_empty(),
        "seed must be clean, got: {:?}",
        state.type_errors
    );
    let entities = module_id_by_suffix(&state.resolved, ".Entities");

    // A surface-preserving body edit (migration name only) stays on the fast
    // path; its `HasSchema Book` constraint must still discharge against the
    // instance keyed by Book's original id.
    let v2 = DATA_ENTITIES.replace("\"0001\"", "\"0002\"");
    write_file(td.path(), "libs/proj/src/Entities.ridge", &v2);
    let recompiled = state.recompile(entities, &v2);

    assert_eq!(
        recompiled,
        vec![entities],
        "a body edit recompiles only itself"
    );
    assert_matches_full(&state, td.path());
}

#[test]
fn body_edit_with_quoted_predicate_over_import_matches_full() {
    let td = build_data_ws();
    let mut state = full_state(td.path());
    assert!(
        state.type_errors.is_empty(),
        "seed must be clean, got: {:?}",
        state.type_errors
    );
    let queries = module_id_by_suffix(&state.resolved, ".Queries");

    // A surface-preserving body edit (predicate binder rename) stays on the fast
    // path; the imported `Book` id must be threaded in so the quoted predicate
    // still resolves to an entity.
    let v2 = DATA_QUERIES.replace("fn (b: Book) -> b.id", "fn (bk: Book) -> bk.id");
    write_file(td.path(), "libs/proj/src/Queries.ridge", &v2);
    let recompiled = state.recompile(queries, &v2);

    assert_eq!(
        recompiled,
        vec![queries],
        "a body edit recompiles only itself"
    );
    assert_matches_full(&state, td.path());
}

#[test]
fn check_workspace_incremental_tracks_buffer_text_and_diagnostics() {
    let td = build_ws(
        "pub fn helper -> Int = 1\n",
        "import proj.Lib\npub fn use_it -> Int = 2\n",
    );

    let opts = CheckOptions::new(td.path().to_path_buf()).with_retain_indices(true);
    let mut state = check_workspace_incremental(opts).expect("seed the engine");
    let lib = module_id_by_suffix(&state.resolved, ".Lib");

    // A clean workspace produces no diagnostics through the LSP-facing path.
    let sources = state.source_cache();
    let diags = collect_diagnostics(
        &state.disc_resolve_errors,
        &state.resolved,
        &state.type_errors,
        &sources,
    );
    assert!(diags.is_empty(), "clean workspace, got: {diags:?}");

    // Edit Lib's buffer to a type error — without writing to disk.
    let lib_v2 = "pub fn helper -> Int = \"oops\"\n";
    state.recompile(lib, lib_v2);

    let sources = state.source_cache();
    // The source cache now reflects the buffer, not the unchanged disk file.
    let lib_text = sources
        .text(sources.id_for_module(lib).as_str())
        .expect("Lib source present");
    assert!(
        lib_text.contains("oops"),
        "source cache must track the edited buffer, got: {lib_text:?}"
    );
    let diags = collect_diagnostics(
        &state.disc_resolve_errors,
        &state.resolved,
        &state.type_errors,
        &sources,
    );
    assert!(
        !diags.is_empty(),
        "the buffer edit's type error must surface through the LSP path"
    );
}
