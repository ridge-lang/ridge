//! Lowering-phase diagnostic types (`L###` namespace).
//!
//! Most variants in [`LowerError`] are **defensive** — reachable only when the
//! input is structurally well-typed and an invariant a lowering rule assumed is
//! violated. Two are not: `L110` and `L111` are ordinary user errors, because
//! the lexer validates a numeric literal's *form* and never its *value*.
//!
//! # Error code namespace
//!
//! `L101`–`L199` — desugaring rule violations (pipe, try, with, guard, …).
//! `L997`–`L999` — internal consistency / catch-all codes.
//!
//! # Display format
//!
//! Each variant's [`std::fmt::Display`] impl produces the message alone. The
//! renderer prints the code and underlines the span, so a message doing either
//! of those says it twice.

use ridge_ast::Span;
use ridge_resolve::Severity;
use std::fmt;

// OQ-L002: L### defensive code surface is kept (not removed) so that invariant
// violations are traceable in production logs, even though they can only fire on
// malformed upstream output (valid programs never emit them).
/// Lowering-phase diagnostics (`L###`).
///
/// Every variant carries a [`Span`] pointing to the offending AST node so that
/// the renderer can highlight the relevant source location.
///
/// All variants are emitted with [`Severity::Error`] severity.
///
/// # Error codes
///
/// | Variant                    | Code   | Rule  |
/// |---------------------------|--------|-------|
/// | `MalformedPipeRhs`        | `L101` | §4.1  |
/// | `UnknownPipeRhsShape`     | `L102` | §4.1  |
/// | `PropagateOutsideScope`   | `L103` | §4.2  |
/// | `DoublePropagate`         | `L104` | §4.3  |
/// | `EmptyTryBlock`           | `L105` | §4.4  |
/// | `BareGuardExpr`           | `L106` | §4.5  |
/// | `ToTextLowering`          | `L107` | §4.6  |
/// | `WithOnNonRecord`         | `L108` | §4.7  |
/// | `RefutableSliceElement`   | `L109` | §4.8  |
/// | `IntLiteralOutOfRange`    | `L110` | §4.9  |
/// | `FloatLiteralNotFinite`   | `L111` | §4.9  |
/// | `UnsolvedTypeInIR`        | `L997` | §5    |
/// | `CapVarInIR`              | `L998` | §5    |
/// | `InternalLoweringError`   | `L999` | §5    |
#[derive(Debug, Clone)]
pub enum LowerError {
    /// `L101` — pipe RHS is not a valid call/section shape (§4.1).
    MalformedPipeRhs {
        /// The span of the offending RHS expression.
        span: Span,
    },
    /// `L102` — pipe RHS shape could not be classified by the lowerer (§4.1).
    UnknownPipeRhsShape {
        /// The span of the unrecognised RHS expression.
        span: Span,
    },
    /// `L103` — `?`/`try` propagation used outside any `Option`/`Result`-typed
    /// scope (§4.2). The propagation-scope stack was empty.
    PropagateOutsideScope {
        /// The span of the propagation operator or `try` expression.
        span: Span,
    },
    /// `L104` — two propagation operators nested in a way that is structurally
    /// ambiguous (§4.3).
    DoublePropagate {
        /// The span of the inner (duplicate) propagation operator.
        span: Span,
    },
    /// `L105` — `try` block with an empty body encountered (§4.4).
    EmptyTryBlock {
        /// The span of the empty `try` block.
        span: Span,
    },
    /// `L106` — guard expression (`when`) appears outside a `match` arm, where
    /// it cannot be desugared (§4.5).
    BareGuardExpr {
        /// The span of the bare `when` guard.
        span: Span,
    },
    /// `L107` — string-interpolation `ToText` lowering encountered a node for
    /// which no `Display` coercion could be synthesised (§4.6).
    ToTextLowering {
        /// The span of the interpolation segment that could not be lowered.
        span: Span,
    },
    /// `L108` — `with` expression applied to a non-record type (§4.7).
    WithOnNonRecord {
        /// The span of the `with` expression.
        span: Span,
    },
    /// `L109` — a refutable sub-pattern appears in a suffix or middle position
    /// of a variable-length list slice pattern (§4.8 P026).
    ///
    /// Suffix and middle positions must be irrefutable (a variable or `_`) in
    /// this version.  The pattern at `span` cannot be matched structurally
    /// because suffix/middle extraction uses runtime list operations that run
    /// in the arm body, not in an Erlang case clause pattern.
    RefutableSliceElement {
        /// The span of the refutable sub-pattern.
        span: Span,
    },
    /// `L110` — an integer literal does not fit in the `Int` range (`i64`).
    ///
    /// Unlike the other variants this one IS reachable from valid-typed user
    /// input: the lexer validates the literal's *form* but not its *value*, so
    /// `99999999999999999999` typechecks as `Int` and only fails here, where
    /// the raw lexeme is parsed.  Not a compiler bug — a user error with a
    /// dedicated code.
    IntLiteralOutOfRange {
        /// The span of the offending literal.
        span: Span,
        /// The raw lexeme as written in the source.
        raw: String,
    },
    /// `L111` — a float literal is not a finite number.
    ///
    /// Reachable from user input the same way `L110` is, and for the same
    /// reason: the lexer validates the literal's form, not its value. What
    /// makes it easy to miss is that Rust's `f64` parser does not consider
    /// overflow an error — `"1.0e400".parse::<f64>()` returns `Ok(inf)` — so
    /// nothing failed and the infinity travelled all the way to codegen, which
    /// cannot render it as a Core Erlang literal and said so in its own terms.
    FloatLiteralNotFinite {
        /// The span of the offending literal.
        span: Span,
        /// The raw lexeme as written in the source.
        raw: String,
    },
    /// `L997` — an unsolved type variable reached the IR, indicating incomplete
    /// typecheck output was passed to the lowerer.
    UnsolvedTypeInIR {
        /// The span of the expression whose type could not be resolved.
        span: Span,
    },
    /// `L998` — a capability variable reached the IR.  Capability polymorphism
    /// must be resolved before lowering.
    CapVarInIR {
        /// The span of the expression whose capability set contained a variable.
        span: Span,
    },
    /// `L999` — catch-all internal lowering invariant violation.
    InternalLoweringError {
        /// The span closest to the violation.
        span: Span,
        /// A developer-facing description of the violated invariant.
        message: String,
    },
}

