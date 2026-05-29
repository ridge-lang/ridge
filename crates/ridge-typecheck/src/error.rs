//! `TypeError` — the `T###` diagnostic type for Phase 4 type checking.
//!
//! Every variant carries a stable [`TypeError::code`] (e.g. `"T001"`) that
//! mirrors the `R###`/`M###` convention from earlier phases.
//!
//! `Display` and `std::error::Error` are implemented in [`crate::render`]
//! where the full multi-line output matching spec §5.3 / §5.4 / §6.4 lives.

use ridge_ast::Span;
use ridge_types::CapabilitySet;

// ---------------------------------------------------------------------------
// TypeError enum
// ---------------------------------------------------------------------------

/// A Phase-4 type-check diagnostic.
///
/// All variants are `#[non_exhaustive]` at the enum level — new variants may be
/// added in 0.2.0.  `Display` renders the full human-readable message (see
/// [`crate::render`]).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TypeError {
    // ── T001 ─────────────────────────────────────────────────────────────────
    /// Type mismatch at an annotation or binding site.
    TypeMismatch {
        /// The expected type.
        expected: String,
        /// The found type.
        found: String,
        /// Source span of the sub-expression.
        span: Span,
    },

    // ── T002 ─────────────────────────────────────────────────────────────────
    /// Type mismatch on a specific argument in a function call.
    TypeMismatchInCall {
        /// Name of the callee function.
        callee: String,
        /// Zero-based index of the mismatched argument.
        arg_index: usize,
        /// Expected argument type.
        expected: String,
        /// Found argument type.
        found: String,
        /// Source span of the argument expression.
        span: Span,
    },

    // ── T003 ─────────────────────────────────────────────────────────────────
    /// Wrong number of arguments at a call site.
    ArityMismatch {
        /// Name of the callee function.
        callee: String,
        /// Number of parameters the function declares.
        expected: usize,
        /// Number of arguments supplied at the call site.
        found: usize,
        /// Source span of the call expression.
        span: Span,
        /// Optional diagnostic hint shown below the main message — for example
        /// "the argument is a curried `fn x -> fn y -> …` chain; pass an
        /// uncurried `fn x y -> …` instead".
        hint: Option<String>,
    },

    // ── T004 ─────────────────────────────────────────────────────────────────
    /// A required field is absent in a record construction expression.
    MissingField {
        /// Name of the record type being constructed.
        record: String,
        /// Name of the missing field.
        field: String,
        /// Source span of the record construction expression.
        span: Span,
    },

    // ── T005 ─────────────────────────────────────────────────────────────────
    /// A field name used in a record construction does not exist on the type.
    UnknownField {
        /// Name of the record type.
        record: String,
        /// The unrecognised field name supplied by the user.
        field: String,
        /// Did-you-mean suggestions (empty if none found).
        suggestions: Vec<String>,
        /// Source span of the field initialiser.
        span: Span,
    },

    // ── T006 ─────────────────────────────────────────────────────────────────
    /// The `with` expression is applied to a non-record type.
    WithOnNonRecord {
        /// The actual type found on the LHS.
        ty: String,
        /// Source span of the LHS expression.
        span: Span,
    },

    // ── T007 ─────────────────────────────────────────────────────────────────
    /// A pattern does not match the scrutinee's type.
    PatternTypeMismatch {
        /// The scrutinee's expected type.
        expected: String,
        /// The type implied by the pattern.
        pattern: String,
        /// Source span of the pattern.
        span: Span,
    },

    // ── T008 ─────────────────────────────────────────────────────────────────
    /// A constructor name used in a pattern or expression is not defined on the
    /// expected union type.
    UnknownConstructor {
        /// The unrecognised constructor name.
        name: String,
        /// The expected union type.
        expected_type: String,
        /// Did-you-mean suggestions.
        suggestions: Vec<String>,
        /// Source span of the constructor reference.
        span: Span,
    },

    // ── T009 ─────────────────────────────────────────────────────────────────
    /// A constructor is applied to the wrong number of arguments.
    WrongConstructorArity {
        /// Name of the constructor.
        ctor: String,
        /// Number of payload positions declared.
        expected: usize,
        /// Number of arguments supplied.
        found: usize,
        /// Source span of the constructor application.
        span: Span,
    },

    // ── T010 ─────────────────────────────────────────────────────────────────
    /// Unification would create an infinite type.
    OccursCheck {
        /// String representation of the unification variable.
        var: String,
        /// String representation of the type that would contain `var`.
        ty: String,
        /// Source span of the unification site.
        span: Span,
    },

    // ── T011 ─────────────────────────────────────────────────────────────────
    /// A chain of type aliases forms a cycle.
    RecursiveTypeAlias {
        /// Ordered list of alias names forming the cycle.
        cycle: Vec<String>,
        /// Source span of the first declaration in the cycle.
        span: Span,
    },

    // ── T012 ─────────────────────────────────────────────────────────────────
    /// A string-interpolation hole contains a value that cannot be converted to
    /// text (the closed `ToText` set — D038).
    ToTextNotDerivable {
        /// The type that is not in the `ToText` closed set.
        ty: String,
        /// Source span of the interpolation hole.
        span: Span,
    },

    // ── T013 ─────────────────────────────────────────────────────────────────
    /// A recursive function is used at a different polymorphic type inside its
    /// own body (polymorphic recursion — banned under Hindley-Milner).
    PolymorphicRecursion {
        /// Name of the recursive declaration.
        decl: String,
        /// Source span of the problematic recursive call.
        recursive_call_span: Span,
    },

    // ── T014 ─────────────────────────────────────────────────────────────────
    /// The capability set inferred from a function body exceeds its declared
    /// annotation.
    CapabilityNotDeclared {
        /// Name of the function/handler declaration.
        decl: String,
        /// Capability set declared by the user.
        declared: CapabilitySet,
        /// Capability set inferred from the body.
        inferred: CapabilitySet,
        /// The capabilities present in `inferred` but absent from `declared`.
        missing: CapabilitySet,
        /// Source span of the capability position on the declaration.
        span: Span,
    },

    // ── T015 ─────────────────────────────────────────────────────────────────
    /// A message name sent to an actor does not match any declared `on` handler.
    UnknownActorHandler {
        /// Name of the actor type.
        actor: String,
        /// The unrecognised handler name.
        handler: String,
        /// Did-you-mean suggestions.
        suggestions: Vec<String>,
        /// Source span of the message-name token.
        span: Span,
    },

    // ── T016 ─────────────────────────────────────────────────────────────────
    /// A `match` expression does not cover all constructors / patterns.
    NonExhaustiveMatch {
        /// String representation of the scrutinee type.
        scrutinee_ty: String,
        /// Example missing patterns (capped at 3).
        witnesses: Vec<String>,
        /// Total number of missing patterns (may exceed `witnesses.len()`).
        total_missing: usize,
        /// Source span of the `match` expression.
        span: Span,
    },

    // ── T017 ─────────────────────────────────────────────────────────────────
    /// A match arm is unreachable because an earlier arm already covers it.
    RedundantPattern {
        /// Zero-based index of the unreachable arm.
        arm_index: usize,
        /// Source span of the unreachable arm.
        span: Span,
    },

    // ── T018 ─────────────────────────────────────────────────────────────────
    /// A function calls another with higher capabilities than itself declares.
    CallerCapabilityInsufficient {
        /// Name of the calling function.
        caller: String,
        /// Name of the callee function.
        callee: String,
        /// The capabilities required by `callee` that `caller` does not declare.
        missing: CapabilitySet,
        /// Source span of the call expression.
        span: Span,
    },

    // ── T019 ─────────────────────────────────────────────────────────────────
    /// An actor handler declares capabilities not present in the actor's own
    /// declared capability set.
    ActorCapabilityLeak {
        /// Name of the actor type.
        actor: String,
        /// Name of the handler.
        handler: String,
        /// Capabilities declared by the handler but absent from the actor set.
        leaking_caps: CapabilitySet,
        /// Source span of the handler name.
        span: Span,
    },

    // ── T020 ─────────────────────────────────────────────────────────────────
    /// The `!` send operator is applied to a non-`Handle` value.
    SendOnNonActor {
        /// The actual type found on the LHS of `!`.
        found_ty: String,
        /// Source span of the LHS expression.
        span: Span,
    },

    // ── T021a ────────────────────────────────────────────────────────────────
    /// The `?>` ask operator is applied to a non-`Handle` value.
    AskOnNonActor {
        /// The actual type found on the LHS of `?>`.
        found_ty: String,
        /// Source span of the LHS expression.
        span: Span,
    },

    // ── T021b ────────────────────────────────────────────────────────────────
    /// The `?` propagate operator is used outside a `Result`/`Option` context.
    PropagateOutsideResultOrOption {
        /// The actual type of the expression `?` is applied to.
        found_ty: String,
        /// The type expected by the enclosing context.
        expected: String,
        /// Source span of the `?` operator.
        span: Span,
    },

    // ── T022 ─────────────────────────────────────────────────────────────────
    /// A non-`Unit` value is silently discarded at statement level.
    DiscardedResult {
        /// The type of the discarded expression.
        ty: String,
        /// Source span of the discarded expression.
        span: Span,
    },

    // ── T023 ─────────────────────────────────────────────────────────────────
    /// A type variable cannot be resolved — the user must add a type annotation.
    UnsolvedTypeVariable {
        /// String representation of the unsolved variable.
        var: String,
        /// Source span of the generalisation site (typically the `let` binding).
        generalisation_site: Span,
    },

    // ── T024 ─────────────────────────────────────────────────────────────────
    /// A capability variable escapes into a user-visible type (D057).
    RowVariableLeak {
        /// Name of the declaration where the leak was detected.
        decl: String,
        /// Source span of the declaration.
        span: Span,
    },

    // ── T025 ─────────────────────────────────────────────────────────────────
    /// A `spawn` expression passes the wrong number of `init` arguments.
    SpawnArityMismatch {
        /// Name of the actor type being spawned.
        actor: String,
        /// Number of `init` parameters the actor declares.
        expected: usize,
        /// Number of arguments supplied to `spawn`.
        found: usize,
        /// Source span of the `spawn` expression.
        span: Span,
    },

    // ── T026 ─────────────────────────────────────────────────────────────────
    /// The expression supplied to `?> ... timeout <expr>` is not `Int`.
    ///
    /// Allocated by Phase 6 T0 (OQ-E001 narrow exception) — the timeout value
    /// must be an integer number of milliseconds.  `timeout never` is the
    /// explicit opt-in for an unlimited wait.
    AskTimeoutNotInt {
        /// The actual type found on the timeout expression.
        found: String,
        /// Source span of the timeout expression.
        span: Span,
    },

    // ── T027 ─────────────────────────────────────────────────────────────────
    /// An actor declares `mailbox bounded N drop oldest`.
    ///
    /// The `drop oldest` overflow policy parses as valid surface syntax but is
    /// not yet implemented: enforcing it requires a broker process intermediary
    /// (BEAM does not permit a sender to remove a message from another
    /// process's mailbox). The two policies that are implemented today are
    /// `drop newest` (silently drop the incoming message) and `error` (signal
    /// failure to the sender).
    MailboxPolicyDropOldestNotShipped {
        /// Name of the actor whose mailbox declaration uses the policy.
        actor: String,
        /// Source span of the `mailbox` member.
        span: Span,
    },

    // ── T999 ─────────────────────────────────────────────────────────────────
    /// Internal type-checker invariant violation — should never reach users.
    ///
    /// In debug builds this is accompanied by a `debug_assert!` panic (see
    /// [`crate::render::emit_internal`]). In release builds the error is pushed
    /// and compilation continues.
    InternalTypeError {
        /// Human-readable description of the violated invariant.
        detail: String,
        /// Best available span (may be a dummy span if no better location).
        span: Span,
    },
}

