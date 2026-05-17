//! Integration tests for `ridge repl`.
//!
//! All 5 tests spawn the real REPL via `assert_cmd::Command::write_stdin`.
//! They require an OTP installation with `erl` and `erlc` on PATH, so they are
//! gated behind the `beam-runtime` feature.
//!
//! Run with:
//! ```text
//! cargo test -p ridge-cli --features beam-runtime --test repl
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[cfg(feature = "beam-runtime")]
use assert_cmd::Command;
#[cfg(feature = "beam-runtime")]
use predicates::str::contains;

// ── Helper ────────────────────────────────────────────────────────────────────

/// Build an `assert_cmd` Command for the `ridge` binary.
#[cfg(feature = "beam-runtime")]
fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

// ── Test 1: arithmetic expression ─────────────────────────────────────────────

/// `ridge repl` evaluates a simple arithmetic expression and prints the result.
///
/// Input:  `1 + 1\n:q\n`
/// Expect: stdout contains `2`.
#[cfg(feature = "beam-runtime")]
#[test]
fn repl_arithmetic() {
    ridge_cmd()
        .arg("repl")
        .write_stdin("1 + 1\n:q\n")
        .assert()
        .success()
        .stdout(contains("2"));
}

// ── Test 2: let-binding accumulation ──────────────────────────────────────────

/// `ridge repl` accumulates `let` bindings across lines.
///
/// `let x = 5` followed by `x + 1` on the next evaluation resolves to `6`
/// (§3.8 edge-case-2 / D162).
///
/// Input:  `let x = 5\nx + 1\n:q\n`
/// Expect: stdout contains `6`.
#[cfg(feature = "beam-runtime")]
#[test]
fn repl_let_binding_accumulation() {
    ridge_cmd()
        .arg("repl")
        .write_stdin("let x = 5\nx + 1\n:q\n")
        .assert()
        .success()
        .stdout(contains("6"));
}

// ── Test 3: type error rendering ──────────────────────────────────────────────

/// `ridge repl` renders type errors inline and continues the loop.
///
/// An expression with a type mismatch should produce a diagnostic on stderr
/// (or stdout via the renderer) and then the REPL should accept further input
/// and exit cleanly with code 0.
///
/// Input:  `1 + "bad"\n:q\n`
/// Expect: exit 0 (REPL continues after error), stderr contains an error
///         indicator.
#[cfg(feature = "beam-runtime")]
#[test]
fn repl_type_error_continues() {
    ridge_cmd()
        .arg("repl")
        .write_stdin("1 + \"bad\"\n:q\n")
        .assert()
        .success()
        .stderr(predicates::str::is_match("(?i)error|type|mismatch").unwrap());
}

// ── Test 4: :q clean exit ─────────────────────────────────────────────────────

/// `ridge repl` exits cleanly with code 0 when `:q` is typed.
///
/// Input:  `:q\n`
/// Expect: exit 0, no panic, no stack trace.
#[cfg(feature = "beam-runtime")]
#[test]
fn repl_quit_clean() {
    ridge_cmd()
        .arg("repl")
        .write_stdin(":q\n")
        .assert()
        .success();
}

// ── Test 5: capability invocation ─────────────────────────────────────────────

/// `ridge repl` allows capability-bearing expressions.
///
/// `Io.println "hi"` should succeed because the REPL session declares
/// `allow = ["io", ...]` (§3.8 edge-case-3 / D150) and pre-imports
/// `import std.io as Io`.
///
/// Input:  `Io.println "hi"\n:q\n`
/// Expect: exit 0, stdout contains `hi`.
#[cfg(feature = "beam-runtime")]
#[test]
fn repl_capability_invocation() {
    ridge_cmd()
        .arg("repl")
        .write_stdin("Io.println \"hi\"\n:q\n")
        .assert()
        .success()
        .stdout(contains("hi"));
}
