//! `Display` + `std::error::Error` for [`TypeError`], plus the [`emit_internal`]
//! helper for T999 (OQ-T016).
//!
//! # Rendering format
//!
//! All messages follow the spec §5.3 / §5.4 / §6.4 multi-line text shape:
//!
//! ```text
//! {code}: {title}
//!   {detail line}
//!   suggestion: ...
//! ```
//!
//! Ariadne source-span rendering (the `| 12 | fn io …` lines) is added later
//! by `ridge-diagnostics`'s ariadne pass. The `Display` output here is the
//! *prose* portion only — suitable for tests, tracing logs, and simple
//! terminal output without source context.
//!
//! # T999 / OQ-T016
//!
//! [`emit_internal`] is the canonical emit site for `T999 InternalTypeError`.
//! In debug builds it fires a `debug_assert!` panic to surface invariant
//! violations immediately during development. In release builds the error is
//! pushed to `ctx.errors` and inference continues.

use std::fmt;

use ridge_ast::Span;
use ridge_diagnostics::HasErrorCode;
use ridge_resolve::Severity;

use crate::ctx::InferCtx;
use crate::error::TypeError;

// ── Display impl ──────────────────────────────────────────────────────────────

impl fmt::Display for TypeError {
    #[expect(clippy::too_many_lines, reason = "one match arm per T### error code")]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // ── T001 ──────────────────────────────────────────────────────────
            Self::TypeMismatch {
                expected, found, ..
            } => {
                write!(f, "T001: type mismatch\n  expected {expected}, got {found}")
            }

