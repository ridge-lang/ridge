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
//! - Both operands are shallow-resolved before dispatch (aliases are
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

use std::cmp::Ordering;

use ridge_ast::Span;
use ridge_types::{CapRow, Row, RowTail, RowVid, TyVid, Type};

use crate::ctx::{CapValue, CapVidKey, InferCtx, RowValue, RowVidKey, TyValue, TyVidKey};
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
    // `Ret (fn … -> r)` is the return-type extractor: when its argument is a
    // concrete function type it reduces to that function's return. Returns
    // `None` for anything else — including `Ret ?p` whose argument is still a
    // variable, which stays a stuck projection until `p` is pinned.
    fn reduce_ret(ctx: &mut InferCtx, t: &Type) -> Option<Type> {
        let Type::Con(id, args) = t else {
            return None;
        };
        if id.0 != ridge_types::RET_TYCON_ID || args.len() != 1 {
            return None;
        }
        match ctx.shallow_resolve(&args[0]) {
            Type::Fn { ret, .. } => Some(*ret),
            _ => None,
        }
    }

    let a = ctx.shallow_resolve(a);
    let b = ctx.shallow_resolve(b);

    // Reduce a top-level `Ret` on either side, then retry. An unreducible `Ret`
    // (argument not yet a function) falls through to the structural `Con/Con`
    // arm, where `Ret p ~ Ret q` unifies the arguments.
    if let Some(reduced) = reduce_ret(ctx, &a) {
        return unify(ctx, &reduced, &b);
    }
    if let Some(reduced) = reduce_ret(ctx, &b) {
        return unify(ctx, &a, &reduced);
    }

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
                let hint = curry_hint(ps.len(), qs.len(), s);
                return Err(TypeError::ArityMismatch {
                    callee: String::new(),
                    expected: ps.len(),
                    found: qs.len(),
                    span: dummy_span(),
                    hint,
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
                    hint: None,
                });
            }
            let xs = xs.clone();
            let ys = ys.clone();
            for (x, y) in xs.iter().zip(ys.iter()) {
                unify(ctx, x, y)?;
            }
            Ok(())
        }

        // ── Two structural records ────────────────────────────────────────────
        (
            Type::Record {
                fields: f1,
                tail: t1,
            },
            Type::Record {
                fields: f2,
                tail: t2,
            },
        ) => {
            let f1 = f1.clone();
            let t1 = t1.clone();
            let f2 = f2.clone();
            let t2 = t2.clone();
            unify_rows(ctx, &f1, &t1, &f2, &t2)
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
/// recursively. `Type::Alias` is traversed via its body (transparent).
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
        // Record: the TyVid can only hide in the field types — the tail is a
        // RowVid, a different namespace.
        Type::Record { fields, .. } => fields.iter().any(|(_, t)| occurs(ctx, v, t)),
        // Alias is transparent; walk the body.
        Type::Alias { body, .. } => occurs(ctx, v, body),
        // Type is #[non_exhaustive] — wildcard for forward-compat (including Error).
        _ => false,
    }
}

// ── Row unification (Rémy-style) ────────────────────────────────────────────────

