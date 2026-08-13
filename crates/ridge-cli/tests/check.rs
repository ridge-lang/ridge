//! Integration tests for `ridge check`.
//!
//! Every test here runs without OTP — `ridge check` does not invoke `erlc`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::{make_example_workspace, make_multi_member_workspace, make_workspace};

// ── helpers ───────────────────────────────────────────────────────────────────

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

// ── Test 1–4: ridge check on each canonical example ──────────────────────────

/// `ridge check` on the `log_analyzer` example.
#[test]
fn check_log_analyzer() {
    let tw = make_example_workspace("log_analyzer");
    ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .assert()
        .success();
}

/// `ridge check` on the `url_shortener` example.
#[test]
fn check_url_shortener() {
    let tw = make_example_workspace("url_shortener");
    ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .assert()
        .success();
}

/// `ridge check` on the `game_of_life` example.
#[test]
fn check_game_of_life() {
    let tw = make_example_workspace("game_of_life");
    ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .assert()
        .success();
}

/// `ridge check` on the `rate_limiter` example.
#[test]
fn check_rate_limiter() {
    let tw = make_example_workspace("rate_limiter");
    ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .assert()
        .success();
}

// ── Test 5: type-incorrect fixture exits non-zero with a diagnostic ───────────

/// A return-type mismatch in canonical Ridge syntax causes `ridge check` to
/// exit non-zero with a typecheck diagnostic.
///
/// Canonical surface: `pub fn name -> Type = expr` (no parens, no braces;
/// body after `=`).  `pub fn foo -> Text = 42` is the "Int where Text was
/// declared" form that should fire `T001 TypeMismatch`.
#[test]
fn check_type_error() {
    let bad_source = "pub fn foo -> Text = 42\n";
    let tw = make_workspace("Broken", bad_source);

    let output = ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    assert!(
        !output.status.success(),
        "expected non-zero exit for type-mismatch source"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TypeMismatch") || stderr.contains("T001"),
        "expected TypeMismatch / T001 on stderr, got: {stderr}"
    );
}

/// A syntactically invalid Ridge source must NOT silently succeed.
///
/// Regression test for the "`parse_errors` silently dropped between parser and
/// driver" bug: a source like `pub fn foo () -> Text { 42 }` (Rust-style
/// braces, not Ridge's `= expr`) would parse to an empty item list, then
/// resolve+typecheck would see nothing and the CLI would falsely report
/// success.  After the fix, parse errors must surface as diagnostics.
#[test]
fn check_parse_error() {
    let bad_source = "pub fn foo () -> Text { 42 }\n";
    let tw = make_workspace("Broken", bad_source);

    let output = ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    assert!(
        !output.status.success(),
        "expected non-zero exit for parse-error source — \
         silent success would mask malformed code"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("parse error") || stderr.contains("expected"),
        "expected parse-error diagnostic on stderr, got: {stderr}"
    );
}

/// A failing `ridge check` must not tack a spurious `C001 NoWorkspaceRoot`
/// onto the real diagnostic — the workspace root WAS found; the check simply
/// failed. Regression test for the `AlreadyReported` migration gap.
#[test]
fn check_type_error_no_spurious_c001() {
    let bad_source = "pub fn foo -> Text = 42\n";
    let tw = make_workspace("Broken", bad_source);

    let output = ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("T001"),
        "expected the real T001 diagnostic, got: {stderr}"
    );
    assert!(
        !stderr.contains("C001") && !stderr.contains("NoWorkspaceRoot"),
        "spurious C001 NoWorkspaceRoot after the real diagnostic: {stderr}"
    );
}

// ── Warning-severity diagnostics ──────────────────────────────────────────────

/// A source whose only diagnostic is `T017 redundant pattern` — the third arm
/// is unreachable because the first two already cover `Role`.
const WARNING_SOURCE: &str = "type Role = Admin | Guest\n\
                              \n\
                              pub fn tag () -> Int =\n\
                              \x20 match Guest\n\
                              \x20     Admin -> 1\n\
                              \x20     Guest -> 2\n\
                              \x20     _ -> 3\n";

/// A warning is advisory: `ridge check` prints it and exits 0.
///
/// Warnings exist to say "you may want to look at this" without stopping the
/// build. While they exited non-zero, no lint could be added to the compiler
/// without breaking every project that tripped it.
#[test]
fn a_warning_does_not_fail_the_check() {
    let tw = make_workspace("Warned", WARNING_SOURCE);

    let output = ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "a warning-only check must exit 0.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("T017"),
        "the warning must still be printed, not swallowed: {stderr}"
    );
    assert!(
        stdout.contains("passed") && stdout.contains("1 warning"),
        "the summary line must count the warning it just printed, got: {stdout}"
    );
}

/// `--deny-warnings` is how a project that wants zero warnings asks for it.
#[test]
fn deny_warnings_makes_a_warning_fatal() {
    let tw = make_workspace("Warned", WARNING_SOURCE);

    let output = ridge_cmd()
        .arg("check")
        .arg("--deny-warnings")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "--deny-warnings must exit non-zero on a warning.\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("T017"),
        "the warning that failed the check must be shown: {stderr}"
    );
    assert!(
        !stdout.contains("passed"),
        "a denied warning must not print a success line, got: {stdout}"
    );
}

