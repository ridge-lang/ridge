//! Structured lexical error types.

use crate::span::Span;

/// A reason a Unicode escape could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnicodeEscapeError {
    /// The digits inside `\u{...}` were not valid hexadecimal.
    InvalidHex,
    /// The value decoded to a surrogate code point or exceeds `0x10FFFF`.
    OutOfRange,
    /// The `\u{...}` sequence was not properly terminated.
    Unterminated,
}

impl std::fmt::Display for UnicodeEscapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHex => write!(f, "invalid hexadecimal digit in \\u{{...}}"),
            Self::OutOfRange => write!(
                f,
                "Unicode scalar value out of range (must be ≤ U+10FFFF and not a surrogate)"
            ),
            Self::Unterminated => write!(f, "unterminated \\u{{...}} escape sequence"),
        }
    }
}

/// A lexical error encountered while scanning Ridge source text.
///
/// Errors are accumulated rather than short-circuiting; the token stream
/// continues (with best-effort recovery) so that multiple diagnostics can be
/// reported in a single pass.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LexError {
    /// A tab character was found in source code outside a string literal.
    /// Tabs are forbidden per spec §4.2 line 517.
    #[error(
        "tab character is not allowed in Ridge source; use spaces for indentation (at byte {span})"
    )]
    TabForbidden { span: Span },

    /// A string literal was opened but never closed before end-of-line or EOF.
    #[error("unterminated string literal opened at byte {open_span}")]
    UnterminatedString { open_span: Span },

    /// An interpolated string (`$"..."`) was opened but never closed.
    #[error("unterminated interpolated string opened at byte {open_span}")]
    UnterminatedInterpolation { open_span: Span },

    /// A block doc-comment (`---` ... `---`) was opened but EOF was reached
    /// before the closing `---` line.
    #[error("unterminated doc-comment block opened at byte {open_span}")]
    UnterminatedDocComment { open_span: Span },

    /// An unrecognised escape sequence inside a string literal or interpolated
    /// text segment (e.g. `\x`, `\j`, …).
    #[error("invalid escape sequence `{got}` at byte {span}")]
    InvalidEscape { span: Span, got: String },

    /// A `\u{{...}}` escape sequence was syntactically present but its value
    /// could not be decoded.
    #[error("invalid Unicode escape at byte {span}: {reason}")]
    InvalidUnicodeEscape {
        span: Span,
        reason: UnicodeEscapeError,
    },

    /// A dedent returned to a column that does not match any previously pushed
    /// level (e.g. indenting by 5 spaces after pushing 4, then dedenting to 2).
    #[error("inconsistent dedent at byte {span}: column {col} does not match any open block (open levels: {expected:?})")]
    InconsistentDedent {
        span: Span,
        /// The column that was found.
        col: u32,
        /// The currently open indent levels (innermost last).
        expected: Vec<u32>,
    },

    /// A numeric literal had a leading underscore where none is allowed
    /// (e.g. `_123`).
    #[error("numeric literal may not begin with an underscore at byte {span}")]
    LeadingUnderscoreLiteral { span: Span },

    /// A numeric literal had a trailing underscore (e.g. `1_000_`).
    #[error("numeric literal may not end with an underscore at byte {span}")]
    TrailingUnderscoreLiteral { span: Span },

    /// A base-prefix literal had no digits after the prefix (e.g. `0x`, `0b`).
    #[error("empty numeric literal: expected digits after the base prefix at byte {span}")]
    EmptyNumericLiteral { span: Span },

    /// An unexpected character that belongs to no token class.
    #[error("unexpected character `{ch}` at byte {span}")]
    UnexpectedCharacter { span: Span, ch: char },

    /// The first non-blank line of the file is indented (column > 0).
    #[error("top-level declaration must begin at column 0; found indentation at byte {span}")]
    IndentAtTopLevel { span: Span },
}

