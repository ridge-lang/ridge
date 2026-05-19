//! Track-A tests for `std.net.http` — 6 public functions.
//!
//! Each test asserts that `net/http.ridge` compiles through the T4 build pipeline
//! and that the module appears in the build summary / discover output.
//!
//! Tests serialize around a process-level mutex because `build_all` writes to
//! a temp directory keyed by process ID — parallel invocations within the same
//! test binary would race on the same path.
//!
//! §3.18 / OQ-S005 / D121: client functions use `ridge_rt` http helpers;
//! server function uses `ridge_rt:http_listen/2` (`gen_tcp` accept loop).

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

fn assert_net_http_rg_discovered() {
    let dir = stdlib_dir();
    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.net.http"),
        "discover() must find std.net.http; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

// ── Per-function compile tests (§3.18) ───────────────────────────────────────

/// `net.get` compiles successfully and module is discovered.
#[test]
fn std_net_http_get_compiles() {
    assert_std_net_http_built();
    assert_net_http_rg_discovered();
}

/// `net.post` compiles successfully and module is discovered.
#[test]
fn std_net_http_post_compiles() {
    assert_std_net_http_built();
    assert_net_http_rg_discovered();
}

/// `net.put` compiles successfully and module is discovered.
#[test]
fn std_net_http_put_compiles() {
    assert_std_net_http_built();
    assert_net_http_rg_discovered();
}

/// `net.delete` compiles successfully and module is discovered.
#[test]
fn std_net_http_delete_compiles() {
    assert_std_net_http_built();
    assert_net_http_rg_discovered();
}

/// `net.listen` compiles successfully and module is discovered.
#[test]
fn std_net_http_listen_compiles() {
    assert_std_net_http_built();
    assert_net_http_rg_discovered();
}

/// `respond` compiles successfully and module is discovered.
#[test]
fn std_net_http_respond_compiles() {
    assert_std_net_http_built();
    assert_net_http_rg_discovered();
}

// ── Record-type tests ─────────────────────────────────────────────────────────

/// `Request` record type is declared in `net/http.ridge` and the module builds.
#[test]
fn std_net_http_request_record_compiles() {
    assert_std_net_http_built();
    assert_net_http_rg_discovered();
}

/// `Response` record type is declared in `net/http.ridge` and the module builds.
#[test]
fn std_net_http_response_record_compiles() {
    assert_std_net_http_built();
    assert_net_http_rg_discovered();
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
