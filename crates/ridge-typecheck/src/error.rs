//! `TypeError` — the `T###` diagnostic type for Phase 4 type checking.
//!
//! Every variant carries a stable [`TypeError::code`] (e.g. `"T001"`) that
//! mirrors the `R###`/`M###` convention from earlier phases.
//!
//! `Display` and `std::error::Error` are implemented in [`crate::render`]
//! where the full multi-line output matching spec §5.3 / §5.4 / §6.4 lives.

use ridge_ast::Span;
use ridge_types::{CapabilitySet, TyConDecl, Type};

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// How a diagnostic names a type to the reader.
///
/// These fields used to be `String`, and the construction site chose the
/// rendering. A `String` accepts anything, the renderer that names types needs
/// the type-constructor table, and so the correct call was three parts long
/// while `format!("{ty:?}")` was one. The short one kept winning: four separate
/// sweeps have removed `Con(TyConId(6), [...])` from messages it had reached.
///
/// The three arms are the three provenances that a census of every construction
/// site actually found — not a type and an escape hatch, which is where the
/// defect would come back, but the three distinct things these fields hold:
///
/// * [`Self::Ty`] leaves nothing for the call site to stringify. There is no
///   `String` to build, so there is no `Debug` dump to write.
/// * [`Self::Text`] is text this site built: a name the author wrote, carried
///   through as they spelled it at sites like `derive.rs` where no `Type` is
///   in scope at all, or a rendering of something that is not a type — the
///   capability rows `unify.rs` compares, for one.
/// * [`Self::Phrase`] is a fixed description for the cases where no single type
///   is the answer: `solve.rs` reports a missing instance for "a function of
///   arity 2".
#[derive(Debug, Clone)]
pub enum TypeDesc {
    /// A type, rendered where the type-constructor table is in hand.
    ///
    /// Must already be resolved: the substitution lives on the inference
    /// context and is gone by the time a diagnostic is rendered. Build one with
    /// `InferCtx::ty_desc`, which resolves first, or [`TypeDesc::ty`].
    ///
    /// Boxed because `TypeError` travels as the `Err` of every `unify`, and a
    /// `Type` inline made the largest variant 136 bytes — every result in the
    /// hot path paying for a value only a failure ever reads.
    Ty(Box<Type>),
    /// Text this site built for the reader: a name the author wrote, or a
    /// rendering of something that is not a `Type` at all.
    Text(String),
    /// A description of a shape, where no one type is what the reader needs.
    Phrase(&'static str),
}

impl TypeDesc {
    /// A resolved type, ready to be named.
    #[must_use]
    pub fn ty(t: Type) -> Self {
        Self::Ty(Box::new(t))
    }

    /// The reader's text, for one description on its own.
    ///
    /// Two descriptions from the same diagnostic must not go through here
    /// separately. Each call starts its type-variable letters at `a`, so two
    /// different variables both come out `a` and one variable used twice can
    /// come out as two letters. Use [`Self::render_pair`], or render the whole
    /// message with [`TypeError::render`], which shares one namer across it.
    #[must_use]
    pub fn render(&self, tycons: &[TyConDecl]) -> String {
        crate::render::render_descs(&[self], tycons).swap_remove(0)
    }

