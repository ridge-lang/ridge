//! Cross-module type seeding — imported type names resolve in the consumer.
//!
//! Runs the full `discover -> resolve -> typecheck` pipeline over a two-module
//! project where `proj.Lib` declares a record and `proj.Main` annotates a
//! parameter with the imported type and accesses its fields. Before type-name
//! seeding these annotations fell through to a fresh type var and every field
//! access was silently absorbed.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};
use tempfile::TempDir;

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// Build a two-module project `proj` (`Main.ridge` + `Lib.ridge`) and run the
/// full pipeline. Returns every `T###` error across the workspace.
fn typecheck_two_modules(main_src: &str, lib_src: &str) -> Vec<TypeError> {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"**\"]\n",
    );
    write_file(td.path(), "libs/proj/src/Main.ridge", main_src);
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_src);

    let disc = discover_workspace(td.path());
    let resolved = resolve_workspace(disc.graph.expect("workspace graph"));
    let result = typecheck_workspace(&resolved);
    result.errors.into_iter().map(|(_, e)| e).collect()
}

fn count_code(errors: &[TypeError], code: &str) -> usize {
    errors.iter().filter(|e| e.code() == code).count()
}

const LIB_PLAIN: &str = "pub type Plain = { x: Int }\n";

#[test]
fn imported_type_annotation_resolves_unknown_field_is_t005() {
    // `(p: Plain)` must resolve to the producer's record so `p.nope` is a real
    // unknown-field error rather than being silently absorbed.
    let main = "import proj.Lib (Plain)\nfn f (p: Plain) -> Int = p.nope\n";
    let errors = typecheck_two_modules(main, LIB_PLAIN);
    assert_eq!(
        count_code(&errors, "T005"),
        1,
        "expected one T005 for unknown field on imported record; got {errors:?}"
    );
}

#[test]
fn imported_type_field_type_flows_t001() {
    // `p.x` is Int; returning it as Text must mismatch — proving the field type
    // crossed the module boundary.
    let main = "import proj.Lib (Plain)\nfn f (p: Plain) -> Text = p.x\n";
    let errors = typecheck_two_modules(main, LIB_PLAIN);
    assert_eq!(
        count_code(&errors, "T001"),
        1,
        "expected one T001 for Int field returned as Text; got {errors:?}"
    );
}

#[test]
fn imported_type_correct_field_use_is_clean() {
    let main = "import proj.Lib (Plain)\nfn f (p: Plain) -> Int = p.x\n";
    let errors = typecheck_two_modules(main, LIB_PLAIN);
    assert!(
        errors.is_empty(),
        "correct cross-module field access must type-check clean; got {errors:?}"
    );
}

// ── Opaque field boundary (T036) — reachable now that imported types resolve ──

const LIB_OPAQUE: &str = "pub opaque type Sql = { raw: Text }\n";

#[test]
fn opaque_cross_module_field_access_is_t036() {
    // Reading an opaque type's field from another module is rejected.
    let main = "import proj.Lib (Sql)\nfn leak (s: Sql) -> Text = s.raw\n";
    let errors = typecheck_two_modules(main, LIB_OPAQUE);
    assert_eq!(
        count_code(&errors, "T036"),
        1,
        "expected one T036 for cross-module opaque field access; got {errors:?}"
    );
}

#[test]
fn opaque_cross_module_with_update_is_t036() {
    // Rebuilding an opaque value's field via `with` from another module is rejected.
    let main = "import proj.Lib (Sql)\nfn tamper (s: Sql) -> Sql = s with { raw = \"x\" }\n";
    let errors = typecheck_two_modules(main, LIB_OPAQUE);
    assert_eq!(
        count_code(&errors, "T036"),
        1,
        "expected one T036 for cross-module opaque with-update; got {errors:?}"
    );
}

