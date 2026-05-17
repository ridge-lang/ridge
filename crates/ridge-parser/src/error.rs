//! Parse error types.
//!
//! `ParseError` codes are **stable across releases** — downstream tooling
//! (LSP, `ariadne` renderer) keys on these strings.  Never renumber an
//! assigned code; only append new ones.
//!
//! T2 implements the four variants required by §6 T2 and §4.7:
//! - `P001 Expected`
//! - `P002 UnexpectedToken`
//! - `P006 LayoutMismatch`
//! - `P999 InternalLayoutInvariantViolated`
//!
//! T5 adds:
//! - `P018 BareRecordPattern`
//!
//! T6 adds:
//! - `P009 NonAssociativeChain`
//!
//! T7 adds:
//! - `P014 EmptyBlock`
//!
//! T10 adds:
//! - `P005 MissingType`
//! - `P012 TopLevelPatternParam`
//! - `P013 DeferredFeature`
//!
//! T11 adds:
//! - `P019 OrphanDocComment`
//!
//! Later tasks (T3–T12) will extend this enum; adding variants is
//! non-breaking because the enum is not `#[non_exhaustive]` — the parser
//! crate owns all construction sites.

use ridge_ast::Span;

/// A parse error produced by `ridge-parser`.
///
/// Every variant carries a [`Span`] pointing to the offending source location
/// and a stable error code returned by [`ParseError::code`].
///
/// `Display` produces a human-readable message suitable for terminal output.
/// `ridge-diagnostics` (Phase 3) will later render these with `ariadne`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ParseError {
    /// P001 — the parser expected a specific token but found something else.
    #[error("expected {expected} but found `{found}`")]
    Expected {
        /// Source location of the unexpected token.
        span: Span,
        /// Static description of the expected token (e.g. `"<EOF>"`, `"->"`).
        expected: &'static str,
        /// The actual token's `Display` representation.
        found: String,
    },

    /// P002 — an unexpected token was encountered with no specific expectation.
    #[error("{description}")]
    UnexpectedToken {
        /// Source location of the unexpected token.
        span: Span,
        /// Human-readable description of the error.
        description: String,
    },

    /// P005 — a type annotation is required but was absent.
    ///
    /// Used by `const` declarations (which always require `: Type`) and
    /// `FieldDecl` in record types.
    #[error("missing type annotation in {context}: expected `: Type`")]
    MissingType {
        /// Source location where the type annotation was expected.
        span: Span,
        /// The syntactic context where the type was expected (e.g. `"const"`,
        /// `"field"`).
        context: &'static str,
    },

    /// P006 — an `Indent`, `Dedent`, or `Newline` token appeared in a context
    /// where the layout invariant was violated.
    #[error("layout mismatch: {hint}")]
    LayoutMismatch {
        /// Source location of the offending layout token.
        span: Span,
        /// Short description of the violation.
        hint: &'static str,
    },

    /// P009 — a non-associative operator was chained without parentheses.
    ///
    /// Ridge comparison operators (`==`, `!=`, `<`, `>`, `<=`, `>=`) are
    /// non-associative (§4.5).  Chaining them — `a == b == c` or `a < b < c`
    /// — is a parse error.  Users must parenthesise: `(a == b) == c`.
    #[error("non-associative operator `{op}` cannot be chained; add parentheses")]
    NonAssociativeChain {
        /// Source location of the second (chained) operator.
        span: Span,
        /// The operator spelling (e.g. `"=="`, `"<"`).
        op: &'static str,
    },

    /// P014 — an `INDENT`/`DEDENT` block contained no statements.
    ///
    /// A block must have at least one expression.  An immediate `DEDENT` after
    /// `INDENT` is a structural error (`P014 EmptyBlock`).
    #[error("empty block: expected at least one statement")]
    EmptyBlock {
        /// Source location of the empty block.
        span: Span,
    },

    /// P012 — a top-level function parameter was a tuple or constructor pattern.
    ///
    /// D037: top-level `fn` parameters must be bare identifiers or annotated
    /// identifiers only.  Full patterns (tuples, constructors, `@` bindings)
    /// are only allowed in lambda parameters.  Use a `let` binding in the body
    /// instead.
    ///
    /// Example: `fn foo (x, y) = x` is invalid; write `fn foo pair = let (x, y) = pair …`.
    #[error("tuple and constructor patterns are not allowed in top-level fn parameters (D037)")]
    TopLevelPatternParam {
        /// Source location of the invalid pattern parameter.
        span: Span,
    },

    /// P013 — a language feature is reserved but deferred to a future version.
    ///
    /// Currently: `class`, `instance`, `deriving`, and `trait` are reserved
    /// keywords (or keyword-like identifiers) with no grammar productions in
    /// 0.1.0.
    #[error("feature `{feature}` is deferred to {since}")]
    DeferredFeature {
        /// Source location of the deferred keyword.
        span: Span,
        /// Short name of the deferred feature (e.g. `"class"`, `"instance"`).
        feature: &'static str,
        /// Target version string (e.g. `"0.2.0"`).
        since: &'static str,
    },

    /// P018 — a record-body pattern `{ … }` was used without a constructor
    /// name.  D051 mandates that a record pattern must start with
    /// `UPPER_IDENT`, e.g. `User { name }`.
    #[error("record patterns require a constructor name (e.g. `User {{ name }}`)")]
    BareRecordPattern {
        /// Source location of the bare `{`.
        span: Span,
    },

    /// P019 — a doc comment appears at a position where it cannot be attached
    /// to any declaration (e.g., trailing at end of file after the last item,
    /// or as the sole content of a file that also has items).
    ///
    /// Per D067 (§15 of spec.md): doc comments must immediately precede a
    /// top-level declaration.  An orphan doc comment is a warning-level error
    /// (the parser does not halt, but the comment is lost).
    #[error("doc comment at invalid position — not attached to any declaration")]
    OrphanDocComment {
        /// Source location of the orphan doc comment.
        span: Span,
    },

    /// P999 — the lexer's bracket-suppression invariant was violated (should
    /// be unreachable; signals a lexer bug, not a user error).
    #[error("internal error: layout invariant violated inside bracketed region")]
    InternalLayoutInvariantViolated {
        /// Source location of the invariant violation.
        span: Span,
    },
}