/// Unifies two record rows using Rémy-style row unification.
///
/// Both rows are peeled first (`InferCtx::resolve_row`) so every currently-known
/// field is visible and each tail is `Closed` or an *unbound* root row var.
/// Fields split into `common` (in both — their types are unified), `only1` (left
/// only), and `only2` (right only), then the four tail combinations dispatch:
///
/// | left \ right | `Closed`                          | `Open(ρ2)`                                   |
/// |--------------|-----------------------------------|----------------------------------------------|
/// | `Closed`     | `only1` and `only2` must be empty  | `only2` empty; bind `ρ2 := { only1 \| Closed }` |
/// | `Open(ρ1)`   | `only1` empty; bind `ρ1 := { only2 \| Closed }` | fresh `ρ`; bind `ρ1 := { only2 \| ρ }`, `ρ2 := { only1 \| ρ }` |
///
/// A side the opposite tail cannot absorb is a row mismatch. The same-variable
/// `Open(ρ)/Open(ρ)` case requires both extras empty (the shared tail cannot
/// expand two ways).
pub fn unify_rows(
    ctx: &mut InferCtx,
    fields1: &[(String, Type)],
    tail1: &RowTail,
    fields2: &[(String, Type)],
    tail2: &RowTail,
) -> Result<(), TypeError> {
    let (mut f1, t1) = ctx.resolve_row(fields1, tail1);
    let (mut f2, t2) = ctx.resolve_row(fields2, tail2);
    f1.sort_by(|a, b| a.0.cmp(&b.0));
    f2.sort_by(|a, b| a.0.cmp(&b.0));

    // Merge-split the two sorted field lists by label.
    let mut common: Vec<(Type, Type)> = Vec::new();
    let mut only1: Vec<(String, Type)> = Vec::new();
    let mut only2: Vec<(String, Type)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < f1.len() && j < f2.len() {
        match f1[i].0.cmp(&f2[j].0) {
            Ordering::Less => {
                only1.push(f1[i].clone());
                i += 1;
            }
            Ordering::Greater => {
                only2.push(f2[j].clone());
                j += 1;
            }
            Ordering::Equal => {
                common.push((f1[i].1.clone(), f2[j].1.clone()));
                i += 1;
                j += 1;
            }
        }
    }
    only1.extend_from_slice(&f1[i..]);
    only2.extend_from_slice(&f2[j..]);

    // Common labels must agree on their field types.
    for (a, b) in &common {
        unify(ctx, a, b)?;
    }

    match (&t1, &t2) {
        (RowTail::Closed, RowTail::Closed) => {
            if only1.is_empty() && only2.is_empty() {
                Ok(())
            } else {
                Err(row_mismatch(&f1, &t1, &f2, &t2, &only1, &only2))
            }
        }
        // Left is exact: it cannot carry the right's extra explicit fields, but
        // the right's open tail absorbs the left's extras.
        (RowTail::Closed, RowTail::Open(rv2)) => {
            if only2.is_empty() {
                bind_row(ctx, *rv2, only1, RowTail::Closed)
            } else {
                Err(row_mismatch(&f1, &t1, &f2, &t2, &only1, &only2))
            }
        }
        (RowTail::Open(rv1), RowTail::Closed) => {
            if only1.is_empty() {
                bind_row(ctx, *rv1, only2, RowTail::Closed)
            } else {
                Err(row_mismatch(&f1, &t1, &f2, &t2, &only1, &only2))
            }
        }
        (RowTail::Open(rv1), RowTail::Open(rv2)) => {
            if rv1.0 == rv2.0 {
                // Same tail var: the explicit parts must already match exactly,
                // otherwise the shared var would have to expand two ways.
                if only1.is_empty() && only2.is_empty() {
                    Ok(())
                } else {
                    Err(row_mismatch(&f1, &t1, &f2, &t2, &only1, &only2))
                }
            } else {
                let fresh = ctx.fresh_rowvid();
                bind_row(ctx, *rv1, only2, RowTail::Open(fresh))?;
                bind_row(ctx, *rv2, only1, RowTail::Open(fresh))
            }
        }
        // RowTail is #[non_exhaustive] — forward-compat wildcard.
        _ => Err(row_mismatch(&f1, &t1, &f2, &t2, &only1, &only2)),
    }
}

/// Binds an unbound row var `rv` to the row `{ fields | tail }`, after an
/// occurs check that rejects an infinite row.
fn bind_row(
    ctx: &mut InferCtx,
    rv: RowVid,
    fields: Vec<(String, Type)>,
    tail: RowTail,
) -> Result<(), TypeError> {
    if row_occurs(ctx, rv, &fields, &tail) {
        return Err(TypeError::OccursCheck {
            var: format!("?r{}", rv.0),
            ty: format!("{}", Type::record(fields, tail)),
            span: dummy_span(),
        });
    }
    ctx.rowvids
        .union_value(RowVidKey(rv.0), RowValue(Some(Row::new(fields, tail))));
    Ok(())
}

/// Returns `true` if row var `rv` occurs in `{ fields | tail }` — directly as
/// the tail or transitively inside a field type — which would make the row
/// infinite.
fn row_occurs(ctx: &mut InferCtx, rv: RowVid, fields: &[(String, Type)], tail: &RowTail) -> bool {
    if let RowTail::Open(t) = tail {
        let rv_root = ctx.rowvids.find(RowVidKey(rv.0));
        let t_root = ctx.rowvids.find(RowVidKey(t.0));
        if rv_root == t_root {
            return true;
        }
        // Bound tail var: follow its row.
        if let Some(row) = ctx.rowvids.probe_value(t_root).0 {
            if row_occurs(ctx, rv, &row.fields, &row.tail) {
                return true;
            }
        }
    }
    for (_, ty) in fields {
        if ty_occurs_rowvid(ctx, rv, ty) {
            return true;
        }
    }
    false
}

