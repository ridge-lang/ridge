//! Integration tests for `ridge test`.
//!
//! Tests that spawn a real BEAM process are gated behind the `beam-runtime`
//! feature (requires OTP installation with `erl` on PATH).
//!
//! Tests that only exercise pre-BEAM validation (arity, capability, no tests)
//! run without OTP and are un-gated.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::{write_file, TempWorkspace};
use predicates::str::contains;

// ── Helper ────────────────────────────────────────────────────────────────────

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// Build a minimal library workspace with one `.ridge` source file.
fn make_test_workspace(module_name: &str, source: &str) -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(&tw.path, &format!("apps/demo/src/{module_name}.ridge"), source);
    tw
}

// ── Test 1: test_canonical_smoke — pass (beam-runtime) ───────────────────────

/// `ridge test` runs a canonical `Result Unit Text` test and exits 0.
#[cfg(feature = "beam-runtime")]
#[test]
fn test_canonical_smoke() {
    // Use a simple constant test to avoid parse issues with chained operators.
    let src = "pub fn test_arith () -> Result Unit Text = Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .success()
        .stdout(contains("passed"));
}

// ── Test 2: test_filter — only runs matching test (beam-runtime) ──────────────

/// `ridge test --filter <pattern>` runs only the matching test.
#[cfg(feature = "beam-runtime")]
#[test]
fn test_filter() {
    let src = "\
pub fn test_only_this () -> Result Unit Text = Ok ()
pub fn test_other () -> Result Unit Text = Err \"should not run\"
";
    let tw = make_test_workspace("Demo", src);

    // With --filter, only test_only_this runs (test_other would fail, but it
    // does not run so the exit code is 0).
    ridge_cmd()
        .arg("test")
        .arg("--filter")
        .arg("*test_only_this*")
        .current_dir(&tw.path)
        .assert()
        .success();
}

// ── Test 3: test_failed_test — non-zero exit + stderr (beam-runtime) ──────────

/// `ridge test` exits 1 and emits the failure message when a test returns `Err`.
#[cfg(feature = "beam-runtime")]
#[test]
fn test_failed_test() {
    let src = "pub fn test_fails () -> Result Unit Text = Err \"expected failure\"\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("expected failure"));
}

// ── Test 4: test_bool_deprecation_warning — pass + C303 warning (beam-runtime) ─

/// `ridge test` runs a Bool-returning test successfully but emits C303 warning.
#[cfg(feature = "beam-runtime")]
#[test]
fn test_bool_deprecation_warning() {
    let src = "pub fn test_legacy () -> Bool = true\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .success()
        .stderr(contains("C303 BoolTestDeprecated"))
        .stdout(contains("Bool acceptance is removed in 0.2.0"));
}

// ── Test 5: test_ffi_rejection — C302 error, exit 1 (un-gated) ───────────────

/// `ridge test` rejects a test function that declares the `ffi` capability.
///
/// This check fires before BEAM spawn so it runs without OTP.
#[test]
fn test_ffi_rejection() {
    // A function with the ffi capability and a Body::Expr (not Body::Ffi,
    // which would require @ffi attribute and stdlib-only path checks).
    // We use `fn ffi test_ffi` syntax to declare the ffi capability.
    let src = "pub fn ffi test_ffi () -> Result Unit Text = Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C302 TestCapabilityForbidden"));
}

// ── Test 6: test_arity_invalid — C301 error, exit 1 (un-gated) ───────────────

/// `ridge test` rejects a test function that takes parameters.
///
/// This check fires before BEAM spawn so it runs without OTP.
#[test]
fn test_arity_invalid() {
    let src = "pub fn test_takes_arg (x: Int) -> Result Unit Text = Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C301 TestArityInvalid"));
}

// ── Test 7: test_no_tests_discovered — exit 0 + notice (un-gated) ─────────────

/// `ridge test` exits 0 with a "no tests discovered" notice when no `test_*`
/// functions exist in the workspace.
#[test]
fn test_no_tests_discovered() {
    let src = "pub fn helper -> Int = 42\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .success()
        .stdout(contains("no tests discovered"));
}