impl ParseError {
    /// Return the stable error code string for this variant.
    ///
    /// Codes are **stable across releases** — never renumber an assigned code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Expected { .. } => "P001",
            Self::UnexpectedToken { .. } => "P002",
            Self::MissingType { .. } => "P005",
            Self::LayoutMismatch { .. } => "P006",
            Self::NonAssociativeChain { .. } => "P009",
            Self::TopLevelPatternParam { .. } => "P012",
            Self::DeferredFeature { .. } => "P013",
            Self::EmptyBlock { .. } => "P014",
            Self::BareRecordPattern { .. } => "P018",
            Self::OrphanDocComment { .. } => "P019",
            Self::InternalLayoutInvariantViolated { .. } => "P999",
        }
    }

    /// Return the source span associated with this error.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Expected { span, .. }
            | Self::UnexpectedToken { span, .. }
            | Self::MissingType { span, .. }
            | Self::LayoutMismatch { span, .. }
            | Self::NonAssociativeChain { span, .. }
            | Self::TopLevelPatternParam { span }
            | Self::DeferredFeature { span, .. }
            | Self::EmptyBlock { span }
            | Self::BareRecordPattern { span }
            | Self::OrphanDocComment { span }
            | Self::InternalLayoutInvariantViolated { span } => *span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p001_code_and_display() {
        let e = ParseError::Expected {
            span: Span::point(0),
            expected: "<EOF>",
            found: "fn".to_string(),
        };
        assert_eq!(e.code(), "P001");
        assert!(e.to_string().contains("<EOF>"));
        assert!(e.to_string().contains("fn"));
    }

    #[test]
    fn p002_code_and_display() {
        let e = ParseError::UnexpectedToken {
            span: Span::point(5),
            description: "unexpected `}`".to_string(),
        };
        assert_eq!(e.code(), "P002");
        assert!(e.to_string().contains("`}`"));
    }

    #[test]
    fn p006_code_and_display() {
        let e = ParseError::LayoutMismatch {
            span: Span::new(10, 15),
            hint: "DEDENT outside block",
        };
        assert_eq!(e.code(), "P006");
        assert!(e.to_string().contains("DEDENT outside block"));
    }

    #[test]
    fn p999_code_and_display() {
        let e = ParseError::InternalLayoutInvariantViolated {
            span: Span::point(0),
        };
        assert_eq!(e.code(), "P999");
        assert!(e.to_string().contains("invariant"));
    }

    #[test]
    fn span_accessor_returns_carried_span() {
        let span = Span::new(3, 7);
        let e = ParseError::LayoutMismatch { span, hint: "test" };
        assert_eq!(e.span(), span);
    }
}
