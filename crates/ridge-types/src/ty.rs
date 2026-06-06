//! Type identifier newtypes and the [`Type`] enum.
//!
//! Defines [`TyVid`], [`CapVid`], [`TyConId`], [`CapRow`], and [`Type`].

use std::fmt;

use crate::{capability_set::CapabilitySet, tycon::TyConId};

// ── Type identifiers ──────────────────────────────────────────────────────────

/// Unification variable index (assigned by the inference table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TyVid(pub u32);

/// Capability-row variable index — used only for stdlib HOF signatures (D041,
/// D057). Users cannot introduce these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapVid(pub u32);

/// Record-row variable index — the unification slot for the open tail of a
/// [`Type::Record`]. Allocated by the inference table, exactly like [`TyVid`]
/// and [`CapVid`], but lives in its own namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RowVid(pub u32);

// ── Record rows ───────────────────────────────────────────────────────────────

/// The tail of a record row: either closed (the field set is exact) or open
/// (there may be more fields, reached through a [`RowVid`]).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowTail {
    /// The field set is exact — no further labels.
    Closed,
    /// The row may carry additional labels, bound through this variable.
    Open(RowVid),
}

/// A record row: a labelled field set plus a tail. This is the substitution
/// target for a bound [`RowVid`] (`Subst::row`).
///
/// The `fields` vector obeys the same invariant as [`Type::Record`]: sorted by
/// label, no duplicates. Build one through [`Row::new`] to enforce it.
#[derive(Debug, Clone)]
pub struct Row {
    /// Labelled field types — sorted by label, duplicate-free.
    pub fields: Vec<(String, Type)>,
    /// Whether the row is closed or open through a row variable.
    pub tail: RowTail,
}

impl Row {
    /// Builds a row, normalising `fields` to the sorted, duplicate-free
    /// invariant (a later duplicate label wins, matching record-literal
    /// shadowing). Construct rows through this rather than the struct literal.
    #[must_use]
    pub fn new(mut fields: Vec<(String, Type)>, tail: RowTail) -> Self {
        normalise_fields(&mut fields);
        Self { fields, tail }
    }
}

/// Sort `fields` by label and drop earlier duplicates (last label wins).
fn normalise_fields(fields: &mut Vec<(String, Type)>) {
    // Stable sort keeps later duplicates after earlier ones for the same label;
    // dedup_by then keeps the first of each equal run, so reverse first to make
    // "last wins" fall out of keeping-the-first.
    fields.reverse();
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    fields.dedup_by(|a, b| a.0 == b.0);
}

// ── Capability row ────────────────────────────────────────────────────────────

/// Capability set carried by a function type.
///
/// In user-written types this is always [`CapRow::Concrete`].
/// In stdlib HOF signatures it may be [`CapRow::Var`] (D041).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapRow {
    /// A concrete (fully-known) capability set.
    Concrete(CapabilitySet),
    /// A capability-row variable — only used in stdlib HOF signatures (D041).
    Var(CapVid),
}

// ── The Type enum ─────────────────────────────────────────────────────────────

