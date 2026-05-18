//! Track-A tests for `std.cli` — 2 public functions.
//!
//! Each test asserts that `cli.ridge` compiles through the T4 build pipeline
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

fn assert_std_cli_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.cli"),
        "std.cli must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

fn assert_cli_rg_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.cli"),
        "discover() must find std.cli; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[test]
fn std_cli_args_compiles() {
    assert_std_cli_built();
    assert_cli_rg_discovered();
}

#[test]
fn std_cli_exit_compiles() {
    assert_std_cli_built();
    assert_cli_rg_discovered();
}
