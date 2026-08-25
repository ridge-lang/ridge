//! A name that is in scope but is not a value is reported where it is written.
//!
//! Three spellings of one mistake used to give three different answers, two of
//! them silent.
//!
//! A module alias is the module and nothing inside it. Written as a value, an
//! upper-case alias parses as a zero-field record, found no constructor, was
//! absorbed as `Type::Error` — which unifies with anything, so nothing
//! downstream objected — and reached a backend as an empty map. The lower-case
//! spelling the bare form of `import` binds took a different path and reported
//! `L999 internal` instead. Both are `R030` now, and the lower-case one is
//! answered with the import to write, because a lower-case name can never
//! prefix a qualified name at all.
//!
//! A type name is not a value either. `Option` and `Result` were accepted like
//! the aliases; a union's own type name reported `T999`, telling the reader
//! they had found a compiler bug. Both are `T044` now, naming the type's
//! constructors — except for an `opaque` type, whose constructors `R025`
//! rejects, so naming one would be advice that cannot be taken.
//!
//! Pure type-check tests (`check_workspace`), no runtime needed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, write_file};
use ridge_driver::{check_workspace, CheckOptions};

const LIB: &str = "
pub type Colour = Red | Green | Blue Int
";

const OPAQUE_LIB: &str = "
pub opaque type Secret = Wrap Int | Empty
";

/// Every diagnostic `src` reports, as rendered debug strings.
fn diags(src: &str) -> Vec<String> {
    let tw = make_workspace("Main", src);
    let result = check_workspace(CheckOptions::new(tw.path)).expect("check ran");
    result
        .diagnostics
        .iter()
        .map(|d| format!("{d:?}"))
        .collect()
}

/// The same, against a sibling module the main one imports.
fn diags_with(lib: &str, src: &str) -> Vec<String> {
    let tw = make_workspace("Main", src);
    write_file(&tw.path, "apps/demo/src/Lib.ridge", lib);
    let result = check_workspace(CheckOptions::new(tw.path)).expect("check ran");
    result
        .diagnostics
        .iter()
        .map(|d| format!("{d:?}"))
        .collect()
}

/// A program naming `name` where an `Int` is expected.
fn as_value(name: &str) -> String {
    format!(
        "
import std.io as IO

pub fn f () -> Int = {name}

pub fn io main () -> Unit = IO.println \"hi\"
"
    )
}

fn count(found: &[String], code: &str) -> usize {
    found.iter().filter(|d| d.contains(code)).count()
}

// ── The module aliases ────────────────────────────────────────────────────────

/// Every alias the prelude injects, one at a time. The list is spelled out
/// rather than derived so that adding one to the prelude and forgetting it here
/// is a visible omission rather than a silently shrinking sweep.
const PRELUDE_ALIASES: &[&str] = &[
    "Int", "Float", "Decimal", "Uuid", "Bytes", "Date", "Time", "Error", "Bool", "Text", "List",
    "Map", "Set", "Json",
];

#[test]
fn every_prelude_module_alias_is_rejected_as_a_value() {
    for name in PRELUDE_ALIASES {
        let found = diags(&as_value(name));
        assert_eq!(
            count(&found, "R030"),
            1,
            "`{name}` as a value must report exactly one R030: {found:?}"
        );
        // One name, one caret. Two layers can both see this mistake, and both
        // reporting it would draw two.
        assert_eq!(
            found.len(),
            1,
            "`{name}` must report once and only once: {found:?}"
        );
    }
}

#[test]
fn a_user_alias_is_rejected_the_same_way() {
    let found = diags(
        "
import std.io as IO

pub fn f () -> Int = IO

pub fn io main () -> Unit = IO.println \"hi\"
",
    );
    assert_eq!(count(&found, "R030"), 1, "found: {found:?}");
}

