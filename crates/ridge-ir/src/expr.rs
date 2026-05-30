//! IR expression nodes — the canonical Ridge Core IR.
// OQ-IR003: all public IR enums carry #[non_exhaustive] so downstream crates cannot exhaustively match on them.
// T0 (Phase 6 OQ-E001 narrow exception): IrExpr::Ask gains `timeout: Option<IrTimeout>`.
// IrTimeout mirrors AskTimeout (ridge-ast) 1:1.

use crate::id::IrNodeId;
use crate::item::IrParam;
use crate::lit::IrLit;
use crate::pat::IrPat;
use crate::symbol::SymbolRef;
use ridge_ast::Span;
use ridge_types::{CapabilitySet, Type};

/// Ridge Core IR expression node.
///
/// **Design invariants** (every executor must respect them — see §3 for
/// rationale and §4 for the rules that produce them):
///
/// 1. **No syntactic-sugar variants.** No `Pipe`, no `Propagate`, no `Try`,
///    no `Guard`, no `Interp`, no `With`, no `If`. All eight collapse to
///    `Match` / `Call` / `Block` / `Construct` / `Concat`-shaped `Call`s.
/// 2. **No actor-message-name resolution at expression level.** `Send` and
///    `Ask` carry a `SymbolRef::Handler { ... }` resolved by the lowering pass
///    against the actor's `ActorSchema.handlers`. The string `message_name`
///    is preserved on the `Handler` `SymbolRef` for diagnostic clarity.
/// 3. **Every node carries an `IrNodeId`** (the IR-side node id) and a
///    `Span`. Type information is in the side-table `LoweredModule.node_types`,
///    not on the node itself, to keep nodes small.
/// 4. **Strict left-to-right evaluation** is honoured by ordering arguments
///    in `Call.args` left-to-right (spec §7.1).  No reordering for any
///    purpose during lowering.
/// 5. **`IrExpr::Block` is the only sequencing primitive.** It always has at
///    least one statement; the value of the block is its last statement.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IrExpr {
    // ── Atoms ────────────────────────────────────────────────────────────────
    /// A literal value node.
    Lit {
        /// The IR-side node identifier for this literal.
        id: IrNodeId,
        /// The literal value.
        value: IrLit,
        /// Source span.
        span: Span,
    },

    /// A reference to a value-level local binding (let-bound, lambda param,
    /// fn param, state field within an actor handler/init body).
    Local {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The name of the local binding.
        name: String,
        /// Source span.
        span: Span,
    },

    /// A reference to a resolved top-level / stdlib / prelude / external symbol.
    Symbol {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The resolved symbol reference.
        sym: SymbolRef,
        /// Source span.
        span: Span,
    },

    // ── Function shape ───────────────────────────────────────────────────────
    /// The single canonical call form.  Both source `f x` and source
    /// `x |> f` and source `f.field` (when used as a function) and
    /// curried-call sequences land here. Multi-argument calls are flat.
    Call {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The callee expression.
        callee: Box<Self>,
        /// The argument list, in strict left-to-right order.
        args: Vec<Self>,
        /// Source span.
        span: Span,
    },

    // OQ-L003: lambda lifting is deferred to Phase 6; Phase 5 emits IrExpr::Lambda as-is.
    /// A lambda value — captures its enclosing locals by reference (codegen
    /// will materialise the closure).  Phase 5 does not lambda-lift.
    Lambda {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The lambda's parameter list.
        params: Vec<IrParam>,
        /// The lambda body.
        body: Box<Self>,
        /// Capability set of this lambda (Phase 4 inferred).
        caps: CapabilitySet,
        /// Source span.
        span: Span,
    },

    // ── Construction / destructuring ────────────────────────────────────────
    /// Record or union-variant construction.
    ///
    /// Source `User { name = n, age = a }` → `Construct { ctor: Record User, fields:
    /// [(name, n), (age, a)] }`.
    /// Source `Some 42` → `Construct { ctor: UnionVariant Some, fields: [($0, 42)] }`.
    /// Source `None` → `Construct { ctor: UnionVariant None, fields: [] }`.
    ///
    /// `with` updates lower to a `Construct` over the *same* `TyCon`,
    /// with field values pulled from the base for un-touched fields and from
    /// the update for touched fields (§4.5).
    Construct {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The constructor symbol (record-auto or union-variant).
        ctor: SymbolRef,
        /// Named field values in source order.
        fields: Vec<(String, Self)>,
        /// Source span.
        span: Span,
    },

    /// Record `with`-update: `base with { field = val, … }`.
    ///
    /// A partial update over an existing record value — only the touched fields
    /// are listed; every other field is preserved from `base`. Backends lower
    /// this directly to a map update (`base#{ key => val, … }`), so it needs no
    /// record schema. This is what lets `with` work on a value whose concrete
    /// record type is not statically known at this point (e.g. an unannotated
    /// closure parameter).
    RecordUpdate {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The base record value being updated.
        base: Box<Self>,
        /// Touched fields and their new values, in source order.
        updates: Vec<(String, Self)>,
        /// Source span.
        span: Span,
    },

    /// Field projection.
    Field {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The base expression whose field is projected.
        base: Box<Self>,
        /// The field name to project.
        field: String,
        /// Source span.
        span: Span,
    },

    /// List literal.  Note: tuples are a separate variant.
    ListLit {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The element expressions, in source order.
        elems: Vec<Self>,
        /// Source span.
        span: Span,
    },

    /// Tuple literal.
    Tuple {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The tuple element expressions, in source order.
        elems: Vec<Self>,
        /// Source span.
        span: Span,
    },

    /// Cons-cell construction (`x :: xs` lowers to `Cons { head: x, tail: xs }`).
    /// Distinct from a generic `Construct` so backends can pattern-match it.
    Cons {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The head element.
        head: Box<Self>,
        /// The tail list.
        tail: Box<Self>,
        /// Source span.
        span: Span,
    },

    // ── Control flow ─────────────────────────────────────────────────────────
    // OQ-L004: decision-tree compilation for pattern-match is deferred to Phase 6;
    // Phase 5 emits IrExpr::Match with arms in source order, no tree optimisation.
    /// The single canonical control form.  All four AST shapes — `if`, `match`,
    /// `guard ... else`, and the `?` propagate — collapse to `Match`.
    /// `arms` is non-empty.  Each arm body is an `IrExpr` (typically a Block).
    Match {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The expression being matched.
        scrutinee: Box<Self>,
        /// The match arms, in source order.  Non-empty.
        arms: Vec<IrArm>,
        /// Source span.
        span: Span,
    },

    /// Block-sequencing.  `stmts` is non-empty; the value is the last stmt.
    Block {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The statement sequence.  Non-empty; the last entry is the block's value.
        stmts: Vec<Self>,
        /// Source span.
        span: Span,
    },

    // OQ-L012: IrExpr::LetIn carries no is_recursive flag; recursive inner functions
    // lower to LetIn(Bind, Lambda) via inner_fn.rs — the recursion is handled by the
    // binding map, not by a special IR flag.
    /// Local binding in continuation form.
    ///
    /// `let p: T = v` lowers to `LetIn { pat, value, body }`
    /// where `body` is the rest of the enclosing block (continuation form).
    /// Multi-stmt blocks of the AST `Block { [let p1=v1, let p2=v2, body] }`
    /// fold right to nested `LetIn`s — the canonical form.
    ///
    /// Continuation form makes the IR semantics of `let` purely expression-level,
    /// so backends do not need a "statement vs. expression" distinction (§3.4).
    LetIn {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The binding pattern.
        pat: IrPat,
        /// The value being bound.
        value: Box<Self>,
        /// The continuation — the rest of the enclosing block.
        body: Box<Self>,
        /// Source span.
        span: Span,
    },

    /// Mutable-binding declaration (D052 — `var` is allowed in fn bodies and
    /// actor init/handler bodies). Lowered shape mirrors `LetIn`.
    VarIn {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The variable name.
        name: String,
        /// The declared type of the variable.
        ty: Type,
        /// The initial value.
        value: Box<Self>,
        /// The continuation — the rest of the enclosing block.
        body: Box<Self>,
        /// Source span.
        span: Span,
    },

    /// Mutation of a `var`-bound local or actor state field.
    ///
    /// AST `target <- value` lowers to `Assign { target_kind, value }`
    /// where `target_kind` distinguishes locals from state fields. Phase 4
    /// already verified `target` resolves to a mutable binding.
    Assign {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The assignment target (local or state field).
        target: AssignTarget,
        /// The new value.
        value: Box<Self>,
        /// Source span.
        span: Span,
    },

    /// Early return (verbatim per spec §16.3).
    ///
    /// `return e` lowers to `Return { value: lower(e) }`. **No implicit
    /// `Ok`/`Err`/`Some`/`None` wrapping.**
    Return {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The returned value.
        value: Box<Self>,
        /// Source span.
        span: Span,
    },

    // OQ-IR001: Call/Send/Ask/Spawn carry no per-call CapabilitySet; capability tracking
    // is at the fn/handler declaration level only (IrFn.caps / IrHandler.caps).
    // ── Actor messaging — preserved as primitives ────────────────────────────
    /// `handle ! message args` — async one-way, returns Unit (spec §7.2).
    ///
    /// `message` is a resolved `SymbolRef::Handler`. `args` is the
    /// flattened argument list. **Send is target-neutral**: BEAM lowers it to
    /// `erlang:send/2` over a tagged tuple; native to a channel push; WASM
    /// (post 0.5) to a host-call.
    Send {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The actor handle expression.
        handle: Box<Self>,
        /// The resolved handler symbol (`SymbolRef::Handler`).
        message: SymbolRef,
        /// The flattened argument list.
        args: Vec<Self>,
        /// Source span.
        span: Span,
    },

    /// `handle ?> message args [timeout <ms|never>]` — sync request-reply
    /// (spec §7.2; timeout field added by Phase 6 T0, OQ-E001).
    ///
    /// `timeout: None` means "use the runtime default (5000 ms per OQ-E001)".
    /// The result type is the handler's return type.
    Ask {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The actor handle expression.
        handle: Box<Self>,
        /// The resolved handler symbol (`SymbolRef::Handler`).
        message: SymbolRef,
        /// The flattened argument list.
        args: Vec<Self>,
        /// Optional timeout — `None` = runtime default (5000 ms);
        /// `Some(IrTimeout::Never)` = infinity;
        /// `Some(IrTimeout::Millis(e))` = `e` milliseconds.
        timeout: Option<IrTimeout>,
        /// Source span.
        span: Span,
    },

    /// `spawn ActorName arg1 ... argN` — returns `Handle ActorType`.
    Spawn {
        /// The IR-side node identifier.
        id: IrNodeId,
        /// The actor type symbol (`SymbolRef::ActorType`).
        actor: SymbolRef,
        /// The spawn arguments (matched against the actor's `init` params).
        args: Vec<Self>,
        /// Source span.
        span: Span,
    },
}

