//! Editor-diagnostics guarantee for the "did you mean" syntax guidance.
//!
//! `ridge-lsp` seeds its state through `check_standalone_incremental` and turns
//! the resulting parse errors into LSP messages verbatim. So the promise that an
//! editor shows the helpful `P033`/`P034`/`P035` guidance — instead of a bare
//! "unexpected token" — for `let … in`, an `if` match guard, or `{ record with
//! … }` reduces to one fact: this pipeline emits those codes for those forms.
//! This test pins it so the LSP counterpart cannot silently regress, and pins
//! the negative direction too: the canonical Ridge spellings emit none of them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;

use ridge_driver::check_standalone_incremental;
use tempfile::tempdir;

/// The parse-error codes the driver pipeline produces for `src`.
fn parse_codes(src: &str) -> Vec<String> {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("guidance.ridge");
    fs::write(&path, src).expect("write source");

    let state = check_standalone_incremental(&[path]);
    state
        .resolved
        .parse_errors
        .iter()
        .map(|(_, e)| e.code().to_owned())
        .collect()
}

#[test]
fn let_in_surfaces_p033() {
    let src = "\
fn f (x: Int) -> Int =
    let y = x + 1 in y * 2
";
    assert!(
        parse_codes(src).iter().any(|c| c == "P033"),
        "the editor should show the let-layout guidance (P033)"
    );
}

#[test]
fn if_match_guard_surfaces_p034() {
    let src = "\
fn f (x: Int) -> Int =
    match x
        n if n > 0 -> 1
        _ -> 0
";
    assert!(
        parse_codes(src).iter().any(|c| c == "P034"),
        "the editor should show the `when` guard guidance (P034)"
    );
}

#[test]
fn record_update_braces_surfaces_p035() {
    let src = "\
type Point = { x: Int, y: Int }

fn move0 (p: Point) -> Point =
    { p with x = 0 }
";
    assert!(
        parse_codes(src).iter().any(|c| c == "P035"),
        "the editor should show the record-update guidance (P035)"
    );
}

#[test]
fn canonical_spellings_emit_no_guidance_codes() {
    let src = "\
type Point = { x: Int, y: Int }

fn f (x: Int) -> Int =
    let y = x + 1
    match y
        n when n > 0 -> 1
        _ -> 0

fn move0 (p: Point) -> Point =
    p with { x = 0 }
";
    let codes = parse_codes(src);
    assert!(
        !codes
            .iter()
            .any(|c| c == "P033" || c == "P034" || c == "P035"),
        "canonical Ridge syntax must not trip the guidance diagnostics, got: {codes:?}"
    );
}
