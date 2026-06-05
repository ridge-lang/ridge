//! [`Scheme`] — a polymorphic type scheme `∀ vars cap_vars. ty`.

use rustc_hash::FxHashSet;

use crate::constraint::Constraint;
use crate::ty::{CapRow, CapVid, RowTail, RowVid, TyVid, Type};

// ── Scheme ────────────────────────────────────────────────────────────────────

/// A type scheme `∀ vars cap_vars. ty` as produced by generalisation.
///
/// `cap_vars` are [`CapVid`]s generalised over function-typed schemes per D041.
/// `constraints` carries class constraints over the universally-quantified type
/// variables, e.g. `∀ a. ToText a => a -> Text`. The constraint solver
/// populates this field; it is empty for all pre-existing, unconstrained
/// declarations.
#[derive(Debug, Clone)]
pub struct Scheme {
    /// Universally-quantified type variables.
    pub vars: Vec<TyVid>,
    /// Universally-quantified capability-row variables (stdlib HOFs only, D041).
    pub cap_vars: Vec<CapVid>,
    /// Universally-quantified record-row variables (open-record polymorphism).
    ///
    /// Each is a [`RowVid`] that appears in an open tail of `ty`. Instantiation
    /// freshens these so a function with an open-record parameter can be applied
    /// at differently-shaped records within the same program.
    pub row_vars: Vec<RowVid>,
    /// The body type, which may mention variables in `vars`, `cap_vars`, and
    /// `row_vars`.
    pub ty: Type,
    /// Class constraints over `vars`. Empty for unconstrained declarations.
    ///
    /// Each constraint's [`Constraint::ty`] is a member of `vars`. The
    /// constraint solver attached these during generalisation; the lowering
    /// pass reads them to thread dictionaries. Ignored by
    /// `free_vars`/`instantiate`/`subst_type` — those operate on `ty` only.
    pub constraints: Vec<Constraint>,
}

impl Scheme {
    /// Constructs a monomorphic scheme (no quantified variables, no constraints).
    #[must_use]
    pub const fn mono(ty: Type) -> Self {
        Self {
            vars: vec![],
            cap_vars: vec![],
            row_vars: vec![],
            ty,
            constraints: vec![],
        }
    }

    /// Returns the free type and capability variables in the scheme body that
    /// are NOT bound by `vars`/`cap_vars`.
    ///
    /// Returns `(free_ty_vars, free_cap_vars)`.
    #[must_use]
    pub fn free_vars(&self) -> (FxHashSet<TyVid>, FxHashSet<CapVid>) {
        let bound_ty: FxHashSet<TyVid> = self.vars.iter().copied().collect();
        let bound_cap: FxHashSet<CapVid> = self.cap_vars.iter().copied().collect();

        let mut free_ty = FxHashSet::default();
        let mut free_cap = FxHashSet::default();

        collect_free_ty(&self.ty, &bound_ty, &bound_cap, &mut free_ty, &mut free_cap);

        (free_ty, free_cap)
    }

    /// Returns the free record-row variables in the scheme body that are NOT
    /// bound by `row_vars`.
    ///
    /// Row variables live in their own namespace ([`RowVid`]), so they are
    /// reported separately from [`Self::free_vars`] to keep that method's
    /// `(TyVid, CapVid)` signature stable.
    #[must_use]
    pub fn free_row_vars(&self) -> FxHashSet<RowVid> {
        let bound: FxHashSet<RowVid> = self.row_vars.iter().copied().collect();
        let mut all = FxHashSet::default();
        collect_row_vars(&self.ty, &mut all);
        all.retain(|rv| !bound.contains(rv));
        all
    }