// ── IrTimeout ─────────────────────────────────────────────────────────────────

/// Optional timeout for a `?>` ask expression (Phase 6 T0, OQ-E001).
///
/// `#[non_exhaustive]` preserves OQ-IR003 — downstream crates cannot
/// exhaustively match this enum, so new variants can be added in 0.2.0+.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IrTimeout {
    /// `timeout never` — wait indefinitely (maps to Erlang `infinity`).
    Never,
    /// `timeout <expr>` — wait at most `expr` milliseconds.
    ///
    /// The expression is guaranteed to be typed `Int` by the type-checker
    /// (T026 `AskTimeoutNotInt`).  `Box` breaks the recursive cycle between
    /// `IrTimeout` and `IrExpr`.
    Millis(Box<IrExpr>),
}

/// The target of a mutation (`<-`) assignment.
#[derive(Debug, Clone)]
pub enum AssignTarget {
    /// A `var`-bound local: `name <- value`.
    Local {
        /// The name of the local variable.
        name: String,
        /// Source span of the target.
        span: Span,
    },
    /// An actor state field: `field_name <- value`.  Only valid inside an
    /// `IrInit.body` or `IrHandler.body`.
    StateField {
        /// The name of the state field.
        name: String,
        /// Source span of the target.
        span: Span,
    },
}

/// A single arm of a `Match` expression.
#[derive(Debug, Clone)]
pub struct IrArm {
    /// The pattern to match the scrutinee against.
    pub pat: IrPat,
    /// `guard ... else`, when-clauses on match arms, and `if cond then T else F`
    /// all encode their predicate here (always lowered to a `Bool`-typed expr).
    pub when: Option<IrExpr>,
    /// The body expression evaluated when the arm matches.
    pub body: IrExpr,
    /// Source span of the arm.
    pub span: Span,
}
