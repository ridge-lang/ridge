//! [`Subst`] — a finite mapping from type/capability variables to their types.

use rustc_hash::FxHashMap;

use crate::{
    capability_set::CapabilitySet,
    scheme::Scheme,
    ty::{CapRow, CapVid, Row, RowTail, RowVid, TyVid, Type},
};

// ── Subst ─────────────────────────────────────────────────────────────────────

/// A finite mapping from [`TyVid`] to [`Type`] and [`CapVid`] to
/// [`CapabilitySet`], used during and after inference.
///
/// The `UnificationTable<TyVid>` (in `ridge-typecheck`) holds the canonical
/// inference state; `Subst` is the externalised snapshot used for
/// generalisation and error rendering.
#[derive(Debug, Clone, Default)]
pub struct Subst {
    /// Type variable substitutions.
    pub ty: FxHashMap<TyVid, Type>,
    /// Capability-row variable substitutions.
    pub cap: FxHashMap<CapVid, CapabilitySet>,
    /// Record-row variable substitutions: each bound [`RowVid`] maps to the
    /// row (fields + tail) that absorbed it during unification.
    pub row: FxHashMap<RowVid, Row>,
}

impl Subst {
    /// Returns the empty substitution.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns a substitution mapping exactly one type variable to a type.
    #[must_use]
    pub fn singleton(v: TyVid, t: Type) -> Self {
        let mut s = Self::default();
        s.ty.insert(v, t);
        s
    }

    /// Composes two substitutions into one.
    ///
    /// The result is right-biased: `compose(s1, s2)` maps `x` to
    /// `s1.apply(s2(x))` — i.e., `s2` is applied first, then `s1` is applied
    /// to the result. This is the standard HM composition order.
    #[must_use]
    pub fn compose(self, other: Self) -> Self {
        // Apply `self` to every value in `other.ty`.
        let mut result_ty: FxHashMap<TyVid, Type> = other
            .ty
            .into_iter()
            .map(|(v, t)| (v, self.apply_to_ty(&t)))
            .collect();

        // Entries in `self.ty` that are NOT overridden by `other`.
        for (v, t) in &self.ty {
            result_ty.entry(*v).or_insert_with(|| t.clone());
        }

        // Cap subst: same merging strategy.
        let mut result_cap = other.cap;
        for (c, s) in &self.cap {
            result_cap.entry(*c).or_insert(*s);
        }

        // Row subst: apply `self` to each of `other`'s rows, then keep `self`'s
        // own rows that `other` did not override. Mirrors the `ty` strategy.
        let mut result_row: FxHashMap<RowVid, Row> = other
            .row
            .iter()
            .map(|(rv, row)| {
                let (fields, tail) = self.apply_to_row(&row.fields, &row.tail);
                (*rv, Row::new(fields, tail))
            })
            .collect();
        for (rv, row) in &self.row {
            result_row.entry(*rv).or_insert_with(|| row.clone());
        }

        Self {
            ty: result_ty,
            cap: result_cap,
            row: result_row,
        }
    }

    /// Applies this substitution to a type.
    ///
    /// Walks the type recursively:
    /// - [`Type::Var`] is replaced by `self.ty[v]` if present.
    /// - All other variants are walked structurally.
    /// - [`Type::Error`] is returned unchanged.
    /// - [`Type::Alias`] is transparent — the body is substituted and the alias
    ///   wrapper is preserved.
    ///
    /// The function is idempotent on already-applied substitutions (since after
    /// applying, free `TyVid`s are gone). No occurs-check — that is T5's job.
    #[must_use]
    pub fn apply_to_ty(&self, ty: &Type) -> Type {
        match ty {
            // Apply recursively so composed substs resolve chains.
            Type::Var(v) => self
                .ty
                .get(v)
                .map_or(Type::Var(*v), |t| self.apply_to_ty(t)),
            Type::Con(id, args) => {
                Type::Con(*id, args.iter().map(|a| self.apply_to_ty(a)).collect())
            }
            Type::Fn { params, ret, caps } => {
                let new_params = params.iter().map(|p| self.apply_to_ty(p)).collect();
                let new_ret = Box::new(self.apply_to_ty(ret));
                let new_caps = match caps {
                    CapRow::Concrete(s) => CapRow::Concrete(*s),
                    CapRow::Var(c) => {
                        if let Some(&concrete) = self.cap.get(c) {
                            CapRow::Concrete(concrete)
                        } else {
                            CapRow::Var(*c)
                        }
                    }
                };
                Type::Fn {
                    params: new_params,
                    ret: new_ret,
                    caps: new_caps,
                }
            }
            Type::Tuple(ts) => Type::Tuple(ts.iter().map(|t| self.apply_to_ty(t)).collect()),
            Type::Record { fields, tail } => {
                let (new_fields, new_tail) = self.apply_to_row(fields, tail);
                Type::record(new_fields, new_tail)
            }
            // Alias is transparent — substitute the body, keep the wrapper.
            Type::Alias { name, body } => Type::Alias {
                name: *name,
                body: Box::new(self.apply_to_ty(body)),
            },
            Type::Error => Type::Error,
        }
    }