impl LowerError {
    /// Returns the stable `L###` error code string for this variant.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MalformedPipeRhs { .. } => "L101",
            Self::UnknownPipeRhsShape { .. } => "L102",
            Self::PropagateOutsideScope { .. } => "L103",
            Self::DoublePropagate { .. } => "L104",
            Self::EmptyTryBlock { .. } => "L105",
            Self::BareGuardExpr { .. } => "L106",
            Self::ToTextLowering { .. } => "L107",
            Self::WithOnNonRecord { .. } => "L108",
            Self::RefutableSliceElement { .. } => "L109",
            Self::IntLiteralOutOfRange { .. } => "L110",
            Self::FloatLiteralNotFinite { .. } => "L111",
            Self::UnsolvedTypeInIR { .. } => "L997",
            Self::CapVarInIR { .. } => "L998",
            Self::InternalLoweringError { .. } => "L999",
        }
    }

    /// Returns the primary source span associated with this diagnostic.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::MalformedPipeRhs { span }
            | Self::UnknownPipeRhsShape { span }
            | Self::PropagateOutsideScope { span }
            | Self::DoublePropagate { span }
            | Self::EmptyTryBlock { span }
            | Self::BareGuardExpr { span }
            | Self::ToTextLowering { span }
            | Self::WithOnNonRecord { span }
            | Self::RefutableSliceElement { span }
            | Self::IntLiteralOutOfRange { span, .. }
            | Self::FloatLiteralNotFinite { span, .. }
            | Self::UnsolvedTypeInIR { span }
            | Self::CapVarInIR { span }
            | Self::InternalLoweringError { span, .. } => *span,
        }
    }

    /// Returns the severity of this diagnostic.
    ///
    /// All lowering errors are [`Severity::Error`] — they indicate violated
    /// lowering invariants that cannot occur on valid, fully-typechecked input.
    ///
    /// Note: [`Severity`] is `#[non_exhaustive]`; this match is exhaustive
    /// because we only ever emit `Severity::Error` here (the closest available
    /// variant to "internal").
    #[must_use]
    pub const fn severity(&self) -> Severity {
        Severity::Error
    }
}

