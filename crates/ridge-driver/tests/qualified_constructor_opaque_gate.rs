//! The opaque-type gate covers the qualified spelling of a constructor.
//!
//! An opaque type's constructor may only build or match a value inside the
//! module that declares the type. That gate reads the constructor binding, and
//! the module-qualified spelling never produced one — so `L.Wrap 1` did not
//! reach the check at all. What stopped the build instead was an unrelated
//! internal-check failure in code generation, which is not the gate doing its
//! job: repairing that failure on its own would have left the hole open. In
//! pattern position there was nothing to check either, because the spelling did
//! not parse.
//!
//! Pure type-check tests (`check_workspace`), no runtime needed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, write_file};
use ridge_driver::{check_workspace, CheckOptions};

const LIB: &str = "
pub opaque type Secret = Wrap Int | Empty
";

/// Check `main_src` against a sibling module holding the opaque type, and
/// return every diagnostic code it reported.
fn codes(main_src: &str) -> Vec<String> {
    let tw = make_workspace("Main", main_src);
    write_file(&tw.path, "apps/demo/src/Lib.ridge", LIB);
    let result = check_workspace(CheckOptions::new(tw.path)).expect("check ran");
    result
        .diagnostics
        .iter()
        .map(|d| format!("{d:?}"))
        .collect()
}

fn reports(main_src: &str, code: &str) -> bool {
    codes(main_src).iter().any(|d| d.contains(code))
}

#[test]
fn constructing_an_opaque_type_through_its_alias_is_rejected() {
    assert!(
        reports(
            "
import demo.Lib as L
import demo.Lib (Secret)

pub fn f () -> Secret = L.Empty
",
            "R025"
        ),
        "building an opaque type outside its module must report R025, whatever the spelling"
    );
}

#[test]
fn matching_an_opaque_type_through_its_alias_is_rejected() {
    assert!(
        reports(
            "
import demo.Lib as L
import demo.Lib (Secret)

pub fn f (s: Secret) -> Int =
    match s
        L.Wrap n -> n
        _ -> 0
",
            "R026"
        ),
        "matching an opaque type outside its module must report R026, whatever the spelling"
    );
}

#[test]
fn the_bare_spelling_is_rejected_the_same_way() {
    // The control the qualified cases are measured against: the gate has always
    // fired here, and it has to keep firing.
    assert!(
        reports(
            "
import demo.Lib (Secret, Empty)

pub fn f () -> Secret = Empty
",
            "R025"
        ),
        "the bare spelling must still report R025"
    );
}

#[test]
fn naming_the_alias_without_touching_a_constructor_reports_nothing() {
    // The guard against a gate that fires on everything: importing the module
    // and never reaching for its constructors is fine.
    let found = codes(
        "
import demo.Lib as L
import demo.Lib (Secret)

pub fn f (s: Secret) -> Int = 1
",
    );
    assert!(
        found.is_empty(),
        "an unused alias is not a construction; got {found:?}"
    );
}
