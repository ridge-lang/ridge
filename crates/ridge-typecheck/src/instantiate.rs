//! Scheme instantiation and generalisation (T6/T7).
//!
//! # `instantiate`
//!
//! Converts a polymorphic [`Scheme`] into a monomorphic [`Type`] by replacing
//! each universally-quantified variable with a fresh unification variable
//! allocated from the active [`InferCtx`].
//!
//! # `generalise`
//!
//! Implements Hindley-Milner let-generalisation (§4.7):
//!
//! ```text
//! generalise(env, ty):
//!     free_in_ty  = free_ty_vars(deep_resolve(ty))
//!     free_in_env = free_ty_vars(env)
//!     vars        = free_in_ty - free_in_env
//!     cap_vars    = free_cap_vars(ty) - free_cap_vars(env)
//!     Scheme { vars, cap_vars, ty }
//! ```

use ridge_types::{CapRow, CapVid, Constraint, Scheme, TyVid, Type};
use rustc_hash::FxHashSet;

use crate::ctx::InferCtx;

// ── instantiate ───────────────────────────────────────────────────────────────

/// Instantiates `scheme` by substituting fresh unification variables for every
/// bound type- and capability-variable.
///
/// Per §4.6 line 790:
/// > Replace each `TyVid` in `scheme.vars` with a fresh `TyVid`;
/// > replace each `CapVid` in `scheme.cap_vars` with a fresh `CapVid`.
///
/// The substitution is performed by [`Scheme::instantiate`] in `ridge-types`;
/// this function is the caller that supplies fresh-variable factories bound to
/// the active [`InferCtx`].
///
/// # Constraint deferral
///
/// If `scheme.constraints` is non-empty, each constraint is remapped through
/// the same `old → fresh` `TyVid` index map built for the type substitution,
/// then pushed to [`InferCtx::deferred_constraints`]. For unconstrained schemes
/// (`constraints` is empty), this is a no-op — the fast path for all
/// pre-typeclass code.
#[must_use]
pub fn instantiate(ctx: &mut InferCtx, scheme: &Scheme) -> Type {
    // We cannot pass two `&mut ctx` closures simultaneously (borrow checker).
    // Allocate all fresh variables upfront, then pass index-based closures.
    let n_ty = scheme.vars.len();
    let n_cap = scheme.cap_vars.len();

    let mut fresh_tyvids: Vec<TyVid> = Vec::with_capacity(n_ty);
    for _ in 0..n_ty {
        fresh_tyvids.push(ctx.fresh_tyvid());
    }
    let mut fresh_capvids: Vec<CapVid> = Vec::with_capacity(n_cap);
    for _ in 0..n_cap {
        fresh_capvids.push(ctx.fresh_capvid());
    }

    let mut ty_idx = 0usize;
    let mut cap_idx = 0usize;
    let instantiated = scheme.instantiate(
        &mut || {
            let v = fresh_tyvids[ty_idx];
            ty_idx += 1;
            v
        },
        &mut || {
            let c = fresh_capvids[cap_idx];
            cap_idx += 1;
            c
        },
    );

    // Remap and defer constraints through the same old→fresh TyVid mapping.
    // For schemes with no constraints (the common pre-typeclass case) this
    // loop is a no-op and has no observable effect.
    if !scheme.constraints.is_empty() {
        // Build old → fresh index: scheme.vars[i] maps to fresh_tyvids[i].
        // Use a small stack-allocated lookup rather than a HashMap for the
        // typical case of ≤ 3 type variables per scheme.
        for c in &scheme.constraints {
            // Find the position of the constraint's TyVid in the scheme's
            // bound variable list, then map it to the corresponding fresh var.
            let fresh_ty = scheme
                .vars
                .iter()
                .position(|&v| v == c.ty)
                .map_or(c.ty, |i| fresh_tyvids[i]); // defensive: if not found, pass through unchanged
            ctx.deferred_constraints.push(Constraint {
                class: c.class,
                ty: fresh_ty,
            });
        }
    }

    instantiated
}

