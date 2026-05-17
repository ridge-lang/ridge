//! Declaration AST nodes (T10).
//!
//! Contains all top-level declaration types:
//! - [`ImportDecl`] / [`ModulePath`] — grammar §2.2
//! - [`ConstDecl`]  — grammar §2.4
//! - [`TypeDecl`] / [`TypeBody`] / [`RecordTypeBody`] / [`FieldDecl`] /
//!   [`UnionTypeBody`] / [`Constructor`]  — grammar §3
//! - [`FnDecl`] / [`Param`]  — grammar §4
//! - [`ActorDecl`] / [`ActorMember`] / [`StateDecl`] / [`InitDecl`] /
//!   [`OnHandler`]  — grammar §5

use crate::{Block, Capability, DocComment, Expr, Ident, Span, Type, Visibility};

// ── FnBody ────────────────────────────────────────────────────────────────────

/// The body of a function declaration (grammar §4.1 / Phase 7 T2).
///
/// Most functions have an ordinary expression body (`Body::Expr`).  Functions
/// in the Ridge standard library that delegate directly to a BEAM built-in are
/// annotated with `@ffi(module, name, arity)` and carry `Body::Ffi` instead —
/// the expression body is **omitted** from source.
///
/// Semantic checking (T3+) is responsible for rejecting `Body::Ffi` outside the
/// `crates/ridge-stdlib/` crate path (error `T003 FfiOutsideStdlib`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// A normal expression body, written as `= <Expr>` in source.
    Expr(Expr),
    /// An FFI passthrough declared with `@ffi("module", "name", arity)`.
    ///
    /// The function header (name, params, return type) is fully present in the
    /// AST; only the expression body is replaced by this BEAM-level bridge.
    Ffi {
        /// The BEAM/Erlang module name (e.g. `"erlang"`).
        module: String,
        /// The function name inside that module (e.g. `"+"`).
        name: String,
        /// The expected arity (must match the Ridge param count — checked in T3).
        arity: u32,
    },
}

// ── ImportDecl / ModulePath ───────────────────────────────────────────────────

/// An `import` declaration (grammar §2.2).
///
/// Examples:
/// ```text
/// import std.list as List
/// import std.map (get, insert)
/// import std.fs as Fs
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecl {
    /// The module path, e.g. `["std", "list"]`.
    pub path: ModulePath,
    /// Optional `as Alias` rename.
    pub alias: Option<Ident>,
    /// Optional explicit import list `(name, …)`.
    ///
    /// - `None` — whole-module import (no parentheses).
    /// - `Some([])` — empty `()` (no specific items imported, unusual).
    /// - `Some([name, …])` — explicit item list.
    pub items: Option<Vec<Ident>>,
    /// Span covering the entire `import …` declaration.
    pub span: Span,
    /// Attached doc comment (set to `None` in T10; T11 fills this).
    // TODO(T11): doc-comment attachment — set once parse_module peels DocComment tokens.
    pub doc: Option<DocComment>,
}

/// A dot-separated module path (grammar §2.2 line 317).
///
/// Each segment is an `Ident` whose text may be lower- or upper-case.
///
/// Examples:
/// - `std.list` → `segments: [Ident("std"), Ident("list")]`
/// - `acme.infra.Postgres` → `segments: [Ident("acme"), Ident("infra"), Ident("Postgres")]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePath {
    /// All path segments in source order.
    pub segments: Vec<Ident>,
    /// Span covering the full dot-separated path.
    pub span: Span,
}

// ── ConstDecl ─────────────────────────────────────────────────────────────────

