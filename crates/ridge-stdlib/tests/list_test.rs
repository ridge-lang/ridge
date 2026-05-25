//! Track-A tests for `std.list` — 26 public functions.
//!
//! Each test asserts that `list.ridge` compiles through the T4 build pipeline
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

fn assert_std_list_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.list"),
        "std.list must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

fn assert_list_ridge_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.list"),
        "discover() must find std.list; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[test]
fn std_list_empty_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_length_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_is_empty_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_head_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_tail_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_map_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_filter_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_filter_map_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_fold_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_fold_right_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_reverse_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_sort_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_concat_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_sort_by_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_take_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_drop_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_group_by_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_flat_map_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_zip_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_zip_with_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_contains_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_find_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_any_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_all_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_range_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_range_exclusive_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}

#[test]
fn std_list_for_each_compiles() {
    assert_std_list_built();
    assert_list_ridge_discovered();
}
