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
    let main = "import std.net.http (secureCookie, withSecure, secureCookieHeader)\nfn ok () -> Text = secureCookieHeader (withSecure false (secureCookie \"n\" \"v\"))\n";
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
    // A field whose type has no SqlType instance cannot be read from a column, so
    // `deriving (Row)` must fail rather than emit a decoder that references a missing
    // `fromSql`. A `List` of a base primitive is a column (an array), but a nested
    // `List (List Int)` is not — its element has no `SqlType` instance.
    let main = "pub type Bad = { grid: List (List Int) } deriving (Row)\n";
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "deriving (Row) with a non-SqlType field must be rejected; got no errors"
    );
}

#[test]
fn deriving_row_list_of_primitive_field_typechecks() {
    // A `List` of a base type is an array column: `deriving (Row)` accepts it and
    // `fromRow` decodes it through the parametric `SqlType (List a)` instance.
    let main = "import std.sql (fromRow, SqlValue)\n\
                pub type Post = { id: Int, tags: List Text } deriving (Row)\n\
                pub fn decode (r: Map Text SqlValue) -> Result Post Error = fromRow r\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "deriving (Row) with a List-of-primitive field must be clean; got {errors:?}"
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

#[test]
fn deriving_row_optional_primitive_field_typechecks() {
    // An `Option` of a base type is a nullable column: `deriving (Row)` accepts it
    // and `fromRow` reads a NULL or missing column as `None`, a value as `Some`.
    let main = "import std.sql (fromRow, SqlValue)\n\
                pub type User = { id: Int, nick: Option Text } deriving (Row)\n\
                pub fn decode (r: Map Text SqlValue) -> Result User Error = fromRow r\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "deriving (Row) with an Option-of-primitive field must be clean; got {errors:?}"
    );
}

#[test]
fn deriving_row_optional_non_primitive_field_is_rejected() {
    // An `Option` of a base type or an array is a column; `Option (Map Text Int)` has
    // no `SqlType` instance for its inner type, so `deriving (Row)` must reject it.
    let main = "pub type Bad = { meta: Option (Map Text Int) } deriving (Row)\n";
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "deriving (Row) with an Option of a non-SqlType field must be rejected; got no errors"
    );
}

#[test]
fn deriving_row_optional_list_field_typechecks() {
    // An `Option` of an array is a nullable array column: `deriving (Row)` accepts it
    // and `fromRow` reads a NULL column as `None`, an array as `Some` of the decoded
    // list, composing the nullable and array codecs.
    let main = "import std.sql (fromRow, SqlValue)\n\
                pub type Post = { id: Int, scores: Option (List Int) } deriving (Row)\n\
                pub fn decode (r: Map Text SqlValue) -> Result Post Error = fromRow r\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "deriving (Row) with an Option-of-List field must be clean; got {errors:?}"
    );
}

// ── std.data Adapter seam + in-memory adapter ─────────────────────────────────

#[test]
fn adapter_mem_insert_and_all_typecheck() {
    // The in-memory adapter implements `Adapter`, so `appendRow`/`all` resolve the
    // instance on `MemAdapter` and type-check clean. `memAdapter` needs `db`, so
    // the callers declare it; the methods themselves are cap-free.
    let main = r#"
import std.data (memAdapter, appendRow, all)
import std.sql (toSql, SqlValue)
import std.map as Map

pub fn db save () -> Result Unit Error =
    appendRow (memAdapter ()) "users" (Map.fromList [("id", toSql 1)])

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
    // `Int` has no `Adapter` instance, so dispatching `appendRow` on it must fail
    // rather than silently resolve.
    let main = r#"
import std.data (appendRow)
import std.sql (SqlValue)

pub fn bad (row: Map Text SqlValue) -> Result Unit Error =
    appendRow 5 "users" row
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "appendRow on a non-Adapter receiver (Int) must be rejected; got no errors"
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
fn adapter_select_rows_with_inline_annotated_predicate_typechecks() {
    // A predicate written inline captures when its parameter is annotated: the
    // annotation `(u: User)` pins the quoted entity (the method's `Quote (e ->
    // Bool)` leaves it generic), so the body is checked against User's columns
    // and `selectRows` dispatches on MemAdapter.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int }

pub fn db adults () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> u.age >= 18)
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
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int }

pub fn db bad () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> u.nope >= 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unknown column in a quoted predicate must be rejected; got no errors"
    );
}

#[test]
fn predicate_like_and_in_helpers_typecheck() {
    // The text-match helpers (`Text.like`/`contains`/`startsWith`/`endsWith`) and
    // the `IN` test (`List.contains col [literals]`) are recognised inside a quoted
    // predicate and check against the column's type, combining with `&&` like any
    // other comparison.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)
import std.text as Text
import std.list as List

pub type User = { id: Int, age: Int, name: Text }

pub fn db matches () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users"
        (fn (u: User) ->
            Text.contains u.name "a"
            && Text.startsWith u.name "l"
            && Text.endsWith u.name "n"
            && Text.like u.name "%a_"
            && List.contains u.age [18, 30, 25])
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "like/in predicate helpers must type-check clean; got {errors:?}"
    );
}

#[test]
fn predicate_in_list_type_mismatch_is_rejected() {
    // The `IN` set must match the column's type — a text literal against an Int
    // column is a real error, not silently absorbed.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)
import std.list as List

pub type User = { id: Int, age: Int }

pub fn db bad () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> List.contains u.age ["x"])
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an IN list whose elements mismatch the column type must be rejected; got no errors"
    );
}

#[test]
fn predicate_text_match_on_non_text_column_is_rejected() {
    // A text match applies only to a Text column; using it on an Int column is a
    // real error rather than being silently accepted.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)
import std.text as Text

pub type User = { id: Int, age: Int }

pub fn db bad () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> Text.contains u.age "1")
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a text match on a non-Text column must be rejected; got no errors"
    );
}

#[test]
fn predicate_arithmetic_typechecks() {
    // Arithmetic (`+ - * / %`) is accepted as a comparison operand: a column with a
    // literal, a column with a column, integer division and modulo — each combining
    // with `&&` like any other comparison.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text, score: Float }

pub fn db matches () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users"
        (fn (u: User) ->
            u.age * 2 > 50
            && u.age + u.id <= 100
            && u.age - 1 >= 0
            && u.age / 10 == 2
            && u.age % 2 == 0
            && u.score * 1.5 > 0.0)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "arithmetic comparison operands must type-check clean; got {errors:?}"
    );
}

#[test]
fn predicate_arithmetic_type_mismatch_is_rejected() {
    // Arithmetic operands must share one numeric type — an Int column plus a Text
    // column is a real error, not a silently coerced expression.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text }

pub fn db bad () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> u.age + u.name > 0)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "arithmetic over operands of different types must be rejected; got no errors"
    );
}

#[test]
fn predicate_modulo_on_float_is_rejected() {
    // `%` (modulo) is Int-only — Postgres does not define it on Float, so a Float
    // modulo is rejected rather than reaching a backend that cannot evaluate it.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)

pub type User = { id: Int, score: Float }

pub fn db bad () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> u.score % 2.0 > 0.0)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "modulo on a Float column must be rejected; got no errors"
    );
}

#[test]
fn predicate_division_by_literal_zero_is_rejected() {
    // A literal-zero divisor is a guaranteed error, caught at compile time rather
    // than left to abort the query (Postgres) or drop the row (in-memory) at run time.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int }

pub fn db bad () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> u.age / 0 == 1)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "division by a literal zero must be rejected; got no errors"
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
fn repo_findby_with_like_and_in_helpers_typechecks() {
    // The fluent `findBy` accepts a predicate built from the text-match helpers and
    // the bare `contains` IN test, dispatched through the same quote machinery as a
    // comparison — the path the Postgres e2e drives over the real wire.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)
import std.text as Text
import std.list (contains)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db hits () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.findBy (fn (u: User) -> Text.contains u.name "a" && contains u.age [18, 30])
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "findBy with like/in predicate helpers must type-check clean; got {errors:?}"
    );
}

#[test]
fn repo_findby_folded_like_in_probe_typechecks() {
    // The exact shape of the Postgres e2e probe: one repository, several `findBy`
    // checks over the text-match and `IN` helpers folded through `countOf` into a
    // comma-joined string. The e2e source only compiles under a live database, so
    // this locks the let-chain + `Int.toText` + `Text.join` shape here, over the
    // in-memory adapter (the backend does not change how the predicate type-checks).
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)
import std.text as Text
import std.list (contains)
import std.int as Int

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

fn listLen (us: List User) -> Int =
    match us
        []        -> 0
        _ :: rest -> 1 + listLen rest

fn countOf (res: Result (List User) Error) -> Int =
    match res
        Ok us -> listLen us
        Err _ -> 0 - 1

pub fn db checks () -> Text =
    let r: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let a = countOf (r |> Repo.findBy (fn (u: User) -> Text.contains u.name "a"))
    let b = countOf (r |> Repo.findBy (fn (u: User) -> Text.startsWith u.name "l"))
    let c = countOf (r |> Repo.findBy (fn (u: User) -> Text.like u.name "_a_"))
    let d = countOf (r |> Repo.findBy (fn (u: User) -> contains u.age [18, 30]))
    let e = countOf (r |> Repo.findBy (fn (u: User) -> contains u.age []))
    Text.join "," [Int.toText a, Int.toText b, Int.toText c, Int.toText d, Int.toText e]
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the folded like/in probe shape must type-check clean; got {errors:?}"
    );
}

