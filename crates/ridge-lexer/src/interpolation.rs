//! Interpolation post-processor.
//!
//! The raw scan pass emits `RawToken::InterpStart`, `RawToken::InterpText`,
//! `RawToken::InterpExprStart`, and context-ambiguous `RawToken::RBrace`
//! tokens.  This pass resolves them into the public `Token::Interp*` variants.
//!
//! It also handles the context-sensitive `}` disambiguation: a `}` that closes
//! a `${...}` expression hole becomes `Token::InterpExprEnd`; all other `}`
//! tokens become `Token::RBrace`.
//!
//! # Approach
//!
//! The raw scanner already does most of the heavy lifting inside
//! `scan_interp_body`.  This pass only needs to:
//! 1. Convert `RawToken::InterpStart` → `Token::InterpStart`.
//! 2. Convert `RawToken::InterpText(s)` → `Token::InterpText(s)`.
//! 3. Convert `RawToken::InterpExprStart` → `Token::InterpExprStart`.
//! 4. Resolve `RawToken::RBrace` → `Token::RBrace` (plain `}` outside interp).
//! 5. Validate escape sequences inside `InterpText` segments.
//! 6. Pass all other `RawToken::Token(t)` through unchanged.

use crate::{
    error::LexError, raw_scan::RawToken, span::Span, strings::validate_escapes, token::Token,
};

/// Convert the raw token stream into the public token stream.
///
/// Resolves `RawToken` variants that require context, primarily the
/// interpolation tokens and the ambiguous `}`.
pub(crate) fn process(
    raw: Vec<(RawToken, Span)>,
    errors: &mut Vec<LexError>,
) -> Vec<(Token, Span)> {
    let mut out = Vec::with_capacity(raw.len());

    for (raw_tok, span) in raw {
        match raw_tok {
            RawToken::Token(tok) => {
                out.push((tok, span));
            }
            RawToken::InterpStart => {
                out.push((Token::InterpStart, span));
            }
            RawToken::InterpText(text) => {
                // Validate escape sequences in the interpolated text segment.
                let esc_errors = validate_escapes(&text, span.start);
                errors.extend(esc_errors);
                out.push((Token::InterpText(text), span));
            }
            RawToken::InterpExprStart => {
                out.push((Token::InterpExprStart, span));
            }
            RawToken::RBrace => {
                // Outside interpolation this is a plain `}`.
                out.push((Token::RBrace, span));
            }
            RawToken::Newline => {
                // Pass newlines through to the layout pass.
                out.push((Token::Newline, span));
            }
            RawToken::BlankLine => {
                // Blank lines are already suppressed by the layout pass.
                out.push((Token::Newline, span));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use crate::tokenize;

    #[test]
    fn simple_interp() {
        let out = tokenize(r#"$"hello""#);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let kinds: Vec<_> = out.tokens.iter().map(|(t, _)| t).collect();
        // InterpStart InterpText("hello") InterpEnd Newline? Eof
        assert!(matches!(kinds[0], crate::token::Token::InterpStart));
        assert!(matches!(kinds[1], crate::token::Token::InterpText(s) if s == "hello"));
        assert!(matches!(kinds[2], crate::token::Token::InterpEnd));
    }

    #[test]
    fn interp_with_expr() {
        let out = tokenize(r#"$"a${x}b""#);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let kinds: Vec<_> = out.tokens.iter().map(|(t, _)| t).collect();
        assert!(matches!(kinds[0], crate::token::Token::InterpStart));
        assert!(matches!(kinds[1], crate::token::Token::InterpText(s) if s == "a"));
        assert!(matches!(kinds[2], crate::token::Token::InterpExprStart));
        // ... x ...
        assert!(matches!(kinds.last(), Some(crate::token::Token::Eof)));
    }

    #[test]
    fn empty_interp() {
        let out = tokenize(r#"$"""#);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let kinds: Vec<_> = out.tokens.iter().map(|(t, _)| t).collect();
        assert!(matches!(kinds[0], crate::token::Token::InterpStart));
        assert!(matches!(kinds[1], crate::token::Token::InterpEnd));
    }

    #[test]
    fn adjacent_exprs() {
        // $"${x}${y}" — no INTERP_TEXT between the two expressions.
        let out = tokenize(r#"$"${x}${y}""#);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let kinds: Vec<_> = out.tokens.iter().map(|(t, _)| t).collect();
        assert!(matches!(kinds[0], crate::token::Token::InterpStart));
        assert!(matches!(kinds[1], crate::token::Token::InterpExprStart));
    }
}
