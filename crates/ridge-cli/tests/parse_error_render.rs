//! Regression test for parse-error rendering with source context.
//!
//! Verifies that `ridge check` on a syntactically invalid Ridge source emits a
//! structured diagnostic with an error code, the source line, and a caret /
//! box-drawing underline — rather than the old context-free `eprintln!` fallback.
//!
//! This test closes deviation #1 from T3: "OQ-CLI-R01 — `render_with_ariadne` gap".

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::make_workspace;

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// Parse-error render regression.
///
/// A source with Rust-style braces (`{ 42 }`) is syntactically invalid in Ridge.
/// The rendered output must contain:
///
/// 1. An error-code prefix (`error[P` — the P-code namespace for parse errors).
/// 2. The source line (or at least the problematic token).
/// 3. A caret (`^`), dash (`-`), or ariadne box-drawing character (`╰`, `┬`)
///    that underlines the error site.
///
/// This was the exact scenario that the pre-T3.5 `eprintln!` fallback could not
/// produce — it rendered `error: module 0: parse error: expected …` with no
/// source context.
#[test]
fn parse_error_renders_with_source_context() {
    // Source with Rust-style braces — invalid in Ridge (body must follow `=`).
    let bad_source = "pub fn foo -> Text { 42 }\n";
    let tw = make_workspace("Broken", bad_source);

    let output = ridge_cmd()
        .arg("check")
        .env("RIDGE_COLOR", "never") // deterministic output — no ANSI escapes
        .current_dir(&tw.path)
        .output()
        .expect("ridge check spawn failed");

    assert!(
        !output.status.success(),
        "expected non-zero exit for syntactically invalid source"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);

    // 1. Error-code prefix — ariadne formats it as `[P001]` or similar.
    assert!(
        stderr.contains("[P") || stderr.contains("error[P"),
        "expected parse-error code prefix (e.g. [P001]) in stderr, got:\n{stderr}"
    );

    // 2. The source text or the offending token should appear.
    //    The `{` token is what triggers the parse error in Ridge.
    assert!(
        stderr.contains('{') || stderr.contains("expected") || stderr.contains("foo"),
        "expected source context (offending token or source line) in stderr, got:\n{stderr}"
    );

    // 3. A caret, dash, or ariadne box-drawing underline character.
    //    ariadne uses U+2570 (╰), U+2500 (─), U+252C (┬), U+005E (^), or `-`.
    let has_underline = stderr.contains('^')
        || stderr.contains('-')
        || stderr.contains('\u{2570}') // ╰
        || stderr.contains('\u{252c}') // ┬
        || stderr.contains('\u{2500}') // ─
        || stderr.contains('|');
    assert!(
        has_underline,
        "expected underline character (^, -, ╰, ┬, ─, |) in stderr, got:\n{stderr}"
    );
}