#[test]
fn repo_findby_folded_arithmetic_probe_typechecks() {
    // The shape of the Postgres e2e arithmetic probe: one repository, several
    // `findBy` checks whose predicates carry arithmetic operands (column-with-literal,
    // column-with-column, integer division and modulo) folded through `countOf` into a
    // comma-joined string. The e2e source only compiles under a live database, so this
    // locks the probe shape here over the in-memory adapter.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)
import std.text as Text
import std.int as Int

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

fn listLen (us: List User) -> Int =
    match us
        []        -> 0
        _ :: rest -> 1 + listLen rest

fn countOf (res: Result (List User) Error) -> Int =
    match res
        Ok us -> listLen us
        Err _ -> 0 - 1

pub fn db checks () -> Text =
    let r: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let a = countOf (r |> Repo.findBy (fn (u: User) -> u.age * 2 > 50))
    let b = countOf (r |> Repo.findBy (fn (u: User) -> u.age + u.id > 20))
    let c = countOf (r |> Repo.findBy (fn (u: User) -> u.age / 10 == 2))
    let d = countOf (r |> Repo.findBy (fn (u: User) -> u.age % 2 == 0))
    Text.join "," [Int.toText a, Int.toText b, Int.toText c, Int.toText d]
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the folded arithmetic probe shape must type-check clean; got {errors:?}"
    );
}

#[test]
fn repo_over_postgres_adapter_typechecks() {
    // The Postgres adapter resolves the same `Adapter` constraint as the
    // in-memory backend: `connect` (db-gated) builds a `Postgres` handle from a
    // `PostgresConfig`, and a `Repo User Postgres` auto-decodes `all` into `List User`.
    // No database is touched — this exercises the type-level wiring (the
    // reconciled `PostgresConfig`/`Postgres`, the `connect` scheme, and the
    // `Adapter Postgres` instance).
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db loadUsers () -> Result (List User) Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
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
fn connect_with_tuned_pool_and_disconnect_typecheck() {
    // `connectWith` opens a Postgres connection with an explicit `PoolConfig`,
    // built by piping `defaultPool ()` through the `with*` setters, and
    // `disconnect` releases a connection on any adapter. Type-level wiring only:
    // exercises the reconciled `PoolConfig`, the `connectWith`/`defaultPool`/
    // `with*` schemes, and the generic `disconnect`.
    let main = r"
import std.data (connectWith, defaultPool, withPoolSize, withQueryTimeoutMs, withConnectTimeoutMs, withCheckoutTimeoutMs, withIdleTimeoutMs, withMaxLifetimeMs, withHealthCheckMs, withConnectRetries, withRetryBackoffMs, withMaxQueueDepth, PostgresConfig, PoolConfig, Postgres)
import std.repo as Repo

pub fn db openTuned (cfg: PostgresConfig) -> Result Unit Error =
    match connectWith cfg (defaultPool () |> withPoolSize 20 |> withQueryTimeoutMs 60000 |> withConnectTimeoutMs 8000 |> withCheckoutTimeoutMs 3000 |> withIdleTimeoutMs 300000 |> withMaxLifetimeMs 900000 |> withHealthCheckMs 30000 |> withConnectRetries 5 |> withRetryBackoffMs 250 |> withMaxQueueDepth 64)
        Err e   -> Err e
        Ok conn -> Repo.disconnect conn
";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "connectWith + PoolConfig builders + disconnect must typecheck clean; got {errors:?}"
    );
}

#[test]
fn tune_maintenance_windows_qualified_typecheck() {
    // The pool-maintenance setters tuned through a qualified module alias.
    // `Data.withIdleTimeoutMs` and friends resolve the same `Int -> PoolConfig ->
    // PoolConfig` scheme as the bare import, but the qualified `Module.verb` form
    // takes a different resolution path that a direct-import test would not cover.
    // Builds a pool through the alias and reads back a maintenance field, so the
    // reconciled `PoolConfig` and the qualified setters are both exercised without
    // a direct import.
    let main = r"
import std.data as Data

pub fn tunedIdle -> Int =
    (Data.defaultPool () |> Data.withIdleTimeoutMs 5000 |> Data.withMaxLifetimeMs 0 |> Data.withHealthCheckMs 100).idleTimeoutMs
";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "qualified PoolConfig maintenance builders must typecheck clean; got {errors:?}"
    );
}

#[test]
fn tune_retry_and_backpressure_qualified_typecheck() {
    // The retry and backpressure setters tuned through a qualified module alias.
    // `Data.withConnectRetries` and friends resolve the same `Int -> PoolConfig ->
    // PoolConfig` scheme as the maintenance setters; this builds a pool through the
    // alias and reads back the queue-depth field, so the reconciled `PoolConfig`
    // carries the new fields and the qualified setters resolve without a direct
    // import.
    let main = r"
import std.data as Data

pub fn tunedQueue -> Int =
    (Data.defaultPool () |> Data.withConnectRetries 5 |> Data.withRetryBackoffMs 250 |> Data.withMaxQueueDepth 64).maxQueueDepth
";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "qualified PoolConfig retry/backpressure builders must typecheck clean; got {errors:?}"
    );
}

#[test]
fn connect_requires_the_db_capability() {
    // Opening a Postgres connection is the gated act: calling `connect` from a
    // pure function must be rejected, exactly as for `memAdapter`. (The handle's
    // later use is cap-free under the handle-as-proof model.)
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)

pub fn openIt () -> Result Postgres Error =
    connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "disable" })
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
    users () |> Repo.query |> Repo.count

pub fn db howManyAdults () -> Result Int Error =
    users () |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 18) |> Repo.count

pub fn db anyMinors () -> Result Bool Error =
    users () |> Repo.query |> Repo.filter (fn (u: User) -> u.age < 18) |> Repo.exists

pub fn db add () -> Result Unit Error =
    users () |> Repo.insertRow (Map.fromList [("id", toSql 1)])

pub fn db purge () -> Result Int Error =
    users () |> Repo.delete (fn (u: User) -> u.age < 18)
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
fn query_builder_distinct_typechecks() {
    // `distinct` is a query-builder modifier (a `SELECT DISTINCT`) that composes
    // between `filter`/`orderBy` and a terminal. It drops the result's duplicate
    // rows over a whole-row `toList` and over the projected columns of a
    // `selectList`, and resolves through the `Adapter` seam like the other verbs.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Name = { name: Text } deriving (Row)

pub fn db distinctUsers () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 18) |> Repo.distinct |> Repo.toList

pub fn db distinctNames () -> Result (List Name) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.distinct |> Repo.orderBy Asc (fn (u: User) -> u.name) |> Repo.select (fn (u: User) -> Name { name = u.name })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "distinct must compose in the query pipeline over both terminals; got {errors:?}"
    );
}

#[test]
fn query_builder_set_operations_typecheck() {
    // `union`/`unionAll`/`intersect`/`except` combine two queries over the same
    // entity and adapter into a `Query e a`, so the result keeps composing through
    // `filter`/`orderBy`/`limit` and a terminal. A combined query also combines
    // again — `intersect` then `except` nests the plans.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db combined () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let adults = users |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 18)
    let admins = users |> Repo.query |> Repo.filter (fn (u: User) -> u.name == "admin")
    adults |> Repo.union admins |> Repo.orderBy Asc (fn (u: User) -> u.name) |> Repo.limit 10 |> Repo.toList

pub fn db nested () -> Result (Option User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let a = users |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 18)
    let b = users |> Repo.query |> Repo.filter (fn (u: User) -> u.age < 18)
    let c = users |> Repo.query |> Repo.filter (fn (u: User) -> u.name == "x")
    a |> Repo.intersect b |> Repo.except c |> Repo.first
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "set operations must compose and type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_scalar_aggregates_typecheck() {
    // The scalar aggregates are query-builder terminals over a quoted column.
    // `sumOf`/`minOf`/`maxOf` answer `Option` of the column's own type (summing an
    // `Int` column is `Option Int`; the greatest `name` is `Option Text`), while
    // `avgOf` is always `Option Float`, since a SQL average is fractional even over
    // an integer column. Each composes after the accumulated filter, and the
    // whole-table form is just `query` with no `filter`. The pinned return types
    // prove the column type flows from the accessor through the result.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db totalAge () -> Result (Option Int) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.sumOf (fn (u: User) -> u.age)

pub fn db meanAge () -> Result (Option Float) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.avgOf (fn (u: User) -> u.age)

pub fn db youngest () -> Result (Option Int) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.minOf (fn (u: User) -> u.age)

pub fn db lastName () -> Result (Option Text) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.maxOf (fn (u: User) -> u.name)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the scalar aggregate terminals must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_unique_and_universal_terminals_typecheck() {
    // The unique-row terminals decode like `first` but assert how many rows match:
    // `single` answers `Option User` (`None` for an empty match, an error for more
    // than one), `singleOrError` answers the bare `User` (the empty match is an
    // error too). `every` is the universal dual of `exists` — it takes a further
    // predicate and answers `Bool` — so it composes after the accumulated filter
    // without decoding a row. The pinned return types prove each terminal's shape.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db maybeAdmin () -> Result (Option User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.filter (fn (u: User) -> u.name == "admin") |> Repo.single

pub fn db theAdmin () -> Result User Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 1) |> Repo.singleOrError

pub fn db allAdult () -> Result Bool Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.every (fn (u: User) -> u.age >= 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the unique-row and universal terminals must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_group_by_and_summarize_typecheck() {
    // `groupBy` partitions a query by a key column; `summarize` projects each group
    // into a named record built from the group vocabulary — `g.key`, `g.count`, and
    // `g.sum`/`avg`/`min`/`max` over a column accessor. The aggregate types flow
    // from the columns (`g.sum salary : Int`, `g.avg age : Float`, `g.min age :
    // Int`), and `having` narrows the groups with a predicate over the same
    // vocabulary — including a cross-aggregate threshold (`g.sum … >= 100000`). The
    // pinned `List DeptStats` proves the named shape fixes the result.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, dept: Text, age: Int, salary: Int } deriving (Row)
pub type DeptStats = { dept: Text, members: Int, payroll: Int, avgAge: Float, youngest: Int, eldest: Int } deriving (Row)

pub fn db deptStats () -> Result (List DeptStats) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.groupBy (fn (u: User) -> u.dept)
      |> Repo.having (fn g -> g.count > 3)
      |> Repo.summarize (fn g -> DeptStats { dept = g.key, members = g.count, payroll = g.sum (fn (u: User) -> u.salary), avgAge = g.avg (fn (u: User) -> u.age), youngest = g.min (fn (u: User) -> u.age), eldest = g.max (fn (u: User) -> u.age) })

pub fn db wealthyDepts () -> Result (List DeptStats) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.groupBy (fn (u: User) -> u.dept)
      |> Repo.having (fn g -> g.sum (fn (u: User) -> u.salary) >= 100000)
      |> Repo.summarize (fn g -> DeptStats { dept = g.key, members = g.count, payroll = g.sum (fn (u: User) -> u.salary), avgAge = g.avg (fn (u: User) -> u.age), youngest = g.min (fn (u: User) -> u.age), eldest = g.max (fn (u: User) -> u.age) })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "groupBy/having/summarize must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_group_aggregate_unknown_column_is_rejected() {
    // A group aggregate's column accessor is checked against the entity, exactly
    // like a filter or an `orderBy` key: summing a field the record does not
    // declare is an error, proving the entity threads through the group vocabulary.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, dept: Text, age: Int } deriving (Row)
pub type Stats = { dept: Text, total: Int } deriving (Row)

pub fn db bad () -> Result (List Stats) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.groupBy (fn (u: User) -> u.dept)
      |> Repo.summarize (fn g -> Stats { dept = g.key, total = g.sum (fn (u: User) -> u.nope) })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unknown column in a group aggregate must be rejected; got no errors"
    );
}

#[test]
fn query_builder_over_postgres_typechecks() {
    // The builder resolves the same `Adapter` constraint on the Postgres backend:
    // `fetch` is a class method both adapters implement, so a `Query User Postgres`
    // runs its terminal through the Postgres instance with no extra annotation.
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.query (SortOrder, Desc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db topAdults () -> Result (List User) Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
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
      |> Repo.select (fn (u: User) -> Summary { name = u.name, year = u.signupYear })

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
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text, signupYear: Int } deriving (Row)
pub type Summary = { name: Text, year: Int } deriving (Row)

pub fn db summaries () -> Result (List Summary) Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            users
              |> Repo.query
              |> Repo.filter (fn (u: User) -> u.age >= 18)
              |> Repo.select (fn (u: User) -> Summary { name = u.name, year = u.signupYear })
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
    users |> Repo.query |> Repo.select (fn (u: User) -> Summary { label = u.nope })
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
    users |> Repo.query |> Repo.select (fn (u: User) -> { name = u.name })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unnamed projection must be rejected; got no errors"
    );
}