impl TypeError {
    /// Returns the stable `T###` error code for this variant.
    ///
    /// The codes are allocated in `T001..T030` and `T999` is the catch-all
    /// internal error. No overlap with `R###`/`M###`.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TypeMismatch { .. } => "T001",
            Self::TypeMismatchInCall { .. } => "T002",
            Self::ArityMismatch { .. } => "T003",
            Self::MissingField { .. } => "T004",
            Self::UnknownField { .. } => "T005",
            Self::WithOnNonRecord { .. } => "T006",
            Self::PatternTypeMismatch { .. } => "T007",
            Self::UnknownConstructor { .. } => "T008",
            Self::WrongConstructorArity { .. } => "T009",
            Self::OccursCheck { .. } => "T010",
            Self::RecursiveTypeAlias { .. } => "T011",
            Self::ToTextNotDerivable { .. } => "T012",
            Self::PolymorphicRecursion { .. } => "T013",
            Self::CapabilityNotDeclared { .. } => "T014",
            Self::UnknownActorHandler { .. } => "T015",
            Self::NonExhaustiveMatch { .. } => "T016",
            Self::RedundantPattern { .. } => "T017",
            Self::CallerCapabilityInsufficient { .. } => "T018",
            Self::ActorCapabilityLeak { .. } => "T019",
            Self::SendOnNonActor { .. } => "T020",
            Self::AskOnNonActor { .. } | Self::PropagateOutsideResultOrOption { .. } => "T021",
            Self::DiscardedResult { .. } => "T022",
            Self::UnsolvedTypeVariable { .. } => "T023",
            Self::RowVariableLeak { .. } => "T024",
            Self::SpawnArityMismatch { .. } => "T025",
            Self::AskTimeoutNotInt { .. } => "T026",
            Self::MailboxPolicyDropOldestNotShipped { .. } => "T027",
            Self::InternalTypeError { .. } => "T999",
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_span() -> Span {
        Span::point(0)
    }

