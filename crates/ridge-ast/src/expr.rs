//! Expression nodes used throughout the AST.
//!
//! T3 added atoms: `Literal`, `Unit`, `Ident`, `Qualified`, `Interp`.
//! T6 adds the full Pratt expression surface: `Binary`, `Unary`, `Call`,
//! `FieldAccess`, `Pipe`, `List`, `Tuple`, `Paren`, `FieldAccessorFn`.
//! T7 adds control-flow: `If`, `Match`, `Try`, `Guard`, `Return`, `Let`,
//! `Var`, `Assign`, `Block` (block-as-expression wrapper).
//! T8 adds actor/lambda forms: `Lambda`, `Record`, `With`, `Ask`, `Send`,
//! `Spawn`, `Propagate`; full `Interp` with expression holes; support types
//! `FieldInit` and `LambdaParam`.
//! T10 adds `InnerFn` (D058: fn-as-expression).
//! T0 (Phase 6 OQ-E001 narrow exception) adds `AskTimeout` and extends
//! `Expr::Ask` with an optional `timeout: Option<AskTimeout>` field.

use crate::{decl::FnDecl, Block, Ident, Literal, Pattern, Span, Type};

// ── AskTimeout ────────────────────────────────────────────────────────────────

/// Optional timeout specifier on a `?> handler(args) [timeout <ms|never>]`
/// expression (Phase 6 T0, OQ-E001 narrow exception).
///
/// `#[non_exhaustive]` allows adding further variants in 0.2.0+ without a
/// breaking change for downstream crates that pattern-match on this type.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AskTimeout {
    /// `timeout never` — wait indefinitely (maps to Erlang `infinity`).
    Never,
    /// `timeout <expr>` — wait at most `expr` milliseconds.
    ///
    /// The expression must have type `Int`; the type-checker (Phase 4)
    /// enforces this via `T026 AskTimeoutNotInt`.
    Millis(Box<Expr>),
}

// ── RecordCtor ────────────────────────────────────────────────────────────────

/// The constructor reference in a record-construction expression.
///
/// T8 (Phase 4 task 11, §3.8): `Expr::Record::constructor` was `Ident`; it is
/// now this enum so that `Http.Response { ... }` can be expressed without a
/// separate `Expr::QualifiedRecord` variant.  The bare form is unchanged.
///
/// `#[non_exhaustive]` is NOT added here — the two arms are complete for 0.1.0
/// and the Phase 4 plan (§3.8 hard fence) explicitly states this is the only
/// record-constructor form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordCtor {
    /// A bare upper-case constructor name: `User { ... }`.
    Bare(Ident),
    /// A qualified constructor name: `Http.Response { ... }`.
    Qualified(QualifiedName),
}

// ── QualifiedName ─────────────────────────────────────────────────────────────

/// A dotted path whose first segment is an `UPPER_IDENT`.
///
/// Grammar §6.15: `UPPER_IDENT ( "." (LOWER_IDENT | UPPER_IDENT) )+`
///
/// Examples:
/// - `Io.println` → `segments: [Ident("Io"), Ident("println")]`
/// - `List.Map.get` → `segments: [Ident("List"), Ident("Map"), Ident("get")]`
///
/// The first segment is always an upper-case identifier; subsequent segments
/// may be upper- or lower-case.  Name resolution (Phase 3) distinguishes
/// module-qualified values from constructor paths; the parser stores them all
/// uniformly as [`QualifiedName`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    /// All segments, first being `UPPER_IDENT`, subsequent any case.
    pub segments: Vec<Ident>,
    /// Span covering the entire dotted path.
    pub span: Span,
}

// ── MatchArm ──────────────────────────────────────────────────────────────────

/// A single arm of a `match` expression (grammar §6.4 line 638).
///
/// Each arm consists of a pattern, an optional `when` guard, and a body
/// expression.  The guard is a plain expression (not a `Block`).
///
/// ```text
/// match x
///     Some v when v > 0 -> v + 1
///     _                 -> 0
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    /// The pattern to match against the scrutinee.
    pub pattern: Pattern,
    /// Optional guard expression introduced by `when`.
    pub guard: Option<Expr>,
    /// The body expression executed when the arm matches.
    pub body: Expr,
    /// Span covering the full arm from pattern to body end.
    pub span: Span,
}

// ── InterpPart ────────────────────────────────────────────────────────────────