#[test]
fn query_builder_join_to_list_typechecks() {
    // An inner join pairs the left query with a right repository on a quoted
    // condition over both entities, and the unified `toList` decodes each matched
    // row pair into `(User, Post)`. The condition's left columns range over `User`,
    // its right over `Post`; both are pinned from the lambda's own annotations.
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
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a typed inner join into entity pairs must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_cross_join_typechecks() {
    // A cross join pairs the left query with a right repository and no condition —
    // the cartesian product — and reuses the inner-join `Join e f a`, so the whole
    // join vocabulary follows: `toList` decodes each pair into `(User, Color)`, and
    // a later `filter`/`orderBy` compose exactly as on an inner join.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Color = { id: Int, label: Text } deriving (Row)

pub fn db pairs () -> Result (List (User, Color)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let colors: Repo Color MemAdapter = Repo.repo (memAdapter ()) "colors"
    users
      |> Repo.query
      |> Repo.crossJoin colors
      |> Repo.toList

pub fn db filteredOrdered () -> Result (List (User, Color)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let colors: Repo Color MemAdapter = Repo.repo (memAdapter ()) "colors"
    users
      |> Repo.query
      |> Repo.crossJoin colors
      |> Repo.filter (fn (u: User) (c: Color) -> u.age >= 18)
      |> Repo.orderBy Asc (fn (u: User) (c: Color) -> c.label)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a cross join into entity pairs and its inherited vocabulary must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_filter_on_join_typechecks() {
    // The one `Repo.filter` takes a two-row predicate on a `Join`, narrowing the
    // join by a post-join `WHERE` over both entities — the same verb that takes a
    // one-row predicate on a `Query`. The functional dependency on `Refinable`
    // makes the predicate's arity follow the receiver.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db publishedPosts () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.filter (fn (u: User) (p: Post) -> p.title == "hello")
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a two-row filter on a join must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_paging_on_join_typechecks() {
    // The unified `limit`/`offset`/`distinct` are the methods of the single-
    // parameter `Pageable q` class, so they apply to a `Join` exactly as to a
    // `Query` — bounding the join's page and de-duplicating its rows — and the join
    // still decodes into `(User, Post)` through `toList`.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db pagedJoin () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.distinct
      |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id)
      |> Repo.offset 1
      |> Repo.limit 2
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "limit/offset/distinct must compose on a join through the Pageable class; got {errors:?}"
    );
}

#[test]
fn query_builder_paging_on_left_join_typechecks() {
    // The same `Pageable` methods over a `LeftJoin`: it keeps every left row and
    // decodes the right entity as `Option`, while `limit`/`offset`/`distinct` bound
    // and de-duplicate the kept rows.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db pagedLeftJoin () -> Result (List (User, Option Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.distinct
      |> Repo.offset 1
      |> Repo.limit 5
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "limit/offset/distinct must compose on a left join; got {errors:?}"
    );
}

#[test]
fn query_builder_count_exists_every_on_join_typechecks() {
    // `count`/`exists`/`every` are the methods of the two-parameter `Countable q p
    // | q -> p` class, so they apply to a `Join` exactly as to a `Query`: `count`
    // answers the joined-row count, `exists` whether any pair joins, and `every`
    // takes a two-row predicate over both entities (the arity the dependency fixes
    // for a join) and answers `Bool`. The pinned return types prove each shape.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db joinCount () -> Result Int Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.count

pub fn db joinExists () -> Result Bool Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.exists

pub fn db joinEvery () -> Result Bool Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.every (fn (u: User) (p: Post) -> p.title == "x")
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "count/exists/every must compose on a join through the Countable class; got {errors:?}"
    );
}

#[test]
fn query_builder_count_exists_every_on_left_join_typechecks() {
    // The same `Countable` methods over a `LeftJoin`: `count`/`exists` ignore the
    // optional right side entirely, and `every` takes the two-row predicate (its
    // right side the plain entity, as a left join's `filter` does) and answers
    // `Bool`. The left join keeps every left row, so `exists` is true whenever a
    // left row the predicate admits exists.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db leftJoinCount () -> Result Int Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.count

pub fn db leftJoinEvery () -> Result Bool Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.every (fn (u: User) (p: Post) -> p.authorId == u.id)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "count/exists/every must compose on a left join through the Countable class; got {errors:?}"
    );
}

#[test]
fn query_builder_group_by_on_join_typechecks() {
    // `groupBy`/`having`/`summarize` compose on a `Join` through the `Groupable`/
    // `Summarizable` classes: the key is a two-row accessor naming a column from
    // either side (`p.title` groups by the right table), `having` reads the group
    // vocabulary, and a `summarize` aggregate folds a column from either side —
    // `g.sum (fn u p -> p.score)` the right, `g.sum (fn u -> u.age)` the left. The
    // pinned `List CatStats` proves the named shape fixes the result.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text, score: Int } deriving (Row)
pub type CatStats = { cat: Text, n: Int, scores: Int, ages: Int } deriving (Row)

pub fn db joinGroup () -> Result (List CatStats) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.groupBy (fn (u: User) (p: Post) -> p.title)
      |> Repo.having (fn g -> g.count > 1)
      |> Repo.summarize (fn g -> CatStats { cat = g.key, n = g.count,
           scores = g.sum (fn (u: User) (p: Post) -> p.score), ages = g.sum (fn (u: User) -> u.age) })

pub fn db joinGroupHavingRight () -> Result (List CatStats) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.groupBy (fn (u: User) (p: Post) -> p.title)
      |> Repo.having (fn g -> g.sum (fn (u: User) (p: Post) -> p.score) >= 100)
      |> Repo.summarize (fn g -> CatStats { cat = g.key, n = g.count,
           scores = g.sum (fn (u: User) (p: Post) -> p.score), ages = g.sum (fn (u: User) -> u.age) })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "groupBy/having/summarize must compose on a join through Groupable/Summarizable; got {errors:?}"
    );
}

#[test]
fn query_builder_group_by_on_left_join_typechecks() {
    // The same grouped pipeline over a `LeftJoin`: the right side reads as the plain
    // entity `f` (as the left-join aggregates do), so the key and the right-column
    // aggregate name `p.title`/`p.score` directly. Every left row joins a group; an
    // unmatched one contributes a NULL right side.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text, score: Int } deriving (Row)
pub type CatStats = { cat: Text, n: Int, scores: Int } deriving (Row)

pub fn db leftJoinGroup () -> Result (List CatStats) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.groupBy (fn (u: User) (p: Post) -> p.title)
      |> Repo.having (fn g -> g.count > 0)
      |> Repo.summarize (fn g -> CatStats { cat = g.key, n = g.count,
           scores = g.sum (fn (u: User) (p: Post) -> p.score) })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "groupBy/having/summarize must compose on a left join; got {errors:?}"
    );
}

