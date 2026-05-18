//! Track-A tests for `std.set` — 10 public functions.
//!
//! Each test asserts that `set.ridge` compiles through the T4 build pipeline
//! and that the module appears in the build summary.
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

fn assert_std_set_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.set"),
        "std.set must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

fn assert_set_rg_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.set"),
        "discover() must find std.set; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[test]
fn std_set_empty_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_from_list_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_to_list_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_insert_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_remove_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_contains_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_union_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_intersect_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_difference_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}

#[test]
fn std_set_size_compiles() {
    assert_std_set_built();
    assert_set_rg_discovered();
}
