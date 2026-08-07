//! Lowering-phase diagnostic types (`L###` namespace).
//!
//! All variants in [`LowerError`] are **defensive** — they can only be reached
//! when the input is structurally well-typed but an invariant assumed by a
//! specific lowering rule is violated.  On valid programs none of these are
//! emitted; they surface only when upstream passes produce partial/erroneous
//! output that the lowerer cannot safely handle.
//!
//! # Error code namespace
//!
//! `L101`–`L099` — desugaring rule violations (pipe, try, with, guard, …).
//! `L997`–`L999` — internal consistency / catch-all codes.
//!
//! # Display format
//!
//! Each variant's [`std::fmt::Display`] impl produces a human-readable message
//! prefixed with its code, e.g. `"[L101] malformed pipe RHS: …"`.

// TODO: impl HasErrorCode for LowerError once the diagnostics rendering
// pipeline (ridge-diagnostics) is wired to Phase 5.

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
/// All variants are emitted with [`Severity::Error`] severity — they indicate a
/// lowering invariant violation and are never surfaced to end-users on valid programs.
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedPipeRhs { span } => {
                write!(f, "[L101] malformed pipe RHS at {span:?}")
            }
            Self::UnknownPipeRhsShape { span } => {
                write!(f, "[L102] unknown pipe RHS shape at {span:?}")
            }
            Self::PropagateOutsideScope { span } => {
                write!(
                    f,
                    "[L103] `?` propagation used outside any Option/Result scope at {span:?}"
                )
            }
            Self::DoublePropagate { span } => {
                write!(f, "[L104] double propagation operator at {span:?}")
            }
            Self::EmptyTryBlock { span } => {
                write!(f, "[L105] empty `try` block at {span:?}")
            }
            Self::BareGuardExpr { span } => {
                write!(
                    f,
                    "[L106] `when` guard expression outside match arm at {span:?}"
                )
            }
            Self::ToTextLowering { span } => {
                write!(
                    f,
                    "[L107] could not synthesise `ToText` coercion for interpolation segment at {span:?}"
                )
            }
            Self::WithOnNonRecord { span } => {
                write!(f, "[L108] `with` applied to non-record type at {span:?}")
            }
            Self::RefutableSliceElement { span } => {
                write!(
                    f,
                    "[L109] refutable pattern in suffix or middle position of a list slice pattern at {span:?}; \
                     use a variable or `_` here (P026)"
                )
            }
            Self::IntLiteralOutOfRange { span, raw } => {
                write!(
                    f,
                    "[L110] integer literal `{raw}` at {span:?} is out of range for Int \
                     (min -9223372036854775808, max 9223372036854775807)"
                )
            }
            Self::UnsolvedTypeInIR { span } => {
                write!(
                    f,
                    "[L997] unsolved type variable reached the IR at {span:?}; pass typecheck output is incomplete"
                )
            }
            Self::CapVarInIR { span } => {
                write!(
                    f,
                    "[L998] capability variable reached the IR at {span:?}; capability polymorphism must be resolved before lowering"
                )
            }
            Self::InternalLoweringError { span, message } => {
                write!(
                    f,
                    "[L999] internal lowering invariant violated at {span:?}: {message}"
                )
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
    fn display_contains_code_prefix() {
        let err = LowerError::PropagateOutsideScope { span: sp() };
        let msg = err.to_string();
        assert!(
            msg.contains("[L103]"),
            "display must contain code prefix; got: {msg}"
        );
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
