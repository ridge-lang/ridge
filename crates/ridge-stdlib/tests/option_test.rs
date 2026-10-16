//! Module-level compilation and discovery checks for `std.option`.
//!
//! These checks compile the library and confirm module discovery.
//! They do not exercise individual function behavior.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Mutex;

use ridge_stdlib::build_driver::{build_all, discover};

static BUILD_LOCK: Mutex<()> = Mutex::new(());

fn stdlib_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib")
}

fn assert_std_option_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.option"),
        "std.option must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

fn assert_option_ridge_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.option"),
        "discover() must find std.option; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[test]
fn std_option_module_builds_and_is_discovered() {
    assert_std_option_built();
    assert_option_ridge_discovered();
}