/// A single segment inside an interpolated string `$"..."`.
///
/// In T3 only the `Text` variant is ever emitted.  `Expr` holes are parsed in
/// T8; the variant is declared here so that T8 can fill it in without a
/// breaking change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterpPart {
    /// A literal text segment inside the interpolated string.
    Text {
        /// Raw text exactly as it appears between the surrounding `$"` / `"` or
        /// adjacent `${…}` holes.
        raw: String,
        /// Source location of this text segment.
        span: Span,
    },
    /// An expression hole `${…}`.  Never emitted in T3; declared for T8.
    Expr {
        /// The interpolated expression.
        expr: Box<Expr>,
        /// Source location covering `${…}`.
        span: Span,
    },
}

// ── FieldInit ─────────────────────────────────────────────────────────────────

/// A single field initialiser in a record-construction or `with` expression.
///
/// D053: `value: None` is the shorthand form — `{ age }` is sugar for
/// `{ age = age }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInit {
    /// The field name (lower-case identifier).
    pub name: Ident,
    /// The value expression.  `None` means shorthand: `name` ≡ `name = name`.
    pub value: Option<Expr>,
    /// Span covering the entire `name [ = Expr ]` fragment.
    pub span: Span,
}

// ── LambdaParam ───────────────────────────────────────────────────────────────

/// A single parameter in a lambda expression (grammar §6.16, D052).
///
/// D052 allows a full `Pattern` as each lambda parameter, optionally with a
/// type annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LambdaParam {
    /// A bare pattern parameter: `x`, `_`, `(x, y)`, `Some v`, etc.
    Pattern(Pattern),
    /// An annotated parameter `(pat : Type)`.
    Annotated {
        /// The destructuring pattern.
        pat: Pattern,
        /// The type annotation.
        ty: Type,
        /// Span covering the full `( pat : Type )` form.
        span: Span,
    },
}

impl LambdaParam {
    /// Return the source span of this parameter.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Pattern(p) => p.span(),
            Self::Annotated { span, .. } => *span,
        }
    }
}

// ── BinOp ─────────────────────────────────────────────────────────────────────

/// Binary operators, ordered by Pratt precedence level (§4.5).
///
/// `BinOp::Pipe` is retained in this enum per the plan (§3.7) even though the
/// parser emits `Expr::Pipe` as a dedicated variant.  The two representations
/// are intentional: `Expr::Pipe` avoids a `BinOp::Pipe` arm in every match on
/// `Expr::Binary`, and `BinOp::Pipe` is kept for completeness per the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // Level 1 — kept per plan §3.7 even though parser emits Expr::Pipe.
    /// `|>` — pipe-forward (see also `Expr::Pipe`).
    #[allow(dead_code)]
    Pipe,

    // Level 2–3
    /// `||` — logical or (right-associative).
    Or,
    /// `&&` — logical and (right-associative).
    And,

    // Level 4–5
    /// `==` — structural equality (non-associative).
    Eq,
    /// `!=` — structural inequality (non-associative).
    Ne,
    /// `<` — less than (non-associative).
    Lt,
    /// `>` — greater than (non-associative).
    Gt,
    /// `<=` — less-or-equal (non-associative).
    Le,
    /// `>=` — greater-or-equal (non-associative).
    Ge,

    // Level 6
    /// `++` — text/list concatenation (right-associative).
    Concat,
    /// `::` — list cons (right-associative).
    Cons,

    // Level 7–9
    /// `+` — addition (left-associative).
    Add,
    /// `-` — subtraction (left-associative).
    Sub,
    /// `*` — multiplication (left-associative).
    Mul,
    /// `/` — division (left-associative).
    Div,
    /// `%` — modulo (left-associative).
    Mod,
    /// `^` — exponentiation (right-associative).
    Pow,
}

// ── UnaryOp ───────────────────────────────────────────────────────────────────

/// Unary operators.  Only negation exists in Ridge 0.1.0 (D044 removed `!`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Arithmetic negation (`-`).
    Neg,
}

// ── Expr ──────────────────────────────────────────────────────────────────────

