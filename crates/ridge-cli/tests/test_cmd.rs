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
///
/// Doubles as the control for the crash-reporting tests below: a test that
/// returns `Err` did not crash, so it keeps its own message and gets none of
/// the apparatus around a failure the runtime had to describe.
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

// ── Crash reporting (beam-runtime) ───────────────────────────────────────────

/// A test that crashes reads like every other Ridge failure.
///
/// It read `FAIL: error:badarith` over a stack through the runtime's own source
/// files — the exact output `ridge run` stopped producing, on the command a
/// person is most likely to be staring at.
#[cfg(feature = "beam-runtime")]
#[test]
fn a_crashing_test_names_the_fault_instead_of_the_erlang() {
    let src = "pub fn test_divides_by_zero () -> Result Unit Text =\n\
        \x20   let d = 0\n\
        \x20   let _ = 10 / d\n\
        \x20   Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("FAIL: divided by zero")
                .and(contains("RIDGE_BACKTRACE"))
                .and(contains("badarith").not())
                .and(contains("stack:").not()),
        );
}

/// The Erlang is one variable away here too, and it is the same variable.
///
/// A reader who learned `RIDGE_BACKTRACE` from `ridge run` should not have to
/// discover that `ridge test` spells it differently, or does not have it.
#[cfg(feature = "beam-runtime")]
#[test]
fn a_crashing_test_still_has_its_stack_behind_the_same_variable() {
    let src = "pub fn test_divides_by_zero () -> Result Unit Text =\n\
        \x20   let d = 0\n\
        \x20   let _ = 10 / d\n\
        \x20   Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .env("RIDGE_BACKTRACE", "1")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("FAIL: divided by zero")
                .and(contains("stack:"))
                .and(contains("badarith")),
        );
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

// ── Test 6b: the two return-type rejections — C305 / C306 (un-gated) ─────────

/// A declared return type that is not the contract is `C305`.
///
/// The message deliberately does not offer `Bool` as an alternative: it is
/// still accepted, but `C303` deprecates it in the same run, and one message
/// should not push what another is retiring.
#[test]
fn a_wrong_return_type_reports_c305() {
    let src = "pub fn test_returns_int -> Int = 1\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C305 TestReturnTypeInvalid"))
        .stderr(contains("must return Result Unit Text"))
        .stderr(contains("Bool").not());
}

/// No declared return type is `C306`, not `C305`.
///
/// The two used to share one message, which told this case its return type was
/// unsupported — of a signature that declares none. Discovery reads the
/// declared signature, not the inferred type, so the remedy is to write one.
#[test]
fn a_missing_return_type_reports_c306() {
    let src = "pub fn test_unannotated = Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C306 TestReturnTypeMissing"))
        .stderr(contains("declares no return type"))
        .stderr(contains("C305").not());
}

/// Every rejection carries a code a reader can look up.
///
/// The point of the check is the negative: `ridge test` used to report one of
/// these three with no code at all, which leaves nothing to search for and
/// nothing to hand to `ridge explain`.
#[test]
fn every_rejection_carries_a_code() {
    let src = "pub fn test_takes_arg (x: Int) -> Result Unit Text = Ok ()\n\
               pub fn test_returns_int -> Int = 1\n\
               pub fn test_unannotated = Ok ()\n";
    let tw = make_test_workspace("Demo", src);

    let out = ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8_lossy(&out);

    for line in stderr.lines().filter(|l| l.starts_with("error: ")) {
        let code = line.trim_start_matches("error: ").split(' ').next();
        assert!(
            matches!(code, Some(c) if c.len() == 4
                && c.starts_with('C')
                && c[1..].chars().all(|ch| ch.is_ascii_digit())),
            "a rejection reached the terminal with no code: {line}"
        );
    }
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

// ── Warnings do not stop the suite, and are reported once ─────────────────────

/// A warning does not stop `ridge test`, and is printed once.
///
/// `test` walks the pipeline twice — type-check to discover the suite, then
/// compile to run it — and both passes carry the same warnings. Now that a
/// warning no longer stops the command at the first pass, rendering the whole
/// batch again at the second would show every warning twice.
///
/// Un-gated: the count holds with or without OTP, since the type-check pass
/// that prints the warning runs before anything needs `erlc`.
///
/// What is counted is `[T017]`, the bracketed form a render puts in its title,
/// not the bare code. The two are not the same thing to count: the batch also
/// closes with a line naming a code for `ridge explain`, and that mention is
/// not a second report of the warning. Counting the render is what this test
/// always meant; counting the string was a proxy that has stopped standing in
/// for it.
#[test]
fn a_warning_is_reported_once_and_does_not_stop_the_suite() {
    let source = "type Role = Admin | Guest\n\
                  \n\
                  pub fn tag () -> Int =\n\
                  \x20 match Guest\n\
                  \x20     Admin -> 1\n\
                  \x20     Guest -> 2\n\
                  \x20     _ -> 3\n\
                  \n\
                  @test \"tag picks the matching arm\"\n\
                  pub fn tagIsTwo () -> Result Unit Text =\n\
                  \x20 if tag () == 2 then Ok () else Err \"expected 2\"\n";
    let tw = make_test_workspace("Warned", source);

    let output = ridge_cmd()
        .arg("test")
        .current_dir(&tw.path)
        .output()
        .expect("ridge test spawn failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let both = format!("{stdout}{stderr}");

    assert_eq!(
        both.matches("[T017]").count(),
        1,
        "the warning must be reported exactly once across both pipeline \
         passes.\nstdout: {stdout}\nstderr: {stderr}"
    );

    if output.status.success() {
        assert!(
            stdout.contains("1 passed"),
            "the suite must run despite the warning, got: {stdout}"
        );
    }
}