    /// Helper: construct a minimal T001 for testing.
    fn t001() -> TypeError {
        TypeError::TypeMismatch {
            expected: "Int".into(),
            found: "Text".into(),
            span: dummy_span(),
        }
    }

    fn t002() -> TypeError {
        TypeError::TypeMismatchInCall {
            callee: "foo".into(),
            arg_index: 0,
            expected: "Int".into(),
            found: "Bool".into(),
            span: dummy_span(),
        }
    }

    fn t003() -> TypeError {
        TypeError::ArityMismatch {
            callee: "bar".into(),
            expected: 2,
            found: 1,
            span: dummy_span(),
            hint: None,
        }
    }

    fn t004() -> TypeError {
        TypeError::MissingField {
            record: "User".into(),
            field: "email".into(),
            span: dummy_span(),
        }
    }

    fn t005() -> TypeError {
        TypeError::UnknownField {
            record: "User".into(),
            field: "nme".into(),
            suggestions: vec!["name".into()],
            span: dummy_span(),
        }
    }

    fn t006() -> TypeError {
        TypeError::WithOnNonRecord {
            ty: "Int".into(),
            span: dummy_span(),
        }
    }

    fn t007() -> TypeError {
        TypeError::PatternTypeMismatch {
            expected: "Int".into(),
            pattern: "Some _".into(),
            span: dummy_span(),
        }
    }