/// An expression in Ridge source code.
///
/// T3 atoms are preserved; T6 adds the full Pratt surface (binary/unary
/// operators, function application, field access), plus the collection
/// literals and parenthesised forms required for T6's atom extensions.
///
/// Variants not yet implemented (T7–T8): `Lambda`, `InnerFn`, `Record`, `With`,
/// `If`, `Match`, `Try`, `Guard`, `Return`, `Let`, `Var`, `Assign`,
/// `Ask`, `Send`, `Spawn`, `Propagate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    // ── Atoms (T3) ────────────────────────────────────────────────────────────
    /// A literal value: integer, float, bool, or text.
    Literal(Literal),

    /// The unit literal `()`.
    Unit(Span),

    /// A lower-case or private (`_foo`) identifier.
    Ident(Ident),

    /// A qualified dotted name whose first segment is `UPPER_IDENT`.
    ///
    /// Grammar §6.15.  Name resolution distinguishes module paths from
    /// constructor paths; the parser makes no such distinction.
    Qualified(QualifiedName),

    /// An interpolated string `$"..."`.
    ///
    /// In T3 only zero-hole interpolations are handled (a single
    /// [`InterpPart::Text`] segment).  Full hole interpolation is T8.
    Interp {
        /// The segments of the interpolated string.
        parts: Vec<InterpPart>,
        /// Source location covering the full `$"…"`.
        span: Span,
    },

    // ── Collection literals and parenthesised forms (T6) ──────────────────────
    /// A list literal `[e₁, e₂, …]`.  May be empty (`[]`).
    List {
        /// The element expressions.
        elems: Vec<Self>,
        /// Source location covering `[…]`.
        span: Span,
    },

    /// A tuple literal `(e₁, e₂, …)`.  Always ≥ 2 elements.
    Tuple {
        /// The element expressions (at least 2).
        elems: Vec<Self>,
        /// Source location covering `(…)`.
        span: Span,
    },

    /// A parenthesised expression `(e)`.
    ///
    /// Preserved as a distinct variant (rather than being stripped) so that
    /// round-trip snapshot tests can detect spurious parentheses.
    Paren {
        /// The inner expression.
        inner: Box<Self>,
        /// Source location covering `(e)`.
        span: Span,
    },

    /// A field-accessor shorthand `(.name)` used as a first-class function.
    ///
    /// Grammar §6.14.  Commonly seen in pipe chains:
    /// `users |> List.map (.name)`
    FieldAccessorFn {
        /// The field being accessed.
        field: Ident,
        /// Source location covering `(.name)`.
        span: Span,
    },

    // ── Operators (T6) ───────────────────────────────────────────────────────
    /// A binary operator application.  See [`BinOp`] for the full operator set.
    Binary {
        /// The operator.
        op: BinOp,
        /// Left-hand operand.
        lhs: Box<Self>,
        /// Right-hand operand.
        rhs: Box<Self>,
        /// Span covering the full binary expression.
        span: Span,
    },

    /// A unary operator application.  Only negation (`-`) exists in 0.1.0.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand expression.
        expr: Box<Self>,
        /// Span covering the full unary expression.
        span: Span,
    },

    // ── Application & suffix (T6) ─────────────────────────────────────────────
    /// Juxtaposition-as-call: `f x y z` → `Call { callee: f, args: [x, y, z] }`.
    ///
    /// The shape is **flat** (multiple args in one `Call`) — not nested
    /// `Call(Call(f,[x]),[y])`.
    Call {
        /// The function being called.
        callee: Box<Self>,
        /// The argument expressions.
        args: Vec<Self>,
        /// Span covering the full call expression.
        span: Span,
    },

    /// Field-access suffix: `base.field`.  Chains left-recursively:
    /// `a.b.c` → `FieldAccess { base: FieldAccess { base: a, field: b }, field: c }`.
    FieldAccess {
        /// The base expression being accessed.
        base: Box<Self>,
        /// The field name.
        field: Ident,
        /// Span covering the full field-access expression.
        span: Span,
    },

    /// Pipe-forward operator `lhs |> rhs`.
    ///
    /// A dedicated variant (rather than `Binary { op: BinOp::Pipe }`) so that
    /// the T8 desugaring pass can detect pipe nodes without inspecting `BinOp`.
    /// `BinOp::Pipe` is still present in the enum per plan §3.7.
    Pipe {
        /// The left-hand (input) expression.
        lhs: Box<Self>,
        /// The right-hand (function) expression.
        rhs: Box<Self>,
        /// Span covering the full pipe expression.
        span: Span,
    },

    // ── Lambda / actor forms (T8) ─────────────────────────────────────────────
    /// An anonymous function `fn Param+ -> Body` (grammar §6.16, D052).
    ///
    /// Each parameter is a `LambdaParam` — a full pattern, optionally annotated
    /// with a type (`(pat : Type)`).
    Lambda {
        /// The parameter list.
        params: Vec<LambdaParam>,
        /// The body expression.
        body: Box<Self>,
        /// Span covering the full lambda expression.
        span: Span,
    },

    /// An inner function expression `fn name params = body` (D058, grammar §6.17).
    ///
    /// Disambiguated from a lambda by the presence of a lower-case name
    /// immediately after `fn`.  The `FnDecl` produced has `vis: Private` and
    /// `doc: None`.
    InnerFn {
        /// The underlying function declaration.
        decl: Box<FnDecl>,
        /// Span covering the whole inner-fn expression.
        span: Span,
    },

    /// A record-construction expression `Constructor { field = val, … }` (D051,
    /// grammar §6.18).
    ///
    /// Shorthand fields (D053) have `FieldInit::value = None`.
    ///
    /// T8 (Phase 4 §3.8): `constructor` is now [`RecordCtor`] to support both
    /// bare (`User { ... }`) and qualified (`Http.Response { ... }`) forms.
    Record {
        /// The constructor — bare or qualified.
        constructor: RecordCtor,
        /// The field initialisers.
        fields: Vec<FieldInit>,
        /// Span covering the full record expression.
        span: Span,
    },

    /// A constructor-less inline record literal `{ field = val, … }`.
    ///
    /// Parsed when a `{` in expression position is followed by
    /// `LOWER_IDENT =`, distinguishing it from a block.  The empty form `{}`
    /// is also parsed as `RecordLit` (zero-field anonymous record value).
    ///
    /// Shorthand fields (`{ x }`) are represented as `FieldInit { value: None }`.
    RecordLit {
        /// The field initialisers (may be empty for `{}`).
        fields: Vec<FieldInit>,
        /// Span covering the full `{ … }` form.
        span: Span,
    },

    /// A functional update `base with { field = val, … }` (D055, grammar §6.18,
    /// Pratt level 5.5).
    ///
    /// Left-associative: `u with {a=1} with {b=2}` = `With { With { u, [a=1] }, [b=2] }`.
    With {
        /// The base record expression.
        base: Box<Self>,
        /// The field updates.
        fields: Vec<FieldInit>,
        /// Span covering the full `with` expression.
        span: Span,
    },

    /// An ask operation `handle ?> message arg* [timeout <ms|never>]`
    /// (D045, grammar §6.20; timeout postfix added by Phase 6 T0, OQ-E001).
    ///
    /// Postfix at Pratt level 12 — single-site per D068.
    Ask {
        /// The actor handle expression.
        handle: Box<Self>,
        /// The message name.
        message: Ident,
        /// The message arguments.
        args: Vec<Self>,
        /// Optional timeout specifier — `None` means "use the runtime default
        /// (5000 ms per resolved OQ-E001)".
        timeout: Option<AskTimeout>,
        /// Span covering the full ask expression (including any `timeout` postfix).
        span: Span,
    },

    /// A send operation `handle ! message` (D044, grammar §6.18).
    ///
    /// Postfix at Pratt level 12 — single-site per D068.
    Send {
        /// The actor handle expression.
        handle: Box<Self>,
        /// The message expression.
        message: Box<Self>,
        /// Span covering the full send expression.
        span: Span,
    },

    /// A spawn expression `spawn Actor arg*` (D061, grammar §6.19).
    Spawn {
        /// The actor type name.
        actor: Ident,
        /// The constructor arguments.
        args: Vec<Self>,
        /// Span covering the full spawn expression.
        span: Span,
    },

    /// A propagate expression `expr?` (grammar §6.21).
    ///
    /// Postfix at Pratt level 12 — single-site per D068.
    Propagate {
        /// The inner expression whose error is propagated.
        inner: Box<Self>,
        /// Span covering `expr?`.
        span: Span,
    },

    // ── Control flow (T7, grammar §§6.3–6.7) ─────────────────────────────────
    /// `if <cond> then <then-branch> [else <else-branch>]`
    ///
    /// Both branches can be either a single expression or a multi-statement
    /// `Block` wrapped as `Expr::Block`.
    If {
        /// The condition expression.
        cond: Box<Self>,
        /// The then-branch expression or block.
        then_branch: Box<Self>,
        /// The optional else-branch expression or block.
        else_branch: Option<Box<Self>>,
        /// Span covering the full if expression.
        span: Span,
    },

    /// `match <scrutinee> INDENT arms DEDENT` (grammar §6.4).
    Match {
        /// The scrutinee expression.
        scrutinee: Box<Self>,
        /// The match arms in source order.
        arms: Vec<MatchArm>,
        /// Span covering the full match expression.
        span: Span,
    },

    /// `try INDENT block DEDENT` — captures a do-block for error propagation
    /// (D060, grammar §6.5).
    Try {
        /// The body block.
        block: Block,
        /// Span covering the full try expression.
        span: Span,
    },

    /// `guard <cond> else <else-branch>` — the else branch is always a
    /// `Block`.  For single-line forms the block has one stmt (D066,
    /// grammar §6.6).
    Guard {
        /// The guard condition.
        cond: Box<Self>,
        /// The else block executed when the condition is false.
        else_branch: Block,
        /// Span covering the full guard expression.
        span: Span,
    },

    /// `return <value>` — early return expression (grammar §6.7).
    ///
    /// `Return` with no following token emits `Return { value: Unit }`.
    Return {
        /// The value to return.
        value: Box<Self>,
        /// Span covering the full return expression.
        span: Span,
    },

    // ── Bindings (T7, grammar §6.1–6.2) ──────────────────────────────────────
    /// `let <pat> [: <ty>] = <value>` — immutable binding (D052, grammar §6.1).
    ///
    /// The pattern may be a full `Pattern` (destructuring allowed per D052).
    Let {
        /// The binding pattern.
        pat: Pattern,
        /// Optional type annotation.
        ty: Option<Type>,
        /// The bound value expression.
        value: Box<Self>,
        /// Span covering the full let binding.
        span: Span,
    },

    /// `var <name> [: <ty>] = <value>` — mutable binding (grammar §6.1).
    ///
    /// Unlike `Let`, the left-hand side is restricted to a single `Ident`
    /// (not a full pattern).
    Var {
        /// The binding name.
        name: Ident,
        /// Optional type annotation.
        ty: Option<Type>,
        /// The initial value expression.
        value: Box<Self>,
        /// Span covering the full var binding.
        span: Span,
    },

    /// `<target> <- <value>` — assignment to a mutable `var` (grammar §6.2).
    ///
    /// The parser accepts any expression as `target` for syntactic simplicity;
    /// the type checker (Phase 4) will enforce that it resolves to a mutable
    /// binding.
    Assign {
        /// The assignment target expression.
        target: Box<Self>,
        /// The new value expression.
        value: Box<Self>,
        /// Span covering the full assignment expression.
        span: Span,
    },

    // ── Block-as-expression (T7 plan-extension) ───────────────────────────────
    /// A multi-statement block used as an expression (e.g., as the branch of
    /// an `if` or `match` arm body).
    ///
    /// **Plan extension (T7):** This variant is not in the original §3.7 enum
    /// listing, but is required because `If::then_branch` and `If::else_branch`
    /// are `Box<Expr>`, and multi-statement blocks must be representable as
    /// expressions.  The plan's `Block` struct (§3.6) cannot appear directly
    /// in an `Expr` field without a wrapper.  This variant acts as that wrapper.
    /// Any downstream phase can pattern-match on `Expr::Block` to detect
    /// block positions.
    Block(Block),
}