#[test]
fn query_builder_group_aggregate_unknown_join_column_is_rejected() {
    // A grouped-join aggregate's column accessor is checked against the side it
    // reads: summing `p.nope`, a column the right entity does not declare, is an
    // error, proving both entities thread through the join group vocabulary.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text, score: Int } deriving (Row)
pub type CatStats = { cat: Text, total: Int } deriving (Row)

pub fn db badJoinGroup () -> Result (List CatStats) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.groupBy (fn (u: User) (p: Post) -> p.title)
      |> Repo.summarize (fn g -> CatStats { cat = g.key,
           total = g.sum (fn (u: User) (p: Post) -> p.nope) })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "summing a column the right entity does not declare must be rejected"
    );
}

#[test]
fn query_builder_one_row_every_on_join_is_rejected() {
    // The dependency fixes `every`'s predicate arity to the receiver, exactly as for
    // `filter`: a `Join` takes a two-row predicate, so a one-row one is an arity
    // error rather than a silent mismatch.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result Bool Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.every (fn (u: User) -> u.age >= 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a one-row every predicate on a join must be rejected by the arity functional dependency"
    );
}

#[test]
fn query_builder_two_row_filter_on_query_is_rejected() {
    // The functional dependency fixes the predicate's arity to the receiver: a
    // `Query` takes a one-row predicate, so a two-row predicate is an arity
    // error rather than a silent mismatch.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db bad () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) (v: User) -> u.age >= 18)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a two-row predicate on a query must be rejected by the arity functional dependency"
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
      |> Repo.select (fn (u: User) (p: Post) -> Line { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a named-shape join projection must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_join_over_postgres_typechecks() {
    // The join resolves the same `Adapter`/`Row` constraints on Postgres: it
    // lowers onto `runPlan`, a class method both adapters implement, so the call
    // is unchanged across backends.
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Line = { who: Text, title: Text } deriving (Row)

pub fn db authorLines () -> Result (List Line) Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            let posts: Repo Post Postgres = Repo.repo conn "posts"
            users
              |> Repo.query
              |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
              |> Repo.select (fn (u: User) (p: Post) -> Line { who = u.name, title = p.title })
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
    users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.nope) |> Repo.toList
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
    users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.title) |> Repo.toList
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
      |> Repo.select (fn (u: User) (p: Post) -> { who = u.name })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unnamed join projection must be rejected; got no errors"
    );
}

#[test]
fn query_builder_left_join_to_list_typechecks() {
    // A left join keeps every left row, so the unified `toList` decodes each into
    // `(User, Option Post)` — the right entity is present only where the row
    // matched. The condition is written and checked exactly as for an inner join.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db authorPosts () -> Result (List (User, Option Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.orderBy Asc (fn (u: User) -> u.name)
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a left join into optional entity pairs must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_left_join_right_side_is_optional() {
    // The right entity of a left join is `Option Post`, not `Post`: an unmatched
    // left row has no right entity. Declaring the result as `(User, Post)` drops
    // the `Option` and must be rejected, proving the optionality is in the type.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a left join paired as non-optional `(User, Post)` must be rejected; got no errors"
    );
}

#[test]
fn query_builder_left_join_over_postgres_typechecks() {
    // The left join resolves the same `Adapter`/`Row` constraints on Postgres: it
    // lowers onto `runPlan`, a class method both adapters implement, so the call
    // is unchanged across backends.
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db authorPosts () -> Result (List (User, Option Post)) Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            let posts: Repo Post Postgres = Repo.repo conn "posts"
            users
              |> Repo.query
              |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
              |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a left join over Postgres must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_right_join_to_list_typechecks() {
    // A right join keeps every right row, so the unified `toList` decodes each into
    // `(Option User, Post)` — the mirror of a left join, with the left entity present
    // only where the row matched. The condition is written exactly as for an inner or
    // left join.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db postAuthors () -> Result (List (Option User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a right join into optional-left entity pairs must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_right_join_left_side_is_optional() {
    // The left entity of a right join is `Option User`, not `User`: an unmatched
    // right row has no left entity. Declaring the result as `(User, Post)` drops the
    // `Option` and must be rejected, proving the optionality moved to the left side.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a right join paired as non-optional `(User, Post)` must be rejected; got no errors"
    );
}