#[test]
fn opaque_in_module_field_access_is_allowed() {
    // The declaring module may read its own opaque fields.
    let lib = "pub opaque type Sql = { raw: Text }\npub fn unwrap (s: Sql) -> Text = s.raw\n";
    let main = "fn main = ()\n";
    let errors = typecheck_two_modules(main, lib);
    assert_eq!(
        count_code(&errors, "T036"),
        0,
        "in-module opaque field access must be allowed; got {errors:?}"
    );
}

// ── Function scheme seeding — imported fn calls are type-checked ───────────────

const LIB_FN: &str = "pub fn needsText (r: Text) -> Text = r\n";

fn count_mismatch(errors: &[TypeError]) -> usize {
    errors
        .iter()
        .filter(|e| matches!(e.code(), "T001" | "T002"))
        .count()
}

#[test]
fn imported_fn_call_wrong_arg_type_is_rejected() {
    // `needsText 123` passes an Int where Text is required: the imported scheme
    // must flow so the mismatch is caught (previously absorbed silently).
    let main = "import proj.Lib (needsText)\nfn f () -> Text = needsText 123\n";
    let errors = typecheck_two_modules(main, LIB_FN);
    assert!(
        count_mismatch(&errors) >= 1,
        "expected a type mismatch for cross-module call with bad arg; got {errors:?}"
    );
}

#[test]
fn imported_fn_call_correct_arg_is_clean() {
    let main = "import proj.Lib (needsText)\nfn f () -> Text = needsText \"ok\"\n";
    let errors = typecheck_two_modules(main, LIB_FN);
    assert!(
        errors.is_empty(),
        "correct cross-module call must type-check clean; got {errors:?}"
    );
}

// ── Stdlib taint wrappers (Sql/Html) are opaque end-to-end ────────────────────

fn typecheck_one(main_src: &str) -> Vec<TypeError> {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/app/ridge.toml",
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/app/src/Main.ridge", main_src);
    let disc = discover_workspace(td.path());
    let resolved = resolve_workspace(disc.graph.expect("workspace graph"));
    let result = typecheck_workspace(&resolved);
    result.errors.into_iter().map(|(_, e)| e).collect()
}

#[test]
fn stdlib_opaque_field_access_is_t036() {
    // Reading `Sql`'s field from user code is rejected (it would expose the
    // representation and let callers skip the escape contract).
    let main = "import std.sql (Sql)\nfn leak (s: Sql) -> Text = s.value\n";
    let errors = typecheck_one(main);
    assert_eq!(
        count_code(&errors, "T036"),
        1,
        "expected one T036 for stdlib opaque field access; got {errors:?}"
    );
}

#[test]
fn stdlib_accessor_reads_value_cleanly() {
    // The exported `sqlValue` accessor is the sanctioned way to read the text.
    let main = "import std.sql (sql, sqlValue)\nfn ok () -> Text = sqlValue (sql \"x\")\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "factory + accessor round-trip must type-check clean; got {errors:?}"
    );
}

#[test]
fn stdlib_sql_value_imports_and_resolves() {
    // The opaque `SqlValue` from std.sql can be imported and named in a
    // signature; passing one through resolves to the builtin opaque type
    // rather than a fresh variable.
    let main = "import std.sql (SqlValue)\nfn id (v: SqlValue) -> SqlValue = v\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "importing and naming SqlValue must type-check clean; got {errors:?}"
    );
}

#[test]
fn stdlib_secure_cookie_field_access_is_t036() {
    let main = "import std.net.http (SecureCookie)\nfn leak (c: SecureCookie) -> Text = c.value\n";
    let errors = typecheck_one(main);
    assert_eq!(
        count_code(&errors, "T036"),
        1,
        "expected one T036 reading a SecureCookie field; got {errors:?}"
    );
}

#[test]
fn stdlib_secure_cookie_setters_are_clean() {
    // Build with defaults, override an attribute through a setter, then serialize.
    let main = "import std.net.http (secureCookie, withSecure, secureCookieHeader)\nfn ok () -> Text = secureCookieHeader (withSecure (secureCookie \"n\" \"v\") false)\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "factory + setter + serializer must type-check clean; got {errors:?}"
    );
}