impl LexError {
    /// Return the stable `L###` error code for this variant.
    ///
    /// Codes are **stable across releases** — never renumber an assigned code.
    /// `L001`–`L010` are allocated for the ten `LexError` variants (lexer
    /// sub-namespace; does not collide with the `L800`–`L899` LSP reservation).
    ///
    /// Approved as a frozen-crate additive exception per FROZEN-01 (2026-05-01).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TabForbidden { .. } => "L001",
            Self::UnterminatedString { .. } => "L002",
            Self::UnterminatedInterpolation { .. } => "L003",
            Self::UnterminatedDocComment { .. } => "L004",
            Self::InvalidEscape { .. } => "L005",
            Self::InvalidUnicodeEscape { .. } => "L006",
            Self::InconsistentDedent { .. } => "L007",
            Self::LeadingUnderscoreLiteral { .. } => "L008",
            Self::TrailingUnderscoreLiteral { .. } => "L009",
            Self::EmptyNumericLiteral { .. } => "L010",
            Self::UnexpectedCharacter { .. } => "L011",
            Self::IndentAtTopLevel { .. } => "L012",
        }
    }

    /// The primary byte span associated with this error.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::TabForbidden { span }
            | Self::InvalidEscape { span, .. }
            | Self::InvalidUnicodeEscape { span, .. }
            | Self::InconsistentDedent { span, .. }
            | Self::LeadingUnderscoreLiteral { span }
            | Self::TrailingUnderscoreLiteral { span }
            | Self::EmptyNumericLiteral { span }
            | Self::UnexpectedCharacter { span, .. }
            | Self::IndentAtTopLevel { span } => *span,

            Self::UnterminatedString { open_span }
            | Self::UnterminatedInterpolation { open_span }
            | Self::UnterminatedDocComment { open_span } => *open_span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── code() stability tests (FROZEN-01, one per variant) ──────────────────

    #[test]
    fn tab_forbidden_code_is_stable() {
        assert_eq!(
            LexError::TabForbidden {
                span: Span::new(0, 1)
            }
            .code(),
            "L001"
        );
    }

    #[test]
    fn unterminated_string_code_is_stable() {
        assert_eq!(
            LexError::UnterminatedString {
                open_span: Span::new(0, 1)
            }
            .code(),
            "L002"
        );
    }

    #[test]
    fn unterminated_interpolation_code_is_stable() {
        assert_eq!(
            LexError::UnterminatedInterpolation {
                open_span: Span::new(0, 1)
            }
            .code(),
            "L003"
        );
    }

    #[test]
    fn unterminated_doc_comment_code_is_stable() {
        assert_eq!(
            LexError::UnterminatedDocComment {
                open_span: Span::new(0, 1)
            }
            .code(),
            "L004"
        );
    }

    #[test]
    fn invalid_escape_code_is_stable() {
        assert_eq!(
            LexError::InvalidEscape {
                span: Span::new(0, 1),
                got: "\\x".into()
            }
            .code(),
            "L005"
        );
    }

    #[test]
    fn invalid_unicode_escape_code_is_stable() {
        assert_eq!(
            LexError::InvalidUnicodeEscape {
                span: Span::new(0, 1),
                reason: UnicodeEscapeError::InvalidHex
            }
            .code(),
            "L006"
        );
    }

    #[test]
    fn inconsistent_dedent_code_is_stable() {
        assert_eq!(
            LexError::InconsistentDedent {
                span: Span::new(0, 1),
                col: 2,
                expected: vec![0, 4]
            }
            .code(),
            "L007"
        );
    }

    #[test]
    fn leading_underscore_literal_code_is_stable() {
        assert_eq!(
            LexError::LeadingUnderscoreLiteral {
                span: Span::new(0, 1)
            }
            .code(),
            "L008"
        );
    }

    #[test]
    fn trailing_underscore_literal_code_is_stable() {
        assert_eq!(
            LexError::TrailingUnderscoreLiteral {
                span: Span::new(0, 1)
            }
            .code(),
            "L009"
        );
    }

    #[test]
    fn empty_numeric_literal_code_is_stable() {
        assert_eq!(
            LexError::EmptyNumericLiteral {
                span: Span::new(0, 1)
            }
            .code(),
            "L010"
        );
    }

    #[test]
    fn unexpected_character_code_is_stable() {
        assert_eq!(
            LexError::UnexpectedCharacter {
                span: Span::new(0, 1),
                ch: '@'
            }
            .code(),
            "L011"
        );
    }

    #[test]
    fn indent_at_top_level_code_is_stable() {
        assert_eq!(
            LexError::IndentAtTopLevel {
                span: Span::new(0, 1)
            }
            .code(),
            "L012"
        );
    }

    // ── existing display / span tests ─────────────────────────────────────────

    #[test]
    fn tab_forbidden_display() {
        let e = LexError::TabForbidden {
            span: Span::new(0, 1),
        };
        let s = e.to_string();
        assert!(s.contains("tab"), "message should mention 'tab': {s}");
        assert!(s.contains("0..1"), "message should contain span: {s}");
    }

    #[test]
    fn unterminated_string_display() {
        let e = LexError::UnterminatedString {
            open_span: Span::new(5, 6),
        };
        let s = e.to_string();
        assert!(s.contains("unterminated"), "{s}");
    }

    #[test]
    fn invalid_escape_display() {
        let e = LexError::InvalidEscape {
            span: Span::new(10, 12),
            got: r"\x".to_string(),
        };
        let s = e.to_string();
        assert!(s.contains(r"\x"), "{s}");
    }

    #[test]
    fn error_span_accessor() {
        let e = LexError::TabForbidden {
            span: Span::new(3, 4),
        };
        assert_eq!(e.span(), Span::new(3, 4));
    }

    #[test]
    fn unicode_escape_errors_display() {
        assert!(UnicodeEscapeError::InvalidHex
            .to_string()
            .contains("hexadecimal"));
        assert!(UnicodeEscapeError::OutOfRange.to_string().contains("range"));
        assert!(UnicodeEscapeError::Unterminated
            .to_string()
            .contains("unterminated"));
    }
}
