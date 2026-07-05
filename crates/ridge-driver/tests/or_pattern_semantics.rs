//! Semantic guarantees for or-patterns (`p1 | p2 | …`) beyond parsing.
//!
//! An or-pattern is only well-formed when every alternative binds the same
//! variables (resolve, R027) with the same types (typecheck, T001), and one arm
//! covers the union of its alternatives for exhaustiveness (T016). These checks
//! drive the shared resolve + typecheck pipeline that both the compiler and the
//! language server use, so pinning them here also pins the editor behaviour.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;

use ridge_driver::check_standalone_incremental;
use tempfile::tempdir;

/// The resolve + type diagnostic codes produced for `src`, checked as a
/// standalone single-file module against the prelude.
fn diag_codes(src: &str) -> Vec<String> {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("or.ridge");
    fs::write(&path, src).expect("write source");

    let state = check_standalone_incremental(&[path]);
    let mut codes: Vec<String> = Vec::new();
    for (_, e) in &state.resolved.errors {
        codes.push(e.code().to_string());
    }
    for (_, e) in &state.type_errors {
        codes.push(e.code().to_string());
    }
    codes
}

#[test]
fn or_pattern_binding_mismatch_fires_r027() {
    // `Pos x` binds `x`, `Neg y` binds `y` — the alternatives disagree.
    let src = "\
type Sign = Pos Int | Neg Int

fn f (s: Sign) -> Int =
    match s
        Pos x | Neg y -> 0
";
    let codes = diag_codes(src);
    assert!(
        codes.iter().any(|c| c == "R027"),
        "expected R027 for or-pattern alternatives binding different variables, got: {codes:?}"
    );
}

#[test]
fn or_pattern_binding_type_mismatch_fires_t001() {
    // Both alternatives bind `x` (so no R027), but `IntBox` wraps `Int` and
    // `TextBox` wraps `Text`, so `x` cannot have one consistent type.
    let src = "\
type Mix = IntBox Int | TextBox Text

fn f (m: Mix) -> Int =
    match m
        IntBox x | TextBox x -> 0
";
    let codes = diag_codes(src);
    assert!(
        codes.iter().any(|c| c == "T001"),
        "expected T001 for an or-pattern binding with clashing types, got: {codes:?}"
    );
}

#[test]
fn or_pattern_covering_all_variants_is_exhaustive() {
    // Two or-pattern arms cover all four constructors — no T016.
    let src = "\
type Dir = North | South | East | West

fn f (d: Dir) -> Text =
    match d
        North | South -> \"vertical\"
        East | West -> \"horizontal\"
";
    let codes = diag_codes(src);
    assert!(
        !codes.iter().any(|c| c == "T016"),
        "an or-pattern covering every variant must be exhaustive, got: {codes:?}"
    );
}

#[test]
fn or_pattern_missing_variant_fires_t016() {
    // Only two of four constructors are covered.
    let src = "\
type Dir = North | South | East | West

fn f (d: Dir) -> Text =
    match d
        North | South -> \"vertical\"
";
    let codes = diag_codes(src);
    assert!(
        codes.iter().any(|c| c == "T016"),
        "expected T016 when an or-pattern match misses variants, got: {codes:?}"
    );
}

#[test]
fn well_formed_or_pattern_has_no_semantic_diagnostics() {
    let src = "\
fn f (n: Int) -> Text =
    match n
        0 | 1 | 2 -> \"low\"
        _ -> \"high\"
";
    let codes = diag_codes(src);
    assert!(
        !codes
            .iter()
            .any(|c| c == "R027" || c == "T001" || c == "T016" || c == "T017"),
        "a well-formed or-pattern match should not produce or-pattern diagnostics, got: {codes:?}"
    );
}