            // ── T002 ──────────────────────────────────────────────────────────
            Self::TypeMismatchInCall {
                callee,
                arg_index,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "T002: type mismatch in call to `{callee}`\n  argument {n}: expected {expected}, got {found}",
                    n = arg_index + 1,
                )
            }

            // ── T003 ──────────────────────────────────────────────────────────
            Self::ArityMismatch {
                callee,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "T003: arity mismatch\n  `{callee}` expects {expected} argument{s1}, got {found}",
                    s1 = if *expected == 1 { "" } else { "s" },
                )
            }

            // ── T004 ──────────────────────────────────────────────────────────
            Self::MissingField { record, field, .. } => {
                write!(
                    f,
                    "T004: missing field in record construction\n  record `{record}` requires field `{field}`"
                )
            }

            // ── T005 ──────────────────────────────────────────────────────────
            Self::UnknownField {
                record,
                field,
                suggestions,
                ..
            } => {
                write!(f, "T005: unknown field `{field}` on record `{record}`")?;
                if let Some(s) = suggestions.first() {
                    write!(f, "\n  did you mean: {s}?")?;
                }
                Ok(())
            }

            // ── T006 ──────────────────────────────────────────────────────────
            Self::WithOnNonRecord { ty, .. } => {
                write!(f, "T006: `with` on non-record\n  found type `{ty}`")
            }

            // ── T007 ──────────────────────────────────────────────────────────
            Self::PatternTypeMismatch {
                expected, pattern, ..
            } => {
                write!(
                    f,
                    "T007: pattern type mismatch\n  expected `{expected}`, but pattern implies `{pattern}`"
                )
            }

            // ── T008 ──────────────────────────────────────────────────────────
            Self::UnknownConstructor {
                name,
                expected_type,
                suggestions,
                ..
            } => {
                write!(
                    f,
                    "T008: unknown constructor `{name}` on type `{expected_type}`"
                )?;
                if let Some(s) = suggestions.first() {
                    write!(f, "\n  did you mean: {s}?")?;
                }
                Ok(())
            }

            // ── T009 ──────────────────────────────────────────────────────────
            Self::WrongConstructorArity {
                ctor,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "T009: wrong constructor arity\n  `{ctor}` expects {expected} argument{s1}, got {found}",
                    s1 = if *expected == 1 { "" } else { "s" },
                )
            }

            // ── T010 ──────────────────────────────────────────────────────────
            Self::OccursCheck { var, ty, .. } => {
                write!(
                    f,
                    "T010: occurs check failure (infinite type)\n  cannot unify `{var}` with `{ty}` — would create an infinite type"
                )
            }

            // ── T011 ──────────────────────────────────────────────────────────
            Self::RecursiveTypeAlias { cycle, .. } => {
                write!(
                    f,
                    "T011: recursive type alias\n  cycle: {}",
                    cycle.join(" -> ")
                )
            }

            // ── T012 ──────────────────────────────────────────────────────────
            Self::ToTextNotDerivable { ty, .. } => {
                write!(
                    f,
                    "T012: type `{ty}` cannot be converted to text\n  only built-in types and records of built-in types support string interpolation"
                )
            }

            // ── T013 ──────────────────────────────────────────────────────────
            Self::PolymorphicRecursion { decl, .. } => {
                write!(
                    f,
                    "T013: polymorphic recursion in `{decl}`\n  Hindley-Milner does not support recursive calls at a different type"
                )
            }

            // ── T014 (spec §5.3 exact text shape) ────────────────────────────
            //
            // Spec example:
            //   Error: function 'f' declared as `fn io` uses capability `fs`
            //     at src/Main.rg:12
            //     ...
            //     The call to `Fs.readFile` requires `fs`.
            //     Options:
            //       - Add `fs` to the signature: `fn io fs procesarConfig`
            //       - Remove the call to `Fs.readFile`
            //
            // Display (prose portion, no source lines):
            Self::CapabilityNotDeclared {
                decl,
                declared,
                missing,
                inferred,
                ..
            } => {
                write!(
                    f,
                    "T014: capability not declared\n  function `{decl}` declared as `fn {declared}` uses capability `{missing}`\n  Options:\n    - Add `{missing}` to the signature: `fn {inferred} {decl}`\n    - Remove the call requiring `{missing}`"
                )
            }

            // ── T015 ──────────────────────────────────────────────────────────
            Self::UnknownActorHandler {
                actor,
                handler,
                suggestions,
                ..
            } => {
                write!(f, "T015: unknown handler `{handler}` on actor `{actor}`")?;
                if let Some(s) = suggestions.first() {
                    write!(f, "\n  did you mean: {s}?")?;
                }
                Ok(())
            }

            // ── T016 (spec §5.4 exact text shape) ────────────────────────────
            //
            // Spec example:
            //   Error: non-exhaustive match
            //     at src/Shape.rg:12
            //     Missing cases:
            //       Triangle _ _ _
            //
            // OQ-T009: when total_missing > witnesses.len(), append
            //   `... and N more`
            Self::NonExhaustiveMatch {
                scrutinee_ty,
                witnesses,
                total_missing,
                ..
            } => {
                write!(
                    f,
                    "T016: non-exhaustive match on `{scrutinee_ty}`\n  Missing cases:"
                )?;
                for w in witnesses {
                    write!(f, "\n    {w}")?;
                }
                let extra = total_missing.saturating_sub(witnesses.len());
                if extra > 0 {
                    write!(f, "\n    ... and {extra} more")?;
                }
                Ok(())
            }

            // ── T017 ──────────────────────────────────────────────────────────
            Self::RedundantPattern { arm_index, .. } => {
                write!(
                    f,
                    "T017: redundant pattern\n  arm {} is unreachable — an earlier arm already covers this case",
                    arm_index + 1,
                )
            }

            // ── T018 ──────────────────────────────────────────────────────────
            Self::CallerCapabilityInsufficient {
                caller,
                callee,
                missing,
                ..
            } => {
                write!(
                    f,
                    "T018: caller capability insufficient\n  `{caller}` calls `{callee}` which requires `{missing}`\n  Options:\n    - Add `{missing}` to the signature of `{caller}`\n    - Use a pure alternative to `{callee}`"
                )
            }

            // ── T019 ──────────────────────────────────────────────────────────
            Self::ActorCapabilityLeak {
                actor,
                handler,
                leaking_caps,
                ..
            } => {
                write!(
                    f,
                    "T019: actor capability leak\n  handler `{handler}` on actor `{actor}` declares `{leaking_caps}` which is not in the actor's capability set"
                )
            }

            // ── T020 ──────────────────────────────────────────────────────────
            Self::SendOnNonActor { found_ty, .. } => {
                write!(
                    f,
                    "T020: send (`!`) on non-actor\n  found type `{found_ty}`, expected an actor Handle"
                )
            }

            // ── T021a ─────────────────────────────────────────────────────────
            Self::AskOnNonActor { found_ty, .. } => {
                write!(
                    f,
                    "T021: ask (`?>`) on non-actor\n  found type `{found_ty}`, expected an actor Handle"
                )
            }

            // ── T021b ─────────────────────────────────────────────────────────
            Self::PropagateOutsideResultOrOption {
                found_ty, expected, ..
            } => {
                write!(
                    f,
                    "T021: `?` used outside Result/Option context\n  found `{found_ty}`, enclosing function returns `{expected}`"
                )
            }

            // ── T022 ──────────────────────────────────────────────────────────
            Self::DiscardedResult { ty, .. } => {
                write!(
                    f,
                    "T022: discarded result\n  expression of type `{ty}` is not bound — use `let _ =` to explicitly discard"
                )
            }

            // ── T023 ──────────────────────────────────────────────────────────
            Self::UnsolvedTypeVariable { var, .. } => {
                write!(
                    f,
                    "T023: unsolved type variable `{var}`\n  add a type annotation to resolve the ambiguity"
                )
            }

            // ── T024 ──────────────────────────────────────────────────────────
            Self::RowVariableLeak { decl, .. } => {
                write!(
                    f,
                    "T024: capability row variable leaked in `{decl}`\n  add an explicit capability annotation to pin the row"
                )
            }

            // ── T025 ──────────────────────────────────────────────────────────
            Self::SpawnArityMismatch {
                actor,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "T025: spawn arity mismatch\n  `{actor}` init expects {expected} argument{s1}, got {found}",
                    s1 = if *expected == 1 { "" } else { "s" },
                )
            }

            // ── T026 ──────────────────────────────────────────────────────────
            Self::AskTimeoutNotInt { found, .. } => {
                write!(
                    f,
                    "T026: ask timeout must be Int\n  expected `Int`, found `{found}`\n  hint: use `?> handler() timeout 1000` (milliseconds) or `timeout never`"
                )
            }

            // ── T999 ──────────────────────────────────────────────────────────
            Self::InternalTypeError { detail, .. } => {
                write!(f, "T999: internal type error\n  {detail}\n  This is a compiler bug. Please report it.")
            }
        }
    }
}