/// A constant declaration (grammar §2.4 line 340).
///
/// ```text
/// const maxRetries: Int = 3
/// pub const PI: Float = 3.14
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstDecl {
    /// Visibility modifier (default: `Private`).
    pub vis: Visibility,
    /// The constant name (`LOWER_IDENT`).
    pub name: Ident,
    /// The required type annotation.
    pub ty: Type,
    /// The initialising expression.
    pub value: Expr,
    /// Span covering the whole declaration.
    pub span: Span,
    /// Attached doc comment (set to `None` in T10; T11 fills this).
    // TODO(T11): doc-comment attachment.
    pub doc: Option<DocComment>,
}

// ── TypeDecl / TypeBody ───────────────────────────────────────────────────────

/// A type declaration (grammar §3.1 line 352).
///
/// ```text
/// type Level = Info | Warn | Error
/// type User  = { name: Text, age: Int }
/// type Option a = None | Some a
/// type UserId = Text
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    /// Visibility modifier (default: `Private`).
    pub vis: Visibility,
    /// The declared type name (`UPPER_IDENT`).
    pub name: Ident,
    /// Type parameters (lowercase type variables).
    pub params: Vec<Ident>,
    /// The type body: record, union, or alias.
    pub body: TypeBody,
    /// Span covering the whole declaration.
    pub span: Span,
    /// Attached doc comment (set to `None` in T10; T11 fills this).
    // TODO(T11): doc-comment attachment.
    pub doc: Option<DocComment>,
}

/// The right-hand side of a type declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeBody {
    /// Record type: `{ field: Type, … }`.
    Record(RecordTypeBody),
    /// Union type: `| A | B …` or `A | B …` (D054: leading `|` optional).
    Union(UnionTypeBody),
    /// Type alias: a bare type expression.
    Alias(Type),
}

/// The body of a record type (grammar §3.2 line 363).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordTypeBody {
    /// The record's field declarations (at least one; trailing comma allowed).
    pub fields: Vec<FieldDecl>,
    /// Span covering `{ … }`.
    pub span: Span,
}

/// A single field declaration in a record type (grammar §3.2 line 365).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    /// The field name (`LOWER_IDENT`).
    pub name: Ident,
    /// The field's type.
    pub ty: Type,
    /// Span covering `name: Type`.
    pub span: Span,
}

/// The body of a union type (grammar §3.3 line 376).
///
/// D054: leading `|` is optional; trailing `|` is prohibited; minimum one alternative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionTypeBody {
    /// One or more constructor alternatives.
    pub alternatives: Vec<Constructor>,
    /// Span covering the entire union body.
    pub span: Span,
}

/// A single union constructor (grammar §3.3 line 378).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constructor {
    /// Positional constructor: `Name Type*`.
    Positional {
        /// Constructor name (`UPPER_IDENT`).
        name: Ident,
        /// Zero or more positional type arguments.
        args: Vec<Type>,
        /// Span covering the constructor.
        span: Span,
    },
    /// Record constructor: `Name { field: Type, … }`.
    Record {
        /// Constructor name (`UPPER_IDENT`).
        name: Ident,
        /// The inline record body.
        body: RecordTypeBody,
        /// Span covering the constructor.
        span: Span,
    },
}

// ── FnDecl / Param ────────────────────────────────────────────────────────────

/// A function declaration (grammar §4.1 line 450).
///
/// ```text
/// fn greet (name: Text) -> Text = $"Hello, ${name}"
/// pub fn io log (msg: Text) -> Unit = Io.println msg
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnDecl {
    /// Visibility modifier (default: `Private`).
    pub vis: Visibility,
    /// Capability annotations.
    pub caps: Vec<Capability>,
    /// The function name (`LOWER_IDENT` or `PRIV_IDENT`).
    pub name: Ident,
    /// The parameter list (D037: only bare or annotated; no pattern params).
    pub params: Vec<Param>,
    /// Optional return type after `->`.
    pub ret: Option<Type>,
    /// The function body — either a normal expression or an FFI passthrough.
    ///
    /// `Body::Expr` is the common case (all non-stdlib functions).
    /// `Body::Ffi` is produced when an `@ffi(...)` attribute precedes the decl.
    pub body: Body,
    /// Span covering the whole declaration.
    pub span: Span,
    /// Attached doc comment (set to `None` in T10; T11 fills this).
    // TODO(T11): doc-comment attachment.
    pub doc: Option<DocComment>,
}