    /// Two descriptions of the same diagnostic, rendered together.
    ///
    /// `expected List b, found a` says the two are different variables;
    /// `expected List a, found a` says the element has to be the caller's own
    /// `a`. Which sentence the reader gets depends entirely on the two sharing
    /// a namer, so the pair has to be rendered in one go.
    #[must_use]
    pub fn render_pair(&self, other: &Self, tycons: &[TyConDecl]) -> (String, String) {
        let mut out = crate::render::render_descs(&[self, other], tycons);
        let second = out.swap_remove(1);
        (out.swap_remove(0), second)
    }
}

/// Test-only shorthand so a fixture can write `"Int".into()`.
///
/// Deliberately `cfg(test)`: the point of the enum is that production code has
/// to say where a string came from, and a blanket `From` would let it say
/// nothing.
#[cfg(test)]
impl From<&str> for TypeDesc {
    fn from(s: &str) -> Self {
        Self::Text(s.to_owned())
    }
}

// Deliberately no `From<String>`: `found_ty: format!("{ty:?}").into()` would
// compile, and that is the whole defect back with an `.into()` in front of it.
// `Text` can still be handed a debug dump, but it has to be spelled out in
// full, which is longer than `ctx.ty_desc(&ty)` rather than shorter — the
// incentive a bare `String` had backwards.

/// What kind of declaration a `T014 CapabilityNotDeclared` was raised against.
///
/// The check is shared by top-level functions, actor `on` handlers, `init`
/// blocks, and inner functions; the kind lets the diagnostic speak in the
/// declaration's own syntax (`fn` / `on` / `init`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapDeclKind {
    /// A top-level `fn`.
    Fn,
    /// An actor `on` message handler.
    Handler,
    /// An actor `init` block.
    Init,
    /// An actor `terminate` callback.
    Terminate,
    /// An actor `onDown` monitor-notification handler.
    OnDown,
    /// An inner `fn` — the check compares its declared set against the
    /// *enclosing* effective set (Rule 4), so `declared` is the enclosing
    /// set, not the inner fn's own annotation.
    InnerFn,
}

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
        expected: TypeDesc,
        /// The found type.
        found: TypeDesc,
        /// Source span of the sub-expression.
        span: Span,
        /// Optional diagnostic hint shown below the main message — for example
        /// "Ridge calls are space-separated: `add 1 2`, not `add(1, 2)`" when
        /// the mismatch comes from a parenthesised comma call, or "record
        /// literals name their constructor" when an anonymous record literal
        /// is supplied where a named record type is expected.
        hint: Option<String>,
    },

    // ── T002 ─────────────────────────────────────────────────────────────────
    /// Type mismatch on a specific argument in a function call.
    TypeMismatchInCall {
        /// Name of the callee function.
        callee: String,
        /// Zero-based index of the mismatched argument.
        arg_index: usize,
        /// Expected argument type.
        expected: TypeDesc,
        /// Found argument type.
        found: TypeDesc,
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
        ty: TypeDesc,
        /// Source span of the LHS expression.
        span: Span,
    },

    // ── T007 ─────────────────────────────────────────────────────────────────
    /// A pattern does not match the scrutinee's type.
    PatternTypeMismatch {
        /// The scrutinee's expected type.
        expected: TypeDesc,
        /// The type implied by the pattern.
        pattern: TypeDesc,
        /// Source span of the pattern.
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
        /// What cannot contain itself, as the reader should see it named — a
        /// single-letter type variable in backticks, or a phrase such as
        /// `this record` where the thing has no name to give.
        var: TypeDesc,
        /// The type it would have to occur inside, rendered with the same
        /// variable letters as `var`.
        ty: TypeDesc,
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

    // ── T013 ─────────────────────────────────────────────────────────────────
    /// A declaration is used at a second type inside its own definition, and
    /// its signature is too thin for that to be checked.
    ///
    /// Legal under a complete signature: the declared type is instantiated at
    /// each occurrence and the body is still held to it. With a position left
    /// off there is nothing to instantiate, and inferring one is undecidable —
    /// so this reports what the signature is missing rather than the rule.
    PolymorphicRecursion {
        /// Name of the declaration being called.
        decl: String,
        /// The annotations that would make the call legal, phrased as an edit.
        fix_hint: String,
        /// Source span of the problematic recursive call.
        recursive_call_span: Span,
    },

    // ── T014 ─────────────────────────────────────────────────────────────────
    /// The capability set inferred from a function body exceeds its declared
    /// annotation.
    CapabilityNotDeclared {
        /// Name of the function/handler declaration.
        decl: String,
        /// What kind of declaration was checked — drives the diagnostic's
        /// wording (`fn` / `on` / `init` / inner `fn`).
        kind: CapDeclKind,
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
        scrutinee_ty: TypeDesc,
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
        found_ty: TypeDesc,
        /// Source span of the LHS expression.
        span: Span,
    },

    // ── T021 ─────────────────────────────────────────────────────────────────
    /// The `?>` ask operator is applied to a non-`Handle` value.
    AskOnNonActor {
        /// The actual type found on the LHS of `?>`.
        found_ty: TypeDesc,
        /// Source span of the LHS expression.
        span: Span,
    },

    // ── T022 ─────────────────────────────────────────────────────────────────
    /// A non-`Unit` value is silently discarded at statement level.
    DiscardedResult {
        /// The type of the discarded expression.
        ty: TypeDesc,
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
    /// A capability variable escapes into a user-visible type.
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
        found: TypeDesc,
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
        ty: TypeDesc,
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
        ty_var: TypeDesc,
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
        ty: TypeDesc,
        /// Module that contains the violating instance declaration.
        instance_module: String,
        /// Every module this instance would be legal in: the one declaring the
        /// class, and the one declaring each type in the head.
        ///
        /// Empty when the pair is built in, and that case needs its own
        /// sentence rather than a shorter list. Telling a reader to move
        /// `instance ToText Date` to "the class's module or the type's module"
        /// names two places they cannot go, which is worse than saying nothing:
        /// it reads as an instruction and cannot be followed.
        legal_modules: Vec<String>,
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
        ty: TypeDesc,
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
        ty: TypeDesc,
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
        ty: TypeDesc,
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
        expected: TypeDesc,
        /// The found record row, rendered (e.g. `{ x: Int, y: Int }`).
        found: TypeDesc,
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
        left: TypeDesc,
        /// Rendered type of the right operand.
        right: TypeDesc,
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

    // ── T043 ─────────────────────────────────────────────────────────────────
    /// A function parameter destructures with a pattern that does not match
    /// every value of its type.
    ///
    /// Top-level parameter patterns must be irrefutable — a function is called
    /// on every value of its parameter type, so the pattern cannot be allowed
    /// to fail. Use a single-constructor pattern (record, newtype, single-variant
    /// union, tuple), or destructure in the body with `match` / `let`.
    RefutablePatternParam {
        /// Rendered example value the pattern fails to match (a witness).
        witness: String,
        /// Rendered type of the parameter.
        ty: TypeDesc,
        /// Source span of the parameter pattern.
        span: Span,
    },

    // ── T044 ─────────────────────────────────────────────────────────────────
    /// A name is used as a constructor (in a value or pattern) but does not
    /// name one.
    ///
    /// The usual cause is a single-variant union written without its leading
    /// `|`, which parses as a type alias (`type X = Foo Int` aliases `Foo Int`
    /// instead of declaring the constructor `Foo`). It also covers record-style
    /// union variants in patterns, which are not yet matchable. A genuinely
    /// unresolved name is reported by the resolver (`R010`) instead.
    NotAConstructor {
        /// The offending name.
        name: String,
        /// Context-specific guidance toward the fix.
        hint: String,
        /// Source span of the use site.
        span: Span,
    },

    // ── T045 ─────────────────────────────────────────────────────────────────
    /// A functional dependency on a class names a variable that is not one of
    /// the class's type parameters.
    ///
    /// In `class Refinable q p | q -> z`, the determined variable `z` is not a
    /// parameter of `Refinable` (whose parameters are `q` and `p`).
    UnknownFunDepVar {
        /// Display name of the class.
        class: String,
        /// The variable named in the fundep that is not a class parameter.
        var: String,
        /// Source span of the functional dependency.
        span: Span,
    },

    // ── T046 ─────────────────────────────────────────────────────────────────
    /// Two instances violate a functional dependency: they agree on the
    /// determining types but differ on a determined one.
    ///
    /// With `class Refinable q p | q -> p`, the dependency `q -> p` means `q`
    /// fixes `p`. Two instances `Refinable T U1` and `Refinable T U2` with
    /// `U1 != U2` would let one `q` determine two different `p`s — rejected.
    ConflictingFunDep {
        /// Display name of the class.
        class: String,
        /// Rendered determining types the two instances share.
        determining: String,
        /// Source span of the first instance.
        first_span: Span,
        /// Source span of the second (conflicting) instance.
        second_span: Span,
    },

    // ── T047 ─────────────────────────────────────────────────────────────────
    /// A full entity was supplied where a typed insert expects its insert
    /// shape — the `<Entity>Insert` companion that drops database-generated
    /// columns.
    ///
    /// `insert (User { id = 1, name = "ada" }) repo` fails this way: `insert`
    /// takes `UserInsert`, the entity minus its generated `id`, so a
    /// hand-written `id` is rejected before it can reach the database.
    InsertShapeFullEntity {
        /// Name of the entity that was supplied (e.g. `"User"`).
        entity: String,
        /// Name of the insert-shape companion expected instead (e.g.
        /// `"UserInsert"`).
        companion: String,
        /// The database-generated columns the companion drops, in declaration
        /// order — the fields the caller must omit.
        omitted: Vec<String>,
        /// Source span of the supplied entity expression.
        span: Span,
    },

    // ── T048 ─────────────────────────────────────────────────────────────────
    /// An actor callback's declared parameters do not match the shape OTP
    /// delivers: `terminate` takes at most one parameter of type `ExitReason`;
    /// `onDown` takes exactly two — a `Monitor` and an `ExitReason`, in that
    /// order.
    ///
    /// Without this check a wrong `terminate` arity surfaced as a late
    /// codegen error, and a wrong parameter type was never diagnosed at all.
    ActorCallbackSignature {
        /// Which callback (`"terminate"` or `"onDown"`).
        member: &'static str,
        /// The required parameter types, e.g. `"Monitor, ExitReason"`.
        expected: String,
        /// The declared parameter types, e.g. `"Int"`.
        found: String,
        /// Source span of the callback declaration.
        span: Span,
    },

    // ── T049 ─────────────────────────────────────────────────────────────────
    /// A versioned type reference (`User@1`) named a version the compiler has
    /// no record of. The snapshot history is empty on a fresh build, so any
    /// `Name@N` reference fails until at least one previous build exists.
    UnknownTypeVersion {
        /// The referenced type or actor-state name.
        name: String,
        /// The referenced ordinal.
        ordinal: u32,
        /// Where the versioned reference appears.
        span: Span,
    },

    // ── T050 ─────────────────────────────────────────────────────────────────
    /// Two `migrate` members on the same type or actor cover the same
    /// version edge. Each edge may be migrated exactly once.
    DuplicateMigration {
        /// The owning type or actor name.
        name: String,
        /// The duplicated ordinal.
        ordinal: u32,
        /// Where the duplicate member appears.
        span: Span,
    },

    // ── T051 ─────────────────────────────────────────────────────────────────
    /// An `instance` head has a form the dispatcher cannot key on. The only
    /// reachable case today is a function-type head whose arity exceeds the
    /// reserved `Fn/0..Fn/15` block (`FN_ARITY_COUNT`): without a synthetic
    /// constructor there is no dispatch key, so collecting the instance
    /// silently would surface later as a confusing `NoInstance` (T029) at use
    /// sites. Rejected here, at the declaration.
    UnsupportedInstanceHead {
        /// The class being instantiated.
        class: String,
        /// Why the head is unsupported.
        reason: String,
        /// Source span of the `instance` declaration.
        span: Span,
    },

    // ── T052 ─────────────────────────────────────────────────────────────────
    /// An arithmetic operator (`+ - * / % **`) was applied to operands whose
    /// type is concrete and not numeric (`Int` or `Float`).
    ///
    /// Arithmetic lowers to the BEAM's numeric BIFs, so `+` over `Text`, a
    /// list, or a user type used to compile cleanly and then crash at runtime
    /// with `badarith`. Text and list concatenation is `++`. Operands whose
    /// type is still an unresolved variable are left alone — pinning generics
    /// to numeric requires a `Num`-style constraint the language does not
    /// have yet.
    ArithmeticOnNonNumeric {
        /// The operator as written in source (e.g. `"+"`).
        op: &'static str,
        /// The concrete non-numeric operand type, rendered for display.
        found: TypeDesc,
        /// Source span of the binary expression.
        span: Span,
    },

    // ── T053 ─────────────────────────────────────────────────────────────────
    /// A top-level `fn main` declares parameters.
    ///
    /// `main` is the program entry point: the BEAM runner invokes it with no
    /// arguments, so a `main` that takes parameters compiled cleanly and then
    /// crashed at startup with `undef` (`main/0` does not exist). The name is
    /// effectively reserved — the lowerer already marks any top-level `main`
    /// as the entry point regardless of project kind — so the arity rule
    /// applies in libraries too. Command-line arguments are read through the
    /// stdlib (`Cli.args ()`), not through parameters.
    MainHasParams {
        /// Number of declared parameters.
        found: usize,
        /// Source span of the `fn main` declaration.
        span: Span,
    },

    // ── T059 ─────────────────────────────────────────────────────────────────
    /// `main` returns a `Result` whose error type cannot be rendered as text.
    ///
    /// Returning `Err` is the documented way for a Ridge program to fail, so it
    /// is the failure path most programs take on purpose. The runner projects
    /// that value onto stderr and an exit code, and by then the type is gone —
    /// several Ridge shapes share one Erlang shape, so anything the runtime
    /// rendered there would be a guess, and it would guess wrong on exactly the
    /// error types people define for themselves.
    ///
    /// So the requirement is a type rule, checked here: `main`'s error type must
    /// have a `ToText` instance, and the program converts at the boundary. Rust
    /// settles `fn main() -> Result<(), E>` the same way, with `E: Debug`.
    ///
    /// Distinct from `T029 NoInstance`, which fires where a *use* needs an
    /// instance the author did not provide. This one fires on a signature that
    /// names no class at all: nothing in `-> Result Unit MyErr` mentions
    /// `ToText`, and the obligation comes from being the entry point.
    MainErrorNotShowable {
        /// Display name of the error type (e.g. `"MyErr"`).
        ty: TypeDesc,
        /// Context-specific fix suggestion shown below the main message.
        fix_hint: String,
        /// Where the error type is declared: the raw `ModuleId` and the span of
        /// its `type` declaration. `None` for a type the reader cannot extend —
        /// a built-in, or one that came from outside the workspace — which is
        /// the same condition under which `fix_hint` stops offering `deriving`.
        ///
        /// Both halves travel together because neither is usable alone: a span
        /// without its module points into whichever file the reader happens to
        /// have open.
        decl_site: Option<(u32, Span)>,
        /// Source span of the `fn main` declaration.
        span: Span,
    },

    // ── T054 ─────────────────────────────────────────────────────────────────
    /// A field access `base.field` is applied to a non-record type.
    ///
    /// Distinct from `T006 WithOnNonRecord`, which covers the `with`-update
    /// expression: the user wrote a field access, so the diagnostic speaks of
    /// field access. When the base type's constructor shares its name with a
    /// stdlib module that exports a function of the field's name (e.g. `xs.length`
    /// on `List Int` ↔ `List.length`), `suggestion` carries that qualified name.
    FieldAccessOnNonRecord {
        /// The actual type of the base expression, rendered user-facing.
        ty: TypeDesc,
        /// The field name the user wrote.
        field: String,
        /// Qualified module function to suggest (e.g. `List.length`), if any.
        suggestion: Option<String>,
        /// Source span of the field access.
        span: Span,
    },

    // ── T055 ─────────────────────────────────────────────────────────────────
    /// The body of a fully-annotated declaration needs a class the signature
    /// does not promise.
    ///
    /// `fn mySort (xs: List a) -> List a = List.sort xs` needs `Ord a`, and a
    /// reader of that signature is told it works for every `a`. Writing the
    /// signature is a claim about every type the caller may choose, so the
    /// requirements come with it; the alternative is that the claim is only
    /// discovered by the caller who breaks it.
    ///
    /// Only fires when every parameter and the return type are annotated. A
    /// declaration that leaves any of them off is asking to be inferred, and
    /// inference still supplies the constraints.
    MissingConstraint {
        /// The declaration whose signature is short a promise.
        decl: String,
        /// The class the body needs, e.g. `Ord`.
        class: String,
        /// The signature's own name for the variable, e.g. `a`.
        ty_var: TypeDesc,
        /// The `where` clause that would satisfy it, ready to paste.
        fix_hint: String,
        /// Source span of the declaration's signature.
        span: Span,
    },

    // ── T056 ─────────────────────────────────────────────────────────────────
    /// A type annotation names a type that does not exist.
    ///
    /// Nothing in the arena carries the name — not a builtin, not the prelude,
    /// not a reconciled standard-library type, not a declaration in the
    /// workspace, and not a parameter of the enclosing declaration. It used to
    /// become a fresh type variable, which unifies with anything, so the
    /// annotation quietly stopped constraining its position and `f "text"`,
    /// `f 42` and `f true` all type-checked against `fn f (x: Bogus)`.
    UnknownTypeName {
        /// The name as written.
        name: String,
        /// Source span of the annotation.
        span: Span,
        /// Up to three near-miss type names, closest first. Empty when nothing
        /// in scope is within edit distance.
        suggestions: Vec<String>,
    },

    // ── T057 ─────────────────────────────────────────────────────────────────
    /// Two tuples meet with different numbers of components.
    ///
    /// Separate from `T003` because nothing about the call is wrong.
    /// `takesPair (1, 2, 3)` passes exactly one argument; what differs is the
    /// width of the tuple inside it. Reported as an arity mismatch it read
    /// `expects 2 arguments, got 3` and sent the reader looking for a fourth
    /// argument nobody wrote.
    TupleWidthMismatch {
        /// Number of components the position expects.
        expected: usize,
        /// Number of components the tuple actually has.
        found: usize,
        /// Source span of the tuple.
        span: Span,
    },

    // ── T058 ─────────────────────────────────────────────────────────────────
    /// The `?` propagate operator is used outside a `Result`/`Option` context.
    ///
    /// Held `T021` alongside `AskOnNonActor` until 0.3.0. They are not one
    /// failure reached two ways: one is about actors and the other about
    /// propagation, they share no fix, and a reader who searched the code they
    /// were given landed on the other one's explanation.
    PropagateOutsideResultOrOption {
        /// The actual type of the expression `?` is applied to.
        found_ty: TypeDesc,
        /// The type expected by the enclosing context.
        expected: TypeDesc,
        /// Source span of the `?` operator.
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
    /// The codes are allocated in `T001..T058` and `T999` is the catch-all
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
            Self::WrongConstructorArity { .. } => "T009",
            Self::OccursCheck { .. } => "T010",
            Self::RecursiveTypeAlias { .. } => "T011",
            Self::PolymorphicRecursion { .. } => "T013",
            Self::CapabilityNotDeclared { .. } => "T014",
            Self::UnknownActorHandler { .. } => "T015",
            Self::NonExhaustiveMatch { .. } => "T016",
            Self::RedundantPattern { .. } => "T017",
            Self::CallerCapabilityInsufficient { .. } => "T018",
            Self::ActorCapabilityLeak { .. } => "T019",
            Self::SendOnNonActor { .. } => "T020",
            Self::AskOnNonActor { .. } => "T021",
            Self::DiscardedResult { .. } => "T022",
            Self::UnsolvedTypeVariable { .. } => "T023",
            Self::RowVariableLeak { .. } => "T024",
            Self::SpawnArityMismatch { .. } => "T025",
            Self::AskTimeoutNotInt { .. } => "T026",
            Self::MailboxPolicyDropOldestNotShipped { .. } => "T027",
            Self::IncompleteRecordPattern { .. } => "T028",
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
            Self::RefutablePatternParam { .. } => "T043",
            Self::NotAConstructor { .. } => "T044",
            Self::UnknownFunDepVar { .. } => "T045",
            Self::ConflictingFunDep { .. } => "T046",
            Self::InsertShapeFullEntity { .. } => "T047",
            Self::ActorCallbackSignature { .. } => "T048",
            Self::UnknownTypeVersion { .. } => "T049",
            Self::DuplicateMigration { .. } => "T050",
            Self::UnsupportedInstanceHead { .. } => "T051",
            Self::ArithmeticOnNonNumeric { .. } => "T052",
            Self::MainHasParams { .. } => "T053",
            Self::MainErrorNotShowable { .. } => "T059",
            Self::FieldAccessOnNonRecord { .. } => "T054",
            Self::MissingConstraint { .. } => "T055",
            Self::UnknownTypeName { .. } => "T056",
            Self::TupleWidthMismatch { .. } => "T057",
            Self::PropagateOutsideResultOrOption { .. } => "T058",
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
            hint: None,
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

    fn t013() -> TypeError {
        TypeError::PolymorphicRecursion {
            decl: "f".into(),
            fix_hint: "annotate the return type of `f`".into(),
            recursive_call_span: dummy_span(),
        }
    }

    fn t014() -> TypeError {
        TypeError::CapabilityNotDeclared {
            decl: "procesarConfig".into(),
            kind: crate::error::CapDeclKind::Fn,
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

    fn t021() -> TypeError {
        TypeError::AskOnNonActor {
            found_ty: "Int".into(),
            span: dummy_span(),
        }
    }

    fn t058() -> TypeError {
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

    fn t047() -> TypeError {
        TypeError::InsertShapeFullEntity {
            entity: "User".into(),
            companion: "UserInsert".into(),
            omitted: vec!["id".into()],
            span: dummy_span(),
        }
    }

    fn t048() -> TypeError {
        TypeError::ActorCallbackSignature {
            member: "onDown",
            expected: "Monitor, ExitReason".into(),
            found: "Monitor".into(),
            span: dummy_span(),
        }
    }

    fn t999() -> TypeError {
        TypeError::InternalTypeError {
            detail: "unexpected node kind".into(),
            span: dummy_span(),
        }
    }

    fn t054() -> TypeError {
        TypeError::FieldAccessOnNonRecord {
            ty: "List Int".into(),
            field: "length".into(),
            suggestion: Some("List.length".into()),
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
    fn code_t021() {
        assert_eq!(t021().code(), "T021");
    }

    /// Propagation left `T021` in 0.3.0; it must not drift back.
    #[test]
    fn code_t058() {
        assert_eq!(t058().code(), "T058");
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
    fn code_t047() {
        assert_eq!(t047().code(), "T047");
    }

    #[test]
    fn t047_message_names_companion_entity_and_omitted_column() {
        let msg = t047().render(&[]);
        assert!(msg.contains("T047"), "{msg}");
        assert!(msg.contains("`UserInsert`"), "{msg}");
        assert!(msg.contains("`User`"), "{msg}");
        assert!(msg.contains("database-generated column `id`"), "{msg}");
    }

    #[test]
    fn code_t048() {
        assert_eq!(t048().code(), "T048");
    }

    #[test]
    fn t048_message_names_member_and_signatures() {
        let msg = t048().render(&[]);
        assert!(msg.contains("T048"), "{msg}");
        assert!(msg.contains("onDown"), "{msg}");
        assert!(msg.contains("Monitor, ExitReason"), "{msg}");
    }

    #[test]
    fn code_t999() {
        assert_eq!(t999().code(), "T999");
    }

    #[test]
    fn code_t054() {
        assert_eq!(t054().code(), "T054");
    }

    fn t057() -> TypeError {
        TypeError::TupleWidthMismatch {
            expected: 2,
            found: 3,
            span: dummy_span(),
        }
    }

    #[test]
    fn code_t057() {
        assert_eq!(t057().code(), "T057");
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
            legal_modules: vec!["app.Log".into()],
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
        let msg = t037().render(&[]);
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
        let msg = t038().render(&[]);
        assert!(msg.contains("T038"), "message should carry the code: {msg}");
        assert!(
            msg.contains('2') && msg.contains('1'),
            "message should report expected and found counts: {msg}"
        );
    }
}
