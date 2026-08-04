//! "Did you mean" guidance for forms other languages spell differently.
//!
//! Ridge intentionally rejects `let … in`, an `if` match guard, and the
//! `{ record with … }` update spelling. Rather than surface a bare "unexpected
//! token", the parser names each confusion with a dedicated code (`P033`,
//! `P034`, `P035`) and underlines the exact token an editor quick-fix acts on.
//! These tests pin both the code and the caret span, since the LSP quick-fix is
//! built from that span.

#![allow(clippy::unwrap_used, clippy::panic)]

use ridge_parser::parse_source;

/// The `(code, underlined-text)` pairs for every parse error `src` produces.
fn parse_diags(src: &str) -> Vec<(String, String)> {
    let r = parse_source(src);
    assert!(
        r.lex_errors.is_empty(),
        "unexpected lex errors: {:?}",
        r.lex_errors
    );
    r.errors
        .iter()
        .map(|e| {
            let span = e.span();
            let text = src[span.start as usize..span.end as usize].to_owned();
            (e.code().to_owned(), text)
        })
        .collect()
}

#[test]
fn let_in_inline_fires_p033_on_the_in() {
    let src = "\
fn f (x: Int) -> Int =
    let y = x + 1 in y * 2
";
    assert_eq!(parse_diags(src), vec![("P033".to_owned(), "in".to_owned())]);
}

#[test]
fn let_in_on_its_own_line_fires_p033() {
    let src = "\
fn f (x: Int) -> Int =
    let y = x + 1
    in y * 2
";
    let diags = parse_diags(src);
    assert!(
        diags.iter().any(|(c, t)| c == "P033" && t == "in"),
        "expected P033 underlining `in`, got: {diags:?}"
    );
}

#[test]
fn if_match_guard_fires_p034_on_the_if() {
    let src = "\
fn f (x: Int) -> Int =
    match x
        n if n > 0 -> 1
        _ -> 0
";
    assert_eq!(parse_diags(src), vec![("P034".to_owned(), "if".to_owned())]);
}

#[test]
fn record_update_in_braces_fires_p035_on_the_with() {
    let src = "\
type Point = { x: Int, y: Int }

fn move0 (p: Point) -> Point =
    { p with x = 0 }
";
    assert_eq!(
        parse_diags(src),
        vec![("P035".to_owned(), "with".to_owned())]
    );
}

#[test]
fn record_update_in_braces_multi_field_fires_p035() {
    let src = "\
type Point = { x: Int, y: Int }

fn move0 (p: Point) -> Point =
    { p with x = 0, y = 1 }
";
    assert_eq!(
        parse_diags(src),
        vec![("P035".to_owned(), "with".to_owned())]
    );
}

// ── P037 — `xs[0]` reads as C-family index syntax ────────────────────────────

#[test]
fn index_syntax_fires_p037_on_the_bracket() {
    let src = "\
fn first (xs: List Int) -> Int =
    xs[0]
";
    assert_eq!(parse_diags(src), vec![("P037".to_owned(), "[".to_owned())]);
}

#[test]
fn spaced_list_literal_argument_still_parses() {
    // `f [0]` with a space is application of `f` to a list literal — not the
    // index-syntax trap — and must keep parsing clean.
    let src = "\
fn apply0 (f: List Int -> Int) -> Int =
    f [0]
";
    assert_eq!(parse_diags(src), Vec::<(String, String)>::new());
}

// ── P038 — `!x` reads as C-family boolean negation ───────────────────────────

#[test]
fn bang_negation_fires_p038_on_the_bang() {
    let src = "\
fn neg (x: Bool) -> Bool =
    !x
";
    assert_eq!(parse_diags(src), vec![("P038".to_owned(), "!".to_owned())]);
}

#[test]
fn bang_in_comment_and_string_is_unaffected() {
    let src = "\
-- a comment mentioning !true is fine
fn s =
    \"! is just text here\"
";
    assert_eq!(parse_diags(src), Vec::<(String, String)>::new());
}

// ── P039 — Rust-style `match x { … }` braces ─────────────────────────────────

#[test]
fn match_with_braces_fires_p039_on_the_brace() {
    let src = "\
fn f (x: Int) -> Int =
    match x {
        n -> 1
    }
";
    let diags = parse_diags(src);
    assert_eq!(
        diags.first(),
        Some(&("P039".to_owned(), "{".to_owned())),
        "the P039 guidance must fire first, got: {diags:?}"
    );
}

#[test]
fn match_arm_inline_record_pattern_is_not_p039() {
    // A `{` after the scrutinee can also open a legitimate inline record
    // pattern for the first arm (no `->` inside the braces); it must NOT be
    // mistaken for the Rust-style brace block.
    let src = "\
type Point = { x: Int, y: Int }

fn getX (p: Point) -> Int =
    match p
    { x, y } -> x
";
    let diags = parse_diags(src);
    assert!(
        !diags.iter().any(|(c, _)| c == "P039"),
        "an inline record pattern arm must not fire P039, got: {diags:?}"
    );
}

// ── P040 — `for` / `while` loops do not exist ────────────────────────────────

#[test]
fn for_loop_fires_p040_on_the_keyword() {
    let src = "\
fn f (xs: List Int) -> Int =
    for x in xs
        x
";
    let diags = parse_diags(src);
    assert_eq!(
        diags.first(),
        Some(&("P040".to_owned(), "for".to_owned())),
        "expected P040 underlining `for`, got: {diags:?}"
    );
}

#[test]
fn while_loop_fires_p040_on_the_keyword() {
    let src = "\
fn f (x: Int) -> Int =
    while x > 0
        x
";
    let diags = parse_diags(src);
    assert_eq!(
        diags.first(),
        Some(&("P040".to_owned(), "while".to_owned())),
        "expected P040 underlining `while`, got: {diags:?}"
    );
}

#[test]
fn for_while_prefixes_stay_identifiers() {
    // Only the exact words are reserved: `format`, `foreach`, and `whileX`
    // remain ordinary identifiers, as does the qualified `List.forEach`.
    let src = "\
fn format (whileX: Int) -> Int =
    foreach whileX
";
    assert_eq!(parse_diags(src), Vec::<(String, String)>::new());
}

// ── The canonical Ridge spellings must keep parsing clean ────────────────────

#[test]
fn layout_let_parses_clean() {
    let src = "\
fn f (x: Int) -> Int =
    let y = x + 1
    y * 2
";
    assert_eq!(parse_diags(src), Vec::<(String, String)>::new());
}

#[test]
fn when_guard_parses_clean() {
    let src = "\
fn f (x: Int) -> Int =
    match x
        n when n > 0 -> 1
        _ -> 0
";
    assert_eq!(parse_diags(src), Vec::<(String, String)>::new());
}

#[test]
fn record_update_with_trailing_braces_parses_clean() {
    let src = "\
type Point = { x: Int, y: Int }

fn move0 (p: Point) -> Point =
    p with { x = 0 }
";
    assert_eq!(parse_diags(src), Vec::<(String, String)>::new());
}

/// A record literal with a real shorthand field must not be mistaken for the
/// record-update confusion: `{ x }` is a valid one-field shorthand literal.
#[test]
fn shorthand_record_literal_is_not_flagged() {
    let src = "\
fn f (x: Int) -> { x: Int } =
    { x }
";
    let diags = parse_diags(src);
    assert!(
        !diags.iter().any(|(c, _)| c == "P035"),
        "a plain shorthand record literal must not fire P035, got: {diags:?}"
    );
}
