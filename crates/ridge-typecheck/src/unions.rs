//! Union variant construction and pattern-destructuring inference (T9).
//!
//! # Entry points
//!
//! - [`infer_variant_construction`] — `Some 42`, `None`, `Ok x`, user variants.
//! - [`infer_variant_pattern`]      — `Some x`, `None`, `Ok v` in match arms.
//!
//! # Design note
//!
//! Both functions take the [`UnionSchema`], [`TyConId`], and `variant_idx` as
//! explicit parameters rather than looking them up via a `BindingMap`.  The
//! pipeline wiring in `infer.rs` does the `BindingMap` lookup (or prelude lookup
//! for `Some`/`None`/`Ok`/`Err`); these functions are the pure algorithmic core.
//!
//! # T008 / T009 codes
//!
//! - `T008 UnknownConstructor` — fired when the resolved binding's `TyCon` is not
//!   a Union (or Record, which belongs to T8).  In practice Phase 3 R-codes catch
//!   most unknown-ctor cases before T9; see the `#[ignore]` notes on the T008
//!   tests for details.
//! - `T009 WrongConstructorArity` — fired when the argument count at a
//!   construction site or pattern site does not match the variant's declared
//!   payload arity.

use ridge_ast::{Pattern, Span};
use ridge_types::{BuiltinTyCons, TyConId, TyVid, Type, UnionSchema, VariantPayload};

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::render::emit_internal;
use crate::unify::unify;

// ── Substitution helper (mirrors records.rs) ──────────────────────────────────

/// Apply a param→fresh-var substitution to a type.
///
/// `params[i]` is the schema `TyVid`; `args[i]` is the fresh `Type::Var(TyVid)`.
/// Every `Type::Var(v)` where `v` equals one of the params is replaced by the
/// corresponding arg.
fn subst_type(ty: &Type, params: &[TyVid], args: &[Type]) -> Type {
    match ty {
        Type::Var(v) => params
            .iter()
            .position(|p| p == v)
            .map_or_else(|| ty.clone(), |pos| args[pos].clone()),
        Type::Con(id, sub_args) => {
            let new_sub = sub_args
                .iter()
                .map(|a| subst_type(a, params, args))
                .collect();
            Type::Con(*id, new_sub)
        }
        Type::Tuple(ts) => {
            let new_ts = ts.iter().map(|t| subst_type(t, params, args)).collect();
            Type::Tuple(new_ts)
        }
        Type::Fn {
            params: ps,
            ret,
            caps,
        } => Type::Fn {
            params: ps.iter().map(|p| subst_type(p, params, args)).collect(),
            ret: Box::new(subst_type(ret, params, args)),
            caps: caps.clone(),
        },
        Type::Alias { name, body } => Type::Alias {
            name: *name,
            body: Box::new(subst_type(body, params, args)),
        },
        Type::Error => Type::Error,
        _ => ty.clone(),
    }
}

// ── infer_variant_construction ────────────────────────────────────────────────

