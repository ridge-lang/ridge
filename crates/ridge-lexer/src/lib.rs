//! Ridge lexer: tokenization and layout algorithm.
// Crate-level lint suppressions that are intentional for this crate.
#![allow(clippy::redundant_pub_crate)] // pub(crate) in private modules is fine
#![allow(clippy::cast_possible_truncation)] // usize→u32 casts: files are < 4 GiB
#![allow(clippy::missing_const_for_fn)] // const fn is optional for readability
#![allow(clippy::must_use_candidate)] // not all returned values need #[must_use]
//!
//! # Overview
//!
//! `tokenize` is the single public entry point.  It converts a Ridge source
//! string into a [`LexOutput`] containing the token stream, any lexical errors,
//! and a [`LineMap`] for converting byte offsets to line/column positions.
//!
//! ## Internal pipeline
//!
//! ```text
//! raw_scan(src)          — logos-driven + hand-written sub-scanners
//!   → interpolation pass — splits $"..." into INTERP_* tokens
//!   → layout pass        — inserts NEWLINE / INDENT / DEDENT / EOF
//! ```
//!
//! ## Span convention
//!
//! Every token carries a [`Span`] of byte offsets.  Synthesised layout tokens
//! carry zero-width spans at the boundary offset.

pub mod error;
pub mod span;
pub mod token;

mod doc_comment;
mod interpolation;
mod layout;
mod numbers;
mod raw_scan;
mod strings;

pub use error::LexError;
pub use span::{LineMap, Span};
pub use token::Token;

/// The result of lexing a single source file.
pub struct LexOutput {
    /// The token stream, including synthesised layout tokens.
    ///
    /// Every token carries a [`Span`].  Layout tokens (`Newline`, `Indent`,
    /// `Dedent`, `Eof`) carry zero-width spans at the relevant boundary offset.
    pub tokens: Vec<(Token, Span)>,
    /// Lexical errors accumulated during scanning.  Non-empty does **not**
    /// stop the token stream — downstream phases may continue in best-effort
    /// mode (e.g. for LSP).
    pub errors: Vec<LexError>,
    /// Line-start table; use [`LineMap::line_col`] to convert byte offsets to
    /// human-readable line/column numbers.
    pub line_map: LineMap,
}

/// Tokenize a Ridge source string.
///
/// # Normalisation
///
/// `\r\n` is normalised to `\n`; bare `\r` is also normalised to `\n`
/// (OQ-L008 default).  All spans in the output refer to offsets in the
/// **normalised** string.
///
/// # Errors
///
/// Lexical errors (tabs, bad escapes, unterminated literals, …) are collected
/// in [`LexOutput::errors`] rather than returned as a `Result`.  This lets
/// downstream phases report multiple diagnostics in a single pass.
pub fn tokenize(src: &str) -> LexOutput {
    // OQ-L008: normalise CR/CRLF → LF.
    let normalised: String = normalise_line_endings(src);

    let line_map = LineMap::new(&normalised);

    let (raw_tokens, mut errors) = raw_scan::scan(&normalised);
    let interp_tokens = interpolation::process(raw_tokens, &mut errors);
    let (tokens, layout_errors) = layout::process(&interp_tokens);
    errors.extend(layout_errors);

    LexOutput {
        tokens,
        errors,
        line_map,
    }
}

/// Normalise `\r\n` → `\n` and bare `\r` → `\n`.
fn normalise_line_endings(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            // Consume a following `\n` if present (CRLF → LF).
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_empty() {
        let out = tokenize("");
        assert_eq!(out.tokens.len(), 1);
        assert!(matches!(out.tokens[0].0, Token::Eof));
        assert!(out.errors.is_empty());
    }

    #[test]
    fn normalise_crlf() {
        let s = normalise_line_endings("a\r\nb\rc");
        assert_eq!(s, "a\nb\nc");
    }

    #[test]
    fn smoke_let() {
        let out = tokenize("let x = 1");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        // Should have: KwLet LowerIdent("x") Assign IntDec("1") Newline Eof
        let kinds: Vec<_> = out.tokens.iter().map(|(t, _)| t).collect();
        assert!(matches!(kinds[0], Token::KwLet));
        assert!(matches!(kinds[1], Token::LowerIdent(_)));
        assert!(matches!(kinds[2], Token::Assign));
        assert!(matches!(kinds[3], Token::IntDec(_)));
    }
}