#[test]
fn the_bare_import_spelling_says_which_import_to_change() {
    // `import std.list` binds `list`, and a qualified name cannot begin with a
    // lower-case name, so the fix is at the import — not at the use site. The
    // member is named because the use site is a field access, which is what
    // makes the line a fix rather than a description.
    let found = diags(
        "
import std.io as IO
import std.list

pub fn f (xs: List Int) -> Int = list.length xs

pub fn io main () -> Unit = IO.println \"hi\"
",
    );
    assert_eq!(count(&found, "R030"), 1, "found: {found:?}");
    let note = found.join(" ");
    assert!(
        note.contains("as List"),
        "the note must name the import to write: {found:?}"
    );
    assert!(
        note.contains("List.length"),
        "the note must name the member the reader reached for: {found:?}"
    );
    assert!(
        !note.contains("L999"),
        "the internal lowering error must be unreachable from this program: {found:?}"
    );
}

// ── The type names ────────────────────────────────────────────────────────────

#[test]
fn a_prelude_type_name_names_its_constructors() {
    let found = diags(&as_value("Option"));
    assert_eq!(count(&found, "T044"), 1, "found: {found:?}");
    let note = found.join(" ");
    assert!(note.contains("Some"), "found: {found:?}");
    assert!(note.contains("None"), "found: {found:?}");

    let found = diags(&as_value("Result"));
    assert_eq!(count(&found, "T044"), 1, "found: {found:?}");
    let note = found.join(" ");
    assert!(note.contains("Ok"), "found: {found:?}");
    assert!(note.contains("Err"), "found: {found:?}");
}

#[test]
fn a_union_type_name_is_not_a_compiler_bug() {
    let found = diags(
        "
import std.io as IO

pub type Colour = Red | Green | Blue Int

pub fn f () -> Int = Colour

pub fn io main () -> Unit = IO.println \"hi\"
",
    );
    assert_eq!(count(&found, "T044"), 1, "found: {found:?}");
    assert_eq!(
        count(&found, "T999"),
        0,
        "naming a type must not tell the reader they found a compiler bug: {found:?}"
    );
    let note = found.join(" ");
    assert!(note.contains("Red"), "found: {found:?}");
    assert!(note.contains("Green"), "found: {found:?}");
}

#[test]
fn an_opaque_type_does_not_have_its_constructors_named() {
    // The variants of an `opaque` type are not the caller's to write — `R025`
    // rejects them — so recommending one would be advice that cannot be taken,
    // and printing them would put the representation in a diagnostic.
    let found = diags_with(
        OPAQUE_LIB,
        "
import std.io as IO
import demo.Lib (Secret)

pub fn f () -> Int = Secret

pub fn io main () -> Unit = IO.println \"hi\"
",
    );
    let note = found.join(" ");
    // The absence below is only worth asserting if something was said at all —
    // an empty diagnostic list would satisfy it without the code ever running.
    assert_eq!(
        count(&found, "T044"),
        1,
        "naming an opaque type where a value belongs is the same T044, so the          absence asserted below is the opaque branch and not an empty run: {found:?}"
    );
    assert!(
        !note.contains("Wrap"),
        "an opaque type's constructors must not be named: {found:?}"
    );
    assert!(
        !note.contains("Empty"),
        "an opaque type's constructors must not be named: {found:?}"
    );
}

// ── Controls ──────────────────────────────────────────────────────────────────

#[test]
fn control_a_constructor_is_still_a_value() {
    let found = diags_with(
        LIB,
        "
import std.io as IO
import demo.Lib (Red)

pub fn f () -> Colour = Red

pub fn io main () -> Unit = IO.println \"hi\"
",
    );
    assert!(
        found.is_empty(),
        "a real constructor must still type-check: {found:?}"
    );
}

#[test]
fn control_a_qualified_call_is_untouched() {
    let found = diags(
        "
import std.io as IO

pub fn f (xs: List Int) -> Int = List.length xs

pub fn io main () -> Unit = IO.println \"hi\"
",
    );
    assert!(
        found.is_empty(),
        "the alias as a qualified prefix is the correct spelling: {found:?}"
    );
}

#[test]
fn control_an_unknown_name_is_still_r010() {
    // R030 says the name resolved and is not a value. A name that never
    // resolved is a different answer, and blurring the two would make R030
    // unfalsifiable.
    let found = diags(&as_value("Nonesuch"));
    assert_eq!(count(&found, "R010"), 1, "found: {found:?}");
    assert_eq!(count(&found, "R030"), 0, "found: {found:?}");
}
