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
//!
//! The second half of the file asks the same questions of `check` and of the
//! editor's incremental path. Those stopped after type-checking, so they called
//! an out-of-range literal well-typed and left `build` to disagree — a clean
//! `check` on a program that cannot build.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::make_workspace;
use ridge_driver::{
    check_workspace, check_workspace_incremental, collect_diagnostics, compile_workspace,
    CheckOptions, CompileOptions, EmitArtefacts,
};

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

// ── The same programs, through `check` and through the editor ────────────────

/// `check`'s answer for a source, as `(code, message)` pairs.
///
/// Same clone as `diagnose` above, and for the same reason: `TempWorkspace`
/// implements `Drop`, so its path cannot be moved out of it.
#[allow(clippy::redundant_clone)]
fn check_diagnose(source: &str) -> Vec<(String, String)> {
    let tw = make_workspace("Main", source);
    let artefacts = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    artefacts
        .diagnostics
        .iter()
        .map(|d| (d.code.to_owned(), d.primary_message.clone()))
        .collect()
}

/// The editor's answer, through the incremental engine rather than a full check.
#[allow(clippy::redundant_clone)]
fn editor_diagnose(source: &str) -> Vec<(String, String)> {
    let tw = make_workspace("Main", source);
    let state =
        check_workspace_incremental(CheckOptions::new(tw.path.clone()).with_retain_indices(true))
            .expect("seed the engine");
    let sources = state.source_cache();
    collect_diagnostics(
        &state.disc_resolve_errors,
        &state.resolved,
        &state.type_errors,
        &state.lower_errors,
        &sources,
        &state.typed.tycons,
    )
    .iter()
    .map(|d| (d.code.to_owned(), d.primary_message.clone()))
    .collect()
}

const TOO_BIG: &str = "
pub fn big () -> Int = 99999999999999999999999999
";

const NOT_FINITE: &str = "
pub fn huge () -> Float = 1.0e400
";

/// `check` used to answer "Type-check passed." here, and `run` refused the same
/// file seconds later.
#[test]
fn check_reports_an_integer_literal_too_large_for_int() {
    let diags = check_diagnose(TOO_BIG);
    assert!(
        diags.iter().any(|(code, _)| code == "L110"),
        "check must report L110, not defer it to build; got {diags:?}"
    );
}

#[test]
fn check_reports_a_float_literal_that_is_not_finite() {
    let diags = check_diagnose(NOT_FINITE);
    assert!(
        diags.iter().any(|(code, _)| code == "L111"),
        "check must report L111, not defer it to build; got {diags:?}"
    );
}

/// The half that matters most: this is where the literal is being typed.
#[test]
fn the_editor_reports_an_integer_literal_too_large_for_int() {
    let diags = editor_diagnose(TOO_BIG);
    assert!(
        diags.iter().any(|(code, _)| code == "L110"),
        "the incremental path must report L110; got {diags:?}"
    );
}

#[test]
fn the_editor_reports_a_float_literal_that_is_not_finite() {
    let diags = editor_diagnose(NOT_FINITE);
    assert!(
        diags.iter().any(|(code, _)| code == "L111"),
        "the incremental path must report L111; got {diags:?}"
    );
}

/// Seeding the engine is not the interesting case — an edit is. A literal typed
/// into a buffer has to be reported without a rebuild, and the previous edit's
/// diagnostic has to disappear when it is corrected.
#[test]
#[allow(clippy::redundant_clone)]
fn an_edit_introduces_and_then_clears_the_diagnostic() {
    let tw = make_workspace(
        "Main",
        "
pub fn ok () -> Int = 1
",
    );
    let mut state =
        check_workspace_incremental(CheckOptions::new(tw.path.clone()).with_retain_indices(true))
            .expect("seed the engine");
    assert!(
        state.lower_errors.is_empty(),
        "a clean workspace must seed with no lowering errors: {:?}",
        state.lower_errors
    );

    let main = state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.fully_qualified_name.ends_with(".Main"))
        .map(|m| m.id)
        .expect("Main present");

    state.recompile(
        main,
        "
pub fn ok () -> Int = 99999999999999999999999999
",
    );
    assert!(
        state.lower_errors.iter().any(|(_, e)| e.code() == "L110"),
        "the edit must raise L110 without a rebuild; got {:?}",
        state.lower_errors
    );

    state.recompile(
        main,
        "
pub fn ok () -> Int = 1
",
    );
    assert!(
        state.lower_errors.is_empty(),
        "correcting the literal must clear it; got {:?}",
        state.lower_errors
    );
}