/// Returns `true` if row var `rv` occurs anywhere in type `t`.
fn ty_occurs_rowvid(ctx: &mut InferCtx, rv: RowVid, t: &Type) -> bool {
    let t = ctx.shallow_resolve(t);
    match &t {
        Type::Record { fields, tail } => row_occurs(ctx, rv, fields, tail),
        Type::Con(_, args) => args.iter().any(|a| ty_occurs_rowvid(ctx, rv, a)),
        Type::Fn { params, ret, .. } => {
            params.iter().any(|p| ty_occurs_rowvid(ctx, rv, p)) || ty_occurs_rowvid(ctx, rv, ret)
        }
        Type::Tuple(ts) => ts.iter().any(|x| ty_occurs_rowvid(ctx, rv, x)),
        Type::Alias { body, .. } => ty_occurs_rowvid(ctx, rv, body),
        _ => false,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Builds a `T037 RowMismatch` from two record rows that failed to unify.
///
/// `f1`/`t1` is the expected row, `f2`/`t2` the found row (the `unify(a, b)`
/// orientation). `only1` are the expected-only labels (missing from the found
/// row) and `only2` the found-only labels (not allowed by the expected row).
fn row_mismatch(
    f1: &[(String, Type)],
    t1: &RowTail,
    f2: &[(String, Type)],
    t2: &RowTail,
    only1: &[(String, Type)],
    only2: &[(String, Type)],
) -> TypeError {
    TypeError::RowMismatch {
        expected: format!("{}", Type::record(f1.to_vec(), t1.clone())),
        found: format!("{}", Type::record(f2.to_vec(), t2.clone())),
        missing_fields: only1.iter().map(|(label, _)| label.clone()).collect(),
        extra_fields: only2.iter().map(|(label, _)| label.clone()).collect(),
        span: dummy_span(),
    }
}

/// Constructs a `T001 TypeMismatch` error with a dummy span.
///
/// # T001 rendering note
///
/// `Type::Display` (the `{expected}` / `{found}` format) renders `Type::Con`
/// as `#N` — it has no arena access.  Named records therefore print as `#N`
/// today; anon records inherit the same limitation.  To render structural
/// shapes here the arena would need to be threaded into `UnifyCtx` and this
/// helper.  That is a non-trivial refactor deferred to a follow-on cut:
// TODO(0.2.12/T7b): thread the arena into UnifyCtx so anon records render
// as `{ … }` in T001 strings rather than `#N`.
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

/// Build an optional T003 hint when the "got" side of an arity mismatch on
/// `Type::Fn` looks like a curried chain of single-argument functions whose
/// total length matches the "expected" side.  The classic shape is
/// `List.fold (fn acc -> fn x -> acc + x) 0 xs`, where the lambda is
/// `Fn{[a], ret: Fn{[b], …}}` (1-arg returning a 1-arg fn) and `List.fold`
/// expects `Fn{[b, a], …}` (uncurried 2-arg).
fn curry_hint(expected_arity: usize, found_arity: usize, found_ret: &Type) -> Option<String> {
    if expected_arity <= 1 || found_arity != 1 {
        return None;
    }
    let mut chain_len = 1usize;
    let mut cursor = found_ret;
    while let Type::Fn { params, ret, .. } = cursor {
        if params.len() != 1 {
            return None;
        }
        chain_len += 1;
        if chain_len == expected_arity {
            return Some(format!(
                "the argument is a curried `fn x1 -> fn x2 -> … -> body` chain ({chain_len} single-arg lambdas); pass an uncurried `fn x1 x2 -> body` ({chain_len}-arg lambda) instead"
            ));
        }
        cursor = ret;
    }
    None
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
    // Ret/1 — the return-type extractor. `Ret (fn … -> r)` reduces to `r`.
    // ─────────────────────────────────────────────────────────────────────────

    fn pure_fn(params: Vec<Type>, ret: Type) -> Type {
        Type::Fn {
            params,
            ret: Box::new(ret),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        }
    }

    fn ret_of(arg: Type) -> Type {
        Type::Con(cid(ridge_types::RET_TYCON_ID), vec![arg])
    }

    /// `deep_resolve(Ret (fn User -> Summary))` reduces to `Summary`.
    #[test]
    fn ret_of_concrete_fn_reduces_in_deep_resolve() {
        let mut ctx = make_ctx();
        let summary = Type::Con(cid(101), vec![]);
        let proj = pure_fn(vec![Type::Con(cid(100), vec![])], summary.clone());
        let resolved = ctx.deep_resolve(&ret_of(proj));
        assert_eq!(format!("{resolved:?}"), format!("{summary:?}"));
    }

    /// The keystone: a result type `Result (List (Ret p))` resolves to
    /// `Result (List Summary)` once the projection arg pins `p = fn User ->
    /// Summary` — the result-element linkage the unified `select` relies on.
    #[test]
    fn ret_links_projection_return_to_result() {
        let mut ctx = make_ctx();
        let user = Type::Con(cid(100), vec![]);
        let summary = Type::Con(cid(101), vec![]);
        let err = Type::Con(cid(12), vec![]);
        let p = ctx.fresh_tyvid();
        // Arg unification pins `p` before the call result is consumed.
        unify(
            &mut ctx,
            &Type::Var(p),
            &pure_fn(vec![user], summary.clone()),
        )
        .unwrap();

        let result_ty = Type::Con(
            cid(10),
            vec![Type::Con(cid(6), vec![ret_of(Type::Var(p))]), err.clone()],
        );
        let expected = Type::Con(cid(10), vec![Type::Con(cid(6), vec![summary]), err]);
        let resolved = ctx.deep_resolve(&result_ty);
        assert_eq!(format!("{resolved:?}"), format!("{expected:?}"));
    }

    /// `Ret ?p` with `p` still unbound is carried intact (a stuck projection),
    /// not reduced and not an error — so a wrapper that stays polymorphic over
    /// the projection can generalise with `Ret p` in its scheme.
    #[test]
    fn ret_of_unpinned_var_stays_stuck() {
        let mut ctx = make_ctx();
        let p = ctx.fresh_tyvid();
        let listed = Type::Con(cid(6), vec![ret_of(Type::Var(p))]);
        match ctx.deep_resolve(&listed) {
            Type::Con(outer, args) => {
                assert_eq!(outer, cid(6));
                assert!(
                    matches!(&args[0], Type::Con(r, _) if r.0 == ridge_types::RET_TYCON_ID),
                    "Ret over an unpinned var must stay a Ret application"
                );
            }
            other => panic!("expected List(Ret ?p) to be carried, got {other:?}"),
        }
    }

    /// Two stuck projections unify structurally (`Ret p ~ Ret q` ⟹ `p ~ q`):
    /// pinning one then pins the other. No spurious mismatch.
    #[test]
    fn ret_unifies_structurally_when_both_stuck() {
        let mut ctx = make_ctx();
        let p = ctx.fresh_tyvid();
        let q = ctx.fresh_tyvid();
        unify(&mut ctx, &ret_of(Type::Var(p)), &ret_of(Type::Var(q))).unwrap();
        let summary = Type::Con(cid(101), vec![]);
        let proj = pure_fn(vec![], summary);
        unify(&mut ctx, &Type::Var(p), &proj).unwrap();
        let resolved = ctx.deep_resolve(&Type::Var(q));
        assert_eq!(format!("{resolved:?}"), format!("{proj:?}"));
    }

    /// `Ret (fn … -> Summary)` unified against a fresh var reduces first, binding
    /// the var to `Summary`.
    #[test]
    fn ret_reduces_during_unify_against_var() {
        let mut ctx = make_ctx();
        let summary = Type::Con(cid(101), vec![]);
        let proj = pure_fn(vec![Type::Con(cid(100), vec![])], summary.clone());
        let r = ctx.fresh_tyvid();
        unify(&mut ctx, &ret_of(proj), &Type::Var(r)).unwrap();
        let resolved = ctx.deep_resolve(&Type::Var(r));
        assert_eq!(format!("{resolved:?}"), format!("{summary:?}"));
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

    // T9b — uncurried Fn vs curried 1-arg → curry hint in T003
    // The "got" side is `fn a -> fn b -> c` (1-arg returning 1-arg), the
    // "expected" side is `fn a b -> c` (uncurried 2-arg).  Arity counts
    // differ; the hint should explain the curried-vs-uncurried mismatch.
    #[test]
    fn fn_fn_curry_hint_emitted() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let expected = Type::Fn {
            params: vec![int.clone(), int.clone()],
            ret: Box::new(int.clone()),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let found = Type::Fn {
            params: vec![int.clone()],
            ret: Box::new(Type::Fn {
                params: vec![int.clone()],
                ret: Box::new(int),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            }),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let err = unify(&mut ctx, &expected, &found).unwrap_err();
        assert_eq!(err.code(), "T003");
        let TypeError::ArityMismatch { hint, .. } = err else {
            panic!("expected ArityMismatch, got {err:?}");
        };
        let h = hint.expect("expected a curry hint");
        assert!(
            h.contains("curried") && h.contains("uncurried"),
            "hint should mention curry vs uncurry: {h}"
        );
    }

    // T9c — non-curried mismatched arities do NOT emit the hint.
    // The "got" side has 1-arg but returns a non-Fn type, so it isn't a
    // curried chain.  The hint must remain `None` so we don't mislead.
    #[test]
    fn fn_fn_non_curried_mismatch_no_hint() {
        let mut ctx = make_ctx();
        let int = Type::Con(cid(0), vec![]);
        let expected = Type::Fn {
            params: vec![int.clone(), int.clone()],
            ret: Box::new(int.clone()),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let found = Type::Fn {
            params: vec![int.clone()],
            ret: Box::new(int),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        let err = unify(&mut ctx, &expected, &found).unwrap_err();
        let TypeError::ArityMismatch { hint, .. } = err else {
            panic!("expected ArityMismatch");
        };
        assert!(hint.is_none(), "hint should not fire for non-curried fn");
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
    // T14 — (Alias{Int}, Int) → unifies (transparent)
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
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(Type::Var(a)),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
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
            &mut || ridge_types::RowVid(0),
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
            &mut || ridge_types::RowVid(0),
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

    // ─────────────────────────────────────────────────────────────────────────
    // Row unification — the full Rémy table (R2)
    // ─────────────────────────────────────────────────────────────────────────

    fn con(n: u32) -> Type {
        Type::Con(cid(n), vec![])
    }

    fn rec(fields: &[(&str, Type)], tail: RowTail) -> Type {
        Type::record(
            fields
                .iter()
                .map(|(l, t)| ((*l).to_string(), t.clone()))
                .collect(),
            tail,
        )
    }

    fn labels(ty: &Type) -> Vec<String> {
        match ty {
            Type::Record { fields, .. } => fields.iter().map(|(l, _)| l.clone()).collect(),
            other => panic!("expected Record, got {other:?}"),
        }
    }

    // Closed/Closed, equal field sets (order-insensitive) → unifies.
    #[test]
    fn rows_closed_closed_equal_unifies() {
        let mut ctx = make_ctx();
        let a = rec(&[("x", con(0)), ("y", con(1))], RowTail::Closed);
        let b = rec(&[("y", con(1)), ("x", con(0))], RowTail::Closed);
        assert!(unify(&mut ctx, &a, &b).is_ok());
    }

    // Closed/Closed with an extra field → mismatch.
    #[test]
    fn rows_closed_closed_extra_field_mismatches() {
        let mut ctx = make_ctx();
        let a = rec(&[("x", con(0))], RowTail::Closed);
        let b = rec(&[("x", con(0)), ("y", con(1))], RowTail::Closed);
        assert_eq!(unify(&mut ctx, &a, &b).unwrap_err().code(), "T037");
    }

    // A shared label with conflicting field types → mismatch.
    #[test]
    fn rows_common_field_type_mismatch() {
        let mut ctx = make_ctx();
        let a = rec(&[("x", con(0))], RowTail::Closed);
        let b = rec(&[("x", con(1))], RowTail::Closed);
        assert!(unify(&mut ctx, &a, &b).is_err());
    }

    // A shared label unifies its field types (binds a var).
    #[test]
    fn rows_common_field_binds_var() {
        let mut ctx = make_ctx();
        let v = ctx.fresh_tyvid();
        let a = rec(&[("x", Type::Var(v))], RowTail::Closed);
        let b = rec(&[("x", con(1))], RowTail::Closed);
        assert!(unify(&mut ctx, &a, &b).is_ok());
        assert!(matches!(
            ctx.shallow_resolve(&Type::Var(v)),
            Type::Con(TyConId(1), _)
        ));
    }

    // Closed/Open: the open right tail absorbs the closed left's extra field.
    #[test]
    fn rows_closed_open_absorbs_into_right_tail() {
        let mut ctx = make_ctx();
        let rho = ctx.fresh_rowvid();
        let a = rec(&[("x", con(0)), ("y", con(1))], RowTail::Closed);
        let b = rec(&[("x", con(0))], RowTail::Open(rho));
        assert!(unify(&mut ctx, &a, &b).is_ok());
        let resolved = ctx.deep_resolve(&b);
        assert_eq!(labels(&resolved), ["x", "y"]);
        assert!(matches!(
            resolved,
            Type::Record {
                tail: RowTail::Closed,
                ..
            }
        ));
    }

    // Open/Closed: symmetric — the open left tail absorbs the closed right's extra.
    #[test]
    fn rows_open_closed_absorbs_into_left_tail() {
        let mut ctx = make_ctx();
        let rho = ctx.fresh_rowvid();
        let a = rec(&[("x", con(0))], RowTail::Open(rho));
        let b = rec(&[("x", con(0)), ("y", con(1))], RowTail::Closed);
        assert!(unify(&mut ctx, &a, &b).is_ok());
        assert_eq!(labels(&ctx.deep_resolve(&a)), ["x", "y"]);
    }

    // Closed/Open where the open side has an extra *explicit* field the closed
    // side lacks → mismatch (the closed side is exact).
    #[test]
    fn rows_closed_open_extra_explicit_field_mismatches() {
        let mut ctx = make_ctx();
        let rho = ctx.fresh_rowvid();
        let a = rec(&[("x", con(0))], RowTail::Closed);
        let b = rec(&[("x", con(0)), ("y", con(1))], RowTail::Open(rho));
        assert_eq!(unify(&mut ctx, &a, &b).unwrap_err().code(), "T037");
    }

    // Open/Open, distinct tail vars: both rows expand to the union, sharing a
    // fresh tail.
    #[test]
    fn rows_open_open_distinct_vars_merge() {
        let mut ctx = make_ctx();
        let r1 = ctx.fresh_rowvid();
        let r2 = ctx.fresh_rowvid();
        let a = rec(&[("x", con(0))], RowTail::Open(r1));
        let b = rec(&[("y", con(1))], RowTail::Open(r2));
        assert!(unify(&mut ctx, &a, &b).is_ok());
        let ra = ctx.deep_resolve(&a);
        let rb = ctx.deep_resolve(&b);
        assert_eq!(labels(&ra), ["x", "y"]);
        assert_eq!(labels(&rb), ["x", "y"]);
        assert!(matches!(
            ra,
            Type::Record {
                tail: RowTail::Open(_),
                ..
            }
        ));
    }

    // Open/Open, same tail var: the explicit parts must already match.
    #[test]
    fn rows_open_open_same_var_distinct_fields_mismatches() {
        let mut ctx = make_ctx();
        let rho = ctx.fresh_rowvid();
        let a = rec(&[("x", con(0))], RowTail::Open(rho));
        let b = rec(&[("y", con(1))], RowTail::Open(rho));
        assert!(unify(&mut ctx, &a, &b).is_err());
    }

    #[test]
    fn rows_open_open_same_var_equal_fields_ok() {
        let mut ctx = make_ctx();
        let rho = ctx.fresh_rowvid();
        let v = ctx.fresh_tyvid();
        let a = rec(&[("x", Type::Var(v))], RowTail::Open(rho));
        let b = rec(&[("x", con(1))], RowTail::Open(rho));
        assert!(unify(&mut ctx, &a, &b).is_ok());
        assert!(matches!(
            ctx.shallow_resolve(&Type::Var(v)),
            Type::Con(TyConId(1), _)
        ));
    }

    // Binding a row var to a row that contains it → occurs check rejects it.
    #[test]
    fn rows_occurs_check_rejects_infinite_row() {
        let mut ctx = make_ctx();
        let rho = ctx.fresh_rowvid();
        let inner = rec(&[], RowTail::Open(rho));
        let a = rec(&[], RowTail::Open(rho));
        let b = rec(&[("a", inner)], RowTail::Closed);
        let err = unify(&mut ctx, &a, &b).unwrap_err();
        assert!(matches!(err, TypeError::OccursCheck { .. }), "got {err:?}");
    }
}