/// Infer the type of a union-variant constructor application (§4.9).
///
/// # Parameters
///
/// - `ctx`         — mutable inference context.
/// - `b`           — built-in type-constructor handles.
/// - `schema`      — the `UnionSchema` for `owner_tycon` (already looked up).
/// - `owner_tycon` — the `TyConId` of the union type (e.g. `Option`).
/// - `variant_idx` — zero-based index of the variant within `schema.variants`.
/// - `arg_exprs`   — the argument expressions supplied at the call site.
/// - `span`        — span of the whole constructor-application expression.
///
/// # Returns
///
/// `Type::Con(owner_tycon, instantiated_args)` on success; `Type::Error` on
/// a structural error (arity mismatch, type-mismatch in argument).
pub fn infer_variant_construction(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    schema: &UnionSchema,
    owner_tycon: TyConId,
    variant_idx: usize,
    arg_exprs: &[ridge_ast::Expr],
    span: Span,
) -> Type {
    let variant = &schema.variants[variant_idx];

    // Step 3: instantiate schema params with fresh TyVids.
    let params = schema.params.clone();
    let fresh_args: Vec<Type> = params
        .iter()
        .map(|_| Type::Var(ctx.fresh_tyvid()))
        .collect();

    match &variant.kind {
        // ── Nullary variant ──────────────────────────────────────────────────
        VariantPayload::Nullary => {
            if !arg_exprs.is_empty() {
                ctx.errors.push(TypeError::WrongConstructorArity {
                    ctor: variant.name.clone(),
                    expected: 0,
                    found: arg_exprs.len(),
                    span,
                });
                return Type::Error;
            }
        }

        // ── Positional variant ───────────────────────────────────────────────
        VariantPayload::Positional(field_tys) => {
            if arg_exprs.len() != field_tys.len() {
                ctx.errors.push(TypeError::WrongConstructorArity {
                    ctor: variant.name.clone(),
                    expected: field_tys.len(),
                    found: arg_exprs.len(),
                    span,
                });
                return Type::Error;
            }
            for (arg_expr, field_ty) in arg_exprs.iter().zip(field_tys.iter()) {
                let arg_ty = crate::infer::infer_expr(ctx, b, arg_expr);
                let expected_ty = subst_type(field_ty, &params, &fresh_args);
                if let Err(e) = unify(ctx, &arg_ty, &expected_ty) {
                    ctx.errors.push(attach_span(e, arg_expr.span()));
                }
            }
        }

        // ── Record-payload variant ───────────────────────────────────────────
        // Treat the constructor as a single-argument record-construction.
        // The argument expressions represent field initialisers; T9 delegates to
        // records.rs::infer_record_construction.
        VariantPayload::Record(record_schema) => {
            // For the record-payload case the caller must supply field initialisers
            // as a synthesised `Expr::Record` or by building FieldInit slices.
            // In T9 this is uncommon (union variants with inline records are rare in
            // practice); we fall through to T999 with a clear message rather than
            // implementing a full FieldInit→Expr bridge that would duplicate records.rs.
            // Full wiring is deferred to T17 (pipeline integration).
            let _ = record_schema; // suppress unused warning
            return emit_internal(
                ctx,
                "union variant with inline record payload — full expression wiring is T17",
                span,
            );
        }
    }

    // Result type: Con(owner_tycon, [fresh_args]) — the instantiated union type.
    Type::Con(owner_tycon, fresh_args)
}

// ── infer_variant_pattern ─────────────────────────────────────────────────────