// ── Class-method signatures resolve types from any module (no spurious T023) ──

// A class method whose signature mentions a type declared in the class's own
// module is seeded into every module's env so bare-name calls resolve. When the
// seed resolved those signature types against only the consuming module's type
// names, the return type fell through to a fresh variable that then surfaced as
// a spurious T023 (unsolved type variable) in an unrelated module. The signature
// must resolve against the workspace-global type map instead.
const LIB_CLASS: &str =
    "pub type Payload = Wrap Int\npub class Codec a =\n    encodePayload (x: a) -> Payload\n";

#[test]
fn class_method_return_type_from_other_module_no_t023() {
    // `Main` neither declares nor imports `Payload`, yet `Codec.encodePayload`
    // is seeded into it. The return type must still resolve to the producer's
    // `TyCon`, not leak a free variable.
    let main = "fn f () -> Int = 1\n";
    let errors = typecheck_two_modules(main, LIB_CLASS);
    assert_eq!(
        count_code(&errors, "T023"),
        0,
        "class-method return type from another module must not leak a T023; got {errors:?}"
    );
}

// ── SqlType codec class — base-type instances ─────────────────────────────────

#[test]
fn stdlib_sql_type_base_instances_typecheck() {
    // toSql resolves the SqlType instance for each base type.
    let main = "import std.sql (toSql, SqlValue)\n\
                fn encInt (n: Int) -> SqlValue = toSql n\n\
                fn encText (s: Text) -> SqlValue = toSql s\n\
                fn encBool (b: Bool) -> SqlValue = toSql b\n\
                fn encFloat (f: Float) -> SqlValue = toSql f\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "toSql on base types must type-check clean; got {errors:?}"
    );
}

#[test]
fn stdlib_sql_type_fromsql_typechecks() {
    let main = "import std.sql (fromSql, SqlValue)\n\
                fn decInt (v: SqlValue) -> Result Int Error = fromSql v\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "fromSql must type-check clean; got {errors:?}"
    );
}

#[test]
fn stdlib_sql_type_missing_instance_is_rejected() {
    // A user record has no SqlType instance, so toSql on it must be rejected.
    let main = "import std.sql (toSql, SqlValue)\n\
                pub type Widget = { n: Int }\n\
                fn bad (w: Widget) -> SqlValue = toSql w\n";
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "toSql on a type with no SqlType instance must be rejected; got no errors"
    );
}

// ── Row decoder — deriving (Row) ──────────────────────────────────────────────

#[test]
fn deriving_row_record_typechecks_and_resolves_from_row() {
    // `deriving (Row)` synthesises a `Row User` instance; a `fromRow` call whose
    // result type is pinned to `User` resolves to that instance and is clean.
    let main = "import std.sql (fromRow, SqlValue)\n\
                pub type User = { id: Int, name: Text } deriving (Row)\n\
                pub fn decode (r: Map Text SqlValue) -> Result User Error = fromRow r\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "deriving (Row) + fromRow on a primitive-field record must be clean; got {errors:?}"
    );
}

#[test]
fn deriving_row_unsupported_field_type_is_rejected() {
    // A field whose type has no SqlType instance (here `List Int`) cannot be
    // read from a column, so `deriving (Row)` must fail rather than emit a
    // decoder that references a missing `fromSql`.
    let main = "pub type Bad = { tags: List Int } deriving (Row)\n";
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "deriving (Row) with a non-SqlType field must be rejected; got no errors"
    );
}

#[test]
fn deriving_row_on_union_is_rejected() {
    // A row maps columns to record fields; a union has no column layout.
    let main = "pub type Shape = Circle Int | Square Int deriving (Row)\n";
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "deriving (Row) on a union must be rejected; got no errors"
    );
}

// ── std.data Adapter seam + in-memory adapter ─────────────────────────────────

