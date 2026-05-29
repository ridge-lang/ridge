//! Tests for multi-line (`"""..."""`) and raw (`r"..."` / `r#"..."#`) string
//! literals: lexer output, token payloads, and parser round-trips.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ridge_lexer::{tokenize, LexError, Token};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn lex_ok(src: &str) -> Vec<Token> {
    let out = tokenize(src);
    assert!(
        out.errors.is_empty(),
        "unexpected lexer errors for {src:?}:\n{:#?}",
        out.errors
    );
    out.tokens.into_iter().map(|(t, _)| t).collect()
}

fn first_text_lit(src: &str) -> String {
    let toks = lex_ok(src);
    for t in toks {
        if let Token::TextLit(s) = t {
            return s;
        }
    }
    panic!("no TextLit in token stream for: {src:?}");
}

fn first_raw_text_lit(src: &str) -> String {
    let toks = lex_ok(src);
    for t in toks {
        if let Token::RawTextLit(s) = t {
            return s;
        }
    }
    panic!("no RawTextLit in token stream for: {src:?}");
}

// ── Triple-quoted strings ─────────────────────────────────────────────────────

#[test]
fn triple_quote_empty() {
    // `"""\n"""` is an empty string.
    let src = "\"\"\"\n\"\"\"";
    let body = first_text_lit(src);
    assert_eq!(body, "", "empty triple-quote should produce empty body");
}

#[test]
fn triple_quote_flush_no_margin() {
    // Closing `"""` at column 0 — no margin stripped.
    let src = "\"\"\"\nhello\n\"\"\"";
    let body = first_text_lit(src);
    assert_eq!(body, "hello");
}

#[test]
fn triple_quote_indented_margin() {
    // Closing `"""` at 4-space indent — strips 4 spaces from each interior line.
    let src = "\"\"\"\n    hello\n    world\n    \"\"\"";
    let body = first_text_lit(src);
    assert_eq!(body, "hello\nworld");
}

#[test]
fn triple_quote_with_blank_interior_line() {
    // A blank line between two content lines is preserved as `\n`.
    let src = "\"\"\"\n    hello\n\n    world\n    \"\"\"";
    let body = first_text_lit(src);
    assert_eq!(body, "hello\n\nworld");
}

#[test]
fn triple_quote_two_space_margin() {
    let src = "\"\"\"\n  line1\n  line2\n  \"\"\"";
    let body = first_text_lit(src);
    assert_eq!(body, "line1\nline2");
}

#[test]
fn triple_quote_single_line_content() {
    let src = "\"\"\"\n  just one line\n  \"\"\"";
    let body = first_text_lit(src);
    assert_eq!(body, "just one line");
}

#[test]
fn triple_quote_cooked_escapes_preserved_for_lower() {
    // The lexer keeps escape sequences un-decoded; only whitespace is stripped.
    let src = "\"\"\"\n  hello\\nworld\n  \"\"\"";
    let body = first_text_lit(src);
    // The backslash-n is preserved literally in the TextLit payload.
    assert_eq!(body, "hello\\nworld");
}

#[test]
fn triple_quote_emits_text_lit_token() {
    let src = "\"\"\"\n  hi\n  \"\"\"";
    let out = tokenize(src);
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    let kinds: Vec<_> = out.tokens.iter().map(|(t, _)| t).collect();
    assert!(
        kinds.iter().any(|t| matches!(t, Token::TextLit(_))),
        "expected TextLit token; got: {kinds:#?}"
    );
}

// ── Triple-quoted error cases ─────────────────────────────────────────────────

#[test]
fn triple_quote_content_on_open_line_is_error() {
    let src = "\"\"\"oops\n\"\"\"";
    let out = tokenize(src);
    assert!(
        out.errors
            .iter()
            .any(|e| matches!(e, LexError::MultilineStringOpenContent { .. })),
        "expected MultilineStringOpenContent error; errors: {:#?}",
        out.errors
    );
}

#[test]
fn triple_quote_unterminated_is_error() {
    let src = "\"\"\"\nhello";
    let out = tokenize(src);
    assert!(
        out.errors
            .iter()
            .any(|e| matches!(e, LexError::UnterminatedMultilineString { .. })),
        "expected UnterminatedMultilineString error; errors: {:#?}",
        out.errors
    );
}

// ── Raw strings ───────────────────────────────────────────────────────────────

#[test]
fn raw_string_basic() {
    let src = "r\"hello\"";
    let body = first_raw_text_lit(src);
    assert_eq!(body, "hello");
}

#[test]
fn raw_string_empty() {
    let src = "r\"\"";
    let body = first_raw_text_lit(src);
    assert_eq!(body, "");
}