#[test]
fn query_builder_right_join_select_left_side_is_optional() {
    // A right-join projection reads the left side as `Option User`, so a projection
    // that names a left column on a plain `User` parameter must be rejected — the
    // nullable side is the left for a right join, the mirror of a left join's right.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Combo = { who: Option Text, title: Text } deriving (Row)

pub fn db postAuthors () -> Result (List Combo) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.select (fn (u: Option User) (p: Post) -> Combo { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a right-join projection reading the left side as Option must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_right_join_over_postgres_typechecks() {
    // The right join resolves the same `Adapter`/`Row` constraints on Postgres: it
    // lowers onto `runPlan`, a class method both adapters implement, so the call is
    // unchanged across backends.
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db postAuthors () -> Result (List (Option User, Post)) Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            let posts: Repo Post Postgres = Repo.repo conn "posts"
            users
              |> Repo.query
              |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
              |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a right join over Postgres must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_full_join_to_list_typechecks() {
    // A full join keeps every row of both tables, so the unified `toList` decodes each
    // into `(Option User, Option Post)` — both sides present only where the row
    // matched. The condition is written exactly as for an inner, left, or right join.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db everyone () -> Result (List (Option User, Option Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a full join into both-optional entity pairs must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_full_join_both_sides_optional() {
    // Both entities of a full join are optional: an unmatched left row has no right
    // entity and an unmatched right row has no left. Declaring the result as
    // `(User, Post)` drops both `Option`s and must be rejected.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a full join paired as non-optional `(User, Post)` must be rejected; got no errors"
    );
}

#[test]
fn query_builder_full_join_select_both_sides_optional() {
    // A full-join projection reads BOTH sides as `Option`, so a projection that names
    // a column on a plain `User`/`Post` parameter is rejected; reading them as
    // `Option User`/`Option Post` type-checks clean.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Combo = { who: Option Text, title: Option Text } deriving (Row)

pub fn db pairs () -> Result (List Combo) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.select (fn (u: Option User) (p: Option Post) -> Combo { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a full-join projection reading both sides as Option must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_full_join_over_postgres_typechecks() {
    // The full join resolves the same `Adapter`/`Row` constraints on Postgres: it
    // lowers onto `runPlan`, a class method both adapters implement, so the call is
    // unchanged across backends.
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db everyone () -> Result (List (Option User, Option Post)) Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            let posts: Repo Post Postgres = Repo.repo conn "posts"
            users
              |> Repo.query
              |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
              |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a full join over Postgres must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_left_join_unknown_column_is_rejected() {
    // The left-join condition is checked against both entities just like an inner
    // join: a column neither entity declares is an error.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int } deriving (Row)

pub fn db bad () -> Result (List (User, Option Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.nope) |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an unknown column in a left-join condition must be rejected; got no errors"
    );
}

#[test]
fn query_builder_query_first_typechecks() {
    // `first` is now a `Fetchable` method, so a query still answers its first
    // decoded entity (`Option User`) — the behaviour it had as a pub fn, now shared
    // with the join receivers through the functional dependency.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db firstAdult () -> Result (Option User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age >= 18)
      |> Repo.first
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "`first` over a query must answer `Option User`; got {errors:?}"
    );
}

#[test]
fn query_builder_join_first_typechecks() {
    // The unified `first` gives an inner join a terminal it never had: the first
    // matched row pair, decoded into `Option (User, Post)`. The fundep fixes the
    // row shape from the `Join` receiver exactly as `toList` does.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db firstPair () -> Result (Option (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.first
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "`first` over an inner join must answer `Option (User, Post)`; got {errors:?}"
    );
}

#[test]
fn query_builder_left_join_first_typechecks() {
    // `first` over a left join keeps the left-row guarantee in its shape: the first
    // row decodes into `Option (User, Option Post)`, the inner `Option` empty where
    // the first left row matched no right row.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db firstOptional () -> Result (Option (User, Option Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.first
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "`first` over a left join must answer `Option (User, Option Post)`; got {errors:?}"
    );
}

#[test]
fn query_builder_join_first_wrong_shape_is_rejected() {
    // The fundep fixes the row shape from the receiver: an inner join's `first` is
    // `Option (User, Post)`, so declaring it `Option (User, Option Post)` (the
    // left-join shape) must be rejected — the right side of an inner join is not
    // optional.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (Option (User, Option Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.first
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an inner join's `first` with an optional right side must be rejected; got no errors"
    );
}

#[test]
fn query_builder_select_left_join_into_named_shape_typechecks() {
    // `selectLeftJoin` projects a left join into a named record. Its right
    // parameter is `Option Post`, so a right column reads as `Option Text`; the
    // shape declares the right-derived field as `Option Text` to match, and an
    // unmatched left row projects it as `None`. The join condition keeps `p: Post`
    // (the match key), only the projection's right side is optional.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Line = { who: Text, title: Option Text } deriving (Row)

pub fn db authorLines () -> Result (List Line) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.select (fn (u: User) (p: Option Post) -> Line { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a left-join projection into a named shape with Option right fields must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_select_left_join_right_field_must_be_optional() {
    // A column read off the projection's `Option Post` parameter is `Option Text`,
    // so the result record's right-derived field must be `Option Text`, not a
    // plain `Text`. Declaring it `Text` drops the optionality and must be rejected
    // — proving the right side's columns are nullable in a left-join projection.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Line = { who: Text, title: Text } deriving (Row)

pub fn db bad () -> Result (List Line) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.select (fn (u: User) (p: Option Post) -> Line { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a left-join projection whose right field is non-optional `Text` must be rejected; got no errors"
    );
}

#[test]
fn query_builder_select_left_join_over_postgres_typechecks() {
    // The left-join projection resolves the same `Adapter`/`Row` constraints on
    // Postgres: it lowers onto `runPlan`, a class method both adapters implement.
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Line = { who: Text, title: Option Text } deriving (Row)

pub fn db authorLines () -> Result (List Line) Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            let posts: Repo Post Postgres = Repo.repo conn "posts"
            users
              |> Repo.query
              |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
              |> Repo.select (fn (u: User) (p: Option Post) -> Line { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a left-join projection over Postgres must type-check clean; got {errors:?}"
    );
}

#[test]
fn qualified_reconciled_fn_resolves_clean() {
    // `Query.orderSql` is seeded via the reconciled arena block (its signature
    // names the reconciled `SortOrder`), not the hand-curated signature table.
    // A qualified call must still find it in the env rather than fall through to
    // the T999 "qualified name unresolved" path.
    let main = "import std.query as Query (Asc)\n\
                import std.sql (Sql, SqlValue)\n\
                pub type Row = { age: Int }\n\
                fn ord (q: Quote (Row -> Int)) -> (Sql, List SqlValue) = Query.orderSql Asc q\n";
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

#[test]
fn typed_set_where_typechecks() {
    // `setWhere` applies a list of typed setters over the repository, with the
    // predicate explicit. Each `set (fn (u: User) -> u.col) value` quotes a single
    // column (its type read off the entity) and the value must match it.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, status: Text } deriving (Row)

pub fn db promote () -> Result Int Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let changes =
        [ Repo.set (fn (u: User) -> u.status) "adult"
        , Repo.set (fn (u: User) -> u.age) 99 ]
    users |> Repo.setWhere changes (fn (u: User) -> u.age > 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "typed `setWhere` with matching setter values must type-check clean; got {errors:?}"
    );
}

#[test]
fn typed_set_where_multiline_form_typechecks() {
    // The natural multi-line shape — pipe into `setWhere`, then the setter list
    // and the predicate on their own deeper-indented lines — now parses thanks to
    // the bracket-leading argument continuation (§5.5). Before that it tripped the
    // P006 layout error and the body never type-checked.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, status: Text } deriving (Row)

pub fn db promote () -> Result Int Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
        |> Repo.setWhere
            [ Repo.set (fn (u: User) -> u.status) "adult"
            , Repo.set (fn (u: User) -> u.age) 99 ]
            (fn (u: User) -> u.age > 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the multi-line `setWhere` form must type-check clean; got {errors:?}"
    );
}

#[test]
fn typed_apply_set_over_query_builder_typechecks() {
    // `applySet` is the query-builder write terminal: the accumulated `filter`
    // picks the rows, the setters assign their columns.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, status: Text } deriving (Row)

pub fn db promote () -> Result Int Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.filter (fn (u: User) -> u.age > 18)
      |> Repo.applySet [ Repo.set (fn (u: User) -> u.status) "adult" ]
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "`applySet` over the query builder must type-check clean; got {errors:?}"
    );
}

#[test]
fn typed_setter_value_type_must_match_column() {
    // The accessor pins the column's type, so a value of the wrong type is a
    // compile-time error — `u.age` is `Int`, assigning `Text` must be rejected.
    // This is the safety the typed setter buys over the untyped `updateWhere` map.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, status: Text } deriving (Row)

pub fn db bad () -> Result Int Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.setWhere [ Repo.set (fn (u: User) -> u.age) "not a number" ] (fn (u: User) -> u.id == 1)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a setter whose value type mismatches the column must be rejected; got no errors"
    );
}

#[test]
fn typed_set_nullable_column_typechecks() {
    // A nullable `Option` column takes an `Option` value: `set` encodes it through
    // the `SqlType (Option a)` instance, so `None` writes SQL NULL — no special
    // case over a plain column.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, nick: Option Text } deriving (Row)

pub fn db clearNick () -> Result Int Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.setWhere [ Repo.set (fn (u: User) -> u.nick) None ] (fn (u: User) -> u.id == 1)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a setter assigning `None` to a nullable column must type-check clean; got {errors:?}"
    );
}

#[test]
fn typed_set_where_over_postgres_typechecks() {
    // The same setters resolve the `Adapter` constraint on Postgres: `setWhere`
    // and `applySet` route through `updateRows`, a class method both adapters
    // implement.
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, status: Text } deriving (Row)

pub fn db promote () -> Result Int Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "users"
            users |> Repo.setWhere [ Repo.set (fn (u: User) -> u.status) "adult" ] (fn (u: User) -> u.age > 18)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "typed `setWhere` over Postgres must type-check clean; got {errors:?}"
    );
}

// ── Functional dependencies ───────────────────────────────────────────────────

#[test]
fn fundep_conflicting_instances_are_t046() {
    // `q -> p` means a determining type fixes the determined one; two instances
    // mapping `W1` to both `Int` and `Bool` break it.
    let main = "class Refinable q p | q -> p =\n    refine (pred: p) (x: q) -> q\n\ntype W1 = { a: Int }\n\ninstance Refinable W1 Int =\n    refine (pred: Int) (x: W1) -> W1 = x\n\ninstance Refinable W1 Bool =\n    refine (pred: Bool) (x: W1) -> W1 = x\n";
    let errors = typecheck_one(main);
    assert_eq!(
        count_code(&errors, "T046"),
        1,
        "two instances mapping W1 to both Int and Bool must violate the q -> p fundep; got {errors:?}"
    );
}

#[test]
fn fundep_consistent_instances_are_clean() {
    // Different determining types (`W1` vs `W2`) carry no conflict.
    let main = "class Refinable q p | q -> p =\n    refine (pred: p) (x: q) -> q\n\ntype W1 = { a: Int }\ntype W2 = { b: Int }\n\ninstance Refinable W1 Int =\n    refine (pred: Int) (x: W1) -> W1 = x\n\ninstance Refinable W2 Bool =\n    refine (pred: Bool) (x: W2) -> W2 = x\n";
    let errors = typecheck_one(main);
    assert_eq!(
        count_code(&errors, "T046"),
        0,
        "instances with different determining types do not conflict; got {errors:?}"
    );
}

#[test]
fn fundep_unknown_variable_is_t045() {
    // `z` is not a parameter of `Bad`.
    let main = "class Bad q p | q -> z =\n    bad (x: q) -> p\n";
    let errors = typecheck_one(main);
    assert_eq!(
        count_code(&errors, "T045"),
        1,
        "a fundep naming a non-parameter `z` must be T045; got {errors:?}"
    );
}

#[test]
fn fundep_determined_type_resolves_clean() {
    // The supplied determined type (`List Int`) matches what `q -> p` fixes for
    // `W1`, so the call type-checks clean.
    let main = "class Tagged q p | q -> p =\n    tagWith (tag: p) (x: q) -> q\n\ntype W1 = { a: Int }\n\ninstance Tagged W1 (List Int) =\n    tagWith (tag: List Int) (x: W1) -> W1 = x\n\nfn good () -> W1 =\n    tagWith [1] (W1 { a = 1 })\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a determined type matching the fundep must type-check clean; got {errors:?}"
    );
}

#[test]
fn fundep_wrong_determined_type_is_rejected() {
    // Both instances put `List` in the determined position, so dispatch by the
    // outer constructor alone would accept `List Bool` for `W1` (whose fundep
    // fixes `List Int`). Functional-dependency improvement catches the inner
    // mismatch and rejects it.
    let main = "class Tagged q p | q -> p =\n    tagWith (tag: p) (x: q) -> q\n\ntype W1 = { a: Int }\ntype W2 = { b: Int }\n\ninstance Tagged W1 (List Int) =\n    tagWith (tag: List Int) (x: W1) -> W1 = x\n\ninstance Tagged W2 (List Bool) =\n    tagWith (tag: List Bool) (x: W2) -> W2 = x\n\nfn bad () -> W1 =\n    tagWith [true] (W1 { a = 1 })\n";
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a determined type the fundep forbids (List Bool, not List Int) must be rejected; got {errors:?}"
    );
}

#[test]
fn fundep_open_determined_position_resolves() {
    // `p` appears only in the result of `zeroOf`, so the determining `q = W1`
    // fixes it through the fundep; without one this would be T030 ambiguous.
    let main = "class HasZero q p | q -> p =\n    zeroOf (x: q) -> p\n\ntype W1 = { a: Int }\n\ninstance HasZero W1 Int =\n    zeroOf (x: W1) -> Int = 0\n\nfn use_zero () -> Int =\n    let z = zeroOf (W1 { a = 1 })\n    z\n";
    let errors = typecheck_one(main);
    assert_eq!(
        count_code(&errors, "T030"),
        0,
        "the fundep fixes the determined result type, so no ambiguity; got {errors:?}"
    );
}

// ── Module-qualified class-method access (workspace user classes) ─────────────
//
// A type-class method declared in one module can be called through that module's
// alias from another (`L.describe x`), exactly like a bare method, with the
// receiver still selecting the instance. The qualified name resolves to the
// class method only when the method's class is declared in the aliased module.

/// Run discover -> resolve -> typecheck over a two-module project and return the
/// resolve-error codes alongside the type errors, so a resolve-stage R012 is
/// observable (the `typecheck_two_modules` helper drops resolve errors).
fn pipeline_two_modules(main_src: &str, lib_src: &str) -> (Vec<String>, Vec<TypeError>) {
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
    let resolve_codes: Vec<String> = resolved
        .errors
        .iter()
        .map(|(_, e)| e.code().to_owned())
        .collect();
    let result = typecheck_workspace(&resolved);
    let type_errors: Vec<TypeError> = result.errors.into_iter().map(|(_, e)| e).collect();
    (resolve_codes, type_errors)
}

const LIB_DESCRIBE: &str =
    "pub class Describe a =\n    describe (x: a) -> Text\n\ninstance Describe Int =\n    describe (x: Int) -> Text = \"int\"\n";

#[test]
fn qualified_workspace_class_method_resolves_and_typechecks() {
    // `L.describe` is the `Describe` method declared in `Lib`; reaching it through
    // the module alias resolves (no R012) and type-checks clean.
    let main = "import proj.Lib as L\n\nfn use (n: Int) -> Text =\n    L.describe n\n";
    let (resolve_codes, type_errors) = pipeline_two_modules(main, LIB_DESCRIBE);
    assert!(
        !resolve_codes.iter().any(|c| c == "R012"),
        "qualified workspace class method must not raise R012; got {resolve_codes:?}"
    );
    assert!(
        type_errors.is_empty(),
        "qualified workspace class method must type-check clean; got {type_errors:?}"
    );
}

#[test]
fn qualified_workspace_class_method_result_type_flows() {
    // `describe` returns `Text`; returning its result as `Int` must mismatch —
    // proving the qualified call dispatched to a real method scheme rather than
    // being absorbed as an unresolved-name error.
    let main = "import proj.Lib as L\n\nfn bad (n: Int) -> Int =\n    L.describe n\n";
    let (_resolve_codes, type_errors) = pipeline_two_modules(main, LIB_DESCRIBE);
    assert_eq!(
        count_code(&type_errors, "T001"),
        1,
        "Text result returned as Int must be one T001; got {type_errors:?}"
    );
}

#[test]
fn qualified_unknown_member_still_r012() {
    // A name that is neither a symbol nor a class method of the aliased module
    // keeps the existing unresolved-qualified-name diagnostic.
    let main = "import proj.Lib as L\n\nfn use (n: Int) -> Text =\n    L.bogus n\n";
    let (resolve_codes, _type_errors) = pipeline_two_modules(main, LIB_DESCRIBE);
    assert!(
        resolve_codes.iter().any(|c| c == "R012"),
        "an unknown qualified member must still raise R012; got {resolve_codes:?}"
    );
}

// ── Unified quote-predicate method over a functional dependency ───────────────
//
// A class `Refinable q p | q -> p` gives one `filter` method whose quoted
// predicate ranges over a 1-row receiver (`Qr`) or a 2-row one (`Jn`). The fundep
// fixes the predicate's arity from the receiver, so a 1-arg lambda works on `Qr`
// and a 2-arg lambda on `Jn` — and the wrong arity on either is rejected. This is
// the type-level core of the unified `Repo.filter` over `Query`/`Join`. The
// determined head is a function type written with the `fn` keyword so its arity
// (`Fn/1` vs `Fn/2`) distinguishes the instances.
const REFINABLE_BASE: &str = "class Refinable q p | q -> p =\n    filter (pred: Quote p) (x: q) -> q\n\ntype User = { age: Int, active: Bool }\ntype Post = { title: Text, published: Bool }\ntype Qr e = { marker: Int }\ntype Jn e f = { marker: Int }\n\ninstance Refinable (Qr e) (fn e -> Bool) =\n    filter (pred: Quote (fn e -> Bool)) (x: Qr e) -> Qr e = x\n\ninstance Refinable (Jn e f) (fn e f -> Bool) =\n    filter (pred: Quote (fn e f -> Bool)) (x: Jn e f) -> Jn e f = x\n\n";

#[test]
fn refinable_filter_dispatches_per_receiver_arity() {
    // A 1-arg predicate on the 1-row receiver and a 2-arg predicate on the 2-row
    // receiver both type-check through the single `filter` name.
    let src = format!(
        "{REFINABLE_BASE}fn useQuery (q: Qr User) -> Qr User =\n    q |> filter (fn (u: User) -> u.active)\n\nfn useJoin (j: Jn User Post) -> Jn User Post =\n    j |> filter (fn (u: User) (p: Post) -> p.published)\n"
    );
    let errors = typecheck_one(&src);
    assert!(
        errors.is_empty(),
        "unified filter must type-check a 1-arg predicate on Qr and a 2-arg on Jn; got {errors:?}"
    );
}

#[test]
fn refinable_filter_rejects_two_arg_predicate_on_one_row_receiver() {
    // The fundep fixes `Qr`'s predicate to one row, so a 2-arg lambda is rejected.
    let src = format!(
        "{REFINABLE_BASE}fn bad (q: Qr User) -> Qr User =\n    q |> filter (fn (u: User) (p: Post) -> p.published)\n"
    );
    let errors = typecheck_one(&src);
    assert!(
        !errors.is_empty(),
        "a 2-arg predicate on a 1-row receiver must be rejected; got {errors:?}"
    );
}

#[test]
fn refinable_filter_rejects_one_arg_predicate_on_two_row_receiver() {
    // The fundep fixes `Jn`'s predicate to two rows, so a 1-arg lambda is rejected.
    let src = format!(
        "{REFINABLE_BASE}fn bad (j: Jn User Post) -> Jn User Post =\n    j |> filter (fn (u: User) -> u.active)\n"
    );
    let errors = typecheck_one(&src);
    assert!(
        !errors.is_empty(),
        "a 1-arg predicate on a 2-row receiver must be rejected; got {errors:?}"
    );
}

// ── Unified `select` / `selectFirst` — first-row, arity, and injection ──────────

#[test]
fn query_builder_select_first_into_named_shape_typechecks() {
    // `selectFirst` is the one-row projection: the same named-record capture as
    // `select`, but answering `Option s` (a pushed-down `LIMIT 1`).
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Desc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text, signupYear: Int } deriving (Row)
pub type Summary = { name: Text, year: Int } deriving (Row)

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
        "a named-shape selectFirst must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_join_select_first_typechecks() {
    // `selectFirst` over an inner join — new under the unified `Projectable` class
    // (the old API had no first-row join projection). Answers `Option Line`.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Line = { who: Text, title: Text } deriving (Row)

pub fn db firstAuthorLine () -> Result (Option Line) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.selectFirst (fn (u: User) (p: Post) -> Line { who = u.name, title = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "selectFirst over a join must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_left_join_select_first_typechecks() {
    // `selectFirst` over a left join — the right side is `Option`, so an unmatched
    // left row projects its right-derived fields as `None`. Answers `Option ComboOpt`.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type ComboOpt = { person: Text, post: Option Text } deriving (Row)

pub fn db firstLeftCombo () -> Result (Option ComboOpt) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.selectFirst (fn (u: User) (p: Option Post) -> ComboOpt { person = u.name, post = p.title })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "selectFirst over a left join must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_select_rejects_two_row_projection_on_a_query() {
    // The fundep `q -> p` fixes a single-table query's projection to one row, so a
    // two-parameter projection lambda is a compile error, not a silent mismatch.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Summary = { name: Text } deriving (Row)

pub fn db bad () -> Result (List Summary) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.select (fn (u: User) (x: User) -> Summary { name = u.name })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a two-row projection on a single-table query must be rejected; got no errors"
    );
}

#[test]
fn query_builder_join_select_rejects_one_row_projection() {
    // The fundep fixes a join's projection to two rows (one per joined entity), so
    // a one-parameter projection lambda is rejected.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)
pub type Line = { who: Text } deriving (Row)

pub fn db bad () -> Result (List Line) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.select (fn (u: User) -> Line { who = u.name })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a one-row projection on a join must be rejected; got no errors"
    );
}

#[test]
fn query_builder_select_projection_literal_is_a_bind_not_injection() {
    // Security: a projection field can be a literal or a computed value, not only
    // a bare column. That is safe because every literal compiles to a `$N` bind
    // parameter, never interpolated SQL text — so the injection-shaped string
    // below travels as a parameter VALUE, not as SQL. The quote sub-language has
    // no raw-SQL node, so user-authored SQL text can never reach the `SELECT`
    // list. The `$N`/bind parameterization itself is asserted in the SQL-compile
    // e2e tests; here the literal field is accepted and stays typed against the
    // declared shape.
    let computed = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Summary = { name: Text } deriving (Row)

pub fn db ok () -> Result (List Summary) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.select (fn (u: User) -> Summary { name = "x'; DROP TABLE users; --" })
"#;
    let errors = typecheck_one(computed);
    assert!(
        errors.is_empty(),
        "a literal projection field is accepted (it binds as a parameter, not SQL text); got {errors:?}"
    );
}

#[test]
fn query_builder_select_allows_computed_projection() {
    // A projection field can be a computed value, not only a bare column —
    // arithmetic over the entity's columns lands in the select-list.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type Item = { id: Int, qty: Int, price: Int } deriving (Row)
pub type Stat = { total: Int } deriving (Row)

pub fn db ok () -> Result (List Stat) Error =
    let items: Repo Item MemAdapter = Repo.repo (memAdapter ()) "items"
    items |> Repo.query |> Repo.select (fn (i: Item) -> Stat { total = i.qty * i.price })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a computed (arithmetic) projection field must typecheck; got {errors:?}"
    );
}

