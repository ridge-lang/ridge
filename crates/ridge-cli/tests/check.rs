//! Integration tests for `ridge check`.
//!
//! All six tests run without OTP — `ridge check` does not invoke `erlc`.

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