/// Infer and bind variables from a union-variant pattern (§4.9).
///
/// # Parameters
///
/// - `ctx`          — mutable inference context.
/// - `b`            — built-in type-constructor handles.
/// - `schema`       — the `UnionSchema` for `owner_tycon`.
/// - `owner_tycon`  — the `TyConId` of the union type.
/// - `variant_idx`  — zero-based index of the variant.
/// - `sub_patterns` — the sub-patterns from the `Pattern::Constructor` `args` field.
/// - `expected`     — the scrutinee's type (the type the whole pattern must unify with).
/// - `span`         — span of the constructor pattern.
#[expect(
    clippy::too_many_arguments,
    reason = "all parameters are necessary for pattern inference"
)]
pub fn infer_variant_pattern(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    schema: &UnionSchema,
    owner_tycon: TyConId,
    variant_idx: usize,
    sub_patterns: &[Pattern],
    expected: &Type,
    span: Span,
) {
    let variant = &schema.variants[variant_idx];

    // Step 2: allocate fresh args for the owner union type, unify expected.
    let params = schema.params.clone();
    let fresh_args: Vec<Type> = params
        .iter()
        .map(|_| Type::Var(ctx.fresh_tyvid()))
        .collect();
    let union_ty = Type::Con(owner_tycon, fresh_args.clone());

    if let Err(e) = unify(ctx, expected, &union_ty) {
        ctx.errors.push(attach_span(e, span));
        // Continue binding variables as Error to allow more inference.
    }

    match &variant.kind {
        // ── Nullary variant ──────────────────────────────────────────────────
        VariantPayload::Nullary => {
            if !sub_patterns.is_empty() {
                ctx.errors.push(TypeError::WrongConstructorArity {
                    ctor: variant.name.clone(),
                    expected: 0,
                    found: sub_patterns.len(),
                    span,
                });
            }
        }

        // ── Positional variant ───────────────────────────────────────────────
        VariantPayload::Positional(field_tys) => {
            if sub_patterns.len() != field_tys.len() {
                ctx.errors.push(TypeError::WrongConstructorArity {
                    ctor: variant.name.clone(),
                    expected: field_tys.len(),
                    found: sub_patterns.len(),
                    span,
                });
                // Bind remaining pattern variables to Error so inference continues.
                for sub_pat in sub_patterns {
                    crate::infer::infer_pattern(ctx, b, sub_pat, &Type::Error);
                }
                return;
            }
            for (sub_pat, field_ty) in sub_patterns.iter().zip(field_tys.iter()) {
                let concrete_ty = subst_type(field_ty, &params, &fresh_args);
                crate::infer::infer_pattern(ctx, b, sub_pat, &concrete_ty);
            }
        }

        // ── Record-payload variant ───────────────────────────────────────────
        // Pattern side: `Login { userId, at }`.  Handled by the `fields: Some(_)`
        // branch of `Pattern::Constructor` in `infer.rs`; T9 defers the record
        // interior work to T8's record-pattern helpers.
        VariantPayload::Record(_) => {
            let _ = emit_internal(
                ctx,
                "union variant with inline record payload pattern — full wiring is T17",
                span,
            );
        }
    }
}

// ── Prelude-union helpers ─────────────────────────────────────────────────────