/// Wraps a monomorphic type in a [`Scheme`] with no quantified variables.
///
/// Used for lambda parameters (lambda params are never polymorphic)
/// and for any binding site that does not yet trigger generalisation.
#[must_use]
pub const fn monoscheme(ty: Type) -> Scheme {
    Scheme::mono(ty)
}

/// Generalises `ty` against the current environment, returning a
/// polymorphic [`Scheme`].
///
/// Implements §4.7 algorithm:
///
/// 1. Deep-resolve `ty` so all union-find chains are followed.
/// 2. Collect the free [`TyVid`]s / [`CapVid`]s in the resolved type.
/// 3. Collect the free vars in the current environment (must not generalise
///    over those — they are still live in outer bindings).
/// 4. Quantify over `free_in_ty - free_in_env`.
///
/// Lambda parameters are bound as *monoschemes*; only
/// `let`-binding sites and top-level decls call this function.
#[must_use]
pub fn generalise(ctx: &mut InferCtx, ty: &Type) -> Scheme {
    let free_in_env_ty = ctx.env_free_tyvids();
    let free_in_env_cap = ctx.env_free_capvids();
    generalise_with_env(ctx, ty, &free_in_env_ty, &free_in_env_cap)
}

/// Variant of [`generalise`] that uses a pre-computed env free-var snapshot
/// instead of reading the current environment.
///
/// This is used by the SCC typechecker (§4.7 mutual recursion): the env free
/// vars must be snapshotted BEFORE the SCC's monomorphic bindings are added.
/// Otherwise the SCC's own `TyVids` would appear as "in env" and prevent them
/// from being generalised.
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashSet is the canonical hasher for this crate"
)]
pub fn generalise_with_env(
    ctx: &mut InferCtx,
    ty: &Type,
    free_in_env_ty: &FxHashSet<TyVid>,
    free_in_env_cap: &FxHashSet<CapVid>,
) -> Scheme {
    // 1. Deep-resolve (follows union-find roots recursively).
    let ty_resolved = ctx.deep_resolve(ty);

    // 2. Free vars in the resolved type.
    let (free_ty, free_cap) = collect_free_vars(&ty_resolved);

    // 3. Generalise over vars that are not in the environment.
    let mut vars: Vec<TyVid> = free_ty.difference(free_in_env_ty).copied().collect();
    let mut cap_vars: Vec<CapVid> = free_cap.difference(free_in_env_cap).copied().collect();

    // Sort for determinism (avoids snapshot-test flakiness).
    vars.sort_by_key(|v| v.0);
    cap_vars.sort_by_key(|c| c.0);

    Scheme {
        vars,
        cap_vars,
        ty: ty_resolved,
        constraints: vec![],
    }
}

/// Collects the free [`TyVid`]s and [`CapVid`]s in a (already-resolved) type.
///
/// A `TyVid` is "free" if it appears as `Type::Var(_)` and is not bound by
/// any enclosing scheme quantifier.  For the purposes of this function the
/// input type is assumed to be fully resolved (no union-find indirection
/// remains — call [`InferCtx::deep_resolve`] first).
///
/// This is a module-level function (not a method) so it can be called
/// without a mutable `InferCtx` borrow; the caller resolves first.
#[must_use]
pub fn collect_free_vars(ty: &Type) -> (FxHashSet<TyVid>, FxHashSet<CapVid>) {
    let mut free_ty: FxHashSet<TyVid> = FxHashSet::default();
    let mut free_cap: FxHashSet<CapVid> = FxHashSet::default();
    collect_free_vars_rec(ty, &mut free_ty, &mut free_cap);
    (free_ty, free_cap)
}