    /// Instantiates the scheme, producing a monomorphic `Type` by substituting
    /// fresh unification variables for every bound variable.
    ///
    /// `fresh_ty` — called once per bound [`TyVid`] to produce a fresh one.
    /// `fresh_cap` — called once per bound [`CapVid`] to produce a fresh one.
    /// `fresh_row` — called once per bound [`RowVid`] to produce a fresh one.
    ///
    /// The returned `Type` has no occurrences of the old bound variables;
    /// all have been replaced by fresh variables.
    #[must_use]
    pub fn instantiate(
        &self,
        fresh_ty: &mut dyn FnMut() -> TyVid,
        fresh_cap: &mut dyn FnMut() -> CapVid,
        fresh_row: &mut dyn FnMut() -> RowVid,
    ) -> Type {
        // Build per-variable substitution maps.
        let ty_subst: std::collections::HashMap<TyVid, Type> = self
            .vars
            .iter()
            .map(|&v| (v, Type::Var(fresh_ty())))
            .collect();
        let cap_subst: std::collections::HashMap<CapVid, CapVid> =
            self.cap_vars.iter().map(|&c| (c, fresh_cap())).collect();
        let row_subst: std::collections::HashMap<RowVid, RowVid> =
            self.row_vars.iter().map(|&r| (r, fresh_row())).collect();

        subst_type(&self.ty, &ty_subst, &cap_subst, &row_subst)
    }
}

// ── Free-variable collection ──────────────────────────────────────────────────

fn collect_free_ty(
    ty: &Type,
    bound_ty: &FxHashSet<TyVid>,
    bound_cap: &FxHashSet<CapVid>,
    free_ty: &mut FxHashSet<TyVid>,
    free_cap: &mut FxHashSet<CapVid>,
) {
    match ty {
        Type::Var(v) => {
            if !bound_ty.contains(v) {
                free_ty.insert(*v);
            }
        }
        Type::Con(_, args) => {
            for a in args {
                collect_free_ty(a, bound_ty, bound_cap, free_ty, free_cap);
            }
        }
        Type::Fn { params, ret, caps } => {
            for p in params {
                collect_free_ty(p, bound_ty, bound_cap, free_ty, free_cap);
            }
            collect_free_ty(ret, bound_ty, bound_cap, free_ty, free_cap);
            match caps {
                CapRow::Var(c) => {
                    if !bound_cap.contains(c) {
                        free_cap.insert(*c);
                    }
                }
                CapRow::Concrete(_) => {}
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                collect_free_ty(t, bound_ty, bound_cap, free_ty, free_cap);
            }
        }
        Type::Record { fields, tail: _ } => {
            for (_, t) in fields {
                collect_free_ty(t, bound_ty, bound_cap, free_ty, free_cap);
            }
            // Row vars in `tail` live in a separate namespace; they are reported
            // by [`Scheme::free_row_vars`], not here.
        }
        Type::Alias { body, .. } => {
            // Alias is transparent — walk the body.
            collect_free_ty(body, bound_ty, bound_cap, free_ty, free_cap);
        }
        Type::Error => {}
    }
}

// ── Instantiation helper ──────────────────────────────────────────────────────

