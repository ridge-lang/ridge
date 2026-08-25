//! A constructor reached through its module alias is type-checked, and a name
//! that is not a constructor is reported.
//!
//! Two holes, both of which let a program through with no diagnostic.
//!
//! The type checker looks a bare constructor up in the environment by its name.
//! Module exports are seeded under their *dotted* names, so reading only the
//! last segment of `Actor.Timeout` found nothing — and the miss was absorbed on
//! the grounds that an unknown name is the resolver's job. `Type::Error`
//! unifies with whatever the context wanted, so an annotation contradicting the
//! constructor went unreported while the lower pass built the right value from
//! the binding. It worked by not being checked.
//!
//! The same silence covered a name that is not a constructor at all. A module
//! alias reaches every export, and `Actor.AskError` is a type: the resolver has
//! no complaint, so nothing reported, and the value reached a backend as an
//! empty map.
//!
//! Pure type-check tests (`check_workspace`), no runtime needed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, write_file};
use ridge_driver::{check_workspace, CheckOptions};

const LIB: &str = "
pub type Colour = Red | Green | Blue Int
";

/// Every diagnostic code `src` reports, as rendered debug strings.
fn codes(src: &str) -> Vec<String> {
    let tw = make_workspace("Main", src);
    let result = check_workspace(CheckOptions::new(tw.path)).expect("check ran");
    result
        .diagnostics
        .iter()
        .map(|d| format!("{d:?}"))
        .collect()
}

/// The same, against a sibling module the main one reaches through an alias.
fn codes_with_lib(src: &str) -> Vec<String> {
    let tw = make_workspace("Main", src);
    write_file(&tw.path, "apps/demo/src/Lib.ridge", LIB);
    let result = check_workspace(CheckOptions::new(tw.path)).expect("check ran");
    result
        .diagnostics
        .iter()
        .map(|d| format!("{d:?}"))
        .collect()
}

#[test]
fn a_qualified_constructor_type_checks_against_its_own_type() {
    let found = codes(
        "
import std.actor as Actor

pub fn f () -> AskError = Actor.Timeout
",
    );
    assert!(
        found.is_empty(),
        "`Actor.Timeout` is an `AskError`; got {found:?}"
    );
}

#[test]
fn a_qualified_constructor_against_the_wrong_type_is_reported() {
    // The one that matters: this used to pass. The constructor typed as
    // `Type::Error`, which unifies with `Int` as happily as with anything else,
    // so the annotation was never checked against it.
    let found = codes(
        "
import std.actor as Actor

pub fn f () -> Int = Actor.Timeout
",
    );
    assert!(
        found
            .iter()
            .any(|d| d.contains("Mismatch") || d.contains("T001")),
        "an `AskError` where an `Int` is declared must be a type mismatch; got {found:?}"
    );
}

#[test]
fn the_bare_spelling_is_checked_the_same_way() {
    // The control: the bare route has always been checked, and stays checked.
    let found = codes(
        "
import std.actor (Timeout)

pub fn f () -> Int = Timeout
",
    );
    assert!(
        found
            .iter()
            .any(|d| d.contains("Mismatch") || d.contains("T001")),
        "the bare spelling must report the same mismatch; got {found:?}"
    );
}

#[test]
fn a_type_name_reached_through_an_alias_is_not_a_constructor() {
    // `AskError` is an export of `std.actor`, so the resolver accepts it and
    // will never complain. Routing it to the constructor path without this
    // check is what made it compile to an empty map.
    let found = codes(
        "
import std.actor as Actor

pub fn f () -> Int = Actor.AskError
",
    );
    assert!(
        found
            .iter()
            .any(|d| d.contains("NotAConstructor") || d.contains("T044")),
        "a type name used as a value must be reported, not absorbed; got {found:?}"
    );
}

#[test]
fn an_unknown_name_behind_an_alias_is_still_the_resolvers_report() {
    // The guard against reporting twice: a name the module does not export is
    // `R014`, and the type checker must not add a second complaint about the
    // same word.
    let found = codes(
        "
import std.actor as Actor

pub fn f () -> Int = Actor.Nonesuch
",
    );
    assert!(
        found
            .iter()
            .any(|d| d.contains("UnknownStdlibSymbol") || d.contains("R014")),
        "an unexported name is the resolver's report; got {found:?}"
    );
    assert!(
        !found.iter().any(|d| d.contains("NotAConstructor")),
        "and the type checker must not double-report it; got {found:?}"
    );
}

#[test]
fn a_workspace_module_constructor_type_checks_through_its_alias() {
    // A constructor from another workspace module is in neither the current
    // module's own set nor its import list, so the environment does not hold it
    // under any name. Its scheme comes from the declaration instead.
    let found = codes_with_lib(
        "
import demo.Lib as L
import demo.Lib (Colour)

pub fn f () -> Colour = L.Red
pub fn g () -> Colour = L.Blue 3
",
    );
    assert!(
        found.is_empty(),
        "`L.Red` is a `Colour` and `L.Blue 3` is too; got {found:?}"
    );
}

#[test]
fn a_workspace_module_constructor_against_the_wrong_type_is_reported() {
    // The counterpart, and the one that used to pass: with no scheme to work
    // from, the constructor typed as `Type::Error` and the annotation was never
    // checked against it.
    let found = codes_with_lib(
        "
import demo.Lib as L
import demo.Lib (Colour)

pub fn f () -> Int = L.Red
",
    );
    assert!(
        found
            .iter()
            .any(|d| d.contains("Mismatch") || d.contains("T001")),
        "a `Colour` where an `Int` is declared must be a type mismatch; got {found:?}"
    );
}
