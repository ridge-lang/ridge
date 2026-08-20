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
use predicates::prelude::PredicateBooleanExt;
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

// ── Test 1b: a crashing expression ────────────────────────────────────────────

/// An expression that crashes in the REPL reports it the way `ridge run` does.
///
/// Same failure, same words: someone moving between the REPL and a real run is
/// looking at one language, and should not have to learn its error vocabulary
/// twice.
#[cfg(feature = "beam-runtime")]
#[test]
fn repl_crash_reads_like_a_run() {
    ridge_cmd()
        .arg("repl")
        .write_stdin("1 / 0\n:q\n")
        .assert()
        .stderr(
            contains("divided by zero")
                .and(contains("badarith").not())
                .and(contains("stack:").not()),
        );
}

/// The REPL runner is Erlang, so it does not live in this crate.
///
/// It was a Rust string constant here: a BEAM module inside the CLI, and the
/// one runner of five a second backend would have inherited rather than
/// replaced. Nothing but this stops it coming back — a string constant is
/// always the easiest place to put "just one" module.
#[test]
fn the_cli_crate_holds_no_erlang_module() {
    fn erlang_modules_under(dir: &std::path::Path, found: &mut Vec<String>, scanned: &mut usize) {
        let entries = std::fs::read_dir(dir).expect("read crate source directory");
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                erlang_modules_under(&path, found, scanned);
            } else if path.extension().is_some_and(|e| e == "rs") {
                *scanned += 1;
                let text = std::fs::read_to_string(&path).expect("read source file");
                if text.contains("-module(") {
                    found.push(path.display().to_string());
                }
            }
        }
    }

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut scanned = 0;
    erlang_modules_under(&src, &mut found, &mut scanned);

    // A walk that reaches nothing reports nothing, which is indistinguishable
    // from a clean result. The floor is well under the real count and only has
    // to rule that out.
    assert!(
        scanned > 10,
        "the walk only reached {scanned} source files, so a clean result proves nothing"
    );
    assert!(
        found.is_empty(),
        "Erlang belongs in ridge-codegen-erl beside the other runners; found it in: {found:?}"
    );
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