fn subst_type(
    ty: &Type,
    ty_subst: &std::collections::HashMap<TyVid, Type>,
    cap_subst: &std::collections::HashMap<CapVid, CapVid>,
    row_subst: &std::collections::HashMap<RowVid, RowVid>,
) -> Type {
    match ty {
        Type::Var(v) => ty_subst.get(v).cloned().unwrap_or(Type::Var(*v)),
        Type::Con(id, args) => Type::Con(
            *id,
            args.iter()
                .map(|a| subst_type(a, ty_subst, cap_subst, row_subst))
                .collect(),
        ),
        Type::Fn { params, ret, caps } => {
            let new_params = params
                .iter()
                .map(|p| subst_type(p, ty_subst, cap_subst, row_subst))
                .collect();
            let new_ret = Box::new(subst_type(ret, ty_subst, cap_subst, row_subst));
            let new_caps = match caps {
                CapRow::Var(c) => {
                    if let Some(&nc) = cap_subst.get(c) {
                        CapRow::Var(nc)
                    } else {
                        CapRow::Var(*c)
                    }
                }
                CapRow::Concrete(s) => CapRow::Concrete(*s),
            };
            Type::Fn {
                params: new_params,
                ret: new_ret,
                caps: new_caps,
            }
        }
        Type::Tuple(ts) => Type::Tuple(
            ts.iter()
                .map(|t| subst_type(t, ty_subst, cap_subst, row_subst))
                .collect(),
        ),
        Type::Record { fields, tail } => Type::record(
            fields
                .iter()
                .map(|(label, t)| (label.clone(), subst_type(t, ty_subst, cap_subst, row_subst)))
                .collect(),
            // Freshen a quantified open tail; leave closed and free tails as-is.
            match tail {
                RowTail::Open(rv) => row_subst
                    .get(rv)
                    .map_or_else(|| tail.clone(), |&nrv| RowTail::Open(nrv)),
                RowTail::Closed => RowTail::Closed,
            },
        ),
        Type::Alias { name, body } => Type::Alias {
            name: *name,
            body: Box::new(subst_type(body, ty_subst, cap_subst, row_subst)),
        },
        Type::Error => Type::Error,
    }
}