fn collect_free_vars_rec(
    ty: &Type,
    free_ty: &mut FxHashSet<TyVid>,
    free_cap: &mut FxHashSet<CapVid>,
) {
    match ty {
        Type::Var(v) => {
            free_ty.insert(*v);
        }
        Type::Con(_, args) => {
            for a in args {
                collect_free_vars_rec(a, free_ty, free_cap);
            }
        }
        Type::Fn { params, ret, caps } => {
            for p in params {
                collect_free_vars_rec(p, free_ty, free_cap);
            }
            collect_free_vars_rec(ret, free_ty, free_cap);
            if let CapRow::Var(c) = caps {
                free_cap.insert(*c);
            }
        }
        Type::Tuple(ts) => {
            for t in ts {
                collect_free_vars_rec(t, free_ty, free_cap);
            }
        }
        Type::Record { fields, .. } => {
            // Walk field types so a record holding a polymorphic value (e.g.
            // `{ id = fn x -> x }`) generalises over the field's variable. The
            // tail is a `RowVid` — a separate namespace, quantified with the
            // open-record surface syntax.
            for (_, t) in fields {
                collect_free_vars_rec(t, free_ty, free_cap);
            }
        }
        Type::Alias { body, .. } => {
            collect_free_vars_rec(body, free_ty, free_cap);
        }
        // Non-exhaustive wildcard: future Type variants (including Error) have no free vars.
        _ => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_types::{CapRow, CapVid, CapabilitySet, TyConId, TyVid};

    fn cid(n: u32) -> TyConId {
        TyConId(n)
    }

    // ── Test 1 ────────────────────────────────────────────────────────────────
    // instantiate(forall a. a) produces a fresh Var

    #[test]
    fn instantiate_forall_a_a_to_a_produces_fresh_var() {
        let mut ctx = InferCtx::new();
        let a = TyVid(0);
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![],
            ty: Type::Var(a),
            constraints: vec![],
        };
        let result = instantiate(&mut ctx, &scheme);
        // Should be a Var (the fresh one), not TyVid(0) itself (which is a scheme
        // bound var, not an allocated unification variable).
        assert!(matches!(result, Type::Var(_)));
    }

    // ── Test 2 ────────────────────────────────────────────────────────────────
    // Two separate instantiations of the same scheme produce distinct fresh vars

    #[test]
    fn instantiate_separate_calls_produce_distinct_vars() {
        let mut ctx = InferCtx::new();
        let a = TyVid(0);
        // forall a. a -> a
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(Type::Var(a)),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        };

        let t1 = instantiate(&mut ctx, &scheme);
        let t2 = instantiate(&mut ctx, &scheme);

        let fv1 = match &t1 {
            Type::Fn { params, .. } => match &params[0] {
                Type::Var(v) => *v,
                other => panic!("expected Var, got {other:?}"),
            },
            other => panic!("expected Fn, got {other:?}"),
        };
        let fv2 = match &t2 {
            Type::Fn { params, .. } => match &params[0] {
                Type::Var(v) => *v,
                other => panic!("expected Var, got {other:?}"),
            },
            other => panic!("expected Fn, got {other:?}"),
        };
        assert_ne!(fv1, fv2, "fresh vars must differ across instantiations");
    }

    // ── Test 3 ────────────────────────────────────────────────────────────────
    // Monomorphic scheme instantiates unchanged

    #[test]
    fn instantiate_monomorphic_scheme_unchanged() {
        let mut ctx = InferCtx::new();
        let int = Type::Con(cid(0), vec![]);
        let scheme = Scheme::mono(int);
        let result = instantiate(&mut ctx, &scheme);
        // No vars substituted — result must equal the body.
        assert!(matches!(result, Type::Con(TyConId(0), _)));
    }

    // ── Test 4 ────────────────────────────────────────────────────────────────
    // forall a b. (a, b) instantiates both variables independently

    #[test]
    fn instantiate_two_vars_produces_two_distinct_fresh_vars() {
        let mut ctx = InferCtx::new();
        let a = TyVid(0);
        let b = TyVid(1);
        let scheme = Scheme {
            vars: vec![a, b],
            cap_vars: vec![],
            ty: Type::Tuple(vec![Type::Var(a), Type::Var(b)]),
            constraints: vec![],
        };
        let result = instantiate(&mut ctx, &scheme);
        let (fv_a, fv_b) = match result {
            Type::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                match (&elems[0], &elems[1]) {
                    (Type::Var(a2), Type::Var(b2)) => (*a2, *b2),
                    other => panic!("expected (Var, Var), got {other:?}"),
                }
            }
            other => panic!("expected Tuple, got {other:?}"),
        };
        assert_ne!(fv_a, fv_b, "two vars must produce two distinct fresh vars");
    }

    // ── Test 5 ────────────────────────────────────────────────────────────────
    // Cap var in scheme is substituted with a fresh CapVid

    #[test]
    fn instantiate_cap_var_produces_fresh_capvid() {
        let mut ctx = InferCtx::new();
        let a = TyVid(0);
        let c = CapVid(0);
        // forall a {c}. fn c a -> a
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![c],
            ty: Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(Type::Var(a)),
                caps: CapRow::Var(c),
            },
            constraints: vec![],
        };
        let result = instantiate(&mut ctx, &scheme);
        match result {
            Type::Fn { caps, .. } => {
                assert!(matches!(caps, CapRow::Var(_)), "cap should be a fresh Var");
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    // ── Test 6 ────────────────────────────────────────────────────────────────
    // monoscheme wraps a type without quantification

    #[test]
    fn monoscheme_has_empty_vars() {
        let int = Type::Con(cid(0), vec![]);
        let s = monoscheme(int);
        assert!(s.vars.is_empty(), "monoscheme must have no vars");
        assert!(s.cap_vars.is_empty(), "monoscheme must have no cap_vars");
        assert!(matches!(s.ty, Type::Con(TyConId(0), _)));
    }

    // ── Test 7 ────────────────────────────────────────────────────────────────
    // generalise on a concrete type returns monomorphic scheme (no free vars)

    #[test]
    fn generalise_concrete_type_returns_monomorphic() {
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let int = Type::Con(cid(0), vec![]);
        let s = generalise(&mut ctx, &int);
        assert!(s.vars.is_empty(), "concrete type has no vars to generalise");
        assert!(
            s.cap_vars.is_empty(),
            "concrete type has no cap vars to generalise"
        );
        ctx.env.pop_frame();
    }

    // ── T7 tests ──────────────────────────────────────────────────────────────

    // ── Test T7-1 ─────────────────────────────────────────────────────────────
    // generalise_excludes_env_free_vars:
    // env contains a scheme binding ?a; generalising (?a -> ?a) must NOT
    // quantify ?a since it is free in env.

    #[test]
    fn generalise_excludes_env_free_vars() {
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Allocate ?a in the unification table.
        let a = ctx.fresh_tyvid();

        // Put ?a into env as a free (unresolved) mono-scheme.
        ctx.env
            .bind("x".to_string(), ridge_types::Scheme::mono(Type::Var(a)));

        // The body to generalise is ?a -> ?a.
        let body = Type::Fn {
            params: vec![Type::Var(a)],
            ret: Box::new(Type::Var(a)),
            caps: CapRow::Concrete(ridge_types::CapabilitySet::PURE),
        };

        let scheme = generalise(&mut ctx, &body);
        // ?a is free in env — must NOT be generalised.
        assert!(
            scheme.vars.is_empty(),
            "?a is free in env; vars must be [] but got {:?}",
            scheme.vars
        );
        ctx.env.pop_frame();
    }

    // ── Test T7-2 ─────────────────────────────────────────────────────────────
    // generalise_includes_unbound_body_vars:
    // env is empty; generalising (?b -> ?b) must quantify ?b.

    #[test]
    fn generalise_includes_unbound_body_vars() {
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let b = ctx.fresh_tyvid();

        // Body is ?b -> ?b; env is empty (no free vars in env).
        let body = Type::Fn {
            params: vec![Type::Var(b)],
            ret: Box::new(Type::Var(b)),
            caps: CapRow::Concrete(ridge_types::CapabilitySet::PURE),
        };

        let scheme = generalise(&mut ctx, &body);
        assert!(
            scheme.vars.contains(&b),
            "?b not in env; must be generalised, got {:?}",
            scheme.vars
        );
        ctx.env.pop_frame();
    }

    // ── Test T7-3 ─────────────────────────────────────────────────────────────
    // let_polymorphic_id:
    // Bind `id` = fn x -> x in an empty env. Call id with an Int, then with
    // Text. Both calls must succeed (no errors), and the two call results must
    // resolve to Int and Text respectively.

    #[test]
    fn let_polymorphic_id_both_types_succeed() {
        use crate::infer::infer_expr;
        use ridge_ast::{Ident, LambdaParam, Literal, Pattern, Span};
        use ridge_types::{BuiltinTyCons, TyConArena};

        fn ds() -> Span {
            Span::point(0)
        }
        fn id(t: &str) -> Ident {
            Ident {
                text: t.to_string(),
                span: ds(),
            }
        }

        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // let id_fn = fn x -> x
        let lambda = ridge_ast::Expr::Lambda {
            params: vec![LambdaParam::Pattern(Pattern::Var {
                name: id("x"),
                span: ds(),
            })],
            body: Box::new(ridge_ast::Expr::Ident(id("x"))),
            span: ds(),
        };

        // Infer the lambda's type.
        let lambda_ty = infer_expr(&mut ctx, &b, &lambda);
        // Generalise it (as a let-binding would).
        let scheme = generalise(&mut ctx, &lambda_ty);
        // After generalisation, the scheme must have at least 1 var.
        assert!(!scheme.vars.is_empty(), "identity fn must be polymorphic");

        // Bind `id` to the generalised scheme.
        ctx.env.bind("id".to_string(), scheme);

        // Call `id 5` — must produce Int.
        let call_int = ridge_ast::Expr::Call {
            callee: Box::new(ridge_ast::Expr::Ident(id("id"))),
            args: vec![ridge_ast::Expr::Literal(Literal::IntDec {
                raw: "5".to_string(),
                span: ds(),
            })],
            span: ds(),
        };
        let t1 = infer_expr(&mut ctx, &b, &call_int);
        assert!(
            ctx.errors.is_empty(),
            "call id 5 must not error; got {:?}",
            ctx.errors
        );
        let r1 = ctx.deep_resolve(&t1);
        assert!(
            matches!(r1, Type::Con(id, _) if id == b.int),
            "id 5 must be Int, got {r1:?}"
        );

        // Call `id "hi"` — must produce Text.
        let call_text = ridge_ast::Expr::Call {
            callee: Box::new(ridge_ast::Expr::Ident(id("id"))),
            args: vec![ridge_ast::Expr::Literal(Literal::Text {
                raw: r#""hi""#.to_string(),
                span: ds(),
            })],
            span: ds(),
        };
        let t2 = infer_expr(&mut ctx, &b, &call_text);
        assert!(
            ctx.errors.is_empty(),
            "call id \"hi\" must not error; got {:?}",
            ctx.errors
        );
        let r2 = ctx.deep_resolve(&t2);
        assert!(
            matches!(r2, Type::Con(id, _) if id == b.text),
            "id \"hi\" must be Text, got {r2:?}"
        );

        // Make sure the two results are distinct types (no accidental unification).
        assert_ne!(
            format!("{r1:?}"),
            format!("{r2:?}"),
            "Int and Text must be distinct"
        );

        ctx.env.pop_frame();
    }

    // ── Test T7-4 ─────────────────────────────────────────────────────────────
    // let_monomorphic_var_lambda:
    // `var f = fn x -> x; f 5` must succeed and type as Int.
    // A second `f "hi"` must fail with T001 because `var` binds monomorphically.

    #[test]
    fn let_monomorphic_var_does_not_generalise() {
        use crate::infer::infer_expr;
        use ridge_ast::{Ident, LambdaParam, Literal, Pattern, Span};
        use ridge_types::{BuiltinTyCons, TyConArena};

        fn ds() -> Span {
            Span::point(0)
        }
        fn id(t: &str) -> Ident {
            Ident {
                text: t.to_string(),
                span: ds(),
            }
        }

        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // var f = fn x -> x  (monoscheme — no generalisation)
        let var_expr = ridge_ast::Expr::Var {
            name: id("f"),
            ty: None,
            value: Box::new(ridge_ast::Expr::Lambda {
                params: vec![LambdaParam::Pattern(Pattern::Var {
                    name: id("x"),
                    span: ds(),
                })],
                body: Box::new(ridge_ast::Expr::Ident(id("x"))),
                span: ds(),
            }),
            span: ds(),
        };
        infer_expr(&mut ctx, &b, &var_expr);
        assert!(ctx.errors.is_empty(), "var f = fn x -> x must not error");

        // f 5 — first call, unifies ?param with Int. No error expected.
        let call_int = ridge_ast::Expr::Call {
            callee: Box::new(ridge_ast::Expr::Ident(id("f"))),
            args: vec![ridge_ast::Expr::Literal(Literal::IntDec {
                raw: "5".to_string(),
                span: ds(),
            })],
            span: ds(),
        };
        infer_expr(&mut ctx, &b, &call_int);
        assert!(ctx.errors.is_empty(), "f 5 must not error on first call");

        // f "hi" — second call, must fail with T001 because f is monomorphic.
        let call_text = ridge_ast::Expr::Call {
            callee: Box::new(ridge_ast::Expr::Ident(id("f"))),
            args: vec![ridge_ast::Expr::Literal(Literal::Text {
                raw: r#""hi""#.to_string(),
                span: ds(),
            })],
            span: ds(),
        };
        infer_expr(&mut ctx, &b, &call_text);
        let has_err = ctx.errors.iter().any(|e| e.code() == "T001");
        assert!(
            has_err,
            "f \"hi\" must fail T001 for mono var; errors: {:?}",
            ctx.errors
        );

        ctx.env.pop_frame();
    }

    // ── Test T7-5 ─────────────────────────────────────────────────────────────
    // deep_resolve_chain:
    // v0 → v1 → Int; deep_resolve(v0) must return Int.

    #[test]
    fn deep_resolve_follows_chain() {
        use crate::ctx::TyValue;
        use crate::ctx::TyVidKey;
        use ridge_types::{BuiltinTyCons, TyConArena};

        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let mut ctx = InferCtx::new();

        let v0 = ctx.fresh_tyvid();
        let v1 = ctx.fresh_tyvid();
        let int_ty = Type::Con(b.int, vec![]);

        // Bind v0 → v1, v1 → Int.
        ctx.tyvids
            .union_value(TyVidKey(v0.0), TyValue(Some(Type::Var(v1))));
        ctx.tyvids
            .union_value(TyVidKey(v1.0), TyValue(Some(int_ty)));

        let resolved = ctx.deep_resolve(&Type::Var(v0));
        assert!(
            matches!(resolved, Type::Con(id, _) if id == b.int),
            "deep_resolve must follow chain v0→v1→Int, got {resolved:?}"
        );
    }

    // ── Test T7-6 ─────────────────────────────────────────────────────────────
    // deep_resolve_fn_type:
    // fn (?a) -> ?b where ?a = Int and ?b is free; deep_resolve gives
    // fn (Int) -> ?b.

    #[test]
    fn deep_resolve_fn_type_partially_resolved() {
        use crate::ctx::TyValue;
        use crate::ctx::TyVidKey;
        use ridge_types::{BuiltinTyCons, TyConArena};

        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let mut ctx = InferCtx::new();

        let va = ctx.fresh_tyvid();
        let vb = ctx.fresh_tyvid();
        let _int_ty = Type::Con(b.int, vec![]);

        // Bind va → Int; vb stays free.
        ctx.tyvids
            .union_value(TyVidKey(va.0), TyValue(Some(Type::Con(b.int, vec![]))));

        let fn_ty = Type::Fn {
            params: vec![Type::Var(va)],
            ret: Box::new(Type::Var(vb)),
            caps: CapRow::Concrete(ridge_types::CapabilitySet::PURE),
        };

        let resolved = ctx.deep_resolve(&fn_ty);
        match resolved {
            Type::Fn { params, ret, .. } => {
                assert!(
                    matches!(&params[0], Type::Con(id, _) if *id == b.int),
                    "param must resolve to Int, got {:?}",
                    params[0]
                );
                // ret is still a free Var.
                assert!(
                    matches!(*ret, Type::Var(_)),
                    "free ret must stay Var, got {ret:?}"
                );
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    // ── Test T7-7 ─────────────────────────────────────────────────────────────
    // collect_free_vars_tuple:
    // (?a, Int, ?b) has two free vars ?a and ?b, not the Int.

    #[test]
    fn collect_free_vars_tuple() {
        let a = TyVid(10);
        let b = TyVid(20);
        let ty = Type::Tuple(vec![
            Type::Var(a),
            Type::Con(ridge_types::TyConId(0), vec![]),
            Type::Var(b),
        ]);
        let (free_ty, free_cap) = collect_free_vars(&ty);
        assert!(free_ty.contains(&a), "a must be free");
        assert!(free_ty.contains(&b), "b must be free");
        assert_eq!(free_ty.len(), 2, "exactly 2 free ty vars");
        assert!(free_cap.is_empty(), "no free cap vars");
    }

    // ── Test T7-8 ─────────────────────────────────────────────────────────────
    // env_free_tyvids_collects_across_frames:
    // push two frames each with a mono scheme over a different var.
    // env_free_tyvids must return both.

    #[test]
    fn env_free_tyvids_collects_across_frames() {
        let mut ctx = InferCtx::new();

        let v0 = ctx.fresh_tyvid();
        let v1 = ctx.fresh_tyvid();

        ctx.env.push_frame();
        ctx.env
            .bind("a".to_string(), ridge_types::Scheme::mono(Type::Var(v0)));

        ctx.env.push_frame();
        ctx.env
            .bind("b".to_string(), ridge_types::Scheme::mono(Type::Var(v1)));

        let free = ctx.env_free_tyvids();
        assert!(free.contains(&v0), "v0 must be in env free vars");
        assert!(free.contains(&v1), "v1 must be in env free vars");

        ctx.env.pop_frame();
        ctx.env.pop_frame();
    }

    // ── Record field generalisation (R3 dropped P029) ─────────────────────────

    #[test]
    fn collect_free_vars_walks_record_fields() {
        let a = TyVid(7);
        let rec = Type::record(
            vec![
                ("x".to_string(), Type::Var(a)),
                ("y".to_string(), Type::Con(cid(0), vec![])),
            ],
            ridge_types::RowTail::Closed,
        );
        let (free_ty, _) = collect_free_vars(&rec);
        assert!(free_ty.contains(&a), "field var `a` must be free");
        assert_eq!(free_ty.len(), 1, "only `a` is free");
    }

    #[test]
    fn generalise_collects_record_field_vars() {
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let a = ctx.fresh_tyvid();
        let rec = Type::record(
            vec![("x".to_string(), Type::Var(a))],
            ridge_types::RowTail::Closed,
        );
        let scheme = generalise(&mut ctx, &rec);
        assert!(
            scheme.vars.contains(&a),
            "a record field var must be generalised, got {:?}",
            scheme.vars
        );
        ctx.env.pop_frame();
    }
}