/// A monomorphic Ridge type.
///
/// # Variants
///
/// - [`Type::Var`] — a unification variable resolved through a `UnificationTable`.
/// - [`Type::Con`] — a fully-applied type constructor `C args…`.
/// - [`Type::Fn`] — a function type with capability annotation.
/// - [`Type::Tuple`] — a structural tuple (unnamed positional fields).
/// - [`Type::Alias`] — diagnostic-naming wrapper for an eagerly-resolved alias.
/// - [`Type::Error`] — the absorbing error type; see below.
///
/// # Records
///
/// Nominal records (`User`, declared with `type`) stay [`Type::Con`] over their
/// `TyCon::Record(RecordSchema)` — they keep their nominal identity for error
/// names and instance dispatch. Structural/anonymous records are [`Type::Record`]
/// with a [`RowTail`]: `Closed` for an exact field set, `Open(ρ)` when the row is
/// polymorphic over a [`RowVid`] tail (`{ name: Text | ρ }`).
///
/// # `Type::Error` — absorbing semantics
///
/// `Type::Error` is the universal absorbing element. Unifying any type with
/// `Type::Error` succeeds silently (no further `T###` diagnostic is emitted).
/// Any expression typed `Error` propagates `Error` upward through inference,
/// so a single upstream error never cascades into many. Mirrors `Binding::Error`
/// from `ridge-resolve` (spec §5).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Type {
    /// An unification variable (resolved lazily via the inference table).
    Var(TyVid),
    /// A type-constructor application: `C arg₁ arg₂ …`.
    Con(TyConId, Vec<Self>),
    /// A function type: parameters, return type, and capability row.
    Fn {
        /// Positional parameter types.
        params: Vec<Self>,
        /// Return type.
        ret: Box<Self>,
        /// Capability row (concrete for user types; variable for stdlib HOFs).
        caps: CapRow,
    },
    /// A structural tuple type.
    Tuple(Vec<Self>),
    /// A structural record type: a labelled field set plus a [`RowTail`].
    ///
    /// `fields` is sorted by label and duplicate-free (see [`Type::record`]).
    /// `Closed` is an exact field set; `Open(ρ)` leaves the row polymorphic over
    /// the tail variable `ρ`. Nominal records are NOT this — they remain
    /// [`Type::Con`] over their record `TyCon`.
    Record {
        /// Labelled field types — sorted by label, duplicate-free.
        fields: Vec<(String, Self)>,
        /// Whether the field set is exact or open through a row variable.
        tail: RowTail,
    },
    /// Diagnostic-naming wrapper for an eagerly-resolved type alias.
    ///
    /// Behaviorally transparent: `shallow_resolve`, unification, occurs-check,
    /// and `Subst::apply_to_ty` walk through it and operate on `body`. The
    /// `Display` impl prefers `name` so error messages render as `User`
    /// instead of the expanded record body. Created at use sites; never nested
    /// (no `Type::Alias { body: Alias { .. }, .. }` — the outermost alias name
    /// wins).
    Alias {
        /// The alias's `TyConId` — its `name` field is the display-friendly name.
        name: TyConId,
        /// The eagerly-substituted alias body.
        body: Box<Self>,
    },
    /// `Type::Error` propagates after a type error is emitted. Acts as a
    /// universal absorbing element so a single error never cascades into many.
    /// Mirrors `Binding::Error` from `ridge-resolve`.
    Error,
}

impl Type {
    /// Returns `true` if this is the absorbing error type [`Type::Error`].
    ///
    /// Used by the unifier: if either operand `is_error()`, unification
    /// succeeds silently without emitting a new `T###`.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self, Self::Error)
    }

    /// Builds a structural record type, normalising `fields` to the sorted,
    /// duplicate-free invariant (a later duplicate label wins, matching
    /// record-literal shadowing). Construct records through this rather than the
    /// `Self::Record { .. }` literal so the invariant always holds.
    #[must_use]
    pub fn record(mut fields: Vec<(String, Self)>, tail: RowTail) -> Self {
        normalise_fields(&mut fields);
        Self::Record { fields, tail }
    }
}