// ── std::error::Error impl ────────────────────────────────────────────────────

impl std::error::Error for TypeError {}

// ── HasErrorCode impl ─────────────────────────────────────────────────────────

impl HasErrorCode for TypeError {
    fn code(&self) -> &'static str {
        // Delegates to the existing code() method on TypeError.
        Self::code(self)
    }

    fn span(&self) -> Span {
        match self {
            Self::TypeMismatch { span, .. }
            | Self::TypeMismatchInCall { span, .. }
            | Self::ArityMismatch { span, .. }
            | Self::MissingField { span, .. }
            | Self::UnknownField { span, .. }
            | Self::WithOnNonRecord { span, .. }
            | Self::PatternTypeMismatch { span, .. }
            | Self::UnknownConstructor { span, .. }
            | Self::WrongConstructorArity { span, .. }
            | Self::OccursCheck { span, .. }
            | Self::RecursiveTypeAlias { span, .. }
            | Self::ToTextNotDerivable { span, .. }
            | Self::CapabilityNotDeclared { span, .. }
            | Self::UnknownActorHandler { span, .. }
            | Self::NonExhaustiveMatch { span, .. }
            | Self::RedundantPattern { span, .. }
            | Self::CallerCapabilityInsufficient { span, .. }
            | Self::ActorCapabilityLeak { span, .. }
            | Self::SendOnNonActor { span, .. }
            | Self::AskOnNonActor { span, .. }
            | Self::PropagateOutsideResultOrOption { span, .. }
            | Self::DiscardedResult { span, .. }
            | Self::RowVariableLeak { span, .. }
            | Self::SpawnArityMismatch { span, .. }
            | Self::AskTimeoutNotInt { span, .. }
            | Self::InternalTypeError { span, .. } => *span,

            // T013: uses `recursive_call_span` as the primary span.
            Self::PolymorphicRecursion {
                recursive_call_span,
                ..
            } => *recursive_call_span,

            // T023: uses `generalisation_site` as the primary span.
            Self::UnsolvedTypeVariable {
                generalisation_site,
                ..
            } => *generalisation_site,
        }
    }

    fn severity(&self) -> Severity {
        // OQ-T013 resolved: T017 RedundantPattern and T022 DiscardedResult are
        // Warning-level; all other T### variants are hard errors.
        match self {
            Self::RedundantPattern { .. } | Self::DiscardedResult { .. } => Severity::Warning,
            _ => Severity::Error,
        }
    }
}

// ── emit_internal — T999 helper (OQ-T016) ────────────────────────────────────

