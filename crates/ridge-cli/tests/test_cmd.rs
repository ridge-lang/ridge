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
use predicates::prelude::PredicateBooleanExt;
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
    write_file(
        &tw.path,
        &format!("apps/demo/src/{module_name}.ridge"),
        source,
    );
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

// ── Regression: std.test module + non-pub @test run on BEAM ──────────────────

/// A `@test` function that imports `std.test` and chains `ensure`/`assertEq`
/// with `?` runs on BEAM and passes — even when the function is not `pub`.
///
/// Locks two runtime regressions: the `std.test` module's `.beam` was skipped by
/// the stdlib codegen because its name collided with the `.test`-file filter, and
/// a non-`pub` `@test` function was not exported so the runner could not call it
/// (both surfaced only at runtime, never at type-check).
#[cfg(feature = "beam-runtime")]
#[test]
fn test_stdlib_test_module_and_non_pub_test_run() {
    let src = "import std.test (ensure, assertEq)\n\n\
               @test \"non-pub std.test chain\"\n\
               fn checks () -> Result Unit Text =\n\
               \x20   ensure (1 + 1 == 2) \"arith\" ?\n\
               \x20   assertEq (2 * 3) 6 \"mul\" ?\n\
               \x20   Ok ()\n";
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

// ── Test 8: @test on private fn is discovered (un-gated) ─────────────────────

/// A private (non-`pub`) function annotated with `@test` is discovered as a
/// test — visibility is ignored when the attribute is present.
///
/// This check runs without OTP: the function has C301 arity-invalid because it
/// takes a parameter, which exercises the discovery path before any BEAM spawn.
/// We verify the test *was* discovered (C301 fires, not "no tests discovered").
#[test]
fn test_attr_private_fn_discovered() {
    // Private fn with @test — will hit ArityInvalid (takes a param) but that
    // proves discovery succeeded.  We cannot run the test without OTP.
    let src = "@test \"my private test\"\nfn private_check (x: Int) -> Result Unit Text = Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C301 TestArityInvalid"));
}

// ── Test 9: legacy test_* emits C304 warning (un-gated) ──────────────────────

/// A `pub fn test_*` function without `@test` emits `C304 PrefixTestDeprecated`
/// as a warning.  The test is still classified (and hits C301 here to avoid
/// needing OTP, proving discovery ran).
#[test]
fn test_legacy_prefix_emits_c304() {
    // pub fn test_* with wrong arity — C301 fires after C304 warning.
    let src = "pub fn test_legacy (x: Int) -> Result Unit Text = Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C304 PrefixTestDeprecated"))
        .stderr(contains("C301 TestArityInvalid"));
}

// ── Test 10: fn with both @test and test_* prefix registered once, no C304 ────

/// A function that carries `@test` AND has a `test_` prefix name is registered
/// once (via the attribute path) and does NOT emit `C304`.
///
/// We verify by checking that C304 is absent from stderr while the test is
/// still discovered (C301 proves discovery ran).
#[test]
fn test_attr_wins_over_prefix_no_c304() {
    let src =
        "@test \"explicit name\"\npub fn test_also_prefixed (x: Int) -> Result Unit Text = Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        // C304 must NOT appear — attribute path was taken.
        .stderr(predicates::str::contains("C304").not())
        // C301 confirms the test was actually discovered.
        .stderr(contains("C301 TestArityInvalid"));
}
