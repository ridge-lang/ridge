//! Negative / diagnostic tests — one per `LexError` variant.
//!
//! Each test asserts:
//! 1. The expected error IS raised.
//! 2. The span is byte-correct.
//! 3. The Display message is human-readable.
#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may use unwrap/expect

use ridge_lexer::{tokenize, LexError};

fn errors(src: &str) -> Vec<LexError> {
    tokenize(src).errors
}

fn first_error(src: &str) -> LexError {
    let errs = errors(src);
    assert!(
        !errs.is_empty(),
        "expected at least one error from: {src:?}"
    );
    errs.into_iter().next().expect("just checked non-empty")
}

// ── TabForbidden ──────────────────────────────────────────────────────────────

#[test]
fn tab_forbidden_at_start() {
    let src = "\tlet x = 1";
    let e = first_error(src);
    assert!(matches!(e, LexError::TabForbidden { span } if span.start == 0));
    // Message must mention "tab".
    assert!(e.to_string().contains("tab"), "{e}");
}

#[test]
fn tab_forbidden_mid_line() {
    let src = "let\tx = 1";
    let e = first_error(src);
    assert!(matches!(e, LexError::TabForbidden { span } if span.start == 3));
}

// ── UnterminatedString ────────────────────────────────────────────────────────

#[test]
fn unterminated_string_basic() {
    let src = r#"let x = "abc"#; // no closing quote
    let e = first_error(src);
    assert!(
        matches!(e, LexError::UnterminatedString { .. }),
        "expected UnterminatedString, got: {e:?}"
    );
    assert!(e.to_string().contains("unterminated"), "{e}");
}

#[test]
fn unterminated_string_span() {
    // Opening `"` is at byte 8.
    let src = r#"let x = "hello"#;
    let e = first_error(src);
    assert!(
        matches!(e, LexError::UnterminatedString { open_span } if open_span.start == 8),
        "wrong span: {e:?}"
    );
}

// ── UnterminatedInterpolation ─────────────────────────────────────────────────

#[test]
fn unterminated_interp_basic() {
    let src = r#"$"hello"#; // no closing "
    let e = first_error(src);
    assert!(
        matches!(e, LexError::UnterminatedInterpolation { .. }),
        "expected UnterminatedInterpolation, got: {e:?}"
    );
}

#[test]
fn unterminated_interp_expr() {
    // `${x` without closing `}`
    let src = r#"$"${x"#;
    let e = first_error(src);
    assert!(
        matches!(e, LexError::UnterminatedInterpolation { .. }),
        "expected UnterminatedInterpolation, got: {e:?}"
    );
}

// ── UnterminatedDocComment ────────────────────────────────────────────────────

#[test]
fn unterminated_doc_comment() {
    let src = "---\nhello\n";
    let e = first_error(src);
    assert!(
        matches!(e, LexError::UnterminatedDocComment { .. }),
        "expected UnterminatedDocComment, got: {e:?}"
    );
    assert!(e.to_string().contains("unterminated"), "{e}");
}

// ── InvalidEscape ─────────────────────────────────────────────────────────────

#[test]
fn invalid_escape_x() {
    let src = r#"let x = "a\x00b""#;
    let e = first_error(src);
    assert!(
        matches!(&e, LexError::InvalidEscape { got, .. } if got == r"\x"),
        "expected InvalidEscape(\\x), got: {e:?}"
    );
    assert!(e.to_string().contains(r"\x"), "{e}");
}

#[test]
fn invalid_escape_j() {
    let src = r#""a\jb""#;
    let e = first_error(src);
    assert!(
        matches!(&e, LexError::InvalidEscape { got, .. } if got == r"\j"),
        "expected InvalidEscape(\\j), got: {e:?}"
    );
}

// ── InvalidUnicodeEscape ──────────────────────────────────────────────────────

#[test]
fn unicode_escape_non_hex() {
    let src = r#""\u{ZZZ}""#;
    let e = first_error(src);
    assert!(
        matches!(
            &e,
            LexError::InvalidUnicodeEscape {
                reason: ridge_lexer::error::UnicodeEscapeError::InvalidHex,
                ..
            }
        ),
        "expected InvalidHex, got: {e:?}"
    );
}

#[test]
fn unicode_escape_out_of_range() {
    let src = r#""\u{110000}""#;
    let e = first_error(src);
    assert!(
        matches!(
            &e,
            LexError::InvalidUnicodeEscape {
                reason: ridge_lexer::error::UnicodeEscapeError::OutOfRange,
                ..
            }
        ),
        "expected OutOfRange, got: {e:?}"
    );
}

#[test]
fn unicode_escape_unterminated() {
    let src = r#""\u{41""#;
    let e = first_error(src);
    assert!(
        matches!(
            &e,
            LexError::InvalidUnicodeEscape {
                reason: ridge_lexer::error::UnicodeEscapeError::Unterminated,
                ..
            }
        ),
        "expected Unterminated, got: {e:?}"
    );
}

// ── TrailingUnderscoreLiteral ─────────────────────────────────────────────────

#[test]
fn trailing_underscore_dec() {
    let src = "let x = 1_";
    let e = first_error(src);
    assert!(
        matches!(e, LexError::TrailingUnderscoreLiteral { .. }),
        "expected TrailingUnderscoreLiteral, got: {e:?}"
    );
    assert!(
        e.to_string().contains("trailing") || e.to_string().contains("underscore"),
        "{e}"
    );
}

