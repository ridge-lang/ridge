//! Unification algorithm for Ridge type inference (T5).
//!
//! # Main entry points
//!
//! - [`unify`] — unify two [`Type`]s; updates the [`InferCtx`] tables in-place.
//! - [`unify_caps`] — unify two [`CapRow`]s.
//! - [`occurs`] — occurs-check for infinite-type detection.
//!
//! # Key invariants
//!
//! - Both operands are shallow-resolved before dispatch (OQ-T015: aliases are
//!   peeled transparently; see `InferCtx::shallow_resolve`).
//! - `Type::Error` is the absorbing element — unifying with it always succeeds.
//! - After `unify(a, b)` succeeds, `ctx.shallow_resolve(&Type::Var(v))` will
//!   return the concrete type for any variable `v` that was unified with one.
//!
//! # Span note
//!
//! `unify` does not carry source spans. Callers in T6/T7 wrap the returned
//! `TypeError` with a `span` before propagating it to the error accumulator.
//! Until then the error is constructed with a dummy `Span::point(0)`.

use ridge_ast::Span;
use ridge_types::{CapRow, TyVid, Type};

use crate::ctx::{CapValue, CapVidKey, InferCtx, TyValue, TyVidKey};
use crate::error::TypeError;

// ── Public API ────────────────────────────────────────────────────────────────

/// Unifies two [`Type`]s, updating `ctx` in place.
///
/// # Algorithm (§4.5)
///
/// Both sides are shallow-resolved first. After resolution the dispatch is:
///
/// | (a, b) | action |
/// |---|---|
/// | `(Var x, Var y)` | union the two roots |
/// | `(Var x, T)` | occurs check; if clear, bind `x = T` |
/// | `(T, Var y)` | symmetric |
/// | `(Con c xs, Con d ys)` | if `c == d` and arities match, zip-unify |
/// | `(Fn …, Fn …)` | arity check; zip params + ret + caps |
/// | `(Tuple xs, Tuple ys)` | arity check; zip-unify |
/// | `(Error, _)` or `(_, Error)` | absorbing — `Ok(())` |
/// | `(Alias {body}, other)` | defensive: shallow_resolve already peels; recurse on body |
/// | otherwise | `T001 TypeMismatch` |
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive match over all Type variants"
)]
#[expect(
    clippy::many_single_char_names,
    reason = "a/b/r/c/s/d are idiomatic in unification"
)]
pub fn unify(ctx: &mut InferCtx, a: &Type, b: &Type) -> Result<(), TypeError> {
    let a = ctx.shallow_resolve(a);
    let b = ctx.shallow_resolve(b);

    match (&a, &b) {
        // ── Absorbing element ─────────────────────────────────────────────────
        (Type::Error, _) | (_, Type::Error) => Ok(()),

        // ── Both vars ─────────────────────────────────────────────────────────
        (Type::Var(x), Type::Var(y)) => {
            ctx.tyvids.union(TyVidKey(x.0), TyVidKey(y.0));
            Ok(())
        }

        // ── Var on the left ───────────────────────────────────────────────────
        (Type::Var(x), other) => {
            let x = *x;
            if occurs(ctx, x, other) {
                return Err(TypeError::OccursCheck {
                    var: format!("?{}", x.0),
                    ty: format!("{other}"),
                    span: dummy_span(),
                });
            }
            ctx.tyvids
                .union_value(TyVidKey(x.0), TyValue(Some(other.clone())));
            Ok(())
        }

        // ── Var on the right ──────────────────────────────────────────────────
        (other, Type::Var(y)) => {
            let y = *y;
            if occurs(ctx, y, other) {
                return Err(TypeError::OccursCheck {
                    var: format!("?{}", y.0),
                    ty: format!("{other}"),
                    span: dummy_span(),
                });
            }
            ctx.tyvids
                .union_value(TyVidKey(y.0), TyValue(Some(other.clone())));
            Ok(())
        }

        // ── Two type-constructor applications ─────────────────────────────────
        (Type::Con(c, xs), Type::Con(d, ys)) => {
            if c != d || xs.len() != ys.len() {
                return Err(mismatch(&a, &b));
            }
            for (x, y) in xs.iter().zip(ys.iter()) {
                unify(ctx, x, y)?;
            }
            Ok(())
        }

        // ── Two function types ────────────────────────────────────────────────
        (
            Type::Fn {
                params: ps,
                ret: r,
                caps: c,
            },
            Type::Fn {
                params: qs,
                ret: s,
                caps: d,
            },
        ) => {
            if ps.len() != qs.len() {
                return Err(TypeError::ArityMismatch {
                    callee: String::new(),
                    expected: ps.len(),
                    found: qs.len(),
                    span: dummy_span(),
                });
            }
            // Clone before consuming borrows.
            let ps = ps.clone();
            let qs = qs.clone();
            let r = *r.clone();
            let s = *s.clone();
            let c = c.clone();
            let d = d.clone();
            for (p, q) in ps.iter().zip(qs.iter()) {
                unify(ctx, p, q)?;
            }
            unify(ctx, &r, &s)?;
            unify_caps(ctx, &c, &d)
        }

        // ── Two tuples ────────────────────────────────────────────────────────
        (Type::Tuple(xs), Type::Tuple(ys)) => {
            if xs.len() != ys.len() {
                return Err(TypeError::ArityMismatch {
                    callee: String::new(),
                    expected: xs.len(),
                    found: ys.len(),
                    span: dummy_span(),
                });
            }
            let xs = xs.clone();
            let ys = ys.clone();
            for (x, y) in xs.iter().zip(ys.iter()) {
                unify(ctx, x, y)?;
            }
            Ok(())
        }

        // ── Alias on either side (defensive: shallow_resolve already peels) ───
        (Type::Alias { body, .. }, other) => {
            let body = *body.clone();
            unify(ctx, &body, other)
        }
        (other, Type::Alias { body, .. }) => {
            let body = *body.clone();
            unify(ctx, other, &body)
        }

        // ── Structural mismatch ────────────────────────────────────────────────
        _ => Err(mismatch(&a, &b)),
    }
}