    fn t008() -> TypeError {
        TypeError::UnknownConstructor {
            name: "Bogus".into(),
            expected_type: "Shape".into(),
            suggestions: vec![],
            span: dummy_span(),
        }
    }

    fn t009() -> TypeError {
        TypeError::WrongConstructorArity {
            ctor: "Some".into(),
            expected: 1,
            found: 2,
            span: dummy_span(),
        }
    }

    fn t010() -> TypeError {
        TypeError::OccursCheck {
            var: "a".into(),
            ty: "List a".into(),
            span: dummy_span(),
        }
    }

    fn t011() -> TypeError {
        TypeError::RecursiveTypeAlias {
            cycle: vec!["A".into(), "B".into()],
            span: dummy_span(),
        }
    }

    fn t012() -> TypeError {
        TypeError::ToTextNotDerivable {
            ty: "User".into(),
            span: dummy_span(),
        }
    }

    fn t013() -> TypeError {
        TypeError::PolymorphicRecursion {
            decl: "f".into(),
            recursive_call_span: dummy_span(),
        }
    }

    fn t014() -> TypeError {
        TypeError::CapabilityNotDeclared {
            decl: "procesarConfig".into(),
            declared: CapabilitySet::singleton(ridge_ast::Capability::Io),
            inferred: CapabilitySet::singleton(ridge_ast::Capability::Fs),
            missing: CapabilitySet::singleton(ridge_ast::Capability::Fs),
            span: dummy_span(),
        }
    }

    fn t015() -> TypeError {
        TypeError::UnknownActorHandler {
            actor: "Counter".into(),
            handler: "incremento".into(),
            suggestions: vec!["increment".into()],
            span: dummy_span(),
        }
    }

    fn t016() -> TypeError {
        TypeError::NonExhaustiveMatch {
            scrutinee_ty: "Shape".into(),
            witnesses: vec!["Rectangle _ _".into()],
            total_missing: 2,
            span: dummy_span(),
        }
    }

    fn t017() -> TypeError {
        TypeError::RedundantPattern {
            arm_index: 1,
            span: dummy_span(),
        }
    }

    fn t018() -> TypeError {
        TypeError::CallerCapabilityInsufficient {
            caller: "pure_fn".into(),
            callee: "Io.println".into(),
            missing: CapabilitySet::singleton(ridge_ast::Capability::Io),
            span: dummy_span(),
        }
    }