#[test]
fn query_builder_select_allows_case_projection() {
    // A CASE (`if/then/else`) projects a value chosen per row; both branches share
    // the declared field's type.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type Item = { id: Int, spend: Int } deriving (Row)
pub type Tier = { label: Text } deriving (Row)

pub fn db ok () -> Result (List Tier) Error =
    let items: Repo Item MemAdapter = Repo.repo (memAdapter ()) "items"
    items |> Repo.query |> Repo.select (fn (i: Item) -> Tier { label = if i.spend > 100 then "gold" else "silver" })
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a CASE projection field must typecheck; got {errors:?}"
    );
}

#[test]
fn query_builder_filter_allows_case_predicate() {
    // A CASE whose branches are predicates is itself a boolean, usable in `filter`.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)

pub type User = { id: Int, vip: Bool, spend: Int }

pub fn db matches () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> if u.vip then u.spend > 100 else u.spend > 500)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a boolean CASE predicate must typecheck; got {errors:?}"
    );
}

#[test]
fn query_builder_case_branch_type_mismatch_is_rejected() {
    // The two branches of a value CASE must share one type — a Text branch and an
    // Int branch is a real error.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type Item = { id: Int, spend: Int } deriving (Row)
pub type Tier = { label: Text } deriving (Row)

pub fn db bad () -> Result (List Tier) Error =
    let items: Repo Item MemAdapter = Repo.repo (memAdapter ()) "items"
    items |> Repo.query |> Repo.select (fn (i: Item) -> Tier { label = if i.spend > 100 then "gold" else 0 })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a CASE with mismatched branch types must be rejected; got no errors"
    );
}