// ── Display ───────────────────────────────────────────────────────────────────

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Var(v) => write!(f, "?{}", v.0),
            Self::Con(id, args) if args.is_empty() => write!(f, "#{}", id.0),
            Self::Con(id, args) => {
                write!(f, "#{}", id.0)?;
                for a in args {
                    write!(f, " ({a})")?;
                }
                Ok(())
            }
            Self::Fn { params, ret, caps } => {
                write!(f, "fn")?;
                match caps {
                    CapRow::Concrete(s) if !s.is_empty() => {
                        // Iterate cap names via the CapabilitySet display
                        write!(f, " {s}")?;
                    }
                    CapRow::Var(v) => write!(f, " ?c{}", v.0)?,
                    _ => {}
                }
                for p in params {
                    write!(f, " ({p})")?;
                }
                write!(f, " -> {ret}")
            }
            Self::Tuple(ts) => {
                let mut first = true;
                write!(f, "(")?;
                for t in ts {
                    if !first {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                    first = false;
                }
                write!(f, ")")
            }
            Self::Record { fields, tail } => {
                write!(f, "{{")?;
                for (i, (label, ty)) in fields.iter().enumerate() {
                    write!(f, "{}{label}: {ty}", if i == 0 { " " } else { ", " })?;
                }
                match tail {
                    // Open row: "{ a: A | ?r0 }" (or "{ | ?r0 }" when empty).
                    RowTail::Open(rv) => write!(f, " | ?r{} }}", rv.0),
                    // Closed: "{ a: A }", or "{}" when there are no fields.
                    RowTail::Closed if fields.is_empty() => write!(f, "}}"),
                    RowTail::Closed => write!(f, " }}"),
                }
            }
            // Prefer the alias name over the expanded body.
            Self::Alias { name, .. } => write!(f, "#{}", name.0),
            Self::Error => write!(f, "<error>"),
        }
    }
}

