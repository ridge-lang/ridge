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
    let main = "import std.net.http (Sql)\nfn leak (s: Sql) -> Text = s.value\n";
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
    let main = "import std.net.http (sql, sqlValue)\nfn ok () -> Text = sqlValue (sql \"x\")\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "factory + accessor round-trip must type-check clean; got {errors:?}"
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
