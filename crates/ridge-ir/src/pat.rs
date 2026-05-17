//! IR pattern nodes.
// OQ-IR003: IrPat is #[non_exhaustive] — see expr.rs for rationale.

use crate::lit::IrLit;
use crate::symbol::SymbolRef;
use ridge_ast::Span;

/// A pattern in the Ridge Core IR.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IrPat {
    /// Wildcard pattern `_`.
    Wild {
        /// Source span.
        span: Span,
    },
    /// Literal pattern.
    Lit {
        /// The literal value to match.
        value: IrLit,
        /// Source span.
        span: Span,
    },
    // OQ-L009: IrPat::Bind collapses Var and As patterns into one variant;
    // inner=None for plain variable bindings, inner=Some for `as`-patterns.
    /// Variable binding.  `as` patterns lower to `IrPat::Bind { name, inner }`.
    Bind {
        /// The name being bound.
        name: String,
        /// Optional inner pattern for `as`-patterns (`p as name`).
        inner: Option<Box<Self>>,
        /// Source span.
        span: Span,
    },
    /// Constructor pattern (record-auto or union-variant).
    ///
    /// `User { name = n, age }` lowers to
    /// `Ctor { sym, fields: [(name, Bind n), (age, Bind age)] }`.
    /// Shorthand fields (`{ age }`, D053) are pre-expanded to
    /// `(age, Bind age)` during lowering — the IR has no shorthand form.
    Ctor {
        /// The constructor symbol.
        sym: SymbolRef,
        /// Named field patterns (record-style constructors).
        fields: Vec<(String, Self)>,
        /// Positional argument patterns (union-variant positional payloads).
        args: Vec<Self>,
        /// Source span.
        span: Span,
    },
    /// Tuple pattern.
    Tuple {
        /// The element patterns, in source order.
        elems: Vec<Self>,
        /// Source span.
        span: Span,
    },
    /// Cons-cell pattern (`head :: tail`).
    Cons {
        /// The head element pattern.
        head: Box<Self>,
        /// The tail pattern.
        tail: Box<Self>,
        /// Source span.
        span: Span,
    },
    /// Empty-list pattern `[]`.
    ///
    /// Matches only the empty list (nil).
    Nil {
        /// Source span.
        span: Span,
    },
}
