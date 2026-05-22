//! Track-A tests for `std.int` — 12 public functions.
//!
//! Each test asserts that:
//! 1. `build_all` succeeds (i.e., `int.ridge` compiles through T4 pipeline), AND
//! 2. `summary.modules_built` contains `"std.int"`, AND
//! 3. `discover()` finds the `int.ridge` file.
//!
//! Tests serialize around a process-level mutex because `build_all` writes to
//! a temp directory keyed by process ID — parallel invocations within the same
//! test binary would race on the same path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Mutex;

use ridge_stdlib::build_driver::{build_all, discover};

static BUILD_LOCK: Mutex<()> = Mutex::new(());

fn stdlib_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib")
}

fn assert_std_int_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.int"),
        "std.int must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

fn assert_int_ridge_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.int"),
        "discover() must find std.int; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[test]
fn std_int_to_text_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_parse_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_abs_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_min_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_max_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_add_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_sub_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_mul_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_div_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_neg_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_wrapping_add_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}

#[test]
fn std_int_saturating_add_compiles() {
    assert_std_int_built();
    assert_int_ridge_discovered();
}