#[test]
fn adapter_mem_insert_and_all_typecheck() {
    // The in-memory adapter implements `Adapter`, so `insert`/`all` resolve the
    // instance on `MemAdapter` and type-check clean. `memAdapter` needs `db`, so
    // the callers declare it; the methods themselves are cap-free.
    let main = r#"
import std.data (memAdapter, insert, all)
import std.sql (toSql, SqlValue)
import std.map as Map

pub fn db save () -> Result Unit Error =
    insert (memAdapter ()) "users" (Map.fromList [("id", toSql 1)])

pub fn db load () -> Result (List (Map Text SqlValue)) Error =
    all (memAdapter ()) "users"
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "std.data Adapter surface must type-check clean; got {errors:?}"
    );
}

#[test]
fn adapter_insert_on_non_adapter_type_is_rejected() {
    // `Int` has no `Adapter` instance, so dispatching `insert` on it must fail
    // rather than silently resolve.
    let main = r#"
import std.data (insert)
import std.sql (SqlValue)

pub fn bad (row: Map Text SqlValue) -> Result Unit Error =
    insert 5 "users" row
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "insert on a non-Adapter receiver (Int) must be rejected; got no errors"
    );
}

#[test]
fn adapter_open_requires_db_capability() {
    // Opening an adapter is the act gated by `db`; a pure function that calls
    // `memAdapter` must be rejected. (The query methods themselves are cap-free.)
    let main = r#"
import std.data (memAdapter, all)
import std.sql (SqlValue)

pub fn opensWithoutDb () -> Result (List (Map Text SqlValue)) Error =
    all (memAdapter ()) "users"
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "calling memAdapter (db) from a pure function must be rejected; got no errors"
    );
}

#[test]
fn adapter_select_with_inline_annotated_predicate_typechecks() {
    // A predicate written inline captures when its parameter is annotated: the
    // annotation `(u: User)` pins the quoted entity (the method's `Quote (e ->
    // Bool)` leaves it generic), so the body is checked against User's columns
    // and `select` dispatches on MemAdapter.
    let main = r#"
import std.data (memAdapter, select)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int }

pub fn db adults () -> Result (List (Map Text SqlValue)) Error =
    select (memAdapter ()) "users" (fn (u: User) -> u.age >= 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "inline annotated predicate must type-check clean; got {errors:?}"
    );
}

#[test]
fn adapter_get_and_delete_typecheck() {
    // `get` looks a row up by an exact column match; `delete` takes a quoted
    // predicate and answers the count removed. Both resolve the MemAdapter
    // instance and type-check clean.
    let main = r#"
import std.data (memAdapter, get, delete)
import std.sql (SqlValue, toSql)

pub type User = { id: Int, age: Int }

pub fn db one () -> Result (Option (Map Text SqlValue)) Error =
    get (memAdapter ()) "users" "id" (toSql 1)

pub fn db purge () -> Result Int Error =
    delete (memAdapter ()) "users" (fn (u: User) -> u.age < 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "get/delete must type-check clean; got {errors:?}"
    );
}

#[test]
fn adapter_select_predicate_unknown_column_is_rejected() {
    // The quoted predicate is checked against the entity's columns, so a field
    // the record does not declare is a real error rather than being absorbed.
    let main = r#"
import std.data (memAdapter, select)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int }

pub fn db bad () -> Result (List (Map Text SqlValue)) Error =
    select (memAdapter ()) "users" (fn (u: User) -> u.nope >= 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unknown column in a quoted predicate must be rejected; got no errors"
    );
}

#[test]
fn repo_all_auto_decodes_to_typed_list() {
    // A repository pinned to `User` decodes `all` straight into `List User`:
    // the reconciled `Repo e a` threads the adapter and the entity, the
    // `Adapter MemAdapter` constraint resolves the in-memory backend, and the
    // `Row User` constraint resolves the `deriving (Row)` decoder.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db loadUsers () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    Repo.all users
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "Repo.all must auto-decode to List User clean; got {errors:?}"
    );
}