    /// Applies this substitution to a record row `{ fields | tail }`, returning
    /// the substituted field set and the resolved tail.
    ///
    /// Each field type is substituted. If the tail is `Open(ρ)` and ρ is bound
    /// in `self.row`, the bound row is spliced in — its fields appended and its
    /// own tail followed — iterating until the tail is `Closed` or unbound. The
    /// returned fields are not yet normalised; [`Type::record`] does that at the
    /// call sites. Assumes an acyclic row substitution (the occurs-check the
    /// unifier runs before binding a `RowVid` guarantees this).
    #[must_use]
    fn apply_to_row(
        &self,
        fields: &[(String, Type)],
        tail: &RowTail,
    ) -> (Vec<(String, Type)>, RowTail) {
        let mut out: Vec<(String, Type)> = fields
            .iter()
            .map(|(label, t)| (label.clone(), self.apply_to_ty(t)))
            .collect();
        // Follow the tail while it is a *bound* row var, splicing each bound
        // row in. `cur.clone()` keeps `cur` live for the final return when the
        // loop exits on a closed or unbound tail (RowTail is cheap to clone).
        let mut cur = tail.clone();
        while let RowTail::Open(rv) = cur.clone() {
            let Some(row) = self.row.get(&rv) else {
                // Unbound row var — leave the tail open at ρ.
                break;
            };
            for (label, t) in &row.fields {
                out.push((label.clone(), self.apply_to_ty(t)));
            }
            cur = row.tail.clone();
        }
        (out, cur)
    }