/// Unifies two [`CapRow`]s, updating the cap unification table in `ctx`.
///
/// | (a, b) | action |
/// |---|---|
/// | `(Concrete s1, Concrete s2)` | `s1 == s2` → Ok; else `T001` |
/// | `(Var v, Concrete c)` | bind `v = c` |
/// | `(Concrete c, Var v)` | bind `v = c` |
/// | `(Var v1, Var v2)` | union the two roots |
pub fn unify_caps(ctx: &mut InferCtx, a: &CapRow, b: &CapRow) -> Result<(), TypeError> {
    let a = ctx.shallow_resolve_caps(a);
    let b = ctx.shallow_resolve_caps(b);

    match (&a, &b) {
        (CapRow::Concrete(s1), CapRow::Concrete(s2)) => {
            if s1 == s2 {
                Ok(())
            } else {
                Err(TypeError::TypeMismatch {
                    expected: format!("{s1}"),
                    found: format!("{s2}"),
                    span: dummy_span(),
                })
            }
        }
        (CapRow::Var(v), CapRow::Concrete(_)) => {
            let v = *v;
            ctx.capvids
                .union_value(CapVidKey(v.0), CapValue(Some(b.clone())));
            Ok(())
        }
        (CapRow::Concrete(_), CapRow::Var(v)) => {
            let v = *v;
            ctx.capvids
                .union_value(CapVidKey(v.0), CapValue(Some(a.clone())));
            Ok(())
        }
        (CapRow::Var(v1), CapRow::Var(v2)) => {
            ctx.capvids.union(CapVidKey(v1.0), CapVidKey(v2.0));
            Ok(())
        }
        // CapRow is #[non_exhaustive] — wildcard for forward-compat.
        _ => Ok(()),
    }
}