    fn t019() -> TypeError {
        TypeError::ActorCapabilityLeak {
            actor: "MyActor".into(),
            handler: "handleMsg".into(),
            leaking_caps: CapabilitySet::singleton(ridge_ast::Capability::Net),
            span: dummy_span(),
        }
    }

    fn t020() -> TypeError {
        TypeError::SendOnNonActor {
            found_ty: "Int".into(),
            span: dummy_span(),
        }
    }

    fn t021a() -> TypeError {
        TypeError::AskOnNonActor {
            found_ty: "Int".into(),
            span: dummy_span(),
        }
    }

    fn t021b() -> TypeError {
        TypeError::PropagateOutsideResultOrOption {
            found_ty: "Int".into(),
            expected: "Result _ _".into(),
            span: dummy_span(),
        }
    }

    fn t022() -> TypeError {
        TypeError::DiscardedResult {
            ty: "Result Unit IoError".into(),
            span: dummy_span(),
        }
    }

    fn t023() -> TypeError {
        TypeError::UnsolvedTypeVariable {
            var: "a0".into(),
            generalisation_site: dummy_span(),
        }
    }

    fn t024() -> TypeError {
        TypeError::RowVariableLeak {
            decl: "myFn".into(),
            span: dummy_span(),
        }
    }

    fn t025() -> TypeError {
        TypeError::SpawnArityMismatch {
            actor: "Limiter".into(),
            expected: 2,
            found: 0,
            span: dummy_span(),
        }
    }

    fn t999() -> TypeError {
        TypeError::InternalTypeError {
            detail: "unexpected node kind".into(),
            span: dummy_span(),
        }
    }

    // ── code() tests — one per T### ───────────────────────────────────────────

    #[test]
    fn code_t001() {
        assert_eq!(t001().code(), "T001");
    }

    #[test]
    fn code_t002() {
        assert_eq!(t002().code(), "T002");
    }

    #[test]
    fn code_t003() {
        assert_eq!(t003().code(), "T003");
    }

    #[test]
    fn code_t004() {
        assert_eq!(t004().code(), "T004");
    }

    #[test]
    fn code_t005() {
        assert_eq!(t005().code(), "T005");
    }

    #[test]
    fn code_t006() {
        assert_eq!(t006().code(), "T006");
    }

    #[test]
    fn code_t007() {
        assert_eq!(t007().code(), "T007");
    }

    #[test]
    fn code_t008() {
        assert_eq!(t008().code(), "T008");
    }

    #[test]
    fn code_t009() {
        assert_eq!(t009().code(), "T009");
    }

    #[test]
    fn code_t010() {
        assert_eq!(t010().code(), "T010");
    }

    #[test]
    fn code_t011() {
        assert_eq!(t011().code(), "T011");
    }

    #[test]
    fn code_t012() {
        assert_eq!(t012().code(), "T012");
    }

    #[test]
    fn code_t013() {
        assert_eq!(t013().code(), "T013");
    }

    #[test]
    fn code_t014() {
        assert_eq!(t014().code(), "T014");
    }

    #[test]
    fn code_t015() {
        assert_eq!(t015().code(), "T015");
    }

    #[test]
    fn code_t016() {
        assert_eq!(t016().code(), "T016");
    }

    #[test]
    fn code_t017() {
        assert_eq!(t017().code(), "T017");
    }

    #[test]
    fn code_t018() {
        assert_eq!(t018().code(), "T018");
    }

    #[test]
    fn code_t019() {
        assert_eq!(t019().code(), "T019");
    }

    #[test]
    fn code_t020() {
        assert_eq!(t020().code(), "T020");
    }

    #[test]
    fn code_t021_ask() {
        assert_eq!(t021a().code(), "T021");
    }

    #[test]
    fn code_t021_propagate() {
        assert_eq!(t021b().code(), "T021");
    }

    #[test]
    fn code_t022() {
        assert_eq!(t022().code(), "T022");
    }

    #[test]
    fn code_t023() {
        assert_eq!(t023().code(), "T023");
    }

    #[test]
    fn code_t024() {
        assert_eq!(t024().code(), "T024");
    }

    #[test]
    fn code_t025() {
        assert_eq!(t025().code(), "T025");
    }

    #[test]
    fn code_t999() {
        assert_eq!(t999().code(), "T999");
    }
}
