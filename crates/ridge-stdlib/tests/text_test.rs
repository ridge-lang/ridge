//! Track-A tests for `std.text` — 16 public functions.
//!
//! Each test asserts that `text.rg` compiles through the T4 build pipeline
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

fn assert_std_text_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.text"),
        "std.text must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

fn assert_text_rg_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.text"),
        "discover() must find std.text; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[test]
fn std_text_byte_size_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_concat_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_split_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_split_n_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_split_any_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_lines_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_trim_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_to_upper_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_to_lower_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_starts_with_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_ends_with_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_contains_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_replace_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_pad_left_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_pad_right_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}

#[test]
fn std_text_is_empty_compiles() {
    assert_std_text_built();
    assert_text_rg_discovered();
}
