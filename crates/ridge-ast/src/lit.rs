//! Literal value nodes used in expressions and patterns.

use crate::Span;

/// A literal value as it appears in Ridge source code.
///
/// Numeric literals carry their raw lexeme so that downstream phases (e.g. a
/// constant evaluator) can parse the exact written form.  The lexer validates
/// that the raw text is well-formed; no further parsing is done here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// Decimal integer literal, e.g. `42` or `1_000`.
    IntDec {
        /// Raw lexeme as written in the source.
        raw: String,
        /// Source location.
        span: Span,
    },
    /// Binary integer literal, e.g. `0b1010`.
    IntBin {
        /// Raw lexeme as written in the source.
        raw: String,
        /// Source location.
        span: Span,
    },
    /// Octal integer literal, e.g. `0o17`.
    IntOct {
        /// Raw lexeme as written in the source.
        raw: String,
        /// Source location.
        span: Span,
    },
    /// Hexadecimal integer literal, e.g. `0xFF`.
    IntHex {
        /// Raw lexeme as written in the source.
        raw: String,
        /// Source location.
        span: Span,
    },
    /// Floating-point literal, e.g. `3.14` or `1.0e-3`.
    Float {
        /// Raw lexeme as written in the source.
        raw: String,
        /// Source location.
        span: Span,
    },
    /// Exact decimal literal, e.g. `19.99m` or `5m`. The raw lexeme keeps the
    /// trailing `m`/`M` suffix; downstream phases strip it before parsing.
    Decimal {
        /// Raw lexeme as written in the source, including the `m` suffix.
        raw: String,
        /// Source location.
        span: Span,
    },
    /// Boolean literal.
    Bool {
        /// The boolean value.
        value: bool,
        /// Source location.
        span: Span,
    },
    /// Plain (non-interpolated) text literal, e.g. `"hello"`.
    ///
    /// Escape sequences are validated by the lexer; decoding them (e.g. `\n`
    /// → newline) is deferred to a later lowering phase.
    Text {
        /// Raw lexeme including surrounding quotes.
        raw: String,
        /// Source location.
        span: Span,
    },

    /// A raw string literal `r"..."` / `r#"..."#`.
    ///
    /// The payload is the literal bytes between the delimiters; no escape
    /// sequences are interpreted.  Lowering must NOT apply escape decoding.
    RawText {
        /// Literal string content (no escape processing).
        raw: String,
        /// Source location.
        span: Span,
    },
}

impl Literal {
    /// Return the source span of this literal.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::IntDec { span, .. }
            | Self::IntBin { span, .. }
            | Self::IntOct { span, .. }
            | Self::IntHex { span, .. }
            | Self::Float { span, .. }
            | Self::Decimal { span, .. }
            | Self::Bool { span, .. }
            | Self::Text { span, .. }
            | Self::RawText { span, .. } => *span,
        }
    }
}
