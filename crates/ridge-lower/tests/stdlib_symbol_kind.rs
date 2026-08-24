//! What a stdlib symbol *is* has to survive lowering.
//!
//! `std.time.epoch` is `() -> Timestamp` and `std.map.empty` is `Map k v`. Both
//! reach a backend as arity 0, so arity cannot tell a backend which of them to
//! reference and which to evaluate — and the one that guessed from arity
//! evaluated both, which handed a `Timestamp` to a caller that wanted a clock
//! and stopped the program with `badfun`. The type checker knows; these tests
//! assert lowering carries the answer down.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, render_lowered_module, run_pipeline};

const SRC: &str = r"
import std.map as Map
import std.time as Time

-- Named, not called: the callee applies it.
fn clockText (clock: fn () -> Timestamp) -> Text = Time.toIso (clock ())

pub fn injected () -> Text = clockText Time.epoch

pub fn counted () -> Int = Map.length Map.empty
";

fn lowered() -> String {
    let tw = make_workspace("stdlib-symbol-kind", "stdlib_symbol_kind", SRC);
    let result = run_pipeline(&tw.path);
    let module = result.lowered.modules[0]
        .as_ref()
        .expect("the workspace's one module lowered");
    render_lowered_module(module)
}

#[test]
fn a_nullary_stdlib_fn_named_as_a_value_stays_a_function() {
    let ir = lowered();
    assert!(
        ir.contains("Stdlib(fn:std.time.epoch)"),
        "`Time.epoch` passed as a value must lower as a function, so a backend \
         emits a reference to it. Lowered module:\n{ir}"
    );
}

#[test]
fn a_stdlib_value_named_as_a_value_stays_a_value() {
    // The counterpart, so the test above cannot pass by calling everything a
    // function: `Map.empty` is the map, and a backend that references it puts
    // an opaque callable where `Map.length` expects a map.
    let ir = lowered();
    assert!(
        ir.contains("Stdlib(value:std.map.empty)"),
        "`Map.empty` must lower as a value. Lowered module:\n{ir}"
    );
}