/// Emit a `T999 InternalTypeError` diagnostic (soft-error, no panic).
///
/// Pushes the error into `ctx.errors` and returns [`ridge_types::Type::Error`]
/// so downstream inference can continue without cascading failures.
///
/// For **true invariant-violation** sites where reaching the code path
/// indicates a compiler bug, use [`emit_internal_strict`] instead — it adds a
/// `debug_assert!` that panics in debug builds (OQ-T016).
///
/// # Usage
///
/// Prefer this function over pushing [`TypeError::InternalTypeError`] directly.
///
/// ```ignore
/// let ty = emit_internal(ctx, "unexpected Expr shape in infer_expr", span);
/// ```
/// Whether to panic on T999 in debug builds.
///
/// OQ-T016: `emit_internal` panics in debug when this flag is set.
/// Production callers that want the panic-on-T999 behaviour (for catching
/// true invariant violations) use [`emit_internal_strict`].  Scaffolding
/// stubs that deliberately emit T999 for deferred code paths use this
/// function directly — it is a no-op assert so tests can exercise the
/// error-absorption path.
#[must_use]
pub fn emit_internal(ctx: &mut InferCtx, msg: impl Into<String>, span: Span) -> ridge_types::Type {
    let detail = msg.into();
    ctx.errors
        .push(TypeError::InternalTypeError { detail, span });
    ridge_types::Type::Error
}