/// A warning next to an error must not launder the error into a pass.
#[test]
fn an_error_alongside_a_warning_still_fails() {
    let source = format!("{WARNING_SOURCE}\npub fn broken () -> Int = \"not an int\"\n");
    let tw = make_workspace("Warned", &source);

    let output = ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "an error must fail the check whatever else is in the batch.\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("T017"),
        "the warning is still worth printing alongside the error: {stderr}"
    );
    assert!(
        !stdout.contains("passed"),
        "no success line when an error was reported, got: {stdout}"
    );
}

// ── Test 6: --member selection ────────────────────────────────────────────────

/// `ridge check --member api` only checks the `api` member.
#[test]
fn check_member_filter() {
    let tw = make_multi_member_workspace();

    ridge_cmd()
        .arg("check")
        .arg("--member")
        .arg("api")
        .current_dir(&tw.path)
        .assert()
        .success();
}

// ── Test 7: a lambda is its own return boundary ──────────────────────────────

/// `return` inside a lambda is checked against the lambda's return type.
///
/// It used to be checked against the enclosing named function's, so this
/// rejected valid code whenever the two differed — `expected Int, got Text`,
/// where the `Text` is `outer`'s return type and has nothing to do with the
/// lambda (#502).
#[test]
fn check_return_inside_lambda_targets_the_lambda() {
    let source = "\
fn pick (f: fn Text -> Int) -> Int = f \"x\"

pub fn outer -> Text =
    let n = pick (fn (s: Text) -> Int =
        guard (s != \"\") else return 0
        1)
    \"done\"
";
    let tw = make_workspace("Lam", source);

    ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .assert()
        .success();
}

/// `?` inside a lambda targets the lambda too.
///
/// Same root cause: `?` lowers to `return Err e`, and the lambda has its own
/// catch frame, so the enclosing function's return type is the wrong context.
#[test]
fn check_propagate_inside_lambda_targets_the_lambda() {
    let source = "\
fn pick (f: fn Text -> Result Int Error) -> Result Int Error = f \"x\"
fn mk (s: Text) -> Result Int Error = Ok 1

pub fn outer -> Text =
    let r = pick (fn (s: Text) -> Result Int Error =
        let v = mk s ?
        Ok v)
    \"done\"
";
    let tw = make_workspace("Prop", source);

    ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .assert()
        .success();
}

/// A lambda's declared return type is checked against its body.
///
/// The parser used to consume the annotation and throw it away, so
/// `fn (s: Text) -> Int = "clearly text"` compiled clean — a declaration the
/// author wrote had no effect at all.
#[test]
fn check_lambda_return_annotation_is_enforced() {
    let source = "\
fn pick (f: fn Text -> Text) -> Text = f \"x\"

pub fn outer -> Text = pick (fn (s: Text) -> Int = \"clearly text\")
";
    let tw = make_workspace("Ann", source);

    let output = ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    assert!(
        !output.status.success(),
        "a lambda body that contradicts its declared return type must be reported"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("T001"),
        "expected T001 for the annotation mismatch, got: {stderr}"
    );
}

/// A `return` of the wrong type inside a lambda is still reported.
///
/// The boundary moved; it did not disappear.
#[test]
fn check_wrong_return_type_inside_lambda_is_reported() {
    let source = "\
fn pick (f: fn Text -> Int) -> Int = f \"x\"

pub fn outer -> Text =
    let n = pick (fn (s: Text) -> Int =
        guard (s != \"\") else return \"not an int\"
        1)
    \"done\"
";
    let tw = make_workspace("BadRet", source);

    let output = ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    assert!(
        !output.status.success(),
        "returning a Text where the lambda returns Int must be reported"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("T001"), "expected T001, got: {stderr}");
}

// ── Test 8: stdlib record types are checked like any other ───────────────────

/// A record literal built from a standard-library record type has its fields
/// checked.
///
/// `Response` is `{ status: Int, body: Text }`. Supplying a `Text` status and
/// an `Int` body used to type-check clean: the importing module resolved the
/// name to a stub scheme with no schema behind it, so the whole field list was
/// dropped without being looked at. Wrong types, unknown field names and
/// missing fields were all accepted (#497).
#[test]
fn check_stdlib_record_literal_fields() {
    let bad_source = "\
import std.net.http as Http (Response)

pub fn bad -> Response = Response { status = \"not an int\", body = 42 }
";
    let tw = make_workspace("Broken", bad_source);

    let output = ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    assert!(
        !output.status.success(),
        "expected non-zero exit for a stdlib record literal with wrong field types"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("T001"),
        "expected T001 on both fields, got: {stderr}"
    );
}

/// The same shape with the right types still passes.
///
/// The interpolation is the case that made #497 visible: with no type for its
/// hole the lowering passed the value through unconverted, so an `Int` hole in
/// this position emitted no conversion at all.
#[test]
fn check_stdlib_record_literal_accepts_correct_fields() {
    let good_source = "\
import std.net.http as Http (Response)

pub fn ok (url: Text) (n: Int) -> Response =
    Response { status = 302, body = $\"to ${url} after ${n}\" }
";
    let tw = make_workspace("Fine", good_source);

    ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .assert()
        .success();
}
