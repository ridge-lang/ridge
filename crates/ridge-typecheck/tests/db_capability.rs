//! End-to-end coverage for the `db` capability (spec §6.1).
//!
//! `db` is the narrow grant for database access: the Postgres/SQLite adapters
//! bridge it to `net`/`fs` inside the runtime, so query sites never hold raw
//! network or filesystem access. These tests prove it rides the generic
//! capability machinery — it parses in prefix position, propagates through the
//! call graph, and a function that doesn't declare it cannot call one that does.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypecheckResult};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

fn typecheck_src(src: &str) -> TypecheckResult {
    let td = tempfile::TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "apps/demo/src/main.ridge", src);

    let disc = discover_workspace(td.path());
    let ws = disc.graph.expect("workspace graph");
    let resolved = resolve_workspace(ws);
    let result = typecheck_workspace(&resolved);
    drop(td);
    result
}

fn has_error(result: &TypecheckResult, code: &str) -> bool {
    result.errors.iter().any(|(_, e)| e.code() == code)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `db` parses in capability-prefix position, and declaring it without using it
/// is allowed (declared ⊇ inferred — over-declaration is fine).
#[test]
fn db_declared_in_prefix_position_typechecks() {
    let result = typecheck_src("fn db queryUser (id: Int) -> Int = id");
    assert!(
        result.errors.is_empty(),
        "a `db` decl with a pure body should typecheck cleanly, got {:?}",
        result.errors
    );
}

/// A function without `db` cannot call one that declares it: the callee's `db`
/// leaks into the caller's inferred set, which exceeds its (pure) declaration.
#[test]
fn non_db_caller_cannot_call_db_function() {
    let src = "\
fn db queryUser (id: Int) -> Int = id
fn lookup (id: Int) -> Int = queryUser id
";
    let result = typecheck_src(src);
    assert!(
        has_error(&result, "T014"),
        "a pure fn calling a `db` fn must be rejected (T014), got {:?}",
        result.errors
    );
}

/// When the caller also declares `db`, the call is allowed.
#[test]
fn db_caller_can_call_db_function() {
    let src = "\
fn db queryUser (id: Int) -> Int = id
fn db lookup (id: Int) -> Int = queryUser id
";
    let result = typecheck_src(src);
    assert!(
        result.errors.is_empty(),
        "a `db` caller calling a `db` fn should typecheck cleanly, got {:?}",
        result.errors
    );
}
