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

    // ── T012 — RESERVED, must not be re-emitted ───────────────────────────────
    // Retired in 0.2.13: the closed-set interpolation restriction is replaced by
    // open typeclass dispatch. Missing `ToText` instances now surface as T029
    // `NoInstance`. The variant is kept so that the diagnostic code slot T012 is
    // never reused and old serialised diagnostics remain decodable.
    /// Interpolation hole type not in the closed `ToText` set (retired).
    ///
    /// Do not emit this variant from new code. It exists solely to reserve the
    /// T012 code slot. Use [`TypeError::NoInstance`] (T029) instead.
    ToTextNotDerivable {
        /// The type that was not in the closed set.
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

    // ── P029 ─────────────────────────────────────────────────────────────────
    /// A field inside an inline record type references a type variable from an
    /// enclosing parametric declaration.  Parametric anonymous records are not
    /// supported in this version.
    ///
    /// The diagnostic code is `P029` (not a `T###`) because it is a semantic
    /// companion to the surface-syntax restriction — analogous to how lower-level
    /// `P026` lives in `ridge-lower`.
    InlineRecordTyVarField {
        /// Name of the type variable that appeared inside the inline record.
        var_name: String,
        /// Source span of the inline record type expression.
        span: Span,
    },

    // ── T028 ─────────────────────────────────────────────────────────────────
    /// A constructor-less record pattern omits one or more fields of the
    /// matched record type and does not include a `..` rest pattern.
    IncompleteRecordPattern {
        /// Structural description of the record type being matched.
        record: String,
        /// Fields that are present in the type but absent from the pattern.
        missing_fields: Vec<String>,
        /// Source span of the record pattern.
        span: Span,
    },

    // ── T029 ─────────────────────────────────────────────────────────────────
    /// A constrained function is called with a type that has no instance for
    /// the required class.
    ///
    /// For example, calling `describe` (which requires `ToText a`) with a
    /// custom type that has no `ToText` instance fires this error. The fix
    /// is to write an `instance` declaration or add the class to the type's
    /// `deriving` list.
    NoInstance {
        /// Display name of the class (e.g. `"ToText"`).
        class: String,
        /// Display name of the concrete type (e.g. `"Color"`).
        ty: String,
        /// Source span of the call or use site.
        span: Span,
        /// Context-specific fix suggestion shown below the main message.
        fix_hint: String,
    },

    // ── T030 ─────────────────────────────────────────────────────────────────
    /// A class constraint's type variable cannot be resolved to a concrete
    /// type and is not being generalised — it is ambiguous.
    ///
    /// This typically means the user wrote an expression where the class
    /// cannot be determined from context. Adding a type annotation that pins
    /// the type variable resolves the ambiguity.
    AmbiguousConstraint {
        /// Display name of the class (e.g. `"ToText"`).
        class: String,
        /// Display name of the ambiguous type variable.
        ty_var: String,
        /// Source span of the ambiguous use site.
        span: Span,
    },

    // ── T031 ─────────────────────────────────────────────────────────────────
    /// An `instance C T` is declared outside both the module that defines `C`
    /// and the module that defines `T` (orphan-instance rule).
    ///
    /// The orphan rule is the coherence property that prevents a third-party
    /// module from hijacking security-critical class instances.
    OrphanInstance {
        /// Display name of the class.
        class: String,
        /// Display name of the type.
        ty: String,
        /// Module that contains the violating instance declaration.
        instance_module: String,
        /// Source span of the `instance` keyword.
        span: Span,
    },

    // ── T032 ─────────────────────────────────────────────────────────────────
    /// A second `instance C T` is declared for the same `(C, T)` pair.
    ///
    /// Only one instance per `(class, type)` pair is allowed (Haskell-98
    /// coherence). The single-value-per-key `InstanceEnv` structurally enforces
    /// this: a duplicate insert is a hard error, never a silent override.
    OverlappingInstance {
        /// Display name of the class.
        class: String,
        /// Display name of the type.
        ty: String,
        /// Span of the first (existing) instance declaration.
        first_span: Span,
        /// Span of the second (conflicting) instance declaration.
        second_span: Span,
    },

    // ── T033 ─────────────────────────────────────────────────────────────────
    /// `instance C T` is declared but a required superclass instance is absent.
    ///
    /// For example, `instance Ord T` requires `instance Eq T` because `Ord`
    /// declares `Eq` as a superclass. The check walks the superclass DAG
    /// transitively; the DAG is acyclic by construction (T035 is reported
    /// earlier if a cycle is detected).
    MissingSuperclassInstance {
        /// Display name of the class being instantiated.
        class: String,
        /// Display name of the type.
        ty: String,
        /// Display name of the missing superclass.
        superclass: String,
        /// Source span of the `instance` declaration that triggered the check.
        span: Span,
    },

    // ── T034 ─────────────────────────────────────────────────────────────────
    /// A type has both a `pub fn toText` (auto-promoted to a `ToText` instance)
    /// and an explicit `instance ToText T` declaration.
    ///
    /// This is a **hard error** — not a warning — because allowing silent
    /// override would mean two different `ToText` behaviours depending on the
    /// collect order, which is a coherence violation.
    // T034 RETIRED-SLOT: if this variant is removed, mark the code slot
    // T034 as RESERVED in this file so the number is not reused.
    ToTextConflict {
        /// Display name of the type.
        ty: String,
        /// Span of the explicit `instance ToText T` declaration.
        totext_span: Span,
        /// Span of the `pub fn toText` declaration that was auto-promoted.
        auto_promote_span: Span,
    },

    // ── T035 ─────────────────────────────────────────────────────────────────
    /// The class hierarchy forms a cycle (e.g. `class A where B` and
    /// `class B where A`).
    ///
    /// Detected during class collection, before any instance solving. A cycle
    /// would make superclass propagation non-terminating; this check ensures
    /// the class DAG is acyclic.
    SuperclassCycle {
        /// The class names forming the cycle, in cycle order.
        cycle: Vec<String>,
        /// Source span of the first class in the cycle.
        span: Span,
    },

    // ── T036 ─────────────────────────────────────────────────────────────────
    /// A field of an `opaque` type was reached (`.field` or `with`) from outside
    /// the module that declares the type. Opaque types hide their representation;
    /// only their defining module may read or rebuild their fields.
    OpaqueFieldAccess {
        /// Name of the opaque record type.
        record: String,
        /// The field being accessed or updated.
        field: String,
        /// Source span of the offending access.
        span: Span,
    },

    // ── T037 ─────────────────────────────────────────────────────────────────
    /// Two record rows cannot be unified because their fixed field sets
    /// disagree — for example a closed record met an extra field, or two closed
    /// records carry different labels. Distinct from T001 so a shape failure
    /// reads in record terms (which fields are missing or unexpected) rather
    /// than as a flat "type mismatch".
    RowMismatch {
        /// The expected record row, rendered (e.g. `{ x: Int }`).
        expected: String,
        /// The found record row, rendered (e.g. `{ x: Int, y: Int }`).
        found: String,
        /// Labels the expected row requires that the found row lacks.
        missing_fields: Vec<String>,
        /// Labels the found row carries that the expected row does not allow.
        extra_fields: Vec<String>,
        /// Source span of the offending expression.
        span: Span,
    },

    // ── T038 ─────────────────────────────────────────────────────────────────
    /// An `instance` head supplies the wrong number of type atoms for its class.
    ///
    /// A class declares a fixed number of type parameters (`class Convert a b`
    /// has two). The instance head must supply exactly that many type atoms, so
    /// `instance Convert Celsius` (one atom for a two-parameter class) and
    /// `instance Eq Int Bool` (two for a one-parameter class) are both rejected.
    InstanceArityMismatch {
        /// Display name of the class.
        class: String,
        /// Number of type parameters the class declares.
        expected: usize,
        /// Number of type atoms the instance head supplied.
        found: usize,
        /// Source span of the `instance` declaration.
        span: Span,
    },

    // ── T039 ─────────────────────────────────────────────────────────────────
    /// A quoted predicate references a field that is not a column of its entity.
    ///
    /// Inside `fn u -> u.age >= 18`, every `u.field` must name a real field of
    /// the entity the quote is checked against. A typo or a dropped column is
    /// caught here rather than producing wrong SQL at runtime.
    QuoteUnknownColumn {
        /// Display name of the entity the quote is checked against.
        entity: String,
        /// The field name that is not a column.
        column: String,
        /// Near-miss column names to suggest.
        suggestions: Vec<String>,
        /// Source span of the offending field access.
        span: Span,
    },

    // ── T040 ─────────────────────────────────────────────────────────────────
    /// A quoted predicate uses a form the quotation layer does not support yet.
    ///
    /// The quoted sub-language is deliberately small: column references,
    /// literals, comparisons, and `&&`/`||`. Anything else (a free variable, an
    /// arithmetic operator, a call) lands here with a description of what was
    /// found.
    QuoteUnsupportedExpr {
        /// What the quote contained that is not supported.
        detail: String,
        /// Source span of the offending expression.
        span: Span,
    },

    // ── T041 ─────────────────────────────────────────────────────────────────
    /// The two sides of a comparison in a quoted predicate have different types.
    ///
    /// `u.age >= "18"` compares an `Int` column with a `Text` literal; the
    /// operands must share a type so the generated SQL is well-typed.
    QuoteComparisonMismatch {
        /// Rendered type of the left operand.
        left: String,
        /// Rendered type of the right operand.
        right: String,
        /// Source span of the comparison.
        span: Span,
    },

    // ── T042 ─────────────────────────────────────────────────────────────────
    /// The entity type a quoted predicate is checked against cannot be
    /// determined at the call site.
    ///
    /// A `Quote (e -> Bool)` parameter needs `e` to be a concrete record type so
    /// `u.field` can be resolved. When `e` is still open (no surrounding query
    /// fixes it), annotate the predicate so the entity is known.
    QuoteEntityUnknown {
        /// Source span of the quoted lambda.
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
    /// The codes are allocated in `T001..T037` and `T999` is the catch-all
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
            Self::IncompleteRecordPattern { .. } => "T028",
            Self::InlineRecordTyVarField { .. } => "P029",
            Self::NoInstance { .. } => "T029",
            Self::AmbiguousConstraint { .. } => "T030",
            Self::OrphanInstance { .. } => "T031",
            Self::OverlappingInstance { .. } => "T032",
            Self::MissingSuperclassInstance { .. } => "T033",
            Self::ToTextConflict { .. } => "T034",
            Self::SuperclassCycle { .. } => "T035",
            Self::OpaqueFieldAccess { .. } => "T036",
            Self::RowMismatch { .. } => "T037",
            Self::InstanceArityMismatch { .. } => "T038",
            Self::QuoteUnknownColumn { .. } => "T039",
            Self::QuoteUnsupportedExpr { .. } => "T040",
            Self::QuoteComparisonMismatch { .. } => "T041",
            Self::QuoteEntityUnknown { .. } => "T042",
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

    // ── T029–T030 helpers and code tests ─────────────────────────────────────

    fn t029() -> TypeError {
        TypeError::NoInstance {
            class: "ToText".into(),
            ty: "Foo".into(),
            span: dummy_span(),
            fix_hint: "add `instance ToText Foo` or add `deriving (ToText)` to the type".into(),
        }
    }

    fn t030() -> TypeError {
        TypeError::AmbiguousConstraint {
            class: "ToText".into(),
            ty_var: "a".into(),
            span: dummy_span(),
        }
    }

    #[test]
    fn code_t029() {
        assert_eq!(t029().code(), "T029");
    }

    #[test]
    fn code_t030() {
        assert_eq!(t030().code(), "T030");
    }

    // ── T031–T035 helpers and code tests ─────────────────────────────────────

    fn t031() -> TypeError {
        TypeError::OrphanInstance {
            class: "Eq".into(),
            ty: "Logger".into(),
            instance_module: "app.Util".into(),
            span: dummy_span(),
        }
    }

    fn t032() -> TypeError {
        TypeError::OverlappingInstance {
            class: "ToText".into(),
            ty: "Color".into(),
            first_span: dummy_span(),
            second_span: dummy_span(),
        }
    }

    fn t033() -> TypeError {
        TypeError::MissingSuperclassInstance {
            class: "Ord".into(),
            ty: "Color".into(),
            superclass: "Eq".into(),
            span: dummy_span(),
        }
    }

    fn t034() -> TypeError {
        TypeError::ToTextConflict {
            ty: "User".into(),
            totext_span: dummy_span(),
            auto_promote_span: dummy_span(),
        }
    }

    fn t035() -> TypeError {
        TypeError::SuperclassCycle {
            cycle: vec!["A".into(), "B".into()],
            span: dummy_span(),
        }
    }

    #[test]
    fn code_t031() {
        assert_eq!(t031().code(), "T031");
    }

    #[test]
    fn code_t032() {
        assert_eq!(t032().code(), "T032");
    }

    #[test]
    fn code_t033() {
        assert_eq!(t033().code(), "T033");
    }

    #[test]
    fn code_t034() {
        assert_eq!(t034().code(), "T034");
    }

    #[test]
    fn code_t035() {
        assert_eq!(t035().code(), "T035");
    }

    fn t037() -> TypeError {
        TypeError::RowMismatch {
            expected: "{ x: Int }".into(),
            found: "{ x: Int, y: Int }".into(),
            missing_fields: vec![],
            extra_fields: vec!["y".into()],
            span: dummy_span(),
        }
    }

    #[test]
    fn code_t037() {
        assert_eq!(t037().code(), "T037");
    }

    #[test]
    fn t037_message_names_the_unexpected_field() {
        let msg = format!("{}", t037());
        assert!(msg.contains("T037"), "message should carry the code: {msg}");
        assert!(
            msg.contains("unexpected field(s): y"),
            "message should name the extra field: {msg}"
        );
    }

    fn t038() -> TypeError {
        TypeError::InstanceArityMismatch {
            class: "Convert".into(),
            expected: 2,
            found: 1,
            span: dummy_span(),
        }
    }

    #[test]
    fn code_t038() {
        assert_eq!(t038().code(), "T038");
    }

    #[test]
    fn t038_message_names_the_counts() {
        let msg = format!("{}", t038());
        assert!(msg.contains("T038"), "message should carry the code: {msg}");
        assert!(
            msg.contains('2') && msg.contains('1'),
            "message should report expected and found counts: {msg}"
        );
    }
}
