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

// ── ListPatElem ───────────────────────────────────────────────────────────────

/// A single element inside a bracketed list pattern `[…]` (D258).
///
/// ```text
/// [a, b, rest @ ..]
///  ^  ^  ^^^^^^^^^^
///  |  |  rest element with binding
///  |  Elem
///  Elem
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListPatElem {
    /// A normal pattern element.
    Elem(Pattern),
    /// A rest position `..` (prefix rest only in 0.2.8; suffix/middle deferred
    /// to the next cut).  The optional `bind` is the name bound to the
    /// remaining list tail: `rest @ ..` binds `rest`, plain `..` does not.
    Rest {
        /// The binding name for the tail (`rest @ ..`), or `None` (`..`).
        bind: Option<Ident>,
        /// Span of this rest element (covers `IDENT @ ..` or just `..`).
        span: Span,
    },
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
        /// Whether a trailing `..` was present in the record-body form (D259).
        ///
        /// Only meaningful when `fields` is `Some`.  When `true`, the pattern
        /// matches any value of the named record type that carries at least the
        /// named fields, ignoring any additional fields.
        has_rest: bool,
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

    /// A bracketed list pattern `[a, b, c]` or `[a, rest @ ..]` (D258).
    ///
    /// Preserves the original bracket syntax for faithful round-tripping by
    /// `ridge-fmt`.  All downstream phases (exhaustiveness, type inference,
    /// lowering) desugar this to the equivalent `Cons`/`ListNil`/`Wildcard`
    /// tree via [`Pattern::desugar_list`] rather than operating on this node
    /// directly.
    ///
    /// Invariants enforced by the parser:
    /// - At most one `ListPatElem::Rest` element.
    /// - If a `Rest` is present it must be the last element (prefix rest only
    ///   in 0.2.8; suffix/middle support is deferred to the next cut).
    List {
        /// The element patterns, in source order.
        elements: Vec<ListPatElem>,
        /// Span covering the full `[…]` construct.
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
            | Self::ListNil { span }
            | Self::List { span, .. } => *span,
        }
    }

    /// Desugar a `Pattern::List` to the equivalent `Cons`/`ListNil`/`Wildcard`
    /// tree (D258).
    ///
    /// This shared helper is called by type inference, exhaustiveness checking,
    /// and pattern lowering so that those passes reuse the existing
    /// `Cons`/`ListNil` machinery without change.
    ///
    /// # Desugar rules
    ///
    /// | Pattern | Desugars to |
    /// |---------|-------------|
    /// | `[]` (empty List) | `ListNil` |
    /// | `[e0, …, en]` (no Rest) | `Cons(e0, Cons(…, Cons(en, ListNil)))` |
    /// | `[e0, …, ek, ..]` (prefix rest, no bind) | `Cons(e0, …, Cons(ek, Wildcard))` |
    /// | `[e0, …, ek, rest @ ..]` (prefix rest, bound) | `Cons(e0, …, Cons(ek, Var(rest)))` |
    ///
    /// Only prefix rest (Rest as the last element) is handled here; the parser
    /// rejects suffix/middle rest with `P025 RestSuffixNotSupported` until the
    /// next cut lifts that restriction.
    ///
    /// # Panics
    ///
    /// Does not panic; any non-`List` variant is returned as-is.
    #[must_use]
    pub fn desugar_list(self) -> Self {
        let Self::List { elements, span } = self else {
            return self;
        };

        // Split off a trailing Rest element if present.
        let (elems, rest_elem) = match elements.last() {
            Some(ListPatElem::Rest { .. }) => {
                let mut e = elements;
                let r = e.pop();
                (e, r)
            }
            _ => (elements, None),
        };

        // Build the tail: Wildcard, Var(bind), or ListNil.
        let tail: Self = match rest_elem {
            Some(ListPatElem::Rest {
                bind: Some(name),
                span: rest_span,
            }) => Self::Var {
                name,
                span: rest_span,
            },
            Some(ListPatElem::Rest {
                bind: None,
                span: rest_span,
            }) => Self::Wildcard { span: rest_span },
            _ => Self::ListNil { span },
        };

        // Wrap element patterns in Cons nodes from right to left.
        elems.into_iter().rev().fold(tail, |acc, elem| match elem {
            ListPatElem::Elem(pat) => {
                let full_span = pat.span().merge(acc.span());
                Self::Cons {
                    head: Box::new(pat),
                    tail: Box::new(acc),
                    span: full_span,
                }
            }
            // Rest was already extracted above; this arm is unreachable
            // in a well-formed List pattern.
            ListPatElem::Rest {
                span: rest_span, ..
            } => Self::Wildcard { span: rest_span },
        })
    }
}