impl fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ridge_ast::Capability;
        let caps: Vec<&str> = [
            (Capability::Io, "io"),
            (Capability::Fs, "fs"),
            (Capability::Net, "net"),
            (Capability::Time, "time"),
            (Capability::Random, "random"),
            (Capability::Env, "env"),
            (Capability::Proc, "proc"),
            (Capability::Spawn, "spawn"),
            (Capability::Ffi, "ffi"),
            (Capability::Db, "db"),
        ]
        .iter()
        .filter_map(|(cap, name)| {
            if self.contains(*cap) {
                Some(*name)
            } else {
                None
            }
        })
        .collect();
        write!(f, "{{{}}}", caps.join(" "))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tycon::TyConId;

    fn vid(n: u32) -> TyVid {
        TyVid(n)
    }
    fn cid(n: u32) -> TyConId {
        TyConId(n)
    }

    // ── Type::Error absorption predicate ─────────────────────────────────────

    #[test]
    fn error_is_error() {
        assert!(Type::Error.is_error());
    }

    #[test]
    fn var_is_not_error() {
        assert!(!Type::Var(vid(0)).is_error());
    }

    #[test]
    fn con_is_not_error() {
        assert!(!Type::Con(cid(0), vec![]).is_error());
    }

    #[test]
    fn fn_is_not_error() {
        let t = Type::Fn {
            params: vec![],
            ret: Box::new(Type::Error),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        assert!(!t.is_error());
    }

    // ── Constructor smoke tests ───────────────────────────────────────────────

    #[test]
    fn var_round_trip() {
        let t = Type::Var(vid(42));
        assert!(matches!(t, Type::Var(TyVid(42))));
    }

    #[test]
    fn con_nullary() {
        let t = Type::Con(cid(1), vec![]);
        assert!(matches!(t, Type::Con(TyConId(1), ref a) if a.is_empty()));
    }

    #[test]
    fn fn_type_construction() {
        let t = Type::Fn {
            params: vec![Type::Con(cid(0), vec![])],
            ret: Box::new(Type::Con(cid(1), vec![])),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        assert!(matches!(t, Type::Fn { .. }));
    }

    #[test]
    fn record_via_con() {
        // Records are represented as Con(record_tycon, args)
        let t = Type::Con(cid(5), vec![]);
        assert!(!t.is_error());
    }

    #[test]
    fn union_via_con() {
        // Unions (e.g. Option, Result) are represented as Con(union_tycon, args)
        let t = Type::Con(cid(9), vec![Type::Var(vid(0))]);
        assert!(matches!(t, Type::Con(TyConId(9), _)));
    }

    #[test]
    fn actor_via_con() {
        // Handle X is Con(handle_tycon, [Con(actor_tycon, [])])
        let inner = Type::Con(cid(11), vec![]);
        let t = Type::Con(cid(11), vec![inner]);
        assert!(!t.is_error());
    }

    #[test]
    fn alias_display_prefers_name_over_body() {
        // Type::Alias renders as the alias name, not the expanded body.
        let alias = Type::Alias {
            name: cid(7),
            body: Box::new(Type::Con(cid(3), vec![])),
        };
        let s = format!("{alias}");
        // Should display as "#7" (the alias name id), not "#3" (the body id).
        assert!(s.contains("#7"), "got: {s}");
        assert!(!s.contains("#3"), "got: {s}");
    }

    #[test]
    fn alias_is_not_error() {
        let alias = Type::Alias {
            name: cid(0),
            body: Box::new(Type::Error),
        };
        assert!(!alias.is_error());
    }

    #[test]
    fn error_display() {
        assert_eq!(format!("{}", Type::Error), "<error>");
    }

    #[test]
    fn tuple_construction() {
        let t = Type::Tuple(vec![Type::Con(cid(0), vec![]), Type::Con(cid(1), vec![])]);
        assert!(matches!(t, Type::Tuple(ref v) if v.len() == 2));
    }

    // ── Record rows (L1) ──────────────────────────────────────────────────────

    fn field(label: &str, id: u32) -> (String, Type) {
        (label.to_string(), Type::Con(cid(id), vec![]))
    }

    #[test]
    fn record_constructor_sorts_fields_by_label() {
        let t = Type::record(vec![field("name", 1), field("age", 2)], RowTail::Closed);
        match t {
            Type::Record { fields, tail } => {
                assert_eq!(tail, RowTail::Closed);
                let labels: Vec<&str> = fields.iter().map(|(l, _)| l.as_str()).collect();
                assert_eq!(labels, ["age", "name"], "fields must be sorted by label");
            }
            _ => panic!("expected Record"),
        }
    }

    #[test]
    fn record_constructor_dedups_last_label_wins() {
        // Two `x` fields: Con(7) then Con(9). Record-literal shadowing → last wins.
        let t = Type::record(
            vec![field("x", 7), field("y", 5), field("x", 9)],
            RowTail::Closed,
        );
        match t {
            Type::Record { fields, .. } => {
                assert_eq!(fields.len(), 2, "duplicate label collapses to one");
                let x = &fields.iter().find(|(l, _)| l == "x").unwrap().1;
                assert!(
                    matches!(x, Type::Con(TyConId(9), _)),
                    "last `x` (Con 9) must win, got {x:?}"
                );
            }
            _ => panic!("expected Record"),
        }
    }

    #[test]
    fn record_display_closed_nonempty() {
        let t = Type::record(vec![field("age", 0), field("name", 1)], RowTail::Closed);
        assert_eq!(format!("{t}"), "{ age: #0, name: #1 }");
    }

    #[test]
    fn record_display_empty_closed() {
        let t = Type::record(vec![], RowTail::Closed);
        assert_eq!(format!("{t}"), "{}");
    }

    #[test]
    fn record_display_open_shows_row_var() {
        let t = Type::record(vec![field("name", 1)], RowTail::Open(RowVid(3)));
        assert_eq!(format!("{t}"), "{ name: #1 | ?r3 }");
    }

    #[test]
    fn record_display_empty_open() {
        let t = Type::record(vec![], RowTail::Open(RowVid(0)));
        assert_eq!(format!("{t}"), "{ | ?r0 }");
    }

    #[test]
    fn record_is_not_error() {
        let t = Type::record(vec![field("a", 0)], RowTail::Closed);
        assert!(!t.is_error());
    }

    #[test]
    fn row_tail_equality() {
        assert_eq!(RowTail::Closed, RowTail::Closed);
        assert_eq!(RowTail::Open(RowVid(1)), RowTail::Open(RowVid(1)));
        assert_ne!(RowTail::Open(RowVid(1)), RowTail::Open(RowVid(2)));
        assert_ne!(RowTail::Closed, RowTail::Open(RowVid(0)));
    }
}