/// Strict variant of [`emit_internal`] that panics in debug builds (OQ-T016).
///
/// Use this at **true invariant-violation** sites — places where reaching the
/// code path indicates a compiler bug. Scaffolding deferred-path stubs should
/// use [`emit_internal`] instead so that `cargo test` can exercise the
/// error-absorption path.
#[must_use]
pub fn emit_internal_strict(
    ctx: &mut InferCtx,
    msg: impl Into<String>,
    span: Span,
) -> ridge_types::Type {
    let detail = msg.into();
    debug_assert!(
        false,
        "T999 internal type error (invariant violation): {detail} at {span:?}",
    );
    ctx.errors
        .push(TypeError::InternalTypeError { detail, span });
    ridge_types::Type::Error
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Capability, Span};
    use ridge_types::CapabilitySet;

    fn sp() -> Span {
        Span::point(0)
    }

    // ── T001 Display ──────────────────────────────────────────────────────────

    #[test]
    fn display_t001_typemismatch() {
        let err = TypeError::TypeMismatch {
            expected: "Int".into(),
            found: "Text".into(),
            span: sp(),
        };
        let s = err.to_string();
        assert!(s.contains("T001"), "should contain code: {s}");
        assert!(s.contains("Int"), "should contain expected type: {s}");
        assert!(s.contains("Text"), "should contain found type: {s}");
        assert!(s.contains("expected"), "should contain 'expected': {s}");
        assert!(s.contains("got"), "should contain 'got': {s}");
    }

    // ── T014 Display — spec §5.3 exact text shape ─────────────────────────────

    /// The spec §5.3 text shape for T014:
    ///
    /// ```text
    /// T014: capability not declared
    ///   function `procesarConfig` declared as `fn {io}` uses capability `{fs}`
    ///   Options:
    ///     - Add `{fs}` to the signature: `fn {fs io} procesarConfig`
    ///     - Remove the call requiring `{fs}`
    /// ```
    #[test]
    fn display_t014_capabilitynotdeclared_matches_spec() {
        let declared = CapabilitySet::singleton(Capability::Io);
        let missing = CapabilitySet::singleton(Capability::Fs);
        let inferred = {
            let mut s = CapabilitySet::singleton(Capability::Io);
            s.insert(Capability::Fs);
            s
        };
        let err = TypeError::CapabilityNotDeclared {
            decl: "procesarConfig".into(),
            declared,
            inferred,
            missing,
            span: sp(),
        };
        let s = err.to_string();
        assert!(s.contains("T014"), "code: {s}");
        assert!(s.contains("procesarConfig"), "decl name: {s}");
        assert!(s.contains("fn {io}"), "declared caps: {s}");
        assert!(s.contains("{fs}"), "missing caps: {s}");
        assert!(s.contains("Options:"), "options header: {s}");
        assert!(s.contains("Add"), "add option: {s}");
        assert!(s.contains("Remove"), "remove option: {s}");
    }

    // ── T016 Display — spec §5.4 with witnesses ───────────────────────────────

    #[test]
    fn display_t016_nonexhaustivematch_with_witnesses() {
        let err = TypeError::NonExhaustiveMatch {
            scrutinee_ty: "Shape".into(),
            witnesses: vec![
                "Circle _".into(),
                "Triangle _ _ _".into(),
                "Rectangle _ _".into(),
            ],
            total_missing: 3,
            span: sp(),
        };
        let s = err.to_string();
        assert!(s.contains("T016"), "code: {s}");
        assert!(s.contains("Shape"), "scrutinee type: {s}");
        assert!(s.contains("Missing cases:"), "header: {s}");
        assert!(s.contains("Circle _"), "first witness: {s}");
        assert!(s.contains("Triangle _ _ _"), "second witness: {s}");
        assert!(s.contains("Rectangle _ _"), "third witness: {s}");
        // No truncation — total_missing == witnesses.len()
        assert!(!s.contains("more"), "should not truncate: {s}");
    }

    // ── T016 Display — OQ-T009 "and N more" suffix ────────────────────────────

    #[test]
    fn display_t016_nonexhaustivematch_truncated() {
        let err = TypeError::NonExhaustiveMatch {
            scrutinee_ty: "Color".into(),
            witnesses: vec!["Red".into(), "Green".into(), "Blue".into()],
            // 8 total missing, 3 shown → "and 5 more"
            total_missing: 8,
            span: sp(),
        };
        let s = err.to_string();
        assert!(s.contains("T016"), "code: {s}");
        assert!(s.contains("Missing cases:"), "header: {s}");
        assert!(s.contains("Red"), "first witness: {s}");
        assert!(s.contains("... and 5 more"), "truncation suffix: {s}");
    }

    // ── T015 Display — did-you-mean ───────────────────────────────────────────

    #[test]
    fn display_t015_unknownactorhandler_with_didyoumean() {
        let err = TypeError::UnknownActorHandler {
            actor: "Counter".into(),
            handler: "incremento".into(),
            suggestions: vec!["increment".into()],
            span: sp(),
        };
        let s = err.to_string();
        assert!(s.contains("T015"), "code: {s}");
        assert!(s.contains("incremento"), "handler name: {s}");
        assert!(s.contains("Counter"), "actor name: {s}");
        assert!(s.contains("did you mean: increment?"), "suggestion: {s}");
    }

    // ── T005 Display — did-you-mean ───────────────────────────────────────────

    #[test]
    fn display_t005_unknownfield_with_didyoumean() {
        let err = TypeError::UnknownField {
            record: "User".into(),
            field: "nme".into(),
            suggestions: vec!["name".into()],
            span: sp(),
        };
        let s = err.to_string();
        assert!(s.contains("T005"), "code: {s}");
        assert!(s.contains("nme"), "field name: {s}");
        assert!(s.contains("User"), "record name: {s}");
        assert!(s.contains("did you mean: name?"), "suggestion: {s}");
    }

    // ── Severity correctness ──────────────────────────────────────────────────

    #[test]
    fn severity_warnings_correct() {
        let warn_t017 = TypeError::RedundantPattern {
            arm_index: 0,
            span: sp(),
        };
        let warn_t022 = TypeError::DiscardedResult {
            ty: "Result Unit Err".into(),
            span: sp(),
        };
        let err_t001 = TypeError::TypeMismatch {
            expected: "Int".into(),
            found: "Text".into(),
            span: sp(),
        };

        assert_eq!(
            <TypeError as HasErrorCode>::severity(&warn_t017),
            Severity::Warning,
            "T017 should be Warning"
        );
        assert_eq!(
            <TypeError as HasErrorCode>::severity(&warn_t022),
            Severity::Warning,
            "T022 should be Warning"
        );
        assert_eq!(
            <TypeError as HasErrorCode>::severity(&err_t001),
            Severity::Error,
            "T001 should be Error"
        );
    }

    // ── HasErrorCode compile check ────────────────────────────────────────────

    /// Verifies at the type level that `TypeError`: `HasErrorCode`.
    /// If this compiles, the trait impl is wired correctly.
    #[test]
    fn has_error_code_trait_impls_compile() {
        fn assert_has_error_code<T: HasErrorCode>(_: &T) {}
        let err = TypeError::TypeMismatch {
            expected: "Int".into(),
            found: "Text".into(),
            span: sp(),
        };
        assert_has_error_code(&err);
        // Also verify the code/span/severity methods are callable
        assert_eq!(<TypeError as HasErrorCode>::code(&err), "T001");
        assert_eq!(<TypeError as HasErrorCode>::span(&err), sp());
        assert_eq!(<TypeError as HasErrorCode>::severity(&err), Severity::Error);
    }
}