#[test]
fn query_builder_case_without_else_is_rejected() {
    // A CASE in a quote must have an else branch — there is no value for the
    // rows the condition does not match otherwise.
    let main = r#"
import std.data (memAdapter, selectRows)
import std.sql (SqlValue)

pub type User = { id: Int, vip: Bool, spend: Int }

pub fn db bad () -> Result (List (Map Text SqlValue)) Error =
    selectRows (memAdapter ()) "users" (fn (u: User) -> if u.vip then u.spend > 100)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a CASE without an else branch must be rejected; got no errors"
    );
}

#[test]
fn query_builder_case_non_boolean_condition_is_rejected() {
    // A CASE condition must be boolean — an Int column is not a condition.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type Item = { id: Int, spend: Int } deriving (Row)
pub type Tier = { label: Text } deriving (Row)

pub fn db bad () -> Result (List Tier) Error =
    let items: Repo Item MemAdapter = Repo.repo (memAdapter ()) "items"
    items |> Repo.query |> Repo.select (fn (i: Item) -> Tier { label = if i.spend then "gold" else "silver" })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a non-boolean CASE condition must be rejected; got no errors"
    );
}

#[test]
fn query_builder_select_computed_field_type_must_match_shape() {
    // A computed projection field's type must match the declared result field — a
    // Text field cannot be filled by an Int arithmetic expression.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type Item = { id: Int, qty: Int, price: Int } deriving (Row)
pub type Bad = { total: Text } deriving (Row)

pub fn db bad () -> Result (List Bad) Error =
    let items: Repo Item MemAdapter = Repo.repo (memAdapter ()) "items"
    items |> Repo.query |> Repo.select (fn (i: Item) -> Bad { total = i.qty * i.price })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a computed field whose type differs from the declared shape must be rejected; got no errors"
    );
}

#[test]
fn query_builder_select_projection_rejects_field_not_in_shape() {
    // A projection field that the named result record does not declare is rejected,
    // so the decode target and the select-list can never drift apart.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row)
pub type Summary = { name: Text } deriving (Row)

pub fn db bad () -> Result (List Summary) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users |> Repo.query |> Repo.select (fn (u: User) -> Summary { id = u.id })
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a projection field absent from the result record must be rejected; got no errors"
    );
}

// ── Unified orderBy ──────────────────────────────────────────────────────────

#[test]
fn query_builder_order_join_by_left_column_typechecks() {
    // The one `Repo.orderBy` takes a two-row key on a `Join`. A key over the left
    // entity sorts the join by a left-table column — the same verb that takes a
    // one-row key on a `Query`, the arity following the receiver through the
    // `Orderable` functional dependency.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db ordered () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.orderBy Asc (fn (u: User) (p: Post) -> u.name)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a two-row order key over the left entity must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_order_join_by_right_column_typechecks() {
    // A join's `orderBy` key may name a column of the *right* entity, sorting the
    // join by a right-table column. The key reads `p.title` (the right side), which
    // the seam qualifies to the right table; only the verb's arity is fixed by the
    // receiver, not which side a key names.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Desc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db ordered () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.orderBy Desc (fn (u: User) (p: Post) -> p.title)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a two-row order key over the right entity must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_order_left_join_by_right_option_column_typechecks() {
    // A left join's `orderBy` reads its right side as `Option` in the key
    // (`fn (u: User) (p: Option Post) -> p.title`), the same shape its projection
    // uses. The key still names a single right column; an unmatched row sorts as a
    // missing key.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db ordered () -> Result (List (User, Option Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.orderBy Asc (fn (u: User) (p: Option Post) -> p.title)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a left join's two-row order key over the optional right entity must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_two_row_order_key_on_query_is_rejected() {
    // The `Orderable` functional dependency fixes the key's arity to the receiver:
    // a `Query` takes a one-row key, so a two-row key is an arity error rather than
    // a silent mismatch — the orderBy dual of the filter arity check.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db bad () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.orderBy Asc (fn (u: User) (v: User) -> u.name)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a two-row order key on a query must be rejected by the arity functional dependency"
    );
}

#[test]
fn query_builder_one_row_order_key_on_join_is_rejected() {
    // The dual rejection: a `Join` takes a two-row key, so a one-row key is an
    // arity error.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.orderBy Asc (fn (u: User) -> u.name)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a one-row order key on a join must be rejected by the arity functional dependency"
    );
}

#[test]
fn query_builder_order_key_computed_is_a_bind_not_injection() {
    // A computed ordering key (arithmetic, a CASE) is accepted and safe: it compiles
    // to a parameterized `ORDER BY` whose literals bind as `$N` placeholders, never
    // interpolated as raw SQL. The key's type is still checked against the entity's
    // schema, so an unknown column or a type mismatch is still rejected — only the
    // column-only restriction is lifted.
    let computed = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db good () -> Result (List User) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.orderBy Asc (fn (u: User) -> u.age + 1)
      |> Repo.toList
"#;
    let errors = typecheck_one(computed);
    assert!(
        errors.is_empty(),
        "a computed ordering key compiles to a parameterized ORDER BY (a bind, not injection); got {errors:?}"
    );
}

#[test]
fn query_builder_order_join_by_unknown_right_column_is_rejected() {
    // The two-row order key resolves a right column against the right entity's
    // schema: a column the right entity does not declare is rejected, so a join's
    // ordering can never name a column that is not there.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (List (User, Post)) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.nope)
      |> Repo.toList
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an order key naming a column absent from the right entity must be rejected; got no errors"
    );
}

// ── Unified aggregates (Aggregable) ──────────────────────────────────────────

#[test]
fn query_builder_aggregate_join_by_left_column_typechecks() {
    // The one `Repo.sumOf` takes a two-row accessor on a `Join`. An accessor over
    // the left entity folds a left-table column — the same verb that takes a
    // one-row accessor on a `Query`, the arity following the receiver through the
    // `Aggregable` functional dependency. The result keeps the column's own type
    // (`Ret p` = `Int`).
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db totalAge () -> Result (Option Int) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.sumOf (fn (u: User) (p: Post) -> u.age)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a two-row aggregate accessor over the left entity must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_aggregate_join_by_right_column_typechecks() {
    // A join's aggregate accessor may name a column of the *right* entity, folding
    // a right-table column. `p.id` reads the right side, which the seam qualifies
    // to the right table; only the verb's arity is fixed by the receiver, not which
    // side a column names. `maxOf` keeps the column's own type (`Int`).
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db topPostId () -> Result (Option Int) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.maxOf (fn (u: User) (p: Post) -> p.id)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a two-row aggregate accessor over the right entity must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_avg_join_returns_float_typechecks() {
    // `avgOf` over a join answers `Option Float` regardless of the folded column's
    // type — a SQL average is fractional even over an integer column — so averaging
    // a right `Int` column type-checks against a `Float` result, not `Int`.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db avgPostId () -> Result (Option Float) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.avgOf (fn (u: User) (p: Post) -> p.id)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "avgOf over a join must answer Option Float; got {errors:?}"
    );
}

#[test]
fn query_builder_aggregate_left_join_by_right_column_typechecks() {
    // A left join's aggregate reads its right side as the plain entity `f` (not
    // `Option f` the way its per-row `select`/`orderBy` do), because an aggregate
    // folds values rather than producing one per row. `maxOf` over the right `Text`
    // column answers `Option Text` (the column's type, `None` over an empty fold);
    // an unmatched left row's right column is a NULL the fold skips.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db latestTitle () -> Result (Option Text) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.maxOf (fn (u: User) (p: Post) -> p.title)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "a left join's two-row aggregate over the right entity must type-check clean; got {errors:?}"
    );
}

#[test]
fn query_builder_two_row_aggregate_accessor_on_query_is_rejected() {
    // The `Aggregable` functional dependency fixes the accessor's arity to the
    // receiver: a `Query` takes a one-row accessor, so a two-row one is an arity
    // error — the aggregate dual of the filter/orderBy arity checks.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db bad () -> Result (Option Int) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.sumOf (fn (u: User) (v: User) -> u.age)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a two-row aggregate accessor on a query must be rejected by the arity functional dependency"
    );
}

#[test]
fn query_builder_one_row_aggregate_accessor_on_join_is_rejected() {
    // The dual rejection: a `Join` takes a two-row accessor, so a one-row one is an
    // arity error.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (Option Int) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.sumOf (fn (u: User) -> u.age)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a one-row aggregate accessor on a join must be rejected by the arity functional dependency"
    );
}