/// Collects every record-row variable that appears in an open tail of `ty`.
///
/// Walks records (recording the tail's [`RowVid`] when it is open) and every
/// compound type so that nested records — a record field that is itself an
/// open record, a tuple of records — are covered.
fn collect_row_vars(ty: &Type, out: &mut FxHashSet<RowVid>) {
    match ty {
        Type::Record { fields, tail } => {
            if let RowTail::Open(rv) = tail {
                out.insert(*rv);
            }
            for (_, t) in fields {
                collect_row_vars(t, out);
            }
        }
        Type::Con(_, args) => {
            for a in args {
                collect_row_vars(a, out);
            }
        }
        Type::Fn { params, ret, .. } => {
            for p in params {
                collect_row_vars(p, out);
            }
            collect_row_vars(ret, out);
        }
        Type::Tuple(ts) => {
            for t in ts {
                collect_row_vars(t, out);
            }
        }
        Type::Alias { body, .. } => collect_row_vars(body, out),
        Type::Var(_) | Type::Error => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_set::CapabilitySet,
        ty::{CapRow, CapVid, RowVid, TyVid, Type},
        tycon::TyConId,
    };

    fn vid(n: u32) -> TyVid {
        TyVid(n)
    }
    fn cid(n: u32) -> TyConId {
        TyConId(n)
    }
    fn cvid(n: u32) -> CapVid {
        CapVid(n)
    }

    // ── free_vars on a monomorphic scheme ─────────────────────────────────────

    #[test]
    fn free_vars_monomorphic_no_free() {
        // `forall. Int -> Int` — no free vars.
        let scheme = Scheme {
            vars: vec![],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Con(cid(0), vec![])],
                ret: Box::new(Type::Con(cid(0), vec![])),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        };
        let (fty, fcap) = scheme.free_vars();
        assert!(fty.is_empty(), "expected no free ty vars, got {fty:?}");
        assert!(fcap.is_empty(), "expected no free cap vars, got {fcap:?}");
    }

    // ── free_vars on `forall a. a -> a` ──────────────────────────────────────

    #[test]
    fn free_vars_polymorphic_scheme_has_none() {
        // Bound `a` appears in body but is in `vars` — not free.
        let a = vid(0);
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(Type::Var(a)),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        };
        let (fty, _) = scheme.free_vars();
        assert!(fty.is_empty(), "bound var a should not appear as free");
    }

    // ── free_vars on a scheme with unbound vars ───────────────────────────────

    #[test]
    fn free_vars_with_unbound_var() {
        // `forall. ?0 -> Int` — ?0 is free.
        let scheme = Scheme::mono(Type::Fn {
            params: vec![Type::Var(vid(0))],
            ret: Box::new(Type::Con(cid(1), vec![])),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        });
        let (fty, _) = scheme.free_vars();
        assert!(fty.contains(&vid(0)));
    }

    // ── free_vars with cap vars ───────────────────────────────────────────────

    #[test]
    fn free_vars_cap_var_not_bound() {
        // `forall. fn ?c a -> Unit` — ?c is a free cap var.
        let scheme = Scheme {
            vars: vec![],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Con(cid(0), vec![])],
                ret: Box::new(Type::Con(cid(4), vec![])),
                caps: CapRow::Var(cvid(0)),
            },
            constraints: vec![],
        };
        let (_, fcap) = scheme.free_vars();
        assert!(fcap.contains(&cvid(0)), "cap var should be free");
    }

    // ── free_vars with cap vars properly bound ────────────────────────────────

    #[test]
    fn free_vars_cap_var_bound() {
        // `forall c. fn c a -> Unit` — c is bound, not free.
        let c = cvid(0);
        let scheme = Scheme {
            vars: vec![],
            cap_vars: vec![c],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Con(cid(0), vec![])],
                ret: Box::new(Type::Con(cid(4), vec![])),
                caps: CapRow::Var(c),
            },
            constraints: vec![],
        };
        let (_, fcap) = scheme.free_vars();
        assert!(fcap.is_empty(), "bound cap var c should not appear as free");
    }

    // ── instantiate produces fresh vars per call ──────────────────────────────

    #[test]
    fn instantiate_produces_fresh_vars() {
        // `forall a. a -> a` instantiated twice must produce different fresh vars.
        let a = vid(0);
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(Type::Var(a)),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        };

        let mut counter1 = 10u32;
        let t1 = scheme.instantiate(
            &mut || {
                let v = TyVid(counter1);
                counter1 += 1;
                v
            },
            &mut || CapVid(0),
            &mut || RowVid(0),
        );

        let mut counter2 = 20u32;
        let t2 = scheme.instantiate(
            &mut || {
                let v = TyVid(counter2);
                counter2 += 1;
                v
            },
            &mut || CapVid(0),
            &mut || RowVid(0),
        );

        // The fresh vars in t1 and t2 should differ.
        let fv1 = match &t1 {
            Type::Fn { params, .. } => match &params[0] {
                Type::Var(v) => *v,
                _ => panic!("expected Var"),
            },
            _ => panic!("expected Fn"),
        };
        let fv2 = match &t2 {
            Type::Fn { params, .. } => match &params[0] {
                Type::Var(v) => *v,
                _ => panic!("expected Var"),
            },
            _ => panic!("expected Fn"),
        };
        assert_ne!(fv1, fv2, "fresh vars must differ across instantiations");
    }

    // ── instantiate doesn't change monomorphic scheme ─────────────────────────

    #[test]
    fn instantiate_monomorphic_unchanged() {
        let scheme = Scheme::mono(Type::Con(cid(0), vec![]));
        let mut n = 0u32;
        let t = scheme.instantiate(
            &mut || {
                let v = TyVid(n);
                n += 1;
                v
            },
            &mut || CapVid(0),
            &mut || RowVid(0),
        );
        // No vars to substitute — body should come back as the same Con.
        assert!(matches!(t, Type::Con(TyConId(0), _)));
        // No fresh vars were consumed.
        assert_eq!(
            n, 0,
            "no fresh vars should be consumed for a monomorphic scheme"
        );
    }

    // ── bound vars excluded from free_vars ───────────────────────────────────

    #[test]
    fn bound_vars_excluded_from_free_vars_multi() {
        // `forall a b. a -> b -> a` — neither a nor b should appear in free_vars.
        let a = vid(1);
        let b = vid(2);
        let scheme = Scheme {
            vars: vec![a, b],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(a), Type::Var(b)],
                ret: Box::new(Type::Var(a)),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        };
        let (fty, _) = scheme.free_vars();
        assert!(!fty.contains(&a), "a should be bound");
        assert!(!fty.contains(&b), "b should be bound");
        assert!(fty.is_empty());
    }
}