impl fmt::Display for LowerError {
    // The message says what went wrong and nothing else. The renderer prints
    // the code and underlines the span itself, so a message that repeats the
    // code and appends `at Span { start: 94, end: 120 }` says everything twice
    // and leaks the compiler's own bookkeeping on the second pass. These read
    // as debug output because for years nothing rendered them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedPipeRhs { .. } => {
                write!(f, "the right-hand side of `|>` is not a call or a section")
            }
            Self::UnknownPipeRhsShape { .. } => {
                write!(f, "this shape cannot appear on the right-hand side of `|>`")
            }
            Self::PropagateOutsideScope { .. } => {
                write!(
                    f,
                    "`?` needs a function returning `Option` or `Result` to propagate out of"
                )
            }
            Self::DoublePropagate { .. } => {
                write!(f, "`?` is applied twice to the same expression")
            }
            Self::EmptyTryBlock { .. } => write!(f, "this `try` block has no body"),
            Self::BareGuardExpr { .. } => {
                write!(f, "a `when` guard belongs to a match arm, not here")
            }
            Self::ToTextLowering { .. } => {
                write!(
                    f,
                    "this interpolated value has no `ToText`, so it cannot be rendered"
                )
            }
            Self::WithOnNonRecord { .. } => {
                write!(f, "`with` updates a record, and this value is not one")
            }
            Self::RefutableSliceElement { .. } => {
                write!(
                    f,
                    "a pattern after a list slice's variable-length part must be a name or `_`"
                )
            }
            Self::IntLiteralOutOfRange { raw, .. } => {
                write!(
                    f,
                    "the integer literal `{raw}` does not fit in `Int` (-9223372036854775808 to 9223372036854775807)"
                )
            }
            Self::FloatLiteralNotFinite { raw, .. } => {
                write!(f, "the float literal `{raw}` is not a finite number")
            }
            // The last three are the compiler admitting a bug in itself. They
            // name the phase that broke rather than the user's code, because
            // there is nothing the reader can change to avoid them.
            Self::UnsolvedTypeInIR { .. } => {
                write!(
                    f,
                    "internal: a type variable was still unsolved when lowering ran"
                )
            }
            Self::CapVarInIR { .. } => {
                write!(
                    f,
                    "internal: a capability variable was still unresolved when lowering ran"
                )
            }
            Self::InternalLoweringError { message, .. } => {
                write!(f, "internal: {message}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::point(0)
    }

    #[test]
    fn error_codes_are_correct() {
        assert_eq!(LowerError::MalformedPipeRhs { span: sp() }.code(), "L101");
        assert_eq!(
            LowerError::UnknownPipeRhsShape { span: sp() }.code(),
            "L102"
        );
        assert_eq!(
            LowerError::PropagateOutsideScope { span: sp() }.code(),
            "L103"
        );
        assert_eq!(LowerError::DoublePropagate { span: sp() }.code(), "L104");
        assert_eq!(LowerError::EmptyTryBlock { span: sp() }.code(), "L105");
        assert_eq!(LowerError::BareGuardExpr { span: sp() }.code(), "L106");
        assert_eq!(LowerError::ToTextLowering { span: sp() }.code(), "L107");
        assert_eq!(LowerError::WithOnNonRecord { span: sp() }.code(), "L108");
        assert_eq!(
            LowerError::RefutableSliceElement { span: sp() }.code(),
            "L109"
        );
        assert_eq!(
            LowerError::IntLiteralOutOfRange {
                span: sp(),
                raw: String::new()
            }
            .code(),
            "L110"
        );
        assert_eq!(LowerError::UnsolvedTypeInIR { span: sp() }.code(), "L997");
        assert_eq!(LowerError::CapVarInIR { span: sp() }.code(), "L998");
        assert_eq!(
            LowerError::InternalLoweringError {
                span: sp(),
                message: String::new()
            }
            .code(),
            "L999"
        );
    }

    #[test]
    fn span_accessor_returns_correct_span() {
        let s = Span::new(10, 20);
        let err = LowerError::MalformedPipeRhs { span: s };
        assert_eq!(err.span(), s);
    }

    #[test]
    /// The message is the message and nothing else.
    ///
    /// This test used to require the opposite — that `to_string` begin with
    /// `[L103]` — which was reasonable while nothing rendered these and a bare
    /// string in a log had to identify itself. Now that they reach a renderer
    /// that prints the code and underlines the span, a message carrying either
    /// one says it twice.
    fn display_carries_neither_the_code_nor_the_span() {
        for err in [
            LowerError::PropagateOutsideScope { span: sp() },
            LowerError::IntLiteralOutOfRange {
                span: sp(),
                raw: "99999999999999999999999999".into(),
            },
            LowerError::FloatLiteralNotFinite {
                span: sp(),
                raw: "1.0e400".into(),
            },
        ] {
            let msg = err.to_string();
            let code = err.code();
            assert!(!msg.contains(code), "message repeats its code: {msg}");
            assert!(!msg.contains("Span {"), "message leaks a span: {msg}");
            assert!(
                !msg.contains("  "),
                "a run of spaces means source indentation was baked in: {msg}"
            );
        }
    }

    #[test]
    fn internal_error_includes_message() {
        let err = LowerError::InternalLoweringError {
            span: sp(),
            message: "unreachable branch hit".into(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("unreachable branch hit"),
            "display must include message; got: {msg}"
        );
    }
}