    /// Applies this substitution to a scheme, skipping bound variables.
    ///
    /// Substitutes through `scheme.ty` but does NOT substitute bound variables
    /// (those listed in `scheme.vars` and `scheme.cap_vars`). This preserves
    /// the scheme's polymorphism while resolving free variables.
    #[must_use]
    pub fn apply_to_scheme(&self, scheme: &Scheme) -> Scheme {
        // Build a restricted subst that skips the scheme's bound vars.
        let restricted_ty: FxHashMap<TyVid, Type> = self
            .ty
            .iter()
            .filter(|(v, _)| !scheme.vars.contains(v))
            .map(|(v, t)| (*v, t.clone()))
            .collect();

        let restricted_cap: FxHashMap<CapVid, CapabilitySet> = self
            .cap
            .iter()
            .filter(|(c, _)| !scheme.cap_vars.contains(c))
            .map(|(c, s)| (*c, *s))
            .collect();

        let restricted = Self {
            ty: restricted_ty,
            cap: restricted_cap,
            // No scheme binds row vars yet (R4 adds `Scheme.row_vars`); until
            // then row substitutions are never scheme-bound, so pass them whole.
            row: self.row.clone(),
        };

        Scheme {
            vars: scheme.vars.clone(),
            cap_vars: scheme.cap_vars.clone(),
            ty: restricted.apply_to_ty(&scheme.ty),
            constraints: scheme.constraints.clone(),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability_set::CapabilitySet,
        scheme::Scheme,
        ty::{CapRow, Row, RowTail, RowVid, TyVid, Type},
        tycon::TyConId,
    };

    fn vid(n: u32) -> TyVid {
        TyVid(n)
    }
    fn cid(n: u32) -> TyConId {
        TyConId(n)
    }

    // ── empty substitution ────────────────────────────────────────────────────

    #[test]
    fn empty_subst_leaves_var_unchanged() {
        let s = Subst::empty();
        let t = Type::Var(vid(0));
        let result = s.apply_to_ty(&t);
        assert!(matches!(result, Type::Var(TyVid(0))));
    }

    #[test]
    fn empty_subst_leaves_con_unchanged() {
        let s = Subst::empty();
        let t = Type::Con(cid(1), vec![]);
        let result = s.apply_to_ty(&t);
        assert!(matches!(result, Type::Con(TyConId(1), _)));
    }

    // ── singleton substitution ────────────────────────────────────────────────

    #[test]
    fn singleton_substitutes_var() {
        let s = Subst::singleton(vid(0), Type::Con(cid(1), vec![]));
        let result = s.apply_to_ty(&Type::Var(vid(0)));
        assert!(matches!(result, Type::Con(TyConId(1), _)));
    }

    #[test]
    fn singleton_leaves_other_var_unchanged() {
        let s = Subst::singleton(vid(0), Type::Con(cid(1), vec![]));
        let result = s.apply_to_ty(&Type::Var(vid(1)));
        assert!(matches!(result, Type::Var(TyVid(1))));
    }

    // ── apply_to_ty: all variants ─────────────────────────────────────────────

    #[test]
    fn apply_to_ty_var_mapped() {
        let s = Subst::singleton(vid(3), Type::Con(cid(0), vec![]));
        let ty = Type::Var(vid(3));
        assert!(matches!(s.apply_to_ty(&ty), Type::Con(TyConId(0), _)));
    }

    #[test]
    fn apply_to_ty_con_walks_args() {
        // List ?0 — substitute ?0 := Int
        let s = Subst::singleton(vid(0), Type::Con(cid(0), vec![])); // Int
        let ty = Type::Con(cid(5), vec![Type::Var(vid(0))]); // List ?0
        let result = s.apply_to_ty(&ty);
        match &result {
            Type::Con(_, args) => assert!(matches!(args[0], Type::Con(TyConId(0), _))),
            _ => panic!("expected Con"),
        }
    }

    #[test]
    fn apply_to_ty_fn_walks_params_and_ret() {
        let s = Subst::singleton(vid(0), Type::Con(cid(1), vec![]));
        let ty = Type::Fn {
            params: vec![Type::Var(vid(0))],
            ret: Box::new(Type::Var(vid(0))),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let result = s.apply_to_ty(&ty);
        match result {
            Type::Fn { params, ret, .. } => {
                assert!(matches!(params[0], Type::Con(TyConId(1), _)));
                assert!(matches!(*ret, Type::Con(TyConId(1), _)));
            }
            _ => panic!("expected Fn"),
        }
    }

    #[test]
    fn apply_to_ty_alias_transparent_body_substituted() {
        // Type::Alias should keep the name but substitute the body.
        let s = Subst::singleton(vid(0), Type::Con(cid(2), vec![]));
        let ty = Type::Alias {
            name: cid(7),
            body: Box::new(Type::Var(vid(0))),
        };
        let result = s.apply_to_ty(&ty);
        match result {
            Type::Alias { name, body } => {
                assert_eq!(name.0, 7);
                assert!(matches!(*body, Type::Con(TyConId(2), _)));
            }
            _ => panic!("expected Alias"),
        }
    }

    #[test]
    fn apply_to_ty_error_unchanged() {
        let s = Subst::singleton(vid(0), Type::Con(cid(1), vec![]));
        let result = s.apply_to_ty(&Type::Error);
        assert!(result.is_error());
    }

    // ── apply_to_scheme: skips bound vars ─────────────────────────────────────

    #[test]
    fn apply_to_scheme_skips_bound_vars() {
        // `forall a. a -> Int` — substituting ?0 := Bool should NOT replace `a`.
        let a = vid(0);
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(Type::Con(cid(1), vec![])),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        };
        let s = Subst::singleton(vid(0), Type::Con(cid(99), vec![]));
        let result = s.apply_to_scheme(&scheme);
        // `a` is bound — it should NOT have been substituted.
        match &result.ty {
            Type::Fn { params, .. } => {
                assert!(
                    matches!(params[0], Type::Var(TyVid(0))),
                    "bound var a should survive apply_to_scheme"
                );
            }
            _ => panic!("expected Fn"),
        }
        // Bound vars list preserved.
        assert_eq!(result.vars, vec![vid(0)]);
    }

    // ── compose ───────────────────────────────────────────────────────────────

    #[test]
    fn compose_empty_with_singleton() {
        let s1 = Subst::empty();
        let s2 = Subst::singleton(vid(0), Type::Con(cid(1), vec![]));
        let c = s1.compose(s2);
        let result = c.apply_to_ty(&Type::Var(vid(0)));
        assert!(matches!(result, Type::Con(TyConId(1), _)));
    }

    #[test]
    fn compose_chains_substitutions() {
        // s1: ?1 := ?0; s2: ?0 := Int.
        // compose(s1, s2)(?1) should be:  s1.apply(s2(?1)) = s1.apply(?1) = ?0
        // Wait — s2 maps ?0 := Int, s1 maps ?1 := ?0.
        // compose(s1, s2)(?0) = s1.apply(s2(?0)) = s1.apply(Int) = Int.
        // compose(s1, s2)(?1) = s1.apply(s2(?1)) = s1.apply(?1) = ?0.
        let s1 = Subst::singleton(vid(1), Type::Var(vid(0)));
        let s2 = Subst::singleton(vid(0), Type::Con(cid(5), vec![]));
        let composed = s1.compose(s2);
        let r0 = composed.apply_to_ty(&Type::Var(vid(0)));
        let r1 = composed.apply_to_ty(&Type::Var(vid(1)));
        assert!(matches!(r0, Type::Con(TyConId(5), _)), "?0 should be Int");
        // ?1 -> ?0 via s1, then ?0 -> Int via the composed result of s2 in composed.
        // Because compose applies self (s1) to each value in other (s2),
        // and also keeps s1's own entries. So composed.ty[0] = Int (from s2),
        // and composed.ty[1] = s1.apply(s2(?1)) = s1.apply(?1) = ?0.
        // Since ?0 is now Int in the composed subst, apply_to_ty chains: ?1 -> ?0 -> Int.
        assert!(
            matches!(r1, Type::Con(TyConId(5), _)),
            "?1 should resolve through the chain to Int, got: {r1:?}"
        );
    }

    // ── row substitution (L1) ─────────────────────────────────────────────────

    fn rec_field(label: &str, id: u32) -> (String, Type) {
        (label.to_string(), Type::Con(cid(id), vec![]))
    }

    #[test]
    fn apply_to_ty_closed_record_substitutes_field_types() {
        // { v: ?0 }  with  ?0 := #1  →  { v: #1 }
        let s = Subst::singleton(vid(0), Type::Con(cid(1), vec![]));
        let rec = Type::record(vec![("v".into(), Type::Var(vid(0)))], RowTail::Closed);
        match s.apply_to_ty(&rec) {
            Type::Record { fields, tail } => {
                assert_eq!(tail, RowTail::Closed);
                assert_eq!(fields.len(), 1);
                assert!(matches!(fields[0].1, Type::Con(TyConId(1), _)));
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn apply_to_ty_open_record_unbound_tail_stays_open() {
        let s = Subst::empty();
        let rec = Type::record(vec![rec_field("a", 0)], RowTail::Open(RowVid(5)));
        match s.apply_to_ty(&rec) {
            Type::Record { tail, .. } => assert_eq!(tail, RowTail::Open(RowVid(5))),
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn apply_to_ty_open_record_splices_bound_row() {
        // { a: #1 | ρ0 }  with  ρ0 := { b: #2 }  →  { a: #1, b: #2 }, closed.
        let mut s = Subst::empty();
        s.row.insert(
            RowVid(0),
            Row::new(vec![rec_field("b", 2)], RowTail::Closed),
        );
        let rec = Type::record(vec![rec_field("a", 1)], RowTail::Open(RowVid(0)));
        match s.apply_to_ty(&rec) {
            Type::Record { fields, tail } => {
                assert_eq!(tail, RowTail::Closed, "spliced row was closed");
                let labels: Vec<&str> = fields.iter().map(|(l, _)| l.as_str()).collect();
                assert_eq!(labels, ["a", "b"]);
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn apply_to_ty_splices_chained_row_vars() {
        // { a | ρ0 }, ρ0 := { b | ρ1 }, ρ1 := { c }  →  { a, b, c }, closed.
        let mut s = Subst::empty();
        s.row.insert(
            RowVid(0),
            Row::new(vec![rec_field("b", 2)], RowTail::Open(RowVid(1))),
        );
        s.row.insert(
            RowVid(1),
            Row::new(vec![rec_field("c", 3)], RowTail::Closed),
        );
        let rec = Type::record(vec![rec_field("a", 1)], RowTail::Open(RowVid(0)));
        match s.apply_to_ty(&rec) {
            Type::Record { fields, tail } => {
                assert_eq!(tail, RowTail::Closed);
                let labels: Vec<&str> = fields.iter().map(|(l, _)| l.as_str()).collect();
                assert_eq!(labels, ["a", "b", "c"]);
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn apply_to_ty_splice_substitutes_bound_field_types() {
        // ρ0 := { b: ?0 }, ?0 := #7, record { | ρ0 }  →  { b: #7 }.
        let mut s = Subst::empty();
        s.ty.insert(vid(0), Type::Con(cid(7), vec![]));
        s.row.insert(
            RowVid(0),
            Row::new(vec![("b".into(), Type::Var(vid(0)))], RowTail::Closed),
        );
        let rec = Type::record(vec![], RowTail::Open(RowVid(0)));
        match s.apply_to_ty(&rec) {
            Type::Record { fields, .. } => {
                assert_eq!(fields.len(), 1);
                assert!(
                    matches!(fields[0].1, Type::Con(TyConId(7), _)),
                    "field type inside the spliced row must be substituted too"
                );
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }
}
