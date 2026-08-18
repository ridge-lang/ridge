//! A `?` inside a lambda belongs to the lambda.
//!
//! Lowering pushed a propagation scope only for a top-level `fn`'s return type,
//! so a lambda body read whatever the enclosing function had left on the stack.
//! The scope was wrong rather than missing, which is why the symptom depended
//! entirely on what that enclosing function returned: an internal `L999` when it
//! was neither `Result` nor `Option`, a working build when it happened to match,
//! and a `.core` file `erlc` refused when it was the other one.
//!
//! The middle case is why this needs all three: it worked, for the wrong reason,
//! in exactly the shape people write most.
//!
//! Emitting `.core` keeps these off the `erlc` guard. The third case failed in
//! `erlc` rather than in lowering, so a diagnostic count cannot see it — that one
//! reads the emitted Core and checks which constructors the desugaring matched.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::make_workspace;
use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// Compile `source` to Core and return `(diagnostic codes, emitted Core text)`.
#[allow(clippy::redundant_clone)]
fn compile(source: &str) -> (Vec<String>, String) {
    let tw = make_workspace("Prop", source);
    let artefacts =
        compile_workspace(CompileOptions::new(tw.path.clone()).with_emit(EmitArtefacts::Core))
            .expect("compile ran");
    let codes = artefacts
        .diagnostics
        .iter()
        .map(|d| d.code.to_owned())
        .collect();
    let core = artefacts
        .core_files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect::<Vec<_>>()
        .join("\n");
    (codes, core)
}

/// The reported case: the enclosing `fn` returns `Text`, which is neither
/// `Result` nor `Option`, so the `?` had nothing valid to desugar against and
/// lowering raised its own internal code at a line the user wrote.
#[test]
fn a_result_lambda_inside_a_text_returning_fn() {
    let (codes, _) = compile(
        "\
fn pick (f: fn Text -> Result Int Error) -> Result Int Error = f \"x\"
fn mk (s: Text) -> Result Int Error = Ok 1

pub fn outer -> Text =
    let r = pick (fn (s: Text) -> Result Int Error =
        let v = mk s ?
        Ok v)
    \"done\"
",
    );
    assert!(
        codes.is_empty(),
        "the lambda declares `Result Int Error`; `?` belongs to it, not to `outer`. Got {codes:?}"
    );
}

/// The case that used to pass by coincidence: enclosing and lambda agree, so
/// borrowing the wrong scope gave the right answer. It has to keep working, and
/// it is worth naming as the reason a passing suite proved nothing here.
#[test]
fn a_result_lambda_inside_a_result_returning_fn_still_works() {
    let (codes, _) = compile(
        "\
fn pick (f: fn Text -> Result Int Error) -> Result Int Error = f \"x\"
fn mk (s: Text) -> Result Int Error = Ok 1

pub fn outer -> Result Int Error =
    let r = pick (fn (s: Text) -> Result Int Error =
        let v = mk s ?
        Ok v)
    r
",
    );
    assert!(codes.is_empty(), "this one always compiled; got {codes:?}");
}

/// The worst of the three: both scopes are propagatable but they are different
/// ones, so lowering produced `Result` arms for an `Option` value and the error
/// surfaced as `erlc` rejecting a generated file, with no Ridge-level location.
///
/// Lowering succeeds either way, so the check is on what was emitted: an
/// `Option` propagation matches `None`, a `Result` one matches `Err`.
#[test]
fn an_option_lambda_inside_a_result_returning_fn_desugars_as_option() {
    let (codes, core) = compile(
        "\
fn pick (f: fn Text -> Option Int) -> Option Int = f \"x\"
fn mk (s: Text) -> Option Int = Some 1

pub fn outer -> Result Int Error =
    let r = pick (fn (s: Text) -> Option Int =
        let v = mk s ?
        Some v)
    Ok 1
",
    );
    assert!(codes.is_empty(), "expected a clean compile; got {codes:?}");
    assert!(
        core.contains("PropSome"),
        "the `?` is in an `Option` lambda, so it must desugar over `Some`/`None`:\n{core}"
    );
    // The two desugarings bind differently, so the binder name says which
    // one ran without depending on the rest of the emitted text.
    assert!(
        !core.contains("PropOk"),
        "it must not have borrowed the enclosing `Result` scope"
    );
}