impl Expr {
    /// Return the source span of this expression.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Literal(lit) => lit.span(),
            Self::Unit(span)
            | Self::Interp { span, .. }
            | Self::List { span, .. }
            | Self::Tuple { span, .. }
            | Self::Paren { span, .. }
            | Self::FieldAccessorFn { span, .. }
            | Self::Binary { span, .. }
            | Self::Unary { span, .. }
            | Self::Call { span, .. }
            | Self::FieldAccess { span, .. }
            | Self::Pipe { span, .. }
            | Self::Lambda { span, .. }
            | Self::InnerFn { span, .. }
            | Self::Record { span, .. }
            | Self::RecordLit { span, .. }
            | Self::With { span, .. }
            | Self::Ask { span, .. }
            | Self::Send { span, .. }
            | Self::Spawn { span, .. }
            | Self::Propagate { span, .. }
            | Self::If { span, .. }
            | Self::Match { span, .. }
            | Self::Try { span, .. }
            | Self::Guard { span, .. }
            | Self::Return { span, .. }
            | Self::Let { span, .. }
            | Self::Var { span, .. }
            | Self::Assign { span, .. } => *span,
            Self::Ident(id) => id.span,
            Self::Qualified(q) => q.span,
            Self::Block(b) => b.span,
        }
    }
}
