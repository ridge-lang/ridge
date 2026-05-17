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
/// # Closed records
///
/// Spec §5.1 reserves row polymorphism for post-0.1.0. In Phase 4 a record type
/// is exactly its `TyCon::Record(RecordSchema)` — no row-extension variable.
/// Records are closed with no row polymorphism in this release.
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
}
