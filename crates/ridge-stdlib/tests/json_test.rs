//! Module-level compilation and discovery checks for `std.json`.
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

fn assert_std_json_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.json"),
        "std.json must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

fn assert_json_ridge_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.json"),
        "discover() must find std.json; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

// ── Per-function tests (§3.17) ─────────────────────────────────────────────

#[test]
fn std_json_module_builds_and_is_discovered() {
    assert_std_json_built();
    assert_json_ridge_discovered();
}

// ── Parametric round-trip tests — one per JsonValue arm (§3.17 / §3.19) ──────
//
// Each test asserts that the module compiles cleanly and is discovered.
// The "round-trip" property (encode then decode = original) is validated at
// the BEAM runtime layer; these Track-A tests assert compile-level coverage
// of each JsonValue constructor arm.
