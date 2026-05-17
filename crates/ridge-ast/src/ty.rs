//! Type AST nodes (grammar §3.4).
//!
//! The module is named `ty` (not `type`) to avoid colliding with the Rust
//! keyword `type`.  Re-exported from the crate root as `ridge_ast::Type`,
//! `ridge_ast::FnType`.

use crate::{Capability, Ident, PrimitiveType, Span};

// ── Type ──────────────────────────────────────────────────────────────────────

/// A type expression in Ridge source code (grammar §3.4).
///
/// Every variant carries a [`Span`] so that downstream diagnostics can always
/// point at the exact byte range.
///
/// # Variants
///
/// - [`Type::Primitive`] — a built-in scalar type such as `Int` or `Bool`.
/// - [`Type::Named`] — an `UPPER_IDENT` with no type arguments (e.g. `User`).
/// - [`Type::App`] — a type constructor applied to one or more arguments
///   (e.g. `Option Int`, `Map k v`).  **Flat** per OQ-P003: `Map k v` is
///   `App { head: Map, args: [k, v] }`, never curried.
/// - [`Type::Tuple`] — a tuple type with ≥ 2 elements (e.g. `(Int, Text)`).
/// - [`Type::List`] — the `[a]` sugar for `List a`.
/// - [`Type::Fn`] — a function type, with or without capability annotations.
/// - [`Type::Paren`] — a single type wrapped in parentheses, preserved for
///   round-trip fidelity (e.g. `(Int)` → `Paren { inner: Int }`).
/// - [`Type::Var`] — a `LOWER_IDENT` type variable (e.g. `a`, `k`, `v`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// A built-in primitive type identified by its `UPPER_IDENT` spelling
    /// (e.g. `Int`, `Float`, `Bool`, `Text`, `Unit`, `Timestamp`).
    Primitive {
        /// The concrete primitive kind.
        name: PrimitiveType,
        /// Source location.
        span: Span,
    },

    /// A named type constructor with no applied arguments (e.g. `User`,
    /// `Error`).  If arguments follow, the caller promotes this to
    /// [`Type::App`].
    Named {
        /// The type constructor name.
        name: Ident,
        /// Source location.
        span: Span,
    },

    /// A type constructor applied to one or more arguments.
    ///
    /// Grammar §3.4 line 424: `TypeApp ::= UPPER_IDENT { TypeAtom }`.
    /// Per OQ-P003 the application is **flat** — `Map k v` produces
    /// `App { head: Map, args: [k, v] }`, not nested `App(App(Map, k), v)`.
    App {
        /// The type constructor being applied.
        head: Ident,
        /// The type arguments.
        args: Vec<Self>,
        /// Span covering the full application.
        span: Span,
    },

    /// A tuple type with at least two elements (grammar §3.4 line 428).
    ///
    /// `(Int, Text, Float)` → `Tuple { elems: [Int, Text, Float], span }`.
    Tuple {
        /// The element types.
        elems: Vec<Self>,
        /// Span covering the full tuple type.
        span: Span,
    },

    /// The list sugar `[a]`, equivalent to `List a` (grammar §3.4 line 432).
    List {
        /// The element type.
        elem: Box<Self>,
        /// Span covering `[a]`.
        span: Span,
    },

    /// A function type, either plain (`Int -> Text`) or capability-annotated
    /// (`fn io Text -> Unit`).  Both forms are represented by [`FnType`].
    Fn {
        /// The function type payload (params, capabilities, return type).
        fn_ty: FnType,
        /// Span covering the full function type.
        span: Span,
    },

    /// A single type wrapped in parentheses, preserved for round-trip
    /// snapshot fidelity.  `(Int)` → `Paren { inner: Int }`.
    Paren {
        /// The inner type.
        inner: Box<Self>,
        /// Source location covering the parentheses.
        span: Span,
    },

    /// A `LOWER_IDENT` type variable, used inside generic type declarations
    /// (e.g. `a`, `k`, `v`).  Grammar §3.4 line 435.
    Var {
        /// The type variable name.
        name: Ident,
        /// Source location.
        span: Span,
    },
}

impl Type {
    /// Return the source span of this type expression.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Primitive { span, .. }
            | Self::Named { span, .. }
            | Self::App { span, .. }
            | Self::Tuple { span, .. }
            | Self::List { span, .. }
            | Self::Fn { span, .. }
            | Self::Paren { span, .. }
            | Self::Var { span, .. } => *span,
        }
    }
}

// ── FnType ────────────────────────────────────────────────────────────────────

/// The payload of a function type (grammar §3.4 `FunctionType`).
///
/// Represents both plain function types (`Int -> Text`) and
/// capability-annotated function types (`fn io Text -> Unit`).
///
/// - **Plain** (`PlainFunctionType`): `caps` is empty, `params` has exactly
///   one element (the left-hand side of `->`) — though the parser wraps
///   nested arrows in the right-hand `ret` to achieve right-associativity.
/// - **Cap** (`CapFunctionType`): `caps` is non-empty, `params` holds all
///   `TypeAtom`s between the capability list and the `->`.
///
/// In both cases `ret` is the right-hand type (recursively parsed to achieve
/// right-associativity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnType {
    /// Capability annotations (empty for plain function types).
    pub caps: Vec<Capability>,
    /// Parameter type(s).  At least one element; can be more for
    /// `CapFunctionType` (e.g. `fn io Text Int -> Unit`).
    pub params: Vec<Type>,
    /// Return type.  Boxed because `FnType` is used inside `Type`, which
    /// would otherwise be infinitely sized.
    pub ret: Box<Type>,
    /// Source span covering the entire function type expression.
    pub span: Span,
}
