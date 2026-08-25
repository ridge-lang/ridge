//! A module-qualified constructor lowers as a constructor.
//!
//! `L.Red` used to resolve to a cross-module symbol rather than to a
//! constructor, and a plain symbol carries neither the variant index nor the
//! record-vs-union flag. The lower pass then fell back to "record, variant 0",
//! which builds an empty map — correct by luck for a record type, and silently
//! wrong for the first variant of every union. That flag exists precisely
//! because the same fallback caused the same miscompile once before.
//!
//! These assert the tag on the way down, where the fault was, so the guard
//! holds on a CI runner with no BEAM.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, render_lowered_module, run_pipeline, write_file};

const LIB: &str = r"
pub type Colour = Red | Green | Blue Int
";

const MAIN: &str = r#"
import demo.Lib as L
import demo.Lib (Colour, Red)
import std.actor as Actor

pub fn qualified () -> Colour = L.Red
pub fn bare () -> Colour = Red
pub fn payload () -> Colour = L.Blue 3
pub fn stdlibVariant () -> AskError = Actor.Timeout

pub fn matched (c: Colour) -> Text =
    match c
        L.Red -> "red"
        _ -> "other"
"#;

/// Lower a two-module workspace and return the rendered `Main`.
fn lowered_main() -> String {
    let tw = make_workspace("qualified-ctor", "Main", MAIN);
    write_file(&tw.path, "apps/demo/src/Lib.ridge", LIB);
    let result = run_pipeline(&tw.path);
    result
        .lowered
        .modules
        .iter()
        .flatten()
        .map(render_lowered_module)
        .find(|ir| ir.contains("qualified"))
        .expect("the Main module lowered")
}

#[test]
fn a_qualified_union_variant_lowers_as_a_variant_not_a_record() {
    let ir = lowered_main();
    // `Red` is variant 0, which is exactly the position the record fallback
    // could not be told apart from — so this is the assertion that fails when
    // the binding does not reach the lower pass.
    assert!(
        ir.contains("Ctor(Variant:Red"),
        "`L.Red` must lower as a union variant; a `Ctor(Record:…)` here is the empty-map \
         miscompile. Lowered module:\n{ir}"
    );
    assert!(
        !ir.contains("Ctor(Record:Red"),
        "no spelling of `Red` may lower as a record constructor. Lowered module:\n{ir}"
    );
}

#[test]
fn a_qualified_variant_with_a_payload_reaches_the_backend_as_a_constructor() {
    let ir = lowered_main();
    // This one used to lower to a cross-module function reference, which the
    // backend emitted as a call to a function that was never generated.
    assert!(
        ir.contains("Ctor(Variant:Blue"),
        "`L.Blue 3` must lower as a constructor, not a symbol reference. Lowered module:\n{ir}"
    );
}

#[test]
fn a_qualified_stdlib_variant_lowers_like_the_bare_one() {
    let ir = lowered_main();
    assert!(
        ir.contains("Ctor(Variant:Timeout"),
        "`Actor.Timeout` must lower as a variant of `AskError`. Lowered module:\n{ir}"
    );
}