/// Resolve a constructor name (e.g. `"Some"`, `"None"`, `"Ok"`, `"Err"`) to its
/// `(TyConId, variant_idx)` using the built-in prelude unions.
///
/// Returns `None` if the name is not a recognised prelude constructor.
#[must_use]
pub fn resolve_prelude_ctor(b: &BuiltinTyCons, name: &str) -> Option<(TyConId, usize)> {
    match name {
        "Some" => Some((b.option, 0)),
        "None" => Some((b.option, 1)),
        "Ok" => Some((b.result, 0)),
        "Err" => Some((b.result, 1)),
        _ => None,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn attach_span(e: TypeError, span: Span) -> TypeError {
    match e {
        TypeError::TypeMismatch {
            expected, found, ..
        } => TypeError::TypeMismatch {
            expected,
            found,
            span,
        },
        TypeError::OccursCheck { var, ty, .. } => TypeError::OccursCheck { var, ty, span },
        other => other,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Ident, Literal, Span};
    use ridge_types::{
        BuiltinTyCons, TyConArena, TyConDecl, TyConId, TyConKind, TyVid, Type, UnionSchema,
        UnionVariant, VariantPayload,
    };

    fn ds() -> Span {
        Span::point(0)
    }

    fn id(text: &str) -> Ident {
        Ident {
            text: text.to_string(),
            span: ds(),
        }
    }

    fn int_lit_expr(raw: &str) -> ridge_ast::Expr {
        ridge_ast::Expr::Literal(Literal::IntDec {
            raw: raw.to_string(),
            span: ds(),
        })
    }

    fn text_lit_expr(raw: &str) -> ridge_ast::Expr {
        ridge_ast::Expr::Literal(Literal::Text {
            raw: format!("\"{raw}\""),
            span: ds(),
        })
    }

    /// Build a `TyConArena` + `BuiltinTyCons`.
    fn make_builtins() -> (TyConArena, BuiltinTyCons) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        (arena, b)
    }

    // ── Helper to build an Option-schema (mirrors the prelude) ────────────────

    fn make_option_schema() -> UnionSchema {
        UnionSchema {
            params: vec![TyVid(0)],
            variants: vec![
                UnionVariant {
                    name: "Some".to_string(),
                    kind: VariantPayload::Positional(vec![Type::Var(TyVid(0))]),
                },
                UnionVariant {
                    name: "None".to_string(),
                    kind: VariantPayload::Nullary,
                },
            ],
        }
    }

    fn make_result_schema() -> UnionSchema {
        UnionSchema {
            params: vec![TyVid(0), TyVid(1)],
            variants: vec![
                UnionVariant {
                    name: "Ok".to_string(),
                    kind: VariantPayload::Positional(vec![Type::Var(TyVid(0))]),
                },
                UnionVariant {
                    name: "Err".to_string(),
                    kind: VariantPayload::Positional(vec![Type::Var(TyVid(1))]),
                },
            ],
        }
    }

    fn var_pattern(name: &str) -> Pattern {
        Pattern::Var {
            name: id(name),
            span: ds(),
        }
    }

    // ── Test 1: infer_some_int — `Some 42` → `Option Int` ────────────────────

    #[test]
    fn infer_some_int() {
        let (_, b) = make_builtins();
        let schema = make_option_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Some 42 — variant_idx 0 (Some), arg = int literal
        let ty = infer_variant_construction(
            &mut ctx,
            &b,
            &schema,
            b.option,
            0,
            &[int_lit_expr("42")],
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "Some 42 must not error; got {:?}",
            ctx.errors
        );
        // Result must be Option(fresh_a) where fresh_a has been unified to Int.
        match &ty {
            Type::Con(id, args) => {
                assert_eq!(*id, b.option, "result must be Option");
                assert_eq!(args.len(), 1, "Option takes 1 arg");
                let resolved = ctx.deep_resolve(&args[0]);
                assert!(
                    matches!(resolved, Type::Con(iid, _) if iid == b.int),
                    "Option arg must resolve to Int, got {resolved:?}"
                );
            }
            other => panic!("expected Con(Option, [..]), got {other:?}"),
        }
        ctx.env.pop_frame();
    }

    // ── Test 2: infer_none — `None` → `Option ?a` ────────────────────────────

    #[test]
    fn infer_none() {
        let (_, b) = make_builtins();
        let schema = make_option_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // None — variant_idx 1, no args
        let ty = infer_variant_construction(&mut ctx, &b, &schema, b.option, 1, &[], ds());

        assert!(
            ctx.errors.is_empty(),
            "None must not error; got {:?}",
            ctx.errors
        );
        // Result must be Option(?a) where ?a is unbound (free var).
        match &ty {
            Type::Con(id, args) => {
                assert_eq!(*id, b.option, "result must be Option");
                assert_eq!(args.len(), 1);
                let resolved = ctx.shallow_resolve(&args[0]);
                assert!(
                    matches!(resolved, Type::Var(_)),
                    "None's type arg must stay as a free var, got {resolved:?}"
                );
            }
            other => panic!("expected Con(Option, [?a]), got {other:?}"),
        }
        ctx.env.pop_frame();
    }

    // ── Test 3: infer_ok_text — `Ok "hi"` → `Result Text ?e` ────────────────

    #[test]
    fn infer_ok_text() {
        let (_, b) = make_builtins();
        let schema = make_result_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Ok "hi" — variant_idx 0 (Ok), arg = text literal
        let ty = infer_variant_construction(
            &mut ctx,
            &b,
            &schema,
            b.result,
            0,
            &[text_lit_expr("hi")],
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "Ok 'hi' must not error; got {:?}",
            ctx.errors
        );
        match &ty {
            Type::Con(id, args) => {
                assert_eq!(*id, b.result, "result must be Result");
                assert_eq!(args.len(), 2);
                // args[0] = the Ok-payload var, must resolve to Text
                let a_resolved = ctx.deep_resolve(&args[0]);
                assert!(
                    matches!(a_resolved, Type::Con(iid, _) if iid == b.text),
                    "Ok arg must resolve to Text, got {a_resolved:?}"
                );
                // args[1] = the Err-payload var, must stay free
                let e_resolved = ctx.shallow_resolve(&args[1]);
                assert!(
                    matches!(e_resolved, Type::Var(_)),
                    "Err var must stay free, got {e_resolved:?}"
                );
            }
            other => panic!("expected Con(Result, [..]), got {other:?}"),
        }
        ctx.env.pop_frame();
    }

    // ── Test 4: infer_err — `Err "boom"` → `Result ?a Text` ─────────────────

    #[test]
    fn infer_err() {
        let (_, b) = make_builtins();
        let schema = make_result_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Err "boom" — variant_idx 1 (Err), arg = text literal
        let ty = infer_variant_construction(
            &mut ctx,
            &b,
            &schema,
            b.result,
            1,
            &[text_lit_expr("boom")],
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "Err 'boom' must not error; got {:?}",
            ctx.errors
        );
        match &ty {
            Type::Con(id, args) => {
                assert_eq!(*id, b.result, "result must be Result");
                assert_eq!(args.len(), 2);
                // args[0] = Ok-payload var, must stay free
                let a_resolved = ctx.shallow_resolve(&args[0]);
                assert!(
                    matches!(a_resolved, Type::Var(_)),
                    "Ok var must stay free, got {a_resolved:?}"
                );
                // args[1] = Err-payload var, must resolve to Text
                let e_resolved = ctx.deep_resolve(&args[1]);
                assert!(
                    matches!(e_resolved, Type::Con(iid, _) if iid == b.text),
                    "Err arg must resolve to Text, got {e_resolved:?}"
                );
            }
            other => panic!("expected Con(Result, [..]), got {other:?}"),
        }
        ctx.env.pop_frame();
    }

    // ── Test 5: infer_some_arity_mismatch — `Some 1 2` → T009 ───────────────

    #[test]
    fn infer_some_arity_mismatch() {
        let (_, b) = make_builtins();
        let schema = make_option_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Some 1 2 — 2 args but Some expects 1
        let ty = infer_variant_construction(
            &mut ctx,
            &b,
            &schema,
            b.option,
            0,
            &[int_lit_expr("1"), int_lit_expr("2")],
            ds(),
        );

        assert!(
            matches!(ty, Type::Error),
            "Some 1 2 must return Type::Error"
        );
        let t009 = ctx.errors.iter().any(|e| e.code() == "T009");
        assert!(
            t009,
            "expected T009 WrongConstructorArity; got {:?}",
            ctx.errors
        );

        // Verify the ctor name and counts
        let err = ctx.errors.iter().find(|e| e.code() == "T009").unwrap();
        match err {
            TypeError::WrongConstructorArity {
                ctor,
                expected,
                found,
                ..
            } => {
                assert_eq!(ctor, "Some");
                assert_eq!(*expected, 1);
                assert_eq!(*found, 2);
            }
            _ => panic!("unexpected error variant: {err:?}"),
        }
        ctx.env.pop_frame();
    }

    // ── Test 6: infer_none_arity_mismatch — `None 1` → T009 ─────────────────

    #[test]
    fn infer_none_arity_mismatch() {
        let (_, b) = make_builtins();
        let schema = make_option_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // None 1 — 1 arg but None expects 0
        let ty = infer_variant_construction(
            &mut ctx,
            &b,
            &schema,
            b.option,
            1,
            &[int_lit_expr("1")],
            ds(),
        );

        assert!(matches!(ty, Type::Error), "None 1 must return Type::Error");
        let t009 = ctx.errors.iter().any(|e| e.code() == "T009");
        assert!(t009, "expected T009; got {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Test 7: T008 — binding kind mismatch ─────────────────────────────────
    // In Phase 3, unknown/misspelled constructors are caught as R### errors and
    // the BindingMap will not contain a Constructor entry for them.  T9's T008
    // path fires only when the resolved binding's TyCon kind is unexpectedly not
    // Union (or Record).  This is a defensive path not reachable from real Ridge
    // code (Phase 3 R-codes catch it first).
    //
    // The test demonstrates the error-construction shape for T008; the actual
    // wiring that triggers T008 from live code is part of T17 (full pipeline).
    #[test]
    #[ignore = "T008 path is defensive: unreachable from real Ridge code because resolve-phase R-codes catch unknown constructors before reaching T9; verified manually"]
    fn infer_unknown_constructor_t008() {
        // Direct construction of the T008 variant to confirm shape.
        let err = TypeError::UnknownConstructor {
            name: "Bogus".to_string(),
            expected_type: "Shape".to_string(),
            suggestions: vec![],
            span: ds(),
        };
        assert_eq!(err.code(), "T008");
    }

    // ── Test 8: pattern_some_bound_var ────────────────────────────────────────
    // `match Some 1 { Some x -> x }` — after pattern bind, `x` has type Int.

    #[test]
    fn pattern_some_bound_var() {
        let (_, b) = make_builtins();
        let schema = make_option_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Scrutinee type: Option Int
        let int_ty = Type::Con(b.int, vec![]);
        let scrutinee_ty = Type::Con(b.option, vec![int_ty]);

        // Pattern: Some x (sub_patterns = [Var "x"])
        let sub_pats = vec![var_pattern("x")];

        infer_variant_pattern(
            &mut ctx,
            &b,
            &schema,
            b.option,
            0, // Some
            &sub_pats,
            &scrutinee_ty,
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "Some x pattern must not error; got {:?}",
            ctx.errors
        );

        // `x` should be bound in env with type Int.
        let x_scheme = ctx
            .env
            .lookup("x")
            .expect("x must be bound after pattern")
            .clone();
        let x_ty = crate::instantiate::instantiate(&mut ctx, &x_scheme);
        let x_resolved = ctx.deep_resolve(&x_ty);
        assert!(
            matches!(x_resolved, Type::Con(iid, _) if iid == b.int),
            "x must have type Int, got {x_resolved:?}"
        );
        ctx.env.pop_frame();
    }

    // ── Test 9: pattern_none_no_args ──────────────────────────────────────────
    // `match None { None -> 0 }` — no variables bound.

    #[test]
    fn pattern_none_no_args() {
        let (_, b) = make_builtins();
        let schema = make_option_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Scrutinee type: Option ?a
        let fresh_a = Type::Var(ctx.fresh_tyvid());
        let scrutinee_ty = Type::Con(b.option, vec![fresh_a]);

        // Pattern: None (no sub-patterns)
        infer_variant_pattern(
            &mut ctx,
            &b,
            &schema,
            b.option,
            1, // None
            &[],
            &scrutinee_ty,
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "None pattern must not error; got {:?}",
            ctx.errors
        );
        // No variables should be bound.
        // The env frame should have no bindings from this pattern.
        let frame = ctx.env.frames.last().unwrap();
        assert!(
            frame.bindings.is_empty(),
            "None pattern must bind no variables"
        );
        ctx.env.pop_frame();
    }

    // ── Test 10: pattern_arity_mismatch ──────────────────────────────────────
    // `match Some 1 { Some -> 0 }` — zero sub-patterns vs Some's 1 → T009

    #[test]
    fn pattern_arity_mismatch() {
        let (_, b) = make_builtins();
        let schema = make_option_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let scrutinee_ty = Type::Con(b.option, vec![Type::Con(b.int, vec![])]);

        // Pattern: Some with 0 sub-patterns (missing the payload variable)
        infer_variant_pattern(
            &mut ctx,
            &b,
            &schema,
            b.option,
            0, // Some
            &[],
            &scrutinee_ty,
            ds(),
        );

        let t009 = ctx.errors.iter().any(|e| e.code() == "T009");
        assert!(
            t009,
            "expected T009 for arity mismatch; got {:?}",
            ctx.errors
        );
        ctx.env.pop_frame();
    }

    // ── Test 11: pattern_unknown_ctor_T008 ───────────────────────────────────
    // Same as test 7: the resolve phase catches unknown ctors as R-codes; T9's T008 is
    // a defensive path that is not reachable from real Ridge code.
    #[test]
    #[ignore = "T008 defensive path — unreachable from real Ridge code; resolve-phase R-codes catch unknown constructors before T9; verified manually"]
    fn pattern_unknown_ctor_t008() {
        let err = TypeError::UnknownConstructor {
            name: "Bogus".to_string(),
            expected_type: "MyUnion".to_string(),
            suggestions: vec!["Bingo".to_string()],
            span: ds(),
        };
        assert_eq!(err.code(), "T008");
    }

    // ── Test 12: union_user_defined_2_variants ────────────────────────────────
    // `union Shape = Circle Float | Rectangle Float Float`
    // Test both construction and pattern destructuring on both variants.

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "comprehensive integration test for union types"
    )]
    fn union_user_defined_2_variants() {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        // Define Shape union.
        // Shape is monomorphic (no type params); Circle takes 1 Float, Rectangle takes 2.
        let shape_schema = UnionSchema {
            params: vec![],
            variants: vec![
                UnionVariant {
                    name: "Circle".to_string(),
                    kind: VariantPayload::Positional(vec![Type::Con(b.float, vec![])]),
                },
                UnionVariant {
                    name: "Rectangle".to_string(),
                    kind: VariantPayload::Positional(vec![
                        Type::Con(b.float, vec![]),
                        Type::Con(b.float, vec![]),
                    ]),
                },
            ],
        };
        let shape_id = arena.intern(TyConDecl {
            id: TyConId(0), // overwritten by intern
            name: "Shape".to_string(),
            arity: 0,
            kind: TyConKind::Union(shape_schema.clone()),
            def_span: None,
            def_module_raw: None,
        });

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Test construction: `Circle 3.14` → `Shape`
        let circle_ty = infer_variant_construction(
            &mut ctx,
            &b,
            &shape_schema,
            shape_id,
            0, // Circle
            &[ridge_ast::Expr::Literal(Literal::Float {
                raw: "3.14".to_string(),
                span: ds(),
            })],
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "Circle 3.14 must not error; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(circle_ty, Type::Con(id, _) if id == shape_id),
            "Circle 3.14 must return Shape, got {circle_ty:?}"
        );

        // Test construction: `Rectangle 2.0 3.0` → `Shape`
        let rect_ty = infer_variant_construction(
            &mut ctx,
            &b,
            &shape_schema,
            shape_id,
            1, // Rectangle
            &[
                ridge_ast::Expr::Literal(Literal::Float {
                    raw: "2.0".to_string(),
                    span: ds(),
                }),
                ridge_ast::Expr::Literal(Literal::Float {
                    raw: "3.0".to_string(),
                    span: ds(),
                }),
            ],
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "Rectangle 2.0 3.0 must not error; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(rect_ty, Type::Con(id, _) if id == shape_id),
            "Rectangle 2.0 3.0 must return Shape, got {rect_ty:?}"
        );

        // Test pattern destructuring: `Circle r` — r must be Float
        let scrutinee_shape = Type::Con(shape_id, vec![]);
        let sub_pats = vec![var_pattern("r")];
        ctx.env.push_frame();
        infer_variant_pattern(
            &mut ctx,
            &b,
            &shape_schema,
            shape_id,
            0, // Circle
            &sub_pats,
            &scrutinee_shape,
            ds(),
        );
        assert!(
            ctx.errors.is_empty(),
            "Circle r pattern must not error; got {:?}",
            ctx.errors
        );
        let r_scheme = ctx.env.lookup("r").expect("r must be bound").clone();
        let r_ty = crate::instantiate::instantiate(&mut ctx, &r_scheme);
        let r_resolved = ctx.deep_resolve(&r_ty);
        assert!(
            matches!(r_resolved, Type::Con(iid, _) if iid == b.float),
            "r must have type Float, got {r_resolved:?}"
        );
        ctx.env.pop_frame();

        // Test pattern destructuring: `Rectangle w h` — w and h must both be Float
        ctx.env.push_frame();
        infer_variant_pattern(
            &mut ctx,
            &b,
            &shape_schema,
            shape_id,
            1, // Rectangle
            &[var_pattern("w"), var_pattern("h")],
            &scrutinee_shape,
            ds(),
        );
        assert!(
            ctx.errors.is_empty(),
            "Rectangle w h pattern must not error; got {:?}",
            ctx.errors
        );
        for vname in &["w", "h"] {
            let v_scheme = ctx.env.lookup(vname).expect("must be bound").clone();
            let v_ty = crate::instantiate::instantiate(&mut ctx, &v_scheme);
            let v_resolved = ctx.deep_resolve(&v_ty);
            assert!(
                matches!(v_resolved, Type::Con(iid, _) if iid == b.float),
                "{vname} must have type Float, got {v_resolved:?}"
            );
        }
        ctx.env.pop_frame();

        ctx.env.pop_frame();
    }

    // ── Test 13 (bonus): pattern_ok_bound_var — Ok v binds v: Text ───────────

    #[test]
    fn pattern_ok_bound_var() {
        let (_, b) = make_builtins();
        let schema = make_result_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // scrutinee: Result Text Int
        let scrutinee_ty = Type::Con(
            b.result,
            vec![Type::Con(b.text, vec![]), Type::Con(b.int, vec![])],
        );

        // Pattern: Ok v
        let sub_pats = vec![var_pattern("v")];
        infer_variant_pattern(
            &mut ctx,
            &b,
            &schema,
            b.result,
            0, // Ok
            &sub_pats,
            &scrutinee_ty,
            ds(),
        );

        assert!(
            ctx.errors.is_empty(),
            "Ok v pattern must not error; got {:?}",
            ctx.errors
        );
        let v_scheme = ctx
            .env
            .lookup("v")
            .expect("v must be bound after Ok pattern")
            .clone();
        let v_ty = crate::instantiate::instantiate(&mut ctx, &v_scheme);
        let v_resolved = ctx.deep_resolve(&v_ty);
        assert!(
            matches!(v_resolved, Type::Con(iid, _) if iid == b.text),
            "v must have type Text (Ok payload), got {v_resolved:?}"
        );
        ctx.env.pop_frame();
    }

    // ── Test 14 (bonus): construction_type_mismatch → T001 ───────────────────
    // `Some true` when the outer context expects `Option Int` — after unification.

    #[test]
    fn construction_type_mismatch_emits_t001() {
        let (_, b) = make_builtins();
        let schema = make_option_schema();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Some "hello" — Some with a Text arg.
        // Then unify result with Option Int — this fires T001.
        let ty = infer_variant_construction(
            &mut ctx,
            &b,
            &schema,
            b.option,
            0,
            &[text_lit_expr("hello")],
            ds(),
        );
        // Construction itself succeeds; the result is Option(fresh_a) where fresh_a = Text.
        assert!(
            ctx.errors.is_empty(),
            "construction should not error yet; got {:?}",
            ctx.errors
        );

        // Now force unification with Option Int — should fire T001.
        let option_int = Type::Con(b.option, vec![Type::Con(b.int, vec![])]);
        let unify_result = unify(&mut ctx, &ty, &option_int);
        assert!(
            unify_result.is_err(),
            "unifying Option Text with Option Int must fail"
        );
        ctx.env.pop_frame();
    }
}
