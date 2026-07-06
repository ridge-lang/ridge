//! std.test — the assertion module resolves and type-checks from user code.
//!
//! Runs the full `discover -> resolve -> typecheck` pipeline over a one-module
//! project that imports `std.test`. This exercises the primary audience (user
//! test code) without compiling the stdlib `.ridge` sources: the helpers'
//! schemes come from the builtin manifest, so the checks below prove the
//! signatures, the structural-equality assertions, and the `?`-chained flat
//! form all hold across the module boundary.

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

/// Type-check a single user module and return every `T###` error.
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

const IMPORT_ALL: &str = "import std.test (ensure, assertEq, assertNe, assertTrue, assertFalse, isOk, isErr, isSome, isNone)\n";

#[test]
fn every_helper_resolves_and_typechecks_clean() {
    let main = format!(
        "{IMPORT_ALL}\
         pub fn a () -> Result Unit Text = ensure true \"a\"\n\
         pub fn b () -> Result Unit Text = assertEq 1 1 \"b\"\n\
         pub fn c () -> Result Unit Text = assertNe 1 2 \"c\"\n\
         pub fn d () -> Result Unit Text = assertTrue true \"d\"\n\
         pub fn e () -> Result Unit Text = assertFalse false \"e\"\n\
         pub fn f () -> Result Unit Text = isOk (Ok 1) \"f\"\n\
         pub fn g () -> Result Unit Text = isErr (Err \"x\") \"g\"\n\
         pub fn h () -> Result Unit Text = isSome (Some 1) \"h\"\n\
         pub fn i () -> Result Unit Text = isNone None \"i\"\n"
    );
    let errors = typecheck_one(&main);
    assert!(
        errors.is_empty(),
        "every std.test helper must type-check clean; got {errors:?}"
    );
}

#[test]
fn question_mark_chain_flattens_clean() {
    // The flat form the module exists to enable: a `?`-chained sequence of
    // checks in place of a nested-`if` staircase.
    let main = format!(
        "{IMPORT_ALL}\
         pub fn checks () -> Result Unit Text =\n\
         \x20   ensure true \"a\" ?\n\
         \x20   assertEq (2 + 2) 4 \"b\" ?\n\
         \x20   assertNe 1 2 \"c\" ?\n\
         \x20   isOk (Ok 1) \"d\" ?\n\
         \x20   Ok ()\n"
    );
    let errors = typecheck_one(&main);
    assert!(
        errors.is_empty(),
        "the ?-chained assertion form must type-check clean; got {errors:?}"
    );
}

#[test]
fn assert_eq_mismatched_operand_types_is_rejected() {
    // assertEq is `forall a. a -> a -> Text -> ...`, so comparing an Int to a
    // Text must unify-fail rather than being silently accepted.
    let main =
        format!("{IMPORT_ALL}pub fn bad () -> Result Unit Text = assertEq 1 \"x\" \"label\"\n");
    let errors = typecheck_one(&main);
    assert!(
        errors.iter().any(|e| e.code() == "T001"),
        "assertEq over mismatched operand types must be rejected; got {errors:?}"
    );
}

#[test]
fn ensure_non_bool_condition_is_rejected() {
    let main = format!("{IMPORT_ALL}pub fn bad () -> Result Unit Text = ensure 5 \"y\"\n");
    let errors = typecheck_one(&main);
    assert!(
        !errors.is_empty(),
        "ensure with a non-Bool condition must be rejected; got no errors"
    );
}

#[test]
fn module_bodies_typecheck_structural_equality() {
    // Mirror of test.ridge's own bodies: structural `==` / `!=` on a bare type
    // var must type-check without a typeclass bound, and the Result/Option
    // matchers must resolve. This proves the module source is sound on its own,
    // independent of the whole-stdlib compile.
    let main = "\
pub fn ensure (cond: Bool) (msg: Text) -> Result Unit Text =
    if cond then Ok () else Err msg

pub fn assertEq (actual: a) (expected: a) (label: Text) -> Result Unit Text =
    if actual == expected then Ok () else Err label

pub fn assertNe (actual: a) (other: a) (label: Text) -> Result Unit Text =
    if actual == other then Err label else Ok ()

pub fn isOk (r: Result a e) (label: Text) -> Result Unit Text =
    match r
        Ok _  -> Ok ()
        Err _ -> Err label

pub fn isSome (o: Option a) (label: Text) -> Result Unit Text =
    match o
        Some _ -> Ok ()
        None   -> Err label
";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "std.test's own bodies (structural == on a polymorphic value) must \
         type-check clean; got {errors:?}"
    );
}

#[test]
fn qualified_import_resolves() {
    // `import std.test as T` then `T.ensure` resolves the same scheme.
    let main = "import std.test as T\npub fn ok () -> Result Unit Text = T.ensure true \"a\"\n";
    let errors = typecheck_one(main);
    assert!(
        errors.is_empty(),
        "qualified std.test import must type-check clean; got {errors:?}"
    );
}