#[test]
fn query_builder_aggregate_accessor_computed_is_a_bind_not_injection() {
    // A computed aggregate accessor (`SUM(price * qty)`) is accepted and safe: it
    // compiles to a parameterized aggregate whose literals bind as placeholders,
    // never interpolated as raw SQL. The accessor's type is still checked against the
    // entity's schema, so an unknown column or a type mismatch is still rejected —
    // only the column-only restriction is lifted.
    let computed = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn db good () -> Result (Option Int) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    users
      |> Repo.query
      |> Repo.sumOf (fn (u: User) -> u.age + 1)
"#;
    let errors = typecheck_one(computed);
    assert!(
        errors.is_empty(),
        "a computed aggregate accessor compiles to a parameterized aggregate (a bind, not injection); got {errors:?}"
    );
}

#[test]
fn query_builder_aggregate_join_unknown_right_column_is_rejected() {
    // The two-row aggregate accessor resolves a right column against the right
    // entity's schema: a column the right entity does not declare is rejected, so a
    // join's aggregate can never name a column that is not there.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, authorId: Int, title: Text } deriving (Row)

pub fn db bad () -> Result (Option Int) Error =
    let users: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    let posts: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    users
      |> Repo.query
      |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)
      |> Repo.sumOf (fn (u: User) (p: Post) -> p.nope)
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "an aggregate accessor naming a column absent from the right entity must be rejected; got no errors"
    );
}

#[test]
fn transaction_over_a_multi_step_write_typechecks() {
    // `Repo.transaction` runs a body on the connection and threads its result out.
    // Here the body inserts two users and answers `Ok unit`, so the whole call is
    // `Result Unit Error`: the `Adapter MemAdapter` constraint resolves the
    // backend, and the body is a live callback whose capability row the call site
    // absorbs (a pure body keeps the call pure, like a list HOF).
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row, Schema)

pub fn db seed () -> Result Unit Error =
    let conn = memAdapter ()
    Repo.transaction conn (fn (tx) ->
        let users: Repo User MemAdapter = Repo.repo tx "users"
        match Repo.insert (UserInsert { name = "ada" }) users
            Err e -> Err e
            Ok _  -> Repo.insert (UserInsert { name = "lin" }) users)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "Repo.transaction over a multi-step write must typecheck clean; got {errors:?}"
    );
}

#[test]
fn transaction_threads_the_body_result_type() {
    // The result is the body's own success type, not fixed to the entity or Unit:
    // a body answering `Result Int Error` makes `transaction` answer `Result Int
    // Error`. The body counts the rows it just inserted through the query builder.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row, Schema)

pub fn db seededCount () -> Result Int Error =
    let conn = memAdapter ()
    Repo.transaction conn (fn (tx) ->
        let users: Repo User MemAdapter = Repo.repo tx "users"
        match Repo.insert (UserInsert { name = "ada" }) users
            Err e -> Err e
            Ok _  -> users |> Repo.query |> Repo.count)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "Repo.transaction must thread the body's Result Int out; got {errors:?}"
    );
}

#[test]
fn transaction_over_postgres_adapter_typechecks() {
    // The same combinator resolves the `Adapter` constraint for the Postgres
    // backend: `connect` builds the handle, and `transaction` runs the body on it.
    // No database is touched — this is the type-level wiring for the other backend.
    let main = r#"
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.sql (SqlValue)

pub type User = { id: Int, name: Text } deriving (Row, Schema)

pub fn db seed () -> Result Unit Error =
    match connect (PostgresConfig { host = "localhost", port = 5432, database = "app", user = "u", password = "p", sslMode = "require" })
        Err e   -> Err e
        Ok conn ->
            Repo.transaction conn (fn (tx) ->
                let users: Repo User Postgres = Repo.repo tx "users"
                Repo.insert (UserInsert { name = "ada" }) users)
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "Repo.transaction over the Postgres adapter must typecheck clean; got {errors:?}"
    );
}

#[test]
fn migrate_run_over_schema_typechecks() {
    // `Migrate.run` applies a list of migrations and answers the names applied
    // (`Result (List Text) Error`). The schema DSL builds each `createTable` from
    // typed columns with their modifiers, and an index over a column; the
    // `Adapter MemAdapter` constraint resolves the backend the runner drives.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.migrate as Migrate
import std.migrate (MigrationOp)

fn usersTable () -> MigrationOp =
    Migrate.createTable "users"
        [ Migrate.intCol  "id"    |> Migrate.primaryKey
        , Migrate.textCol "name"
        , Migrate.textCol "email" |> Migrate.unique
        , Migrate.textCol "bio"   |> Migrate.nullable ]

fn postsTable () -> MigrationOp =
    Migrate.createTable "posts"
        [ Migrate.intCol   "id"     |> Migrate.primaryKey
        , Migrate.intCol   "author"
        , Migrate.floatCol "score"
        , Migrate.boolCol  "live" ]

pub fn db setup () -> Result (List Text) Error =
    let conn = memAdapter ()
    let schema = [ Migrate.migration "0001_users" [ usersTable () ], Migrate.migration "0002_posts" [ postsTable (), Migrate.createIndex "posts_author_idx" "posts" ["author"] ] ]
    Migrate.run conn schema
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "Migrate.run over a typed schema must typecheck clean; got {errors:?}"
    );
}

#[test]
fn migrate_full_migration_op_surface_typechecks() {
    // The rest of the schema verbs — `addColumn`, `dropColumn`, `uniqueIndex`,
    // `dropTable` — all build `MigrationOp` values the `migration` builder and runner
    // accept.
    let main = r#"
import std.data (memAdapter, MemAdapter)
import std.migrate as Migrate

pub fn db alter () -> Result (List Text) Error =
    let conn = memAdapter ()
    let ops = [ Migrate.addColumn "users" (Migrate.intCol "age" |> Migrate.nullable), Migrate.dropColumn "users" "bio", Migrate.uniqueIndex "users_name_idx" "users" ["name"], Migrate.dropTable "posts" ]
    let schema = [ Migrate.migration "0003_alter" ops ]
    Migrate.run conn schema
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the full schema-op surface must typecheck clean; got {errors:?}"
    );
}

#[test]
fn migrate_run_over_postgres_typechecks() {
    // The same runner resolves the `Adapter` constraint for the Postgres backend:
    // given a Postgres handle, `Migrate.run` applies the schema on it. No database
    // is touched — this is the type-level wiring for the other backend.
    let main = r#"
import std.data (Postgres)
import std.migrate as Migrate
import std.migrate (MigrationOp)

fn usersTable () -> MigrationOp =
    Migrate.createTable "users"
        [ Migrate.intCol  "id"   |> Migrate.primaryKey
        , Migrate.textCol "name" ]

pub fn setup (conn: Postgres) -> Result (List Text) Error =
    let schema = [ Migrate.migration "0001_users" [ usersTable () ] ]
    Migrate.run conn schema
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "Migrate.run over the Postgres adapter must typecheck clean; got {errors:?}"
    );
}

#[test]
fn migrate_column_is_opaque_cross_module() {
    // `Column` is opaque: reading its representation from user code is rejected, so
    // the only way to build one is through the typed declarators and modifier steps.
    let main = r"
import std.migrate (Column)

fn leak (c: Column) -> Text = c.name
";
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "reading an opaque Column's field from user code must be rejected; got no errors"
    );
}

#[test]
fn raw_query_decode_and_exec_typecheck() {
    // The raw escape hatch over the in-memory adapter: `query` decodes the rows of a
    // parameterised SELECT into a `deriving (Row)` entity, `queryFirst` keeps the
    // first, and `exec` runs a row-less statement for its affected count. The
    // `Adapter MemAdapter` and `Row User` constraints both resolve.
    let main = r#"
import std.data (memAdapter)
import std.raw as Raw
import std.sql (sqlInt, sqlText, sqlBool)

pub type User = { id: Int, name: Text } deriving (Row)

pub fn db loadAdults () -> Result (List User) Error =
    let conn = memAdapter ()
    Raw.query conn "SELECT id, name FROM users WHERE age > $1" [sqlInt 18]

pub fn db firstNamed () -> Result (Option User) Error =
    let conn = memAdapter ()
    Raw.queryFirst conn "SELECT id, name FROM users WHERE name = $1" [sqlText "ada"]

pub fn db deactivate () -> Result Int Error =
    let conn = memAdapter ()
    Raw.exec conn "UPDATE users SET active = $1 WHERE id = $2" [sqlBool false, sqlInt 1]
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the raw escape hatch must typecheck clean; got {errors:?}"
    );
}

#[test]
fn raw_over_postgres_typechecks() {
    // The same verbs resolve the `Adapter` constraint for the Postgres backend:
    // given a Postgres handle, the raw query and statement type-check. No database
    // is touched — this is the type-level wiring for the other backend.
    let main = r#"
import std.data (Postgres)
import std.raw as Raw
import std.sql (sqlInt)

pub type User = { id: Int, name: Text } deriving (Row)

pub fn loadAdults (conn: Postgres) -> Result (List User) Error =
    Raw.query conn "SELECT id, name FROM users WHERE age > $1" [sqlInt 18]

pub fn affected (conn: Postgres) -> Result Int Error =
    Raw.exec conn "DELETE FROM users WHERE id = $1" [sqlInt 1]
"#;
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "the raw escape hatch over Postgres must typecheck clean; got {errors:?}"
    );
}

#[test]
fn raw_query_params_must_be_sql_values() {
    // `params` is a `List SqlValue`: a bare value that did not go through a `sqlInt`/
    // `sqlText`/… factory is a type error, so a bind can never smuggle an unencoded
    // value into the statement.
    let main = r#"
import std.data (memAdapter)
import std.raw as Raw

pub type User = { id: Int, name: Text } deriving (Row)

pub fn db bad () -> Result (List User) Error =
    let conn = memAdapter ()
    Raw.query conn "SELECT * FROM users WHERE age > $1" [18]
"#;
    let errors = typecheck_one(main);
    assert!(
        !errors.is_empty(),
        "a non-SqlValue bind must be rejected; got no errors"
    );
}