#[test]
fn repo_over_postgres_adapter_typechecks() {
    // The Postgres adapter resolves the same `Adapter` constraint as the
    // in-memory backend: `connect` (db-gated) builds a `Postgres` handle from a
    // `Config`, and a `Repo User Postgres` auto-decodes `all` into `List User`.
    // No database is touched — this exercises the type-level wiring (the
    // reconciled `Config`/`Postgres`, the `connect` scheme, and the
    // `Adapter Postgres` instance).
    let main = r#"
import std.data (connect, Config, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db loadUsers () -> Result (List User) Error =
    match connect (Config { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require", poolSize = 1 })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            Repo.all users
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "Repo over the Postgres adapter must typecheck clean; got {errors:?}"
    );
}

#[test]
fn connect_requires_the_db_capability() {
    // Opening a Postgres connection is the gated act: calling `connect` from a
    // pure function must be rejected, exactly as for `memAdapter`. (The handle's
    // later use is cap-free under the handle-as-proof model.)
    let main = r#"
import std.data (connect, Config, Postgres)

pub fn openIt () -> Result Postgres Error =
    connect (Config { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "disable", poolSize = 1 })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "calling connect (db) from a pure function must be rejected; got no errors"
    );
}

#[test]
fn repo_full_surface_typechecks_with_pipe_and_inline_predicates() {
    // The repository verbs read as a pipeline (`repo |> Repo.findBy ...`), the
    // predicate is an inline annotated lambda captured as a query tree, and the
    // read verbs auto-decode to `User` while the aggregate/write verbs answer
    // counts and units. One module exercises the whole surface.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue, toSql)
import std.map as Map

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db users () -> Repo User MemAdapter =
    Repo.repo (memAdapter ()) "users"

pub fn db adults () -> Result (List User) Error =
    users () |> Repo.findBy (fn (u: User) -> u.age >= 18)

pub fn db firstAdult () -> Result (Option User) Error =
    users () |> Repo.find (fn (u: User) -> u.age >= 18)

pub fn db byId () -> Result (Option User) Error =
    users () |> Repo.getBy "id" (toSql 1)

pub fn db howMany () -> Result Int Error =
    Repo.count (users ())

pub fn db howManyAdults () -> Result Int Error =
    users () |> Repo.countBy (fn (u: User) -> u.age >= 18)

pub fn db anyMinors () -> Result Bool Error =
    users () |> Repo.exists (fn (u: User) -> u.age < 18)

pub fn db add () -> Result Unit Error =
    users () |> Repo.insertRow (Map.fromList [("id", toSql 1)])

pub fn db purge () -> Result Int Error =
    users () |> Repo.deleteWhere (fn (u: User) -> u.age < 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the full Repo surface with pipes and inline predicates must be clean; got {errors:?}"
    );
}

#[test]
fn repo_predicate_unknown_column_is_rejected() {
    // The repository predicate is checked against the entity: a field the record
    // does not declare is an error, just as at the adapter seam.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db bad () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.findBy (fn (u: User) -> u.nope >= 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unknown column in a repository predicate must be rejected; got no errors"
    );
}

#[test]
fn qualified_imported_fn_call_is_type_checked() {
    // `import x as Lib` then `Lib.needsText` resolves to the producer's scheme.
    let main = "import proj.Lib as Lib\nfn ok () -> Text = Lib.needsText \"ok\"\n";
    let errors = typecheck_two_modules(main, LIB_FN);
    assert!(
        errors.is_empty(),
        "qualified cross-module call with correct arg must be clean; got {errors:?}"
    );

    let bad = "import proj.Lib as Lib\nfn bad () -> Text = Lib.needsText 123\n";
    let errors = typecheck_two_modules(bad, LIB_FN);
    assert!(
        count_mismatch(&errors) >= 1,
        "qualified cross-module call with bad arg must be rejected; got {errors:?}"
    );
}

