//! Pattern nodes (grammar §7).
//!
//! Patterns appear in `let` bindings, `match` arms, and lambda parameters
//! (D052).  All pattern forms carry a [`Span`] for diagnostics.

use crate::{Ident, Literal, Span};

// ── FieldPattern ──────────────────────────────────────────────────────────────

/// A single field binding inside a record pattern (grammar §7.5, D053).
///
/// ```text
/// User { name = n, age }
///         ^^^^^^^^  ^^^
///         explicit  shorthand (D053): binds `age` to a local named `age`
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPattern {
    /// The field name as it appears in the source.
    pub name: Ident,
    /// `Some(pat)` for an explicit binding `name = pat`; `None` for the
    /// D053 shorthand form where the field is bound to a variable of the same
    /// name.
    pub pattern: Option<Pattern>,
    /// Span of this field entry.
    pub span: Span,
}

// ── Pattern ───────────────────────────────────────────────────────────────────

/// A match pattern in Ridge source code.
///
/// Grammar §7 productions:
///
/// | Variant | Grammar form | Example |
/// |---------|--------------|---------|
/// | `Wildcard` | `_` | `_` |
/// | `Literal` | literal | `42`, `"hi"`, `true` |
/// | `Var` | lower-ident | `x`, `name` |
/// | `Constructor` | `UPPER_IDENT [ { fields } ] [ args ]` | `Some x`, `User { name }` |
/// | `Tuple` | `(p, p, …)` ≥2 | `(x, y)` |
/// | `Cons` | `head :: tail` | `x :: xs` |
/// | `As` | `name @ inner` | `admin @ User { role = Admin }` |
/// | `Paren` | `(p)` | `(x)` |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// The wildcard `_` — matches anything and binds nothing.
    Wildcard {
        /// Source location of the `_` token.
        span: Span,
    },

    /// A literal value pattern: integer, float, boolean, or text.
    Literal {
        /// The literal value to match against.
        lit: Literal,
        /// Source location of the literal.
        span: Span,
    },

    /// A variable pattern: binds the matched value to a lower-case name.
    ///
    /// `LOWER_IDENT` — includes private `_foo` identifiers (emitted as
    /// `Token::LowerIdent`).
    Var {
        /// The binding name.
        name: Ident,
        /// Source location.
        span: Span,
    },

    /// A constructor pattern (grammar §7.4, D051).
    ///
    /// Covers both positional form (`Some x`) and record-body form
    /// (`User { name }`).  A constructor name (`UPPER_IDENT`) is always
    /// required; bare `{ … }` is rejected with `P018 BareRecordPattern`.
    Constructor {
        /// Constructor name (upper-case identifier).
        name: Ident,
        /// Record-body field patterns.  `Some(…)` iff the `{ … }` form was
        /// used; `None` for the positional form.
        fields: Option<Vec<FieldPattern>>,
        /// Zero or more positional sub-patterns (only when `fields` is `None`).
        args: Vec<Self>,
        /// Span covering the entire constructor pattern.
        span: Span,
    },

    /// A tuple pattern of at least 2 elements.
    Tuple {
        /// The element patterns.
        elems: Vec<Self>,
        /// Source location covering the full tuple pattern.
        span: Span,
    },

    /// A cons (list) pattern `head :: tail`.
    ///
    /// Right-associative: `a :: b :: rest` →
    /// `Cons { head: a, tail: Cons { head: b, tail: rest } }`.
    Cons {
        /// The head pattern (left of `::`).
        head: Box<Self>,
        /// The tail pattern (right of `::`).
        tail: Box<Self>,
        /// Span covering the full cons pattern.
        span: Span,
    },

    /// An alias pattern `name @ inner`.
    ///
    /// Binds the whole matched value to `name` AND matches `inner`.
    As {
        /// The binding name.
        name: Ident,
        /// The inner pattern to also match against.
        inner: Box<Self>,
        /// Span covering the full alias pattern.
        span: Span,
    },

    /// A parenthesised pattern `(p)` — preserves grouping for round-tripping.
    Paren {
        /// The inner pattern.
        inner: Box<Self>,
        /// Source location covering the parentheses.
        span: Span,
    },

    /// The empty list pattern `[]`.
    ///
    /// Matches only an empty list.  The span covers the `[` `]` token pair.
    ListNil {
        /// Source location of the `[` `]` pair.
        span: Span,
    },
}

impl Pattern {
    /// Return the source span of this pattern.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Wildcard { span }
            | Self::Literal { span, .. }
            | Self::Var { span, .. }
            | Self::Constructor { span, .. }
            | Self::Tuple { span, .. }
            | Self::Cons { span, .. }
            | Self::As { span, .. }
            | Self::Paren { span, .. }
            | Self::ListNil { span } => *span,
        }
    }
}
