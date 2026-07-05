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
    /// A rest position `..`, which may appear in prefix, middle, or suffix
    /// position.  The optional `bind` is the name bound to the captured
    /// middle segment: `mid @ ..` binds `mid`, plain `..` does not.
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

    /// A bracketed list pattern `[a, b, c]`, `[a, rest @ ..]`, `[.., last]`,
    /// or `[first, .., last]` (D258).
    ///
    /// Preserves the original bracket syntax for faithful round-tripping by
    /// `ridge-fmt`.  Downstream phases desugar this via [`Pattern::desugar_list`]
    /// for prefix-only rest patterns.  Suffix and middle rest patterns require
    /// guard-and-extraction lowering handled in `ridge-lower`.
    ///
    /// Invariants enforced by the parser:
    /// - At most one `ListPatElem::Rest` element.
    /// - The `Rest` element may appear in any position (prefix, middle, suffix).
    List {
        /// The element patterns, in source order.
        elements: Vec<ListPatElem>,
        /// Span covering the full `[…]` construct.
        span: Span,
    },

    /// A constructor-less inline record pattern `{ field, … }` or `{ field, .. }`.
    ///
    /// Parsed when a `{` in pattern position is followed by a lowercase
    /// identifier (field binding) or `..`.  The empty form `{}` is also valid.
    ///
    /// When `has_rest = true`, a `..` rest was present and unmentioned fields
    /// are ignored.  When `has_rest = false`, every field of the matched type
    /// must be mentioned (enforced at type-check time — T010).
    Record {
        /// The field patterns (may be empty for `{}`).
        fields: Vec<FieldPattern>,
        /// Whether a trailing `..` was present.
        has_rest: bool,
        /// Span covering the full `{ … }` form.
        span: Span,
    },

    /// An or-pattern `p1 | p2 | …` — matches when ANY alternative matches.
    ///
    /// Only valid at the root of a match arm (grammar §6.4 `MatchArm`); the
    /// parser does not accept it nested inside another pattern. Every
    /// alternative must bind the same variables — enforced during name
    /// resolution — with the same types, enforced during type inference. Always
    /// holds at least two alternatives.
    Or {
        /// The alternatives, in source order (length ≥ 2).
        alts: Vec<Self>,
        /// Span covering the full `p1 | … | pn` sequence.
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
            | Self::List { span, .. }
            | Self::Record { span, .. }
            | Self::Or { span, .. } => *span,
        }
    }

    /// True when this `List` pattern has a `Rest` element in a non-last position
    /// (suffix or middle rest — `[.., z]`, `[a, .., z]`).
    ///
    /// Returns `false` for all non-`List` variants and for prefix-only rest.
    #[must_use]
    pub fn is_varlen_list(&self) -> bool {
        let Self::List { elements, .. } = self else {
            return false;
        };
        elements
            .iter()
            .position(|e| matches!(e, ListPatElem::Rest { .. }))
            .is_some_and(|rest_idx| rest_idx < elements.len() - 1)
    }

    /// Collect every variable name this pattern binds, in sorted order.
    ///
    /// Mirrors what name resolution registers as locals: `Var`, the alias of
    /// `As`, shorthand record/constructor fields, and a bound list `Rest`, plus
    /// the recursive union over sub-patterns. Used to enforce the or-pattern
    /// same-variables rule (a `Pattern::Or` is only well-formed when every
    /// alternative binds the same set).
    #[must_use]
    pub fn bound_var_names(&self) -> std::collections::BTreeSet<String> {
        let mut names = std::collections::BTreeSet::new();
        self.collect_bound_var_names(&mut names);
        names
    }

    fn collect_bound_var_names(&self, out: &mut std::collections::BTreeSet<String>) {
        match self {
            Self::Wildcard { .. } | Self::Literal { .. } | Self::ListNil { .. } => {}
            Self::Var { name, .. } => {
                out.insert(name.text.clone());
            }
            Self::As { name, inner, .. } => {
                out.insert(name.text.clone());
                inner.collect_bound_var_names(out);
            }
            Self::Constructor { fields, args, .. } => {
                if let Some(fps) = fields {
                    for fp in fps {
                        match &fp.pattern {
                            Some(inner) => inner.collect_bound_var_names(out),
                            None => {
                                out.insert(fp.name.text.clone());
                            }
                        }
                    }
                }
                for arg in args {
                    arg.collect_bound_var_names(out);
                }
            }
            Self::Tuple { elems, .. } => {
                for e in elems {
                    e.collect_bound_var_names(out);
                }
            }
            Self::Cons { head, tail, .. } => {
                head.collect_bound_var_names(out);
                tail.collect_bound_var_names(out);
            }
            Self::Paren { inner, .. } => inner.collect_bound_var_names(out),
            Self::List { elements, .. } => {
                for elem in elements {
                    match elem {
                        ListPatElem::Elem(p) => p.collect_bound_var_names(out),
                        ListPatElem::Rest {
                            bind: Some(name), ..
                        } => {
                            out.insert(name.text.clone());
                        }
                        ListPatElem::Rest { bind: None, .. } => {}
                    }
                }
            }
            Self::Record { fields, .. } => {
                for fp in fields {
                    match &fp.pattern {
                        Some(inner) => inner.collect_bound_var_names(out),
                        None => {
                            out.insert(fp.name.text.clone());
                        }
                    }
                }
            }
            // Nested or-patterns do not parse, but stay total: a nested
            // alternative contributes the names its own first alternative binds.
            Self::Or { alts, .. } => {
                if let Some(first) = alts.first() {
                    first.collect_bound_var_names(out);
                }
            }
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
    /// For suffix/middle rest patterns (`[.., z]`, `[a, .., z]`), this method
    /// is not called by the lowering pass — those are handled by guard-and-body
    /// extraction in `ridge-lower`.  If called on such a pattern (e.g. for
    /// exhaustiveness), the Rest is treated as Wildcard to remain sound
    /// (conservatively under-approximates coverage).
    ///
    /// # Panics
    ///
    /// Does not panic; any non-`List` variant is returned as-is.
    #[must_use]
    pub fn desugar_list(self) -> Self {
        let Self::List { elements, span } = self else {
            return self;
        };

        // Find the Rest element position.
        let rest_pos = elements
            .iter()
            .position(|e| matches!(e, ListPatElem::Rest { .. }));

        match rest_pos {
            None => {
                // No rest: exact fixed-length list.
                let tail = Self::ListNil { span };
                elements
                    .into_iter()
                    .rev()
                    .fold(tail, |acc, elem| build_cons_elem(elem, acc, span))
            }
            Some(idx) if idx == elements.len() - 1 => {
                // Prefix rest: Rest is the last element.
                let mut elems = elements;
                let rest_elem = elems.pop();
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
                elems
                    .into_iter()
                    .rev()
                    .fold(tail, |acc, elem| build_cons_elem(elem, acc, span))
            }
            Some(_) => {
                // Suffix or middle rest: conservatively desugar to Wildcard.
                // The lowering pass handles these via guard-and-body extraction;
                // exhaustiveness sees them as Wildcard (sound, under-approximates).
                Self::Wildcard { span }
            }
        }
    }
}

/// Build a `Cons` node wrapping `acc` with the head from `elem`.
///
/// `Rest` elements in the middle of a fold (should not appear in well-formed
/// calls but handled defensively) produce a Wildcard.
fn build_cons_elem(elem: ListPatElem, acc: Pattern, list_span: Span) -> Pattern {
    match elem {
        ListPatElem::Elem(pat) => {
            let full_span = pat.span().merge(acc.span());
            Pattern::Cons {
                head: Box::new(pat),
                tail: Box::new(acc),
                span: full_span,
            }
        }
        ListPatElem::Rest {
            span: rest_span, ..
        } => Pattern::Wildcard {
            span: rest_span.merge(list_span),
        },
    }
}