#[test]
fn query_builder_pipeline_and_terminals_typecheck() {
    // The query builder reads as a pipeline: `query` lifts a repository, `filter`
    // narrows it, `orderBy` (multi-key), `limit`, and `offset` page it, and the
    // `toList`/`first` terminals decode the rows into the pinned entity. The
    // `orderBy` key is a quoted column whose return type is phantom.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc, Desc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db topAdults () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.orderBy Desc (fn (u: User) -> u.age)
      |> Repo.orderBy Asc (fn (u: User) -> u.name)
      |> Repo.limit 10
      |> Repo.offset 5
      |> Repo.toList

pub fn db oldest () -> Result (Option User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.first
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the query builder pipeline and terminals must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_over_postgres_typechecks() {
    // The builder resolves the same `Adapter` constraint on the Postgres backend:
    // `fetch` is a class method both adapters implement, so a `Query User Postgres`
    // runs its terminal through the Postgres instance with no extra annotation.
    let main = r#"
import std.data (connect, Config, Postgres)
import std.repo as Repo
import std.query (SortOrder, Desc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db topAdults () -> Result (List User) Error =
    match connect (Config { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require", poolSize = 4 })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            users
              |> Repo.query
              |> Repo.filter (fn (u: User) -> u.age >= 18)
              |> Repo.orderBy Desc (fn (u: User) -> u.age)
              |> Repo.limit 10
              |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the query builder over the Postgres adapter must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_filter_unknown_column_is_rejected() {
    // A `filter` predicate is checked against the entity, exactly like `findBy`:
    // a field the record does not declare is an error, proving the entity-typed
    // scheme threads through the builder rather than erasing to the row map.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db bad () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.filter (fn (u: User) -> u.nope >= 18) |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unknown column in a builder filter must be rejected; got no errors"
    );
}

#[test]
fn query_builder_projection_into_named_shape_typechecks() {
    // A projection names its result record (`Summary { … }`), which pins the
    // decode target so `selectList` answers `List Summary` and `selectFirst`
    // answers `Option Summary` — no binding annotation needed to fix the shape.
    // The projection runs after the filter/order/page accumulated on the query.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Desc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text, signupYear: Int } deriving (Row)
pub type Summary = { name: Text, year: Int } deriving (Row)

pub fn db summaries () -> Result (List Summary) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.orderBy Desc (fn (u: User) -> u.age)
      |> Repo.limit 10
      |> Repo.selectList (fn (u: User) -> Summary { name = u.name, year = u.signupYear })

pub fn db topSummary () -> Result (Option Summary) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.orderBy Desc (fn (u: User) -> u.age)
      |> Repo.selectFirst (fn (u: User) -> Summary { name = u.name, year = u.signupYear })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a named-shape projection must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_projection_over_postgres_typechecks() {
    // The projection resolves the same `Adapter`/`Row` constraints on Postgres:
    // `project` is a class method both adapters implement, so the select-list is
    // pushed down with no change to the call.
    let main = r#"
import std.data (connect, Config, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text, signupYear: Int } deriving (Row)
pub type Summary = { name: Text, year: Int } deriving (Row)

pub fn db summaries () -> Result (List Summary) Error =
    match connect (Config { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require", poolSize = 4 })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            users
              |> Repo.query
              |> Repo.filter (fn (u: User) -> u.age >= 18)
              |> Repo.selectList (fn (u: User) -> Summary { name = u.name, year = u.signupYear })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a named-shape projection over Postgres must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_projection_unknown_column_is_rejected() {
    // A projection field must be a column of the queried entity, exactly like a
    // filter predicate. Projecting a field the entity does not declare is an
    // error, proving the projection is checked against the entity rather than
    // erasing to the row map.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Summary = { label: Text } deriving (Row)

pub fn db bad () -> Result (List Summary) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.selectList (fn (u: User) -> Summary { label = u.nope })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unknown column in a projection must be rejected; got no errors"
    );
}

#[test]
fn query_builder_projection_must_name_its_shape() {
    // An anonymous projection (`{ … }`, no constructor) cannot pin the decode
    // target at a generic `selectList`, so it is rejected with guidance to name
    // the result record rather than failing opaquely.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db bad () -> Result (List Unit) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.selectList (fn (u: User) -> { name = u.name })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unnamed projection must be rejected; got no errors"
    );
}

#[test]
fn query_builder_join_to_pairs_typechecks() {
    // An inner join pairs the left query with a right repository on a quoted
    // condition over both entities, and `toPairs` decodes each matched row pair
    // into `(User, Post)`. The condition's left columns range over `User`, its
    // right over `Post`; both are pinned from the lambda's own annotations.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db authorPosts () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.orderBy Asc (fn (u: User) -> u.name)
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.toPairs
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a typed inner join into entity pairs must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_join_select_into_named_shape_typechecks() {
    // `selectJoin` names a result record built from columns of both entities,
    // which pins the decode target so the join answers `List Line` directly —
    // the two-table analogue of `selectList`.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Line = { who: Text, title: Text } deriving (Row)

pub fn db authorLines () -> Result (List Line) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.selectJoin (fn (u: User) (p: Post) -> Line { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a named-shape join projection must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_join_over_postgres_typechecks() {
    // The join resolves the same `Adapter`/`Row` constraints on Postgres: `join`
    // and `joinSelect` are class methods both adapters implement, so the call is
    // unchanged across backends.
    let main = r#"
import std.data (connect, Config, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Line = { who: Text, title: Text } deriving (Row)

pub fn db authorLines () -> Result (List Line) Error =
    match connect (Config { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require", poolSize = 4 })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            let posts: Repo Post Postgres = Repo.repo conn "posts"
            users
              |> Repo.query
              |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
              |> Repo.selectJoin (fn (u: User) (p: Post) -> Line { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a join over Postgres must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_join_unknown_column_is_rejected() {
    // The join condition is checked against both entities: a column neither
    // entity declares is an error, proving each side resolves against its own
    // record rather than erasing to the row map.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int } deriving (Row)

pub fn db bad () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.nope) |> Repo.toPairs
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unknown column in a join condition must be rejected; got no errors"
    );
}

#[test]
fn query_builder_join_condition_type_mismatch_is_rejected() {
    // The two sides of a join comparison must have the same column type. Equating
    // a `Text` column on one entity with an `Int` column on the other is a
    // mismatch, proving the per-side column types reach the comparison check.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.title) |> Repo.toPairs
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a cross-entity type mismatch in a join condition must be rejected; got no errors"
    );
}

#[test]
fn query_builder_join_select_must_name_its_shape() {
    // An anonymous join projection cannot pin the decode target, so it is
    // rejected with the same guidance as a single-table projection.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (List Unit) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.selectJoin (fn (u: User) (p: Post) -> { who = u.name })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unnamed join projection must be rejected; got no errors"
    );
}

#[test]
fn qualified_reconciled_fn_resolves_clean() {
    // `Query.orderSql` is seeded via the reconciled arena block (its signature
    // names the reconciled `SortOrder`), not the hand-curated signature table.
    // A qualified call must still find it in the env rather than fall through to
    // the T999 "qualified name unresolved" path.
    let main = "import std.query as Query (Asc)\n\
                import std.sql (Sql)\n\
                pub type Row = { age: Int }\n\
                fn ord (q: Quote (Row -> Int)) -> Sql = Query.orderSql Asc q\n";
    let errors = typecheck_two_modules(main, LIB_FN);
    assert_eq!(
        count_code(&errors, "T999"),
        0,
        "qualified `Query.orderSql` must resolve via the reconciled block; got {errors:?}"
    );
    assert!(
        errors.is_empty(),
        "the reconciled qualified call must type-check clean; got {errors:?}"
    );
}
