//! Editor-diagnostics guarantee for the Wave 1 syntax-hardening fixes.
//!
//! `ridge-lsp` does not parse or lex on its own — it seeds an incremental state
//! through this crate's `check_standalone_incremental` and turns the resulting
//! diagnostics into LSP messages verbatim. So the promise that an editor shows
//! no spurious parse squiggle on a spec-legal multi-line union, a signature
//! wrapped across lines, an `if`/`then` split over two lines, a
//! trailing-operator continuation, a multi-line interpolated string
//! (`$"""..."""`), or an or-pattern arm (`1 | 2 | 3 ->`) reduces to a single
//! fact: this pipeline
//! reports zero lex/parse errors for those forms. This test pins that fact so
//! the LSP counterpart of the parser/lexer fixes cannot silently regress.
//!
//! Undefined names in the samples are fine — those surface as resolve/type
//! diagnostics, which this test deliberately ignores. It asserts only on the
//! lex/parse layer that the Wave 1 work changed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;

use ridge_driver::check_standalone_incremental;
use tempfile::tempdir;

/// Number of lex + parse diagnostics the driver pipeline produces for `src`,
/// with a rendered detail string for failure messages.
fn lex_parse_error_count(src: &str) -> (usize, String) {
    let dir = tempdir().expect("create temp dir");
    let path = dir.path().join("wave1.ridge");
    fs::write(&path, src).expect("write source");

    let state = check_standalone_incremental(&[path]);
    let count = state.resolved.lex_errors.len() + state.resolved.parse_errors.len();
    let detail = format!(
        "lex={:?} parse={:?}",
        state.resolved.lex_errors, state.resolved.parse_errors
    );
    (count, detail)
}

/// Each spec-legal form the Wave 1 fixes taught the parser/lexer to accept.
const WAVE1_FORMS: &[(&str, &str)] = &[
    (
        "record-variant before the union bar",
        "type Shape = Circle { radius: Int } | Square\n",
    ),
    (
        "multi-line union with deriving",
        "type Color =\n    | Red\n    | Green\n    deriving (Eq)\n",
    ),
    (
        "where clause on the next line",
        "fn showIt (x: a) -> Text\n    where ToText a = ToText.toText x\n",
    ),
    (
        "function parameters wrapped across lines",
        "fn add (x: Int)\n       (y: Int) -> Int = x + y\n",
    ),
    (
        "if with then on the next line",
        "fn pick (c: Bool) -> Int =\n    if c\n    then 1\n    else 2\n",
    ),
    (
        "trailing-operator continuation",
        "fn total (a: Int) (b: Int) -> Int =\n    a +\n        b\n",
    ),
    (
        "multi-line interpolated string",
        "fn greet (name: Text) -> Text =\n    $\"\"\"\n    Hello, ${name}!\n    \"\"\"\n",
    ),
    (
        "or-pattern in a match arm",
        "fn classify (n: Int) -> Text =\n    match n\n        0 | 1 | 2 -> \"low\"\n        _ -> \"high\"\n",
    ),
];

#[test]
fn wave1_forms_produce_no_lex_or_parse_diagnostics() {
    for (name, src) in WAVE1_FORMS {
        let (count, detail) = lex_parse_error_count(src);
        assert_eq!(
            count, 0,
            "the editor would flag a spec-legal form ({name}); \
             expected no lex/parse diagnostics but got {count}: {detail}\nsource:\n{src}"
        );
    }
}