/// Occurs check: returns `true` if [`TyVid`] `v` appears free in `t`.
///
/// Both `Type::Var` lookup is done via `find` (root comparison), so path
/// compression is respected. All structurally composite types are walked
/// recursively. `Type::Alias` is traversed via its body (OQ-T015 transparent).
pub fn occurs(ctx: &mut InferCtx, v: TyVid, t: &Type) -> bool {
    match t {
        Type::Var(w) => {
            let vr = ctx.tyvids.find(TyVidKey(v.0));
            let wr = ctx.tyvids.find(TyVidKey(w.0));
            if vr == wr {
                return true;
            }
            // If w is bound, check its binding too.
            ctx.tyvids
                .probe_value(wr)
                .0
                .is_some_and(|bound| occurs(ctx, v, &bound))
        }
        Type::Con(_, args) => args.iter().any(|a| occurs(ctx, v, a)),
        Type::Fn { params, ret, .. } => {
            params.iter().any(|p| occurs(ctx, v, p)) || occurs(ctx, v, ret)
        }
        Type::Tuple(ts) => ts.iter().any(|t| occurs(ctx, v, t)),
        // OQ-T015: Alias is transparent; walk the body.
        Type::Alias { body, .. } => occurs(ctx, v, body),
        // Type is #[non_exhaustive] — wildcard for forward-compat (including Error).
        _ => false,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Constructs a `T001 TypeMismatch` error with a dummy span.
fn mismatch(expected: &Type, found: &Type) -> TypeError {
    TypeError::TypeMismatch {
        expected: format!("{expected}"),
        found: format!("{found}"),
        span: dummy_span(),
    }
}

/// A zero-offset dummy span used when no source location is available at the
/// unification layer. Callers in T6/T7 replace this with a real span.
const fn dummy_span() -> Span {
    Span::point(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::Capability;
    use ridge_types::{CapVid, CapabilitySet, TyConId};

    fn cid(n: u32) -> TyConId {
        TyConId(n)
    }

    fn make_ctx() -> InferCtx {
        InferCtx::new()
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T1 — (Con, Con) same id and arity → unifies
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn con_con_same_id_same_arity_unifies() {
        let mut ctx = make_ctx();
        let a = Type::Con(cid(0), vec![]);
        let b = Type::Con(cid(0), vec![]);
        assert!(unify(&mut ctx, &a, &b).is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T2 — (Con, Con) different id → T001
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn con_con_different_id_type_mismatch() {
        let mut ctx = make_ctx();
        let a = Type::Con(cid(0), vec![]);
        let b = Type::Con(cid(1), vec![]);
        let err = unify(&mut ctx, &a, &b).unwrap_err();
        assert_eq!(err.code(), "T001");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T3 — (Con c [a], Con c [b]) same id different arg arity → T001
    //       (Con arity mismatch falls through to T001 since Con(c, xs) arms
    //        check both id equality and arg-count equality together)
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn con_con_same_id_different_arg_count_mismatch() {
        let mut ctx = make_ctx();
        let a = Type::Con(cid(5), vec![Type::Con(cid(0), vec![])]);
        let b = Type::Con(cid(5), vec![]);
        let err = unify(&mut ctx, &a, &b).unwrap_err();
        // xs.len() != ys.len() → T001 (wrapped through mismatch)
        assert_eq!(err.code(), "T001");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T4 — (Var, T) where T does not contain V → unifies, sets V = T
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn var_con_binds_var() {
        let mut ctx = make_ctx();
        let v = ctx.fresh_tyvid();
        let int = Type::Con(cid(0), vec![]);
        unify(&mut ctx, &Type::Var(v), &int).unwrap();
        // After unification, resolving v should give Int.
        let resolved = ctx.shallow_resolve(&Type::Var(v));
        assert!(matches!(resolved, Type::Con(TyConId(0), _)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T5 — (Var, T) where T contains V → T010 occurs check
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn var_containing_var_occurs_check() {
        let mut ctx = make_ctx();
        let v = ctx.fresh_tyvid();
        // ?v unify List(?v) — v appears in the argument
        let list_v = Type::Con(cid(9), vec![Type::Var(v)]);
        let err = unify(&mut ctx, &Type::Var(v), &list_v).unwrap_err();
        assert_eq!(err.code(), "T010");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T6 — (Var, Var) → unifies as union
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn var_var_unions() {
        let mut ctx = make_ctx();
        let v1 = ctx.fresh_tyvid();
        let v2 = ctx.fresh_tyvid();
        assert!(unify(&mut ctx, &Type::Var(v1), &Type::Var(v2)).is_ok());
        // After union, unioning with Int via either variable should work.
        let int = Type::Con(cid(0), vec![]);
        unify(&mut ctx, &Type::Var(v1), &int).unwrap();
        let r1 = ctx.shallow_resolve(&Type::Var(v1));
        let r2 = ctx.shallow_resolve(&Type::Var(v2));
        assert!(matches!(r1, Type::Con(TyConId(0), _)));
        assert!(matches!(r2, Type::Con(TyConId(0), _)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T7 — (Var, Var) chain with path compression
    //      v1 = v2, v2 = Int → resolving v1 returns Int
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn var_chain_path_compression() {
        let mut ctx = make_ctx();
        let v1 = ctx.fresh_tyvid();
        let v2 = ctx.fresh_tyvid();
        unify(&mut ctx, &Type::Var(v1), &Type::Var(v2)).unwrap();
        let int = Type::Con(cid(0), vec![]);
        unify(&mut ctx, &Type::Var(v2), &int).unwrap();
        let resolved = ctx.shallow_resolve(&Type::Var(v1));
        assert!(matches!(resolved, Type::Con(TyConId(0), _)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T8 — (Fn, Fn) same arity → unifies
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn fn_fn_same_arity_unifies() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let a = Type::Fn {
            params: vec![int.clone()],
            ret: Box::new(int.clone()),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let b = Type::Fn {
            params: vec![int.clone()],
            ret: Box::new(int),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        assert!(unify(&mut ctx, &a, &b).is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T9 — (Fn, Fn) different arity → T003
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn fn_fn_different_arity_error() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let a = Type::Fn {
            params: vec![int.clone(), int.clone()],
            ret: Box::new(int.clone()),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let b = Type::Fn {
            params: vec![int.clone()],
            ret: Box::new(int),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let err = unify(&mut ctx, &a, &b).unwrap_err();
        assert_eq!(err.code(), "T003");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T10a — cap unify: Concrete == Concrete equal → Ok
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn cap_concrete_equal_unifies() {
        let mut ctx = make_ctx();
        let io = CapRow::Concrete(CapabilitySet::singleton(Capability::Io));
        assert!(unify_caps(&mut ctx, &io, &io).is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T10b — cap unify: Var = Concrete binds
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn cap_var_concrete_binds() {
        let mut ctx = make_ctx();
        let raw = ctx.fresh_capvid();
        let cv = CapVid(raw.0);
        let io = CapRow::Concrete(CapabilitySet::singleton(Capability::Io));
        unify_caps(&mut ctx, &CapRow::Var(cv), &io).unwrap();
        let resolved = ctx.shallow_resolve_caps(&CapRow::Var(cv));
        assert_eq!(resolved, io);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T10c — cap unify: Var = Var unions
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn cap_var_var_unions() {
        let mut ctx = make_ctx();
        let c1 = ctx.fresh_capvid();
        let c2 = ctx.fresh_capvid();
        assert!(unify_caps(&mut ctx, &CapRow::Var(c1), &CapRow::Var(c2)).is_ok());
        // Bind one and the other should resolve to it.
        let io = CapRow::Concrete(CapabilitySet::singleton(Capability::Io));
        ctx.capvids
            .union_value(CapVidKey(c1.0), CapValue(Some(io.clone())));
        let resolved = ctx.shallow_resolve_caps(&CapRow::Var(c2));
        assert_eq!(resolved, io);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T11 — (Tuple, Tuple) same arity → unifies
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn tuple_tuple_same_arity_unifies() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let a = Type::Tuple(vec![int.clone(), int.clone()]);
        let b = Type::Tuple(vec![int.clone(), int]);
        assert!(unify(&mut ctx, &a, &b).is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T12 — (Tuple, Tuple) different arity → T003
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn tuple_tuple_different_arity_error() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let a = Type::Tuple(vec![int.clone()]);
        let b = Type::Tuple(vec![int.clone(), int]);
        let err = unify(&mut ctx, &a, &b).unwrap_err();
        assert_eq!(err.code(), "T003");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T13 — (Error, Int) → absorbing Ok(())
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn error_absorbs_any_type() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        assert!(unify(&mut ctx, &Type::Error, &int).is_ok());
        assert!(unify(&mut ctx, &int, &Type::Error).is_ok());
        assert!(unify(&mut ctx, &Type::Error, &Type::Error).is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T14 — (Alias{Int}, Int) → unifies (transparent per OQ-T015)
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn alias_int_vs_int_unifies() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let alias = Type::Alias {
            name: cid(7),
            body: Box::new(int.clone()),
        };
        assert!(unify(&mut ctx, &alias, &int).is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T15 — (Alias{Foo, Int}, Alias{Foo, Int}) → unifies
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn alias_same_body_unifies() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let alias1 = Type::Alias {
            name: cid(7),
            body: Box::new(int.clone()),
        };
        let alias2 = Type::Alias {
            name: cid(7),
            body: Box::new(int),
        };
        assert!(unify(&mut ctx, &alias1, &alias2).is_ok());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T16 — shallow_resolve: bound Var resolves to its type, unbound stays
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn shallow_resolve_correctness() {
        let mut ctx = make_ctx();
        let v = ctx.fresh_tyvid();
        // Unbound → Var
        let r = ctx.shallow_resolve(&Type::Var(v));
        assert!(matches!(r, Type::Var(_)));
        // Bind to Int
        let int = Type::Con(cid(0), vec![]);
        unify(&mut ctx, &Type::Var(v), &int).unwrap();
        // Now resolves to Int
        let r = ctx.shallow_resolve(&Type::Var(v));
        assert!(matches!(r, Type::Con(TyConId(0), _)));
        // Alias peeled
        let alias = Type::Alias {
            name: cid(99),
            body: Box::new(Type::Con(cid(1), vec![])),
        };
        let r = ctx.shallow_resolve(&alias);
        assert!(matches!(r, Type::Con(TyConId(1), _)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T17 — occurs check: unify(?a, Fn { params: [?a], ret: Int }) → T010
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn occurs_check_fn_param_cycle() {
        let mut ctx = make_ctx();
        let v = ctx.fresh_tyvid();
        let int = Type::Con(cid(0), vec![]);
        let fn_ty = Type::Fn {
            params: vec![Type::Var(v)],
            ret: Box::new(int),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let err = unify(&mut ctx, &Type::Var(v), &fn_ty).unwrap_err();
        assert_eq!(err.code(), "T010");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T18 — instantiate(forall a. a -> a) produces fresh TyVid each call and
    //        unify chains across two instantiations correctly
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn instantiate_forall_chains_across_calls() {
        use ridge_types::{CapabilitySet, Scheme};

        // Build `forall a. a -> a`
        let a = ridge_types::TyVid(0);
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(Type::Var(a)),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
        };

        // Use a plain counter for fresh TyVid allocation to avoid double-borrow.
        let mut ty_counter = 1u32; // start at 1 — TyVid(0) is used as scheme var
        let mut cap_counter = 0u32;

        let t1 = scheme.instantiate(
            &mut || {
                let v = ridge_types::TyVid(ty_counter);
                ty_counter += 1;
                v
            },
            &mut || {
                let c = CapVid(cap_counter);
                cap_counter += 1;
                c
            },
        );
        let t2 = scheme.instantiate(
            &mut || {
                let v = ridge_types::TyVid(ty_counter);
                ty_counter += 1;
                v
            },
            &mut || {
                let c = CapVid(cap_counter);
                cap_counter += 1;
                c
            },
        );

        // Extract the fresh vars from each instantiation.
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
        // Fresh vars differ across instantiations.
        assert_ne!(fv1, fv2);

        // Now chain through unify using a fresh InferCtx seeded with these vars.
        let mut ctx = make_ctx();
        // Register fv1 and fv2 as keys in the table.
        while ctx.tyvids.len() <= fv2.0 as usize {
            ctx.tyvids.new_key(TyValue(None));
        }

        let int = Type::Con(cid(0), vec![]);
        unify(&mut ctx, &Type::Var(fv1), &int).unwrap();
        unify(
            &mut ctx,
            &t1,
            &Type::Fn {
                params: vec![int.clone()],
                ret: Box::new(int.clone()),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
        )
        .unwrap();
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T19 — (Con, Var) symmetry — same pair both ways succeeds
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn con_var_symmetry() {
        let int = Type::Con(cid(0), vec![]);

        // Way 1: Var on left
        let mut ctx1 = make_ctx();
        let v1 = ctx1.fresh_tyvid();
        unify(&mut ctx1, &Type::Var(v1), &int).unwrap();
        let r1 = ctx1.shallow_resolve(&Type::Var(v1));
        assert!(matches!(r1, Type::Con(TyConId(0), _)));

        // Way 2: Var on right
        let mut ctx2 = make_ctx();
        let v2 = ctx2.fresh_tyvid();
        unify(&mut ctx2, &int, &Type::Var(v2)).unwrap();
        let r2 = ctx2.shallow_resolve(&Type::Var(v2));
        assert!(matches!(r2, Type::Con(TyConId(0), _)));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T20 — (Fn, Fn) with cap Var = Concrete binds, then unifies
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn fn_fn_cap_var_concrete_binds() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let cv = ctx.fresh_capvid();
        let io_caps = CapabilitySet::singleton(Capability::Io);
        let a = Type::Fn {
            params: vec![int.clone()],
            ret: Box::new(int.clone()),
            caps: CapRow::Var(cv),
        };
        let b = Type::Fn {
            params: vec![int.clone()],
            ret: Box::new(int),
            caps: CapRow::Concrete(io_caps),
        };
        unify(&mut ctx, &a, &b).unwrap();
        let resolved = ctx.shallow_resolve_caps(&CapRow::Var(cv));
        assert_eq!(resolved, CapRow::Concrete(io_caps));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T21 — (Fn, Fn) with mismatched params → T001 on param mismatch
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn fn_fn_mismatched_param_types_error() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let text = Type::Con(cid(1), vec![]);
        let a = Type::Fn {
            params: vec![int.clone()],
            ret: Box::new(int.clone()),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let b = Type::Fn {
            params: vec![text],
            ret: Box::new(int),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let err = unify(&mut ctx, &a, &b).unwrap_err();
        assert_eq!(err.code(), "T001");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T22 — cap mismatch: Concrete(io) vs Concrete(fs) → T001
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn cap_concrete_mismatch_error() {
        let mut ctx = make_ctx();
        let io = CapRow::Concrete(CapabilitySet::singleton(Capability::Io));
        let fs = CapRow::Concrete(CapabilitySet::singleton(Capability::Fs));
        let err = unify_caps(&mut ctx, &io, &fs).unwrap_err();
        assert_eq!(err.code(), "T001");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T23 — (Fn, Con) structural mismatch → T001
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn fn_con_structural_mismatch() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let fn_ty = Type::Fn {
            params: vec![],
            ret: Box::new(int.clone()),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let err = unify(&mut ctx, &fn_ty, &int).unwrap_err();
        assert_eq!(err.code(), "T001");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T24 — occurs(v, Fn { params: [v], .. }) returns true
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn occurs_fn_return() {
        let mut ctx = make_ctx();
        let v = ctx.fresh_tyvid();
        let int = Type::Con(cid(0), vec![]);
        // v does not occur in Int
        assert!(!occurs(&mut ctx, v, &int));
        // v occurs in Fn { ret: v, .. }
        let fn_ty = Type::Fn {
            params: vec![],
            ret: Box::new(Type::Var(v)),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        assert!(occurs(&mut ctx, v, &fn_ty));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // T25 — Var unifies with Tuple containing a compatible nested Var
    // ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn var_unifies_with_tuple_of_con() {
        let mut ctx = make_ctx();
        let v = ctx.fresh_tyvid();
        let int = Type::Con(cid(0), vec![]);
        let tup = Type::Tuple(vec![int.clone(), int]);
        unify(&mut ctx, &Type::Var(v), &tup).unwrap();
        let resolved = ctx.shallow_resolve(&Type::Var(v));
        assert!(matches!(resolved, Type::Tuple(_)));
    }
}