/// A top-level function parameter (grammar §4.1 line 459, D037).
///
/// D037 restricts top-level `Param` to bare names or annotated names only.
/// Full patterns (tuples, constructors) are **not** allowed; use a `let`
/// binding in the body instead.  See `parse_param_top` (P012 on violation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Param {
    /// A bare identifier parameter: `x`, `_foo`.
    Bare(Ident),
    /// An annotated parameter: `(name: Type)`.
    Annotated {
        /// The parameter name.
        name: Ident,
        /// The type annotation.
        ty: Type,
        /// Span covering `( name : Type )`.
        span: Span,
    },
}

impl Param {
    /// Return the source span of this parameter.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Bare(id) => id.span,
            Self::Annotated { span, .. } => *span,
        }
    }
}

// ── ActorDecl / ActorMember ───────────────────────────────────────────────────

/// An actor declaration (grammar §5.1 line 487).
///
/// ```text
/// actor Counter =
///     state count: Int = 0
///     on increment = count <- count + 1
///     on get -> Int = count
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorDecl {
    /// Visibility modifier (default: `Private`).
    pub vis: Visibility,
    /// The actor name (`UPPER_IDENT`).
    pub name: Ident,
    /// The actor's member declarations.
    pub members: Vec<ActorMember>,
    /// Span covering the whole declaration.
    pub span: Span,
    /// Attached doc comment (set to `None` in T10; T11 fills this).
    // TODO(T11): doc-comment attachment.
    pub doc: Option<DocComment>,
}

/// A member declaration inside an actor body (grammar §5.1 line 493).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorMember {
    /// A `state` field declaration.
    State(StateDecl),
    /// An `init` block (at most one per actor — semantic check, not grammar).
    Init(InitDecl),
    /// An `on` message handler.
    On(OnHandler),
}

/// A state field declaration (grammar §5.2 line 500).
///
/// ```text
/// state count: Int = 0
/// state capacity: Int          -- no default; init block initialises it (D061)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDecl {
    /// The state field name (`LOWER_IDENT`).
    pub name: Ident,
    /// The state field's type.
    pub ty: Type,
    /// Optional default expression.
    ///
    /// `None` is allowed when an `init` block is present (OQ-P006 — the parser
    /// does not enforce the "either default or init" rule; that is Phase 3).
    pub default: Option<Expr>,
    /// Span covering the `state` declaration.
    pub span: Span,
}

/// An initialisation block (grammar §5.3 line 511, D061).
///
/// ```text
/// init (cap: Int) (rate: Float) =
///     capacity   <- cap
///     tokens     <- Float.fromInt cap
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitDecl {
    /// Capability annotations.
    pub caps: Vec<Capability>,
    /// The parameter list (same Param restriction as top-level fn).
    pub params: Vec<Param>,
    /// The initialisation block body.
    pub body: Block,
    /// Span covering the whole `init` declaration.
    pub span: Span,
}

/// A message handler declaration (grammar §5.4 line 522).
///
/// ```text
/// on increment = count <- count + 1
/// on get -> Int = count
/// on io allow () -> Bool = …
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnHandler {
    /// Capability annotations.
    pub caps: Vec<Capability>,
    /// The handler name (`LOWER_IDENT`).
    pub name: Ident,
    /// The parameter list.
    pub params: Vec<Param>,
    /// Optional return type.
    pub ret: Option<Type>,
    /// The handler body expression.
    pub body: Expr,
    /// Span covering the whole handler.
    pub span: Span,
    /// Attached doc comment (set to `None` in T10; T11 fills this).
    // TODO(T11): doc-comment attachment.
    pub doc: Option<DocComment>,
}
