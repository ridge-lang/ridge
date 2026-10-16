//! Module-level compilation and discovery checks for `std.net.http`.
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

fn assert_std_net_http_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.net.http"),
        "std.net.http must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

fn assert_net_http_ridge_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.net.http"),
        "discover() must find std.net.http; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

// Module compilation and discovery.

#[test]
fn std_net_http_module_builds_and_is_discovered() {
    assert_std_net_http_built();
    assert_net_http_ridge_discovered();
}

// ── Discovery-path tests ──────────────────────────────────────────────────────

/// `discover` finds `std.net.http` at the `net/` subdirectory path.
///
/// This exercises the `module_path("std.net.http") == "net/http.ridge"` logic
/// in `build_driver::module_path` (T4 subdirectory handling).
#[test]
fn std_net_http_discovered_in_subdirectory() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    let m = modules
        .iter()
        .find(|m| m.name == "std.net.http")
        .expect("std.net.http must be discovered");
    // Tier 4
    assert_eq!(m.tier, 4, "std.net.http must be tier 4");
    // Path ends with net/http.ridge (using OS separators)
    let path_str = m.path.to_string_lossy();
    assert!(
        path_str.contains("net") && path_str.ends_with("http.ridge"),
        "path must end with net/http.ridge; got: {path_str}"
    );
}

/// `build_all` places `std.net.http` after tier-3 modules in output order.
#[test]
fn std_net_http_is_tier4_in_build_order() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();
    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    // All tier-3 modules must appear before std.net.http in modules_built.
    let net_pos = summary
        .modules_built
        .iter()
        .position(|m| m == "std.net.http")
        .expect("std.net.http must be in modules_built");
    let tier3_modules = [
        "std.io",
        "std.fs",
        "std.time",
        "std.random",
        "std.env",
        "std.cli",
        "std.proc",
    ];
    for t3 in tier3_modules {
        if let Some(t3_pos) = summary.modules_built.iter().position(|m| m == t3) {
            assert!(
                t3_pos < net_pos,
                "{t3} (tier 3) must appear before std.net.http (tier 4) in build order"
            );
        }
    }
}