#[test]
fn trailing_underscore_hex() {
    let src = "let x = 0xDEAD_";
    let e = first_error(src);
    assert!(
        matches!(e, LexError::TrailingUnderscoreLiteral { .. }),
        "expected TrailingUnderscoreLiteral, got: {e:?}"
    );
}

// ── InconsistentDedent ────────────────────────────────────────────────────────

#[test]
fn inconsistent_dedent() {
    // Indent by 4 spaces then dedent to 2 (not in the stack).
    let src = "let x =\n    a\n  b";
    let errs = errors(src);
    assert!(
        errs.iter()
            .any(|e| matches!(e, LexError::InconsistentDedent { col, .. } if *col == 2)),
        "expected InconsistentDedent(col=2): {errs:?}"
    );
    let e = errs
        .iter()
        .find(|e| matches!(e, LexError::InconsistentDedent { .. }))
        .expect("found InconsistentDedent above");
    assert!(
        e.to_string().contains("inconsistent") || e.to_string().contains("dedent"),
        "{e}"
    );
}

// ── IndentAtTopLevel ──────────────────────────────────────────────────────────

#[test]
fn indent_at_top_level() {
    let src = "  let x = 1";
    let e = first_error(src);
    assert!(
        matches!(e, LexError::IndentAtTopLevel { .. }),
        "expected IndentAtTopLevel, got: {e:?}"
    );
    assert!(
        e.to_string().contains("top-level") || e.to_string().contains("column 0"),
        "{e}"
    );
}

// ── UnexpectedCharacter ───────────────────────────────────────────────────────

#[test]
fn unexpected_character() {
    // `#` is not a valid Ridge token.
    let src = "let x = #";
    let e = first_error(src);
    assert!(
        matches!(&e, LexError::UnexpectedCharacter { ch: '#', .. }),
        "expected UnexpectedCharacter('#'), got: {e:?}"
    );
    assert!(e.to_string().contains('#'), "{e}");
}

// ── MultilineStringOpenContent ───────────────────────────────────────────────

#[test]
fn multiline_open_content_on_same_line() {
    // Content immediately after the opening `"""` — must error.
    let src = "\"\"\"hello\nworld\n\"\"\"";
    let e = first_error(src);
    assert!(
        matches!(e, LexError::MultilineStringOpenContent { .. }),
        "expected MultilineStringOpenContent, got: {e:?}"
    );
    assert!(
        e.to_string().contains("content") || e.to_string().contains("\"\"\""),
        "message should mention the delimiter: {e}"
    );
}

// ── MultilineStringInsufficientIndent ────────────────────────────────────────

#[test]
fn multiline_insufficient_indent() {
    // Closing `"""` has 4-space margin, but the interior line has only 2.
    let src = "\"\"\"\n  hello\n    \"\"\"";
    let e = first_error(src);
    assert!(
        matches!(e, LexError::MultilineStringInsufficientIndent { .. }),
        "expected MultilineStringInsufficientIndent, got: {e:?}"
    );
}

// ── UnterminatedMultilineString ───────────────────────────────────────────────

#[test]
fn unterminated_triple_quote() {
    let src = "\"\"\"\nhello\n";
    let e = first_error(src);
    assert!(
        matches!(
            e,
            LexError::UnterminatedMultilineString {
                kind: "triple-quoted",
                ..
            }
        ),
        "expected UnterminatedMultilineString (triple-quoted), got: {e:?}"
    );
    assert!(e.to_string().contains("unterminated"), "{e}");
}

#[test]
fn unterminated_raw_string() {
    let src = "r\"hello";
    let e = first_error(src);
    assert!(
        matches!(e, LexError::UnterminatedMultilineString { kind: "raw", .. }),
        "expected UnterminatedMultilineString (raw), got: {e:?}"
    );
    assert!(e.to_string().contains("unterminated"), "{e}");
}

#[test]
fn unterminated_raw_string_hashed() {
    // Closed with `"#` but needs `"##` — unterminated.
    let src = "r##\"hello\"#";
    let e = first_error(src);
    assert!(
        matches!(e, LexError::UnterminatedMultilineString { kind: "raw", .. }),
        "expected UnterminatedMultilineString (raw), got: {e:?}"
    );
}

// ── Span correctness ─────────────────────────────────────────────────────────

#[test]
fn error_span_accessor_works() {
    let src = "\tlet x = 1";
    let e = first_error(src);
    let span = e.span();
    assert_eq!(span.start, 0);
    assert!(span.end > 0);
}

// ── Multiple errors collected ─────────────────────────────────────────────────

#[test]
fn multiple_errors_in_one_file() {
    let src = "\tlet x = \"a\\x00\" + \"b\\j\"";
    let errs = errors(src);
    // Expect: TabForbidden + at least 2 InvalidEscape errors.
    assert!(errs.len() >= 3, "expected at least 3 errors: {errs:?}");
    assert!(errs
        .iter()
        .any(|e| matches!(e, LexError::TabForbidden { .. })));
    assert!(errs
        .iter()
        .any(|e| matches!(e, LexError::InvalidEscape { .. })));
}