#[test]
fn raw_string_backslash_not_decoded() {
    // `\n` in a raw string is the literal two characters `\` and `n`, not a newline.
    let src = "r\"hello\\nworld\"";
    let body = first_raw_text_lit(src);
    assert_eq!(body, "hello\\nworld");
}

#[test]
fn raw_string_one_hash_embeds_plain_quote() {
    // `r#"say "hi""#` contains an embedded `"` in the body.
    let src = "r#\"say \\\"hi\\\"\"#";
    // Actually construct the source properly:
    // r#"say "hi""# in Ridge source is: r#"say "hi""#
    let src2 = "r#\"say \"hi\"\"#";
    let body = first_raw_text_lit(src2);
    assert_eq!(body, "say \"hi\"");
    // The original with escapes should also work — but those ARE literal.
    let body2 = first_raw_text_lit(src);
    assert!(
        body2.contains('\\'),
        "backslash must be literal in raw string"
    );
}

#[test]
fn raw_string_multiline_no_dedent() {
    // Raw strings may span lines without any dedent processing.
    let src = "r\"line1\nline2\"";
    let body = first_raw_text_lit(src);
    assert_eq!(body, "line1\nline2");
}

#[test]
fn raw_string_two_hashes_embeds_quote_hash() {
    // `r##"..."## ` allows `"#` inside the body.
    let src = "r##\"hello \"# world\"##";
    let body = first_raw_text_lit(src);
    assert_eq!(body, "hello \"# world");
}

#[test]
fn raw_string_emits_raw_text_lit_token() {
    let src = "r\"test\"";
    let out = tokenize(src);
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    let kinds: Vec<_> = out.tokens.iter().map(|(t, _)| t).collect();
    assert!(
        kinds.iter().any(|t| matches!(t, Token::RawTextLit(_))),
        "expected RawTextLit token; got: {kinds:#?}"
    );
}

#[test]
fn raw_string_plain_r_followed_by_space_is_ident_not_raw() {
    // `r "hello"` — `r` with a space before the quote is a regular identifier
    // followed by a plain string, NOT a raw string.
    let src = "r \"hello\"";
    let out = tokenize(src);
    assert!(out.errors.is_empty(), "{:#?}", out.errors);
    let kinds: Vec<_> = out.tokens.iter().map(|(t, _)| t).collect();
    assert!(
        kinds
            .iter()
            .any(|t| matches!(t, Token::LowerIdent(s) if s == "r")),
        "expected LowerIdent 'r'; got: {kinds:#?}"
    );
    assert!(
        kinds.iter().any(|t| matches!(t, Token::TextLit(_))),
        "expected TextLit after 'r'; got: {kinds:#?}"
    );
    assert!(
        !kinds.iter().any(|t| matches!(t, Token::RawTextLit(_))),
        "must NOT produce RawTextLit for `r \"...\"`; got: {kinds:#?}"
    );
}

#[test]
fn raw_string_unterminated_is_error() {
    let src = "r\"no close";
    let out = tokenize(src);
    assert!(
        out.errors
            .iter()
            .any(|e| matches!(e, LexError::UnterminatedMultilineString { kind: "raw", .. })),
        "expected UnterminatedMultilineString (raw); errors: {:#?}",
        out.errors
    );
}

#[test]
fn raw_string_hash_mismatch_is_error() {
    // `r##"..."#` — closed with only one `#`, needs two.
    let src = "r##\"hello\"#";
    let out = tokenize(src);
    assert!(
        out.errors
            .iter()
            .any(|e| matches!(e, LexError::UnterminatedMultilineString { .. })),
        "expected UnterminatedMultilineString for hash mismatch; errors: {:#?}",
        out.errors
    );
}

// ── Regression: existing plain strings still work ────────────────────────────

#[test]
fn plain_string_still_works() {
    let src = "\"hello world\"";
    let body = first_text_lit(src);
    assert_eq!(body, "hello world");
}

#[test]
fn plain_string_with_escape_still_works() {
    let src = "\"hello\\nworld\"";
    let body = first_text_lit(src);
    assert_eq!(body, "hello\\nworld"); // un-decoded at lex time
}

#[test]
fn plain_string_does_not_accept_newline() {
    // `"..."` is still single-line.
    let src = "\"hello\nworld\"";
    let out = tokenize(src);
    assert!(
        out.errors
            .iter()
            .any(|e| matches!(e, LexError::UnterminatedString { .. })),
        "expected UnterminatedString for multi-line plain string; errors: {:#?}",
        out.errors
    );
}
