//! Diagnostics raised while lowering reach the caller.
//!
//! They did not, for as long as the phase has existed. `LowerCtx` accumulated
//! them, `finish_with_items` consumed the context and dropped them, no field on
//! `LoweredModule` held them, no adapter turned them into a `Diagnostic`, and no
//! caller looked. A literal too large for `Int` therefore became a zero and
//! `build` reported success over it.
//!
//! Emitting `.core` rather than `.beam` keeps these off the `erlc` guard: the
//! question is what the front end reports, and codegen never runs on a program
//! that fails here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::make_workspace;
use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// The clone reads as redundant because `tw` is not touched again, but the
// temporary directory has to outlive the compile and moving the path out of it
// would take the workspace with it.
#[allow(clippy::redundant_clone)]
fn diagnose(source: &str) -> Vec<(String, String)> {
    let tw = make_workspace("Main", source);
    let artefacts =
        compile_workspace(CompileOptions::new(tw.path.clone()).with_emit(EmitArtefacts::Core))
            .expect("compile ran");
    artefacts
        .diagnostics
        .iter()
        .map(|d| (d.code.to_owned(), d.primary_message.clone()))
        .collect()
}

/// The case that exposed the missing channel: the number in the source is not
/// the number in the program, and every command used to report success.
#[test]
fn an_integer_literal_too_large_for_int_is_reported() {
    let diags = diagnose(
        "
pub fn big () -> Int = 99999999999999999999999999
",
    );
    assert!(
        diags.iter().any(|(code, _)| code == "L110"),
        "expected L110 for a literal outside Int; got {diags:?}"
    );
}

/// Rust's `f64` parser answers `Ok(inf)` for an overflowing literal, so nothing
/// upstream failed and the infinity used to reach codegen, which rejected it in
/// its own terms with an internal message.
#[test]
fn a_float_literal_that_is_not_finite_is_reported() {
    let diags = diagnose(
        "
pub fn huge () -> Float = 1.0e400
",
    );
    assert!(
        diags.iter().any(|(code, _)| code == "L111"),
        "expected L111 for a non-finite float literal; got {diags:?}"
    );
}

/// The largest value that does fit is not rejected. Without this the fix could
/// be an off-by-one that refuses a legal literal, and the test above would
/// still pass.
#[test]
fn the_largest_representable_integer_is_accepted() {
    let diags = diagnose(
        "
pub fn edge () -> Int = 9223372036854775807
",
    );
    assert!(
        diags.is_empty(),
        "i64::MAX is a legal literal; got {diags:?}"
    );
}

/// Base-prefixed literals go through the same parser and must answer the same
/// way, since the prefix is stripped before the range is tested.
#[test]
fn an_out_of_range_hex_literal_is_reported_too() {
    let diags = diagnose(
        "
pub fn big () -> Int = 0xFFFFFFFFFFFFFFFFFF
",
    );
    assert!(
        diags.iter().any(|(code, _)| code == "L110"),
        "expected L110 for a hex literal outside Int; got {diags:?}"
    );
}

/// The message is what a reader sees, and these were written when nothing
/// rendered them: every one carried its own `[L###]` prefix and the `Debug` of
/// its `Span`. The renderer supplies both, so a message repeating them says
/// everything twice and leaks the compiler's bookkeeping on the second pass.
#[test]
fn the_message_carries_neither_the_code_nor_a_span_debug() {
    let diags = diagnose(
        "
pub fn big () -> Int = 99999999999999999999999999
",
    );
    let (_, message) = diags
        .iter()
        .find(|(code, _)| code == "L110")
        .expect("L110 was reported");
    assert!(
        !message.contains("L110"),
        "the renderer prints the code; the message should not: {message}"
    );
    assert!(
        !message.contains("Span {"),
        "internal span representation leaked into the message: {message}"
    );
    assert!(
        !message.contains("  "),
        "a run of spaces means source indentation was baked into the text: {message}"
    );
}
