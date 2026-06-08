//! Algorithm W core: `infer_expr`, `infer_pattern`, `infer_block` (T6).
//!
//! # Scope (T6)
//!
//! Implements type inference for:
//! - Literals, identifiers, qualified names
//! - Lambda, call, let, var, assign
//! - If, match (without exhaustiveness — T12), block
//! - Return (verbatim), inner-fn (D058)
//! - Pattern: wildcard, var, literal, tuple, as, cons, paren, constructor
//!
//! # Deferred to later tasks
//!
//! - Records, `with`, field-access → T8
//! - Union variant construction/patterns → T9
//! - Pipe `|>`, `?` propagate, `try` → T10
//! - String interpolation → T11
//! - Match exhaustiveness → T12
//! - Send, Ask, Spawn → T15
//!
//! # AST drift (vs. plan §4.6 pseudocode)
//!
//! - `Let`/`Var`/`Assign`/`Return` are *statement expressions* inside a Block;
//!   they carry no `body` continuation field.  The block continuation is the
//!   remainder of `Block.stmts`.
//! - `Pattern::Var`    ↔ plan's `Pattern::Ident`
//! - `Pattern::As`     ↔ plan's `Pattern::Bind`
//! - No `Pattern::Or` in the AST (plan mentions it; it simply doesn't exist).
//! - `Pattern::Constructor` covers both record patterns (`fields: Some(...)`)
//!   and positional-constructor patterns (`fields: None`). T8/T9 handle these.
//! - `Expr::Call { callee, args }` is flat multi-arg (not nested).
//! - `Expr::Lambda { params: Vec<LambdaParam>, body }` — params may carry
//!   type annotations.
//! - `Expr::InnerFn { decl }` — the decl itself holds the body.

use ridge_ast::{
    BinOp, Block, Body, Expr, FieldInit, FieldPattern, LambdaParam, ListPatElem, Literal, Pattern,
    Span, UnaryOp,
};
use ridge_resolve::NodeKind;
use ridge_types::{BuiltinTyCons, CapRow, CapabilitySet, Scheme, TyConKind, Type};

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::instantiate::{generalise, instantiate, monoscheme};
use crate::prelude::{lookup_prelude, lookup_prelude_tycon};
use crate::render::emit_internal;
use crate::unify::unify;

// ── Public entry points ────────────────────────────────────────────────────────

/// Infers the type of `expr` in the current `InferCtx` and writes back the
/// resolved type to `ctx.node_types_accum` if a `NodeIdMap` is attached.
///
/// This is the public shim (Phase 4.5 T3): it calls `infer_expr_inner` and
/// then records the result in `ctx.node_types_accum` keyed by the expression's
/// `NodeId` (looked up via `NodeKind::Expr` for non-wrapper expressions, or
/// `NodeKind::Block`/`NodeKind::Try` for block/try expressions per
/// OQ-PHASE45-004).
///
/// Errors are pushed into `ctx.errors`; the returned type may be
/// [`Type::Error`] (the absorbing element) if a fatal sub-expression
/// type error was encountered.
///
/// # Deferred variants
///
/// Several `Expr` variants are not yet handled in T6 (records, unions, pipe,
/// propagate, try, interp, send/ask/spawn). Those arms push a
/// `T999 InternalTypeError` noting the deferral task and return
/// `Type::Error`.
// OQ-PHASE45-001: single infer_expr_outer shim; write-back is uniform across all shapes.
// OQ-PHASE45-004: Block and Try use their dedicated NodeKind for the write-back key.
pub fn infer_expr(ctx: &mut InferCtx, b: &BuiltinTyCons, expr: &Expr) -> Type {
    let ty = infer_expr_inner(ctx, b, expr);
    // Write back the shallow-resolved type for this expression position.
    // Expr::Block and Expr::Try use NodeKind::Block / NodeKind::Try respectively
    // so that try_block::resolve_block_type can look up the block's type.
    // All other Expr variants use NodeKind::Expr.
    match expr {
        Expr::Block(block) => {
            ctx.write_node_type(block.span, NodeKind::Block, &ty);
        }
        Expr::Try { span, .. } => {
            ctx.write_node_type(*span, NodeKind::Try, &ty);
        }
        _ => {
            ctx.write_node_type(expr.span(), NodeKind::Expr, &ty);
        }
    }
    ty
}

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive match over all Expr variants"
)]
#[allow(
    clippy::cognitive_complexity,
    reason = "same as too_many_lines — exhaustive Expr match is irreducibly branchy"
)]
fn infer_expr_inner(ctx: &mut InferCtx, b: &BuiltinTyCons, expr: &Expr) -> Type {
    match expr {
        // ── Literals ─────────────────────────────────────────────────────────
        Expr::Literal(lit) => type_of_literal(b, lit),

        // ── Unit ─────────────────────────────────────────────────────────────
        Expr::Unit(_) => Type::Con(b.unit, vec![]),

        // ── Identifier ───────────────────────────────────────────────────────
        Expr::Ident(id) => {
            let name = &id.text;
            // 1. Check local env first.
            if let Some(scheme) = ctx.env.lookup(name) {
                let scheme = scheme.clone();
                return instantiate(ctx, &scheme);
            }
            // 2. Check implicit prelude (Some, None, Ok, Err, etc.)
            if let Some(scheme) = lookup_prelude(b, name) {
                return instantiate(ctx, &scheme);
            }
            // 3. Unknown — the resolver already emitted R010 for this name
            //    (with its suggestion list).  Emitting T999 here too would
            //    double-report and, worse, frame a known unresolved as a
            //    "compiler bug" — the actual message R010 gave is the right
            //    one for the user.  Absorb silently.
            Type::Error
        }

        // ── Qualified name ────────────────────────────────────────────────────
        Expr::Qualified(q) => {
            // T17: env is pre-seeded with stdlib qualified names (e.g. "Io.println")
            // by `stdlib_env::seed_stdlib_env`. Check the env first (which
            // includes locally-shadowing bindings and all pre-seeded stdlib names).
            let full_name: String = q
                .segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if let Some(scheme) = ctx.env.lookup(&full_name) {
                let scheme = scheme.clone();
                return instantiate(ctx, &scheme);
            }
            // Try last segment alone (e.g. "Some" via qualified "Option.Some").
            let last_seg = q.segments.last().map_or("", |s| s.text.as_str());
            if let Some(scheme) = ctx.env.lookup(last_seg) {
                let scheme = scheme.clone();
                return instantiate(ctx, &scheme);
            }
            // Unknown qualified name — T999 (cross-module lookup beyond stdlib
            // is deferred; the resolver should have caught truly missing names).
            emit_internal(
                ctx,
                format!(
                    "qualified name '{full_name}' not found in env (cross-module lookup deferred)"
                ),
                q.span,
            )
        }

        // ── Lambda ────────────────────────────────────────────────────────────
        Expr::Lambda { params, body, .. } => {
            ctx.env.push_frame();

            let mut param_types: Vec<Type> = Vec::with_capacity(params.len());
            for lp in params {
                let (pat, ann_ty) = match lp {
                    LambdaParam::Pattern(p) => (p, None),
                    LambdaParam::Annotated { pat, ty, .. } => (pat, Some(ty)),
                };
                // Fresh type variable for this parameter (monomorphic).
                let ty = if let Some(ann) = ann_ty {
                    // Annotated param — resolve the annotation to a Type.
                    ast_type_to_type(ctx, b, ann)
                } else {
                    Type::Var(ctx.fresh_tyvid())
                };
                // Bind pattern variables at mono-scheme level.
                infer_pattern(ctx, b, pat, &ty.clone());
                param_types.push(ty);
            }

            let ret_ty = infer_expr(ctx, b, body);
            ctx.env.pop_frame();

            // Caps placeholder: use PURE as stated in plan.
            // T13 owns real capability inference; T6 sets the concrete empty set.
            Type::Fn {
                params: param_types,
                ret: Box::new(ret_ty),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            }
        }

        // ── Call ──────────────────────────────────────────────────────────────
        Expr::Call { callee, args, span } => {
            let callee_ty = infer_expr(ctx, b, callee);
            // Quotation: a lambda flowing into a `Quote (e -> _)` parameter is
            // captured as an expression tree, not checked as an ordinary
            // function. Peek the callee's parameter types so each such lambda is
            // routed to the isolated quote checker instead of `infer_expr`.
            let callee_params: Option<Vec<Type>> = match ctx.deep_resolve(&callee_ty) {
                Type::Fn { params, .. } => Some(params),
                _ => None,
            };
            let arg_types: Vec<Type> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    // The lambda is usually parenthesised at the call site
                    // (`f (fn u -> …)`), so look through `Paren` to find it.
                    let inner = peel_parens(a);
                    if matches!(inner, Expr::Lambda { .. }) {
                        if let Some(pty) = callee_params.as_ref().and_then(|p| p.get(i)) {
                            let pty = ctx.deep_resolve(pty);
                            if crate::quote::is_quote_param(ctx, &pty) {
                                let expected_ret = crate::quote::quote_result(ctx, &pty);
                                return match crate::quote::quote_entity(ctx, &pty) {
                                    Some(entity)
                                        if crate::quote::check_quote(
                                            ctx,
                                            b,
                                            inner,
                                            entity,
                                            expected_ret.as_ref(),
                                        ) =>
                                    {
                                        pty
                                    }
                                    Some(_) => Type::Error,
                                    None => {
                                        ctx.errors.push(TypeError::QuoteEntityUnknown {
                                            span: inner.span(),
                                        });
                                        Type::Error
                                    }
                                };
                            }
                        }
                    }
                    infer_expr(ctx, b, a)
                })
                .collect();

            // D069: zero-param call convention. `fn f ()` declares `params = []`
            // in the AST, producing scheme `∀. Fn { params: [], ret: T }`.
            // At the call site `f ()`, `()` is `Expr::Unit` → `arg_types = [Unit]`.
            // Treat `Fn{params:[]} (Unit)` as a valid zero-arg application of `f`.
            let resolved_callee = ctx.deep_resolve(&callee_ty);
            let is_zero_param_unit_call = matches!(
                &resolved_callee,
                Type::Fn { params, .. } if params.is_empty()
            ) && arg_types.len() == 1
                && matches!(&arg_types[0], Type::Con(id, _) if *id == b.unit);

            if is_zero_param_unit_call {
                // Extract the return type directly — no argument unification needed.
                if let Type::Fn { ret, caps, .. } = resolved_callee {
                    // Still unify the caps var with current context (T13/T14 compat).
                    let cap_var = ctx.fresh_capvid();
                    let _ = unify(
                        ctx,
                        &Type::Fn {
                            params: vec![],
                            ret: ret.clone(),
                            caps: CapRow::Var(cap_var),
                        },
                        &Type::Fn {
                            params: vec![],
                            ret: ret.clone(),
                            caps,
                        },
                    );
                    return ctx.deep_resolve(&ret);
                }
            }

            // Partial application (curried functions per spec §163).
            // Ridge functions are curried by default. When the callee type is
            // `Fn{params:[a, b, c], ret:T}` and we supply fewer args than params,
            // the result is the partially-applied function `Fn{params:[b, c], ret:T}`.
            // This is NOT a T003; T003 only fires when MORE args than params are given.
            let resolved_for_partial = ctx.deep_resolve(&callee_ty);
            if let Type::Fn {
                params: callee_params,
                ret: callee_ret,
                caps: callee_caps,
            } = &resolved_for_partial
            {
                let n_params = callee_params.len();
                let n_args = arg_types.len();
                // Partial application: fewer args than declared params.
                if n_args < n_params {
                    // Unify each supplied arg with the corresponding param.
                    let mut ok = true;
                    for (arg_ty, param_ty) in arg_types.iter().zip(callee_params.iter()) {
                        if let Err(e) = unify(ctx, arg_ty, param_ty) {
                            ctx.errors.push(attach_span(e, *span));
                            ok = false;
                        }
                    }
                    if !ok {
                        return Type::Error;
                    }
                    // Build partially-applied function type: remaining params + same ret.
                    let remaining_params: Vec<Type> = callee_params[n_args..]
                        .iter()
                        .map(|p| ctx.deep_resolve(p))
                        .collect();
                    return Type::Fn {
                        params: remaining_params,
                        ret: callee_ret.clone(),
                        caps: callee_caps.clone(),
                    };
                }
            }

            let ret_var = Type::Var(ctx.fresh_tyvid());
            let cap_var = ctx.fresh_capvid();

            let expected_fn_ty = Type::Fn {
                params: arg_types.clone(),
                ret: Box::new(ret_var.clone()),
                caps: CapRow::Var(cap_var),
            };
            if let Err(e) = unify(ctx, &callee_ty, &expected_fn_ty) {
                // Attach the call site span to the error and push it.
                let e_with_span = attach_span(e, *span);
                ctx.errors.push(e_with_span);
                return Type::Error;
            }
            ctx.shallow_resolve(&ret_var)
        }

        // ── If ────────────────────────────────────────────────────────────────
        Expr::If {
            cond,
            then_branch,
            else_branch,
            span,
        } => {
            let cond_ty = infer_expr(ctx, b, cond);
            let bool_ty = Type::Con(b.bool, vec![]);
            if let Err(e) = unify(ctx, &cond_ty, &bool_ty) {
                ctx.errors.push(attach_span(e, *span));
            }

            let then_ty = infer_expr(ctx, b, then_branch);
            match else_branch {
                Some(else_expr) => {
                    let else_ty = infer_expr(ctx, b, else_expr);
                    if let Err(e) = unify(ctx, &then_ty, &else_ty) {
                        ctx.errors.push(attach_span(e, *span));
                        return Type::Error;
                    }
                    ctx.shallow_resolve(&then_ty)
                }
                None => {
                    // No else branch — result is Unit.
                    Type::Con(b.unit, vec![])
                }
            }
        }

        // ── Match ─────────────────────────────────────────────────────────────
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => {
            let scrutinee_ty = infer_expr(ctx, b, scrutinee);
            let result_var = Type::Var(ctx.fresh_tyvid());

            for arm in arms {
                ctx.env.push_frame();
                infer_pattern(ctx, b, &arm.pattern, &scrutinee_ty.clone());

                // Guard — must be Bool.
                if let Some(guard_expr) = &arm.guard {
                    let guard_ty = infer_expr(ctx, b, guard_expr);
                    let bool_ty = Type::Con(b.bool, vec![]);
                    if let Err(e) = unify(ctx, &guard_ty, &bool_ty) {
                        ctx.errors.push(attach_span(e, *span));
                    }
                }

                let arm_ty = infer_expr(ctx, b, &arm.body);
                if let Err(e) = unify(ctx, &arm_ty, &result_var) {
                    ctx.errors.push(attach_span(e, arm.span));
                }
                ctx.env.pop_frame();
            }

            // T12: exhaustiveness + redundancy check runs after per-arm body
            // type-check with the deep-resolved scrutinee type.
            let resolved_scrutinee = ctx.deep_resolve(&scrutinee_ty);
            {
                use ridge_types::TyConArena;
                // Reconstruct the full arena from ctx.tycon_decls so user-defined
                // unions/records are also recognised as closed domains by
                // ctor_set_for (Phase 4 §4.12).  The TyConIds in scrutinee_ty
                // refer to this arena, so render_type produces the real type
                // names rather than `?N` placeholders.
                let mut full_arena = TyConArena::new();
                for decl in &ctx.tycon_decls {
                    full_arena.intern(decl.clone());
                }
                crate::exhaustiveness::check_exhaustiveness(
                    ctx,
                    &full_arena,
                    b,
                    &resolved_scrutinee,
                    arms,
                    *span,
                );
            }
            ctx.shallow_resolve(&result_var)
        }

        // ── Block ─────────────────────────────────────────────────────────────
        Expr::Block(block) => infer_block(ctx, b, block),

        // ── Return ────────────────────────────────────────────────────────────
        // Verbatim return: unify value with the enclosing fn's return type.
        Expr::Return { value, span } => {
            let val_ty = infer_expr(ctx, b, value);
            if let Some(fn_ret) = ctx.current_fn_ret.clone() {
                if let Err(e) = unify(ctx, &val_ty, &fn_ret) {
                    ctx.errors.push(attach_span(e, *span));
                }
            }
            // `return` itself has type Unit (control never flows past it).
            Type::Con(b.unit, vec![])
        }

        // ── Let binding (as a statement-expression) ───────────────────────────
        Expr::Let {
            pat,
            value,
            ty: ann,
            span: _,
        } => {
            let val_ty = infer_expr(ctx, b, value);

            // If annotated, unify the inferred type with the annotation.
            if let Some(ann_ty) = ann {
                let ann_converted = ast_type_to_type(ctx, b, ann_ty);
                if let Err(e) = unify(ctx, &val_ty, &ann_converted) {
                    ctx.errors.push(e);
                }
            }

            // T7: real let-generalisation (§4.7). Lambda params are still
            // monoschemes; only `let` boundaries generalise.
            let scheme = generalise(ctx, &val_ty);
            // Bind the pattern variables.
            bind_pattern_scheme(ctx, b, pat, &scheme);
            // A let-binding expression itself has type Unit (side-effect only).
            Type::Con(b.unit, vec![])
        }

        // ── Var binding (mutable, as a statement-expression) ──────────────────
        Expr::Var {
            name,
            value,
            ty: ann,
            span: _,
        } => {
            let val_ty = infer_expr(ctx, b, value);
            if let Some(ann_ty) = ann {
                let ann_converted = ast_type_to_type(ctx, b, ann_ty);
                if let Err(e) = unify(ctx, &val_ty, &ann_converted) {
                    ctx.errors.push(e);
                }
            }
            // Bind name to a mono-scheme.
            let scheme = monoscheme(val_ty);
            ctx.env.bind(name.text.clone(), scheme);
            Type::Con(b.unit, vec![])
        }

        // ── Assign ───────────────────────────────────────────────────────────
        Expr::Assign {
            target,
            value,
            span,
        } => {
            let target_ty = infer_expr(ctx, b, target);
            let val_ty = infer_expr(ctx, b, value);
            if let Err(e) = unify(ctx, &target_ty, &val_ty) {
                ctx.errors.push(attach_span(e, *span));
            }
            // Assign returns Unit (spec §3).
            Type::Con(b.unit, vec![])
        }

        // ── Binary operators ──────────────────────────────────────────────────
        Expr::Binary { op, lhs, rhs, span } => infer_binary(ctx, b, *op, lhs, rhs, *span),

        // ── Unary operators ───────────────────────────────────────────────────
        Expr::Unary { op, expr, span } => {
            let ty = infer_expr(ctx, b, expr);
            match op {
                UnaryOp::Neg => {
                    // `-x` is valid for Int and Float.
                    let int_ty = Type::Con(b.int, vec![]);
                    let float_ty = Type::Con(b.float, vec![]);
                    // Try Int first; if it fails try Float; if both fail emit T001.
                    let resolved = ctx.shallow_resolve(&ty);
                    match &resolved {
                        Type::Con(id, _) if *id == b.float => resolved,
                        _ => {
                            if let Err(e) = unify(ctx, &ty, &int_ty) {
                                // Maybe it's a float? Only emit if also not float.
                                let fresh = ctx.shallow_resolve(&ty);
                                if !matches!(&fresh, Type::Con(id, _) if *id == b.float) {
                                    ctx.errors.push(attach_span(e, *span));
                                    return Type::Error;
                                }
                                float_ty
                            } else {
                                int_ty
                            }
                        }
                    }
                }
            }
        }

        // ── Tuple ─────────────────────────────────────────────────────────────
        Expr::Tuple { elems, .. } => {
            let elem_types: Vec<Type> = elems.iter().map(|e| infer_expr(ctx, b, e)).collect();
            Type::Tuple(elem_types)
        }

        // ── List ──────────────────────────────────────────────────────────────
        Expr::List { elems, span } => {
            let elem_var = Type::Var(ctx.fresh_tyvid());
            for elem in elems {
                let et = infer_expr(ctx, b, elem);
                if let Err(e) = unify(ctx, &et, &elem_var) {
                    ctx.errors.push(attach_span(e, *span));
                }
            }
            Type::Con(b.list, vec![ctx.shallow_resolve(&elem_var)])
        }

        // ── Paren ─────────────────────────────────────────────────────────────
        Expr::Paren { inner, .. } => infer_expr(ctx, b, inner),

        // ── FieldAccessorFn (.name) ───────────────────────────────────────────
        // T8 handles field access.  Placeholder: return an opaque fn type.
        Expr::FieldAccessorFn { span, .. } => {
            emit_internal(ctx, "FieldAccessorFn typing deferred to T8", *span)
        }

        // ── InnerFn (D058) ────────────────────────────────────────────────────
        Expr::InnerFn { decl, span } => {
            // The inner-fn name is bound in the *outer* scope (so the body can
            // recurse), but the params are in an inner scope.
            let name = decl.name.text.clone();

            // Build a fresh Fn type for the inner fn.
            let param_types: Vec<Type> = decl
                .params
                .iter()
                .map(|p| match p {
                    ridge_ast::Param::Bare(_) => Type::Var(ctx.fresh_tyvid()),
                    ridge_ast::Param::Annotated { ty, .. }
                    | ridge_ast::Param::PatternAnnotated { ty, .. } => ast_type_to_type(ctx, b, ty),
                })
                .collect();
            #[expect(
                clippy::map_unwrap_or,
                reason = "map_or_else borrows ctx mutably twice"
            )]
            let ret_ty_declared = decl
                .ret
                .as_ref()
                .map(|t| ast_type_to_type(ctx, b, t))
                .unwrap_or_else(|| Type::Var(ctx.fresh_tyvid()));

            // Bind the inner fn name to its monomorphic scheme in the outer scope
            // so recursive calls within the body can find it.
            // We keep the Fn type around so T7 can generalise it after body inference.
            let fn_ty_for_bind = Type::Fn {
                params: param_types.clone(),
                ret: Box::new(ret_ty_declared.clone()),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            let fn_ty_for_generalise = fn_ty_for_bind.clone();
            ctx.env.bind(name.clone(), monoscheme(fn_ty_for_bind));

            // Infer the body in a new scope.
            ctx.env.push_frame();
            for (param, ty) in decl.params.iter().zip(param_types.iter()) {
                match param {
                    ridge_ast::Param::Bare(id) | ridge_ast::Param::Annotated { name: id, .. } => {
                        ctx.env.bind(id.text.clone(), monoscheme(ty.clone()));
                    }
                    ridge_ast::Param::PatternAnnotated { pat, span, .. } => {
                        infer_pattern(ctx, b, pat, ty);
                        crate::exhaustiveness::check_param_irrefutable(ctx, b, pat, ty, *span);
                    }
                }
            }

            // Set the return type context.
            let saved_ret = ctx.current_fn_ret.take();
            ctx.current_fn_ret = Some(ret_ty_declared.clone());

            // Inner fns always have Body::Expr; Body::Ffi is top-level stdlib only.
            let body_ty = match &decl.body {
                Body::Expr(e) => infer_expr(ctx, b, e),
                // Body::Ffi carries a fully-declared signature; skip inference.
                Body::Ffi { .. } => ret_ty_declared.clone(),
            };
            if let Err(e) = unify(ctx, &body_ty, &ret_ty_declared) {
                ctx.errors.push(attach_span(e, *span));
            }

            ctx.current_fn_ret = saved_ret;
            ctx.env.pop_frame();

            // T7: generalise the inner-fn's type and update the binding in the
            // outer scope.  The initial monomorphic binding (set above for
            // recursive calls) is replaced with the polymorphic scheme.
            // Polymorphic recursion is prevented by HM monomorphic binding
            // during body inference — any poly-rec attempt causes a T001 via
            // unification, not T013 (T013 is only reachable with explicit
            // type annotations on recursive fns, which are not yet supported).
            let generalised = generalise(ctx, &fn_ty_for_generalise);
            ctx.env.bind(name, generalised);

            // Cap-subset check is T14; T6/T7 just return Unit (the inner-fn
            // expression itself evaluates to Unit; its *value* is accessed via
            // the name binding above).
            Type::Con(b.unit, vec![])
        }

        // ── Guard ─────────────────────────────────────────────────────────────
        Expr::Guard {
            cond,
            else_branch,
            span,
        } => {
            let cond_ty = infer_expr(ctx, b, cond);
            let bool_ty = Type::Con(b.bool, vec![]);
            if let Err(e) = unify(ctx, &cond_ty, &bool_ty) {
                ctx.errors.push(attach_span(e, *span));
            }
            infer_block(ctx, b, else_branch);
            // Guard expression type is Unit.
            Type::Con(b.unit, vec![])
        }

        // ── Record construction / union constructor value (T8/T9) ───────────
        //
        // The parser emits `Expr::Record { fields: vec![] }` for ANY bare
        // upper-case identifier in expression position (e.g. `None`, `Some`,
        // and user-defined constructors).  When `fields` is non-empty it is
        // always a record construction.  When `fields` is empty it can be:
        //
        //   (a) A true zero-arg record construction (user-defined: `Point{}`).
        //   (b) A prelude/union constructor USED AS A VALUE — e.g. `Some` or
        //       `None` appearing alone or as the callee of a juxtaposition call
        //       like `Some 42`.  These look up the value binding from the env
        //       (seeded by `seed_stdlib_env` / `prelude_types`).
        //
        // Precedence: env lookup (handles both (a) and (b) after `seed_stdlib_env`
        // seeds `Some`, `None`, `Ok`, `Err`, and any user-defined ctors) first.
        // If not in env, fall through to the record-schema lookup.
        Expr::Record {
            constructor,
            fields,
            span,
        } => {
            use ridge_ast::RecordCtor;
            use ridge_types::TyConKind;

            let ctor_name = match constructor {
                RecordCtor::Bare(id) => id.text.as_str(),
                RecordCtor::Qualified(qn) => qn.segments.last().map_or("", |s| s.text.as_str()),
            };

            // ── Path (b): bare ctor with no fields — try env / prelude first.
            if fields.is_empty() {
                // Try local env (covers user-defined ctor schemes seeded by collect_user_tycons).
                if let Some(scheme) = ctx.env.lookup(ctor_name).cloned() {
                    let ty = instantiate(ctx, &scheme);
                    // Nullary constructor auto-apply: if the instantiated type is a
                    // zero-param function `Fn{params:[], ret:T}`, return `T` directly.
                    // This handles user-defined union variants like `Info`, `Warn`, `Error`
                    // (of type `Level`) that are bound as `∀. Fn{params:[], ret:Level}`.
                    // In expression position they denote the VALUE, not the constructor fn.
                    return if let Type::Fn { params, ret, .. } = &ty {
                        if params.is_empty() {
                            *ret.clone()
                        } else {
                            ty
                        }
                    } else {
                        ty
                    };
                }
                // Try implicit prelude (Some, None, Ok, Err).
                if let Some(scheme) = lookup_prelude(b, ctor_name) {
                    let ty = instantiate(ctx, &scheme);
                    // Same nullary auto-apply for prelude constructors.
                    return if let Type::Fn { params, ret, .. } = &ty {
                        if params.is_empty() {
                            *ret.clone()
                        } else {
                            ty
                        }
                    } else {
                        ty
                    };
                }
                // Fall through to record-schema lookup (handles true zero-field records).
            }

            // ── Path (a): record construction with fields (or fallback for unknown bare ctors).
            if let Some(&tycon_id) = ctx.user_tycon_names.get(ctor_name) {
                let decl = ctx.tycon_decls.get(tycon_id.0 as usize).cloned();
                match decl {
                    Some(d) => {
                        if let TyConKind::Record(schema) = &d.kind {
                            let schema = schema.clone();
                            crate::records::infer_record_construction(
                                ctx, b, &schema, tycon_id, ctor_name, fields, *span,
                            )
                        } else if matches!(d.kind, TyConKind::Union(_)) {
                            // User-defined union constructor used as a value (no fields).
                            // Look up in env (should already be bound by bind_constructor_schemes).
                            if let Some(scheme) = ctx.env.lookup(ctor_name).cloned() {
                                instantiate(ctx, &scheme)
                            } else {
                                emit_internal(
                                    ctx,
                                    format!("union ctor '{ctor_name}' not in env"),
                                    *span,
                                )
                            }
                        } else {
                            emit_internal(
                                ctx,
                                format!("type '{ctor_name}' is not a record or union"),
                                *span,
                            )
                        }
                    }
                    None => {
                        emit_internal(ctx, format!("TyConDecl not found for '{ctor_name}'"), *span)
                    }
                }
            } else {
                // Not a user-defined TyCon. Check if it's a stdlib stub
                // bound in the env (e.g. Response, Request from std.net.http).
                // If the scheme has Type::Error, absorb silently.
                if let Some(scheme) = ctx.env.lookup(ctor_name).cloned() {
                    // If this is already Type::Error (stub), absorb without T999.
                    instantiate(ctx, &scheme)
                } else {
                    emit_internal(
                        ctx,
                        format!("unknown record constructor '{ctor_name}'"),
                        *span,
                    )
                }
            }
        }

        // ── With-update (T8) ─────────────────────────────────────────────────
        Expr::With { base, fields, span } => {
            let base_ty = infer_expr(ctx, b, base);
            let tycon_decls = ctx.tycon_decls.clone();
            crate::records::infer_record_with(ctx, b, &base_ty, fields, *span, &tycon_decls)
        }

        // ── Field access (T8) ────────────────────────────────────────────────
        Expr::FieldAccess { base, field, span } => {
            let base_ty = infer_expr(ctx, b, base);
            let tycon_decls = ctx.tycon_decls.clone();
            crate::records::infer_field_access(ctx, b, &base_ty, field, *span, &tycon_decls)
        }

        // ── Pipe `|>` (T10) ──────────────────────────────────────────────────
        Expr::Pipe { lhs, rhs, span } => crate::pipe_propagate::infer_pipe(ctx, b, lhs, rhs, *span),

        // ── Propagate `?` (T10) ───────────────────────────────────────────────
        Expr::Propagate { inner, span } => {
            crate::pipe_propagate::infer_propagate(ctx, b, inner, *span)
        }

        // ── Try block (T10) ───────────────────────────────────────────────────
        Expr::Try { block, span } => crate::pipe_propagate::infer_try(ctx, b, block, *span),

        // ── String interpolation ──────────────────────────────────────────────
        // The ToText instance set is cloned out of the context (populated before
        // per-body inference by the SCC pass). When absent (unit tests without
        // the full pipeline), the built-in closed set is used as a fallback.
        // Cloning is cheap: the set is small (prelude types + user types with
        // ToText) and is only cloned once per Interp expression.
        Expr::Interp { parts, span } => {
            let to_text_set = ctx.to_text_tycons.clone();
            crate::interp::infer_interp(ctx, b, parts, *span, to_text_set.as_ref())
        }

        // ── Send (T15) ────────────────────────────────────────────────────────
        // Full actor type resolution requires the workspace TyConArena (wired in
        // T17). Here we build a minimal builtin-only arena; actor handles not
        // registered in it will produce T020/T021 diagnostics rather than
        // silently deferring. The actor.rs unit tests exercise the full path
        // directly (passing a populated arena).
        Expr::Send {
            handle,
            message,
            span,
        } => {
            let arena = build_arena_from_ctx(ctx);
            crate::actor::infer_send(ctx, b, handle, message, *span, &arena)
        }

        // ── Ask (T15) ─────────────────────────────────────────────────────────
        Expr::Ask {
            handle,
            message,
            args,
            timeout,
            span,
        } => {
            let arena = build_arena_from_ctx(ctx);
            crate::actor::infer_ask(
                ctx,
                b,
                handle,
                message,
                args.as_slice(),
                timeout.as_ref(),
                *span,
                &arena,
            )
        }

        // ── Spawn (T15) ───────────────────────────────────────────────────────
        Expr::Spawn { actor, args, span } => {
            let arena = build_arena_from_ctx(ctx);
            crate::actor::infer_spawn(ctx, b, actor, args.as_slice(), *span, &arena)
        }

        // ── Inline record literal ─────────────────────────────────────────────
        Expr::RecordLit { fields, span } => infer_record_lit(ctx, b, fields, *span),
    }
}

/// Builds a temporary [`ridge_types::TyConArena`] from the snapshot stored in
/// `ctx.tycon_decls`.
///
/// Used by Send/Ask/Spawn arms that need a full arena at inference time.
/// The returned arena has the same `TyConId` space as the module inference context.
fn build_arena_from_ctx(ctx: &InferCtx) -> ridge_types::TyConArena {
    let mut arena = ridge_types::TyConArena::new();
    for decl in &ctx.tycon_decls {
        arena.intern(decl.clone());
    }
    arena
}

/// Infers the type of a `Block` (sequence of statement-expressions).
///
/// Each statement except the last is typed and checked for `T022
/// DiscardedResult`: if a non-last statement's type is not `Unit`
/// and the statement is not a `let`/`var` binding, `T022` is emitted.
/// The block's type is the type of its final statement.
/// An empty `stmts` vec is a parser-level error (`P014 EmptyBlock`);
/// `infer_block` returns `Unit` defensively.
pub fn infer_block(ctx: &mut InferCtx, b: &BuiltinTyCons, block: &Block) -> Type {
    if block.stmts.is_empty() {
        return Type::Con(b.unit, vec![]);
    }

    let n = block.stmts.len();
    for stmt in &block.stmts[..n - 1] {
        let stmt_ty = infer_expr(ctx, b, stmt);
        crate::pipe_propagate::check_discarded_result(ctx, b, stmt, &stmt_ty);
    }
    infer_expr(ctx, b, &block.stmts[n - 1])
}

/// Infers pattern variable types and binds them in the current scope frame.
///
/// `expected_ty` is the type the pattern must match (the scrutinee's type for
/// match patterns, or the inferred binding type for let/var).
///
/// Deferred pattern forms (record body, union constructors) push a
/// `T999 InternalTypeError` and do not bind any variables.
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive match over all Pattern variants"
)]
pub fn infer_pattern(ctx: &mut InferCtx, b: &BuiltinTyCons, pat: &Pattern, expected_ty: &Type) {
    match pat {
        // ── Wildcard ──────────────────────────────────────────────────────────
        Pattern::Wildcard { .. } => {
            // No binding; expected_ty is unconstrained.
        }

        // ── Variable binding ──────────────────────────────────────────────────
        Pattern::Var { name, .. } => {
            // Lambda params are never polymorphic; same here.
            let scheme = monoscheme(expected_ty.clone());
            ctx.env.bind(name.text.clone(), scheme);
        }

        // ── Literal ───────────────────────────────────────────────────────────
        Pattern::Literal { lit, span } => {
            let lit_ty = type_of_literal(b, lit);
            if let Err(e) = unify(ctx, &lit_ty, expected_ty) {
                ctx.errors.push(attach_span(e, *span));
            }
        }

        // ── Tuple ─────────────────────────────────────────────────────────────
        Pattern::Tuple { elems, span } => {
            // Build a tuple of fresh vars, unify with expected, recurse.
            let fresh_vars: Vec<Type> =
                elems.iter().map(|_| Type::Var(ctx.fresh_tyvid())).collect();
            let tuple_ty = Type::Tuple(fresh_vars.clone());
            if let Err(e) = unify(ctx, &tuple_ty, expected_ty) {
                ctx.errors.push(attach_span(e, *span));
                return;
            }
            // Resolve each element type after unification and recurse.
            for (sub_pat, elem_var) in elems.iter().zip(fresh_vars.iter()) {
                let resolved = ctx.shallow_resolve(elem_var);
                infer_pattern(ctx, b, sub_pat, &resolved);
            }
        }

        // ── Cons (list) ───────────────────────────────────────────────────────
        Pattern::Cons { head, tail, span } => {
            // expected must be List ?a
            let elem_var = Type::Var(ctx.fresh_tyvid());
            let list_ty = Type::Con(b.list, vec![elem_var.clone()]);
            if let Err(e) = unify(ctx, &list_ty, expected_ty) {
                ctx.errors.push(attach_span(e, *span));
                return;
            }
            let resolved_elem = ctx.shallow_resolve(&elem_var);
            infer_pattern(ctx, b, head, &resolved_elem);
            // tail has type List ?a
            let resolved_list = ctx.shallow_resolve(&list_ty);
            infer_pattern(ctx, b, tail, &resolved_list);
        }

        // ── As (alias) ────────────────────────────────────────────────────────
        // Binds the outer name AND matches the inner pattern.
        Pattern::As { name, inner, .. } => {
            let scheme = monoscheme(expected_ty.clone());
            ctx.env.bind(name.text.clone(), scheme);
            infer_pattern(ctx, b, inner, expected_ty);
        }

        // ── Paren ─────────────────────────────────────────────────────────────
        Pattern::Paren { inner, .. } => {
            infer_pattern(ctx, b, inner, expected_ty);
        }

        // ── Empty list `[]` ───────────────────────────────────────────────────
        // Unify expected with `List ?a` (any element type).
        Pattern::ListNil { span } => {
            let elem_var = Type::Var(ctx.fresh_tyvid());
            let list_ty = Type::Con(b.list, vec![elem_var]);
            if let Err(e) = unify(ctx, &list_ty, expected_ty) {
                ctx.errors.push(attach_span(e, *span));
            }
        }

        // ── Bracketed list pattern ────────────────────────────────────────────
        // Infer each element in place against the element type, and bind an
        // optional rest binder at `List ?a`.  Unlike `desugar_list` (prefix
        // only), this types suffix/middle binders such as `last` in `[.., last]`.
        Pattern::List { elements, span } => {
            let elem_var = Type::Var(ctx.fresh_tyvid());
            let list_ty = Type::Con(b.list, vec![elem_var.clone()]);
            if let Err(e) = unify(ctx, &list_ty, expected_ty) {
                ctx.errors.push(attach_span(e, *span));
                return;
            }
            let resolved_elem = ctx.shallow_resolve(&elem_var);
            let resolved_list = ctx.shallow_resolve(&list_ty);
            for elem in elements {
                match elem {
                    ListPatElem::Elem(p) => infer_pattern(ctx, b, p, &resolved_elem),
                    ListPatElem::Rest {
                        bind: Some(name), ..
                    } => {
                        ctx.env
                            .bind(name.text.clone(), monoscheme(resolved_list.clone()));
                    }
                    ListPatElem::Rest { bind: None, .. } => {}
                }
            }
        }

        // ── Constructor ───────────────────────────────────────────────────────
        // Two forms:
        //   fields: None  → positional constructor pattern → T9 (unions.rs)
        //   fields: Some  → record-body constructor pattern → records.rs
        Pattern::Constructor {
            name,
            fields,
            has_rest,
            args,
            span,
        } => {
            if let Some(field_pats) = fields {
                // Record-body constructor pattern: resolve the record type and
                // type each field against its declared type.
                if let Some(&tycon_id) = ctx.user_tycon_names.get(&name.text) {
                    if let Some(decl) = ctx.tycon_decls.get(tycon_id.0 as usize).cloned() {
                        if let ridge_types::TyConKind::Record(schema) = &decl.kind {
                            let schema = schema.clone();
                            crate::records::infer_record_pattern(
                                ctx,
                                b,
                                &schema,
                                tycon_id,
                                &name.text,
                                field_pats,
                                *has_rest,
                                expected_ty,
                                *span,
                            );
                            return;
                        }
                    }
                }
                // Not a known record type — report and keep inference going by
                // typing any sub-patterns against Error.
                let _ = emit_internal(
                    ctx,
                    format!("record pattern `{}` is not a known record type", name.text),
                    *span,
                );
                for fp in field_pats {
                    if let Some(sub) = &fp.pattern {
                        infer_pattern(ctx, b, sub, &Type::Error);
                    }
                }
                return;
            }

            // Positional constructor pattern — dispatch to unions.rs.
            // Look up the constructor name in the prelude union map.
            if let Some((owner_tycon, variant_idx)) =
                crate::unions::resolve_prelude_ctor(b, &name.text)
            {
                // Retrieve the UnionSchema from the context.
                // The schema is stored in the prelude (Option/Result) — look it up
                // from the BuiltinTyCons by matching the known TyConId.
                let schema = crate::prelude::get_prelude_union_schema(b, owner_tycon);
                crate::unions::infer_variant_pattern(
                    ctx,
                    b,
                    &schema,
                    owner_tycon,
                    variant_idx,
                    args,
                    expected_ty,
                    *span,
                );
            } else {
                // Constructor not in prelude — search user-defined unions in
                // ctx.tycon_decls for a variant matching the constructor name.
                let ctor_name = name.text.as_str();
                let found = ctx.tycon_decls.iter().enumerate().find_map(|(idx, decl)| {
                    if let ridge_types::TyConKind::Union(schema) = &decl.kind {
                        let variant_idx =
                            schema.variants.iter().position(|v| v.name == ctor_name)?;
                        #[expect(clippy::cast_possible_truncation, reason = "arena index fits u32")]
                        Some((
                            ridge_types::TyConId(idx as u32),
                            schema.clone(),
                            variant_idx,
                        ))
                    } else {
                        None
                    }
                });
                if let Some((owner_tycon, schema, variant_idx)) = found {
                    crate::unions::infer_variant_pattern(
                        ctx,
                        b,
                        &schema,
                        owner_tycon,
                        variant_idx,
                        args,
                        expected_ty,
                        *span,
                    );
                } else {
                    // Truly unknown constructor — Phase 3 R-codes should have caught
                    // this; T9 falls back to T999 as a defensive measure.
                    let _ = emit_internal(
                        ctx,
                        format!(
                            "constructor pattern '{}' not found in any union type",
                            name.text
                        ),
                        *span,
                    );
                }
            }
        }

        // ── Inline record pattern ─────────────────────────────────────────────
        Pattern::Record {
            fields,
            has_rest,
            span,
        } => {
            infer_inline_record_pattern(ctx, b, fields, *has_rest, expected_ty, *span);
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Extracts the type of a literal.
#[must_use]
pub const fn type_of_literal(b: &BuiltinTyCons, lit: &Literal) -> Type {
    match lit {
        Literal::IntDec { .. }
        | Literal::IntBin { .. }
        | Literal::IntOct { .. }
        | Literal::IntHex { .. } => Type::Con(b.int, vec![]),
        Literal::Float { .. } => Type::Con(b.float, vec![]),
        Literal::Bool { .. } => Type::Con(b.bool, vec![]),
        Literal::Text { .. } | Literal::RawText { .. } => Type::Con(b.text, vec![]),
    }
}

/// Converts an AST `Type` expression to a `ridge_types::Type`.
///
/// Handles the subset of `ridge_ast::Type` that appears in annotations:
/// primitive types, named types, type applications (`App`), tuples, list
/// sugar, and function types.  Unknown named types are resolved to a fresh
/// unification variable (T7/T8 handle user-defined type resolution).
fn ast_type_to_type(ctx: &mut InferCtx, b: &BuiltinTyCons, ast_ty: &ridge_ast::Type) -> Type {
    use ridge_ast::PrimitiveType;

    match ast_ty {
        // ── Primitive: Int, Float, Bool, Text, Unit, Timestamp ────────────────
        ridge_ast::Type::Primitive { name, .. } => {
            let tycon = match name {
                PrimitiveType::Int => b.int,
                PrimitiveType::Float => b.float,
                PrimitiveType::Bool => b.bool,
                PrimitiveType::Text => b.text,
                PrimitiveType::Unit => b.unit,
                PrimitiveType::Timestamp => b.timestamp,
            };
            Type::Con(tycon, vec![])
        }

        // ── Named: `User`, `Option`, etc. — zero args ─────────────────────────
        ridge_ast::Type::Named { name, .. } => {
            let base_name = &name.text;
            if let Some(id) = lookup_prelude_tycon(b, base_name) {
                return Type::Con(id, vec![]);
            }
            // T17: check user-defined TyCons collected by tycon_collect.
            if let Some(&id) = ctx.user_tycon_names.get(base_name.as_str()) {
                return Type::Con(id, vec![]);
            }
            // Unknown named type — fresh var placeholder.
            Type::Var(ctx.fresh_tyvid())
        }

        // ── App: `Option Int`, `Map k v`, etc. ───────────────────────────────
        ridge_ast::Type::App { head, args, .. } => {
            let base_name = &head.text;
            let arg_tys: Vec<Type> = args.iter().map(|a| ast_type_to_type(ctx, b, a)).collect();
            if let Some(id) = lookup_prelude_tycon(b, base_name) {
                return Type::Con(id, arg_tys);
            }
            // T17: check user-defined TyCons.
            if let Some(&id) = ctx.user_tycon_names.get(base_name.as_str()) {
                return Type::Con(id, arg_tys);
            }
            Type::Var(ctx.fresh_tyvid())
        }

        // ── Tuple ─────────────────────────────────────────────────────────────
        ridge_ast::Type::Tuple { elems, .. } => {
            let ts: Vec<Type> = elems.iter().map(|e| ast_type_to_type(ctx, b, e)).collect();
            Type::Tuple(ts)
        }

        // ── List sugar: [a] → List a ──────────────────────────────────────────
        ridge_ast::Type::List { elem, .. } => {
            let elem_ty = ast_type_to_type(ctx, b, elem);
            Type::Con(b.list, vec![elem_ty])
        }

        // ── Fn type ───────────────────────────────────────────────────────────
        ridge_ast::Type::Fn { fn_ty, .. } => {
            let param_tys: Vec<Type> = fn_ty
                .params
                .iter()
                .map(|p| ast_type_to_type(ctx, b, p))
                .collect();
            let ret_ty = ast_type_to_type(ctx, b, &fn_ty.ret);
            let cap_row = if fn_ty.caps.is_empty() {
                CapRow::Concrete(CapabilitySet::PURE)
            } else {
                let mut cs = CapabilitySet::PURE;
                for cap in &fn_ty.caps {
                    cs = cs.union(&CapabilitySet::singleton(*cap));
                }
                CapRow::Concrete(cs)
            };
            Type::Fn {
                params: param_tys,
                ret: Box::new(ret_ty),
                caps: cap_row,
            }
        }

        // ── Paren ─────────────────────────────────────────────────────────────
        ridge_ast::Type::Paren { inner, .. } => ast_type_to_type(ctx, b, inner),

        // ── Var: lower-ident type variable (a, k, v, …) ──────────────────────
        ridge_ast::Type::Var { .. } => {
            // Type variable names in annotation context — allocate a fresh
            // unification variable.  T7 will wire proper annotation-variable
            // tracking; for T6 this is sufficient for monosignatures.
            Type::Var(ctx.fresh_tyvid())
        }

        // ── Inline record type → a structural `Type::Record` ───────────────────
        ridge_ast::Type::Record { fields, tail, .. } => {
            let resolved: Vec<(String, Type)> = fields
                .iter()
                .map(|f| {
                    let ty = ast_type_to_type(ctx, b, &f.ty);
                    (f.name.text.clone(), ty)
                })
                .collect();
            // A `| r` tail makes the row open over a fresh row variable.
            let row_tail = if tail.is_some() {
                ridge_types::RowTail::Open(ctx.fresh_rowvid())
            } else {
                ridge_types::RowTail::Closed
            };
            Type::record(resolved, row_tail)
        }
    }
}

/// Binds a pattern against a `Scheme` (for `let`-bindings where generalisation
/// is already complete).  For a monomorphic scheme this is equivalent to
/// `infer_pattern` with the scheme's body type.
fn bind_pattern_scheme(ctx: &mut InferCtx, b: &BuiltinTyCons, pat: &Pattern, scheme: &Scheme) {
    // For the T6 monoscheme stub, the scheme has no vars so instantiation is
    // a no-op — use the body type directly.
    let ty = instantiate(ctx, scheme);
    infer_pattern(ctx, b, pat, &ty);
}

/// Infers types for binary operators.
fn infer_binary(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
) -> Type {
    let lhs_ty = infer_expr(ctx, b, lhs);
    let rhs_ty = infer_expr(ctx, b, rhs);

    match op {
        // Arithmetic `+ - * / % **` or Concat `++`: unify operands, return same type
        BinOp::Add
        | BinOp::Sub
        | BinOp::Mul
        | BinOp::Div
        | BinOp::Mod
        | BinOp::Pow
        | BinOp::Concat => {
            if let Err(e) = unify(ctx, &lhs_ty, &rhs_ty) {
                ctx.errors.push(attach_span(e, span));
                return Type::Error;
            }
            ctx.shallow_resolve(&lhs_ty)
        }

        // Comparisons: any type, Bool result
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            if let Err(e) = unify(ctx, &lhs_ty, &rhs_ty) {
                ctx.errors.push(attach_span(e, span));
            }
            Type::Con(b.bool, vec![])
        }

        // Boolean logic: Bool -> Bool -> Bool
        BinOp::Or | BinOp::And => {
            let bool_ty = Type::Con(b.bool, vec![]);
            if let Err(e) = unify(ctx, &lhs_ty, &bool_ty) {
                ctx.errors.push(attach_span(e, span));
            }
            if let Err(e) = unify(ctx, &rhs_ty, &bool_ty) {
                ctx.errors.push(attach_span(e, span));
            }
            bool_ty
        }

        // Cons `::`: a -> List a -> List a
        BinOp::Cons => {
            let elem_var = lhs_ty;
            let list_ty = Type::Con(b.list, vec![elem_var]);
            if let Err(e) = unify(ctx, &rhs_ty, &list_ty) {
                ctx.errors.push(attach_span(e, span));
                return Type::Error;
            }
            ctx.shallow_resolve(&list_ty)
        }

        // Pipe: handled by Expr::Pipe variant (T10).
        BinOp::Pipe => emit_internal(
            ctx,
            "BinOp::Pipe in Binary — should be Expr::Pipe; deferred to T10",
            span,
        ),
    }
}

// ── Inline record helpers ─────────────────────────────────────────────────────

/// Infer the type of a constructor-less record literal `{ f = v, … }`.
///
/// A record literal *defines* a structural record: each field's value type is
/// that field's type, so the result is a closed [`Type::Record`]. There is no
/// schema to validate against, and a field may stay a free type variable — it
/// generalises like any other component of the row.
fn infer_record_lit(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    fields: &[FieldInit],
    span: Span,
) -> Type {
    let _ = span;
    let resolved_fields: Vec<(String, Type)> = fields
        .iter()
        .map(|fi| {
            let raw_ty = match &fi.value {
                Some(val_expr) => infer_expr(ctx, b, val_expr),
                None => {
                    // Shorthand `{ x }` — look up `x` in scope.
                    if let Some(s) = ctx.env.lookup(&fi.name.text).cloned() {
                        instantiate(ctx, &s)
                    } else {
                        emit_internal(
                            ctx,
                            format!("shorthand field '{}' not in scope", fi.name.text),
                            fi.span,
                        )
                    }
                }
            };
            (fi.name.text.clone(), ctx.deep_resolve(&raw_ty))
        })
        .collect();
    Type::record(resolved_fields, ridge_types::RowTail::Closed)
}

/// Check an inline record pattern `{ f1, f2, .. }` against `expected_ty`.
///
/// A structural [`Type::Record`] scrutinee is destructured directly against its
/// row. A free type variable is unified with a fresh structural record built
/// from the pattern's field names (open when the pattern has a trailing `..`).
/// A legacy anon `Type::Con` record still delegates to the schema-based path.
fn infer_inline_record_pattern(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    fields: &[FieldPattern],
    has_rest: bool,
    expected_ty: &Type,
    span: Span,
) {
    let resolved = ctx.deep_resolve(expected_ty);

    // Structural record scrutinee — destructure against the row directly.
    if let Type::Record { fields: row, tail } = &resolved {
        let row = row.clone();
        let is_open = matches!(tail, ridge_types::RowTail::Open(_));
        infer_structural_record_pattern(ctx, b, fields, has_rest, &resolved, &row, is_open, span);
        return;
    }

    // Legacy anon `Type::Con` with a Record schema → schema-based path.
    if let Type::Con(anon_id, _) = &resolved {
        let anon_id = *anon_id;
        if let Some(decl) = ctx.tycon_decls.get(anon_id.0 as usize) {
            if let TyConKind::Record(schema) = &decl.kind.clone() {
                let schema = schema.clone();
                let anon_name = decl.name.clone();
                crate::records::infer_record_pattern(
                    ctx, b, &schema, anon_id, &anon_name, fields, has_rest, &resolved, span,
                );
                return;
            }
        }
    }

    // Free-variable scrutinee — unify with a fresh structural record. The row is
    // open iff the pattern ends in `..` (so `{ a, .. }` matches any record with
    // an `a`; `{ a }` matches exactly `{ a }`).
    if matches!(&resolved, Type::Var(_) | Type::Error) {
        let pat_fields: Vec<(String, Type)> = fields
            .iter()
            .map(|fp| (fp.name.text.clone(), Type::Var(ctx.fresh_tyvid())))
            .collect();
        let tail = if has_rest {
            ridge_types::RowTail::Open(ctx.fresh_rowvid())
        } else {
            ridge_types::RowTail::Closed
        };
        let rec_ty = Type::record(pat_fields.clone(), tail);
        if matches!(&resolved, Type::Var(_)) {
            if let Err(e) = crate::unify::unify(ctx, expected_ty, &rec_ty) {
                ctx.errors.push(crate::records::attach_span_pub(e, span));
            }
        }
        for fp in fields {
            let field_ty = pat_fields
                .iter()
                .find(|(l, _)| *l == fp.name.text)
                .map_or(Type::Error, |(_, t)| t.clone());
            bind_or_check_field_pattern(ctx, b, fp, &field_ty);
        }
        return;
    }

    // Unexpected scrutinee type for an inline record pattern.
    let _ = emit_internal(
        ctx,
        format!("inline record pattern on unexpected type: {resolved:?}"),
        span,
    );
}

/// Destructure an inline record pattern against a known structural row.
#[allow(clippy::too_many_arguments)]
fn infer_structural_record_pattern(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    fields: &[FieldPattern],
    has_rest: bool,
    resolved: &Type,
    row: &[(String, Type)],
    is_open: bool,
    span: Span,
) {
    for fp in fields {
        let field_ty = if let Some((_, ft)) = row.iter().find(|(l, _)| *l == fp.name.text) {
            ft.clone()
        } else if is_open {
            // Open row: the field may be supplied by the tail. Grow the row to
            // record that the scrutinee carries it.
            let fresh = Type::Var(ctx.fresh_tyvid());
            let grown = Type::record(
                vec![(fp.name.text.clone(), fresh.clone())],
                ridge_types::RowTail::Open(ctx.fresh_rowvid()),
            );
            if let Err(e) = crate::unify::unify(ctx, resolved, &grown) {
                ctx.errors.push(crate::records::attach_span_pub(e, span));
            }
            fresh
        } else {
            ctx.errors.push(TypeError::UnknownField {
                record: format!("{resolved}"),
                field: fp.name.text.clone(),
                suggestions: ridge_resolve::suggest::suggest(
                    &fp.name.text,
                    row.iter().map(|(l, _)| l.clone()),
                ),
                span: fp.span,
            });
            Type::Error
        };
        bind_or_check_field_pattern(ctx, b, fp, &field_ty);
    }

    // A closed row without `..` requires every field be named.
    if !has_rest && !is_open {
        for (label, _) in row {
            if !fields.iter().any(|fp| fp.name.text == *label) {
                ctx.errors.push(TypeError::MissingField {
                    record: format!("{resolved}"),
                    field: label.clone(),
                    span,
                });
            }
        }
    }
}

/// Bind a shorthand field pattern (`{ age }`) as a new local of the field's
/// type, or recurse into an explicit sub-pattern (`{ age = p }`).
fn bind_or_check_field_pattern(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    fp: &FieldPattern,
    field_ty: &Type,
) {
    match &fp.pattern {
        Some(sub) => infer_pattern(ctx, b, sub, field_ty),
        None => ctx.env.bind(
            fp.name.text.clone(),
            crate::instantiate::monoscheme(field_ty.clone()),
        ),
    }
}

/// Attaches a source span to a `TypeError`.
///
/// `unify` returns errors with dummy `Span::point(0)`; callers replace the
/// span with the most informative location they have.
/// Look through any `Paren` wrappers to the expression they enclose.
///
/// Call-site arguments are routinely parenthesised (`f (fn u -> …)`), so the
/// quotation hook peels parentheses before testing for a lambda.
fn peel_parens(e: &Expr) -> &Expr {
    let mut cur = e;
    while let Expr::Paren { inner, .. } = cur {
        cur = inner;
    }
    cur
}

fn attach_span(err: TypeError, span: Span) -> TypeError {
    match err {
        TypeError::TypeMismatch {
            expected, found, ..
        } => TypeError::TypeMismatch {
            expected,
            found,
            span,
        },
        TypeError::ArityMismatch {
            callee,
            expected,
            found,
            hint,
            ..
        } => TypeError::ArityMismatch {
            callee,
            expected,
            found,
            span,
            hint,
        },
        TypeError::OccursCheck { var, ty, .. } => TypeError::OccursCheck { var, ty, span },
        other => other,
    }
}

// ── Convenience: build a Scheme for stdlib lookups ────────────────────────────

/// Looks up a stdlib symbol scheme by qualified name (last-segment dispatch).
///
/// This is a thin wrapper used in tests; production lookup routes through
/// `stdlib_signatures::stdlib_signature` with a real `StdlibModuleId`.
/// Returns `None` if the segment combination is not recognised.
#[cfg(test)]
pub(crate) fn lookup_stdlib_by_segments(
    b: &BuiltinTyCons,
    module: &str,
    name: &str,
) -> Option<Scheme> {
    use crate::stdlib_signatures::stdlib_signature;
    use ridge_resolve::StdlibModuleId;
    let mid = match module {
        "Int" => StdlibModuleId(0),
        "Float" => StdlibModuleId(1),
        "Bool" => StdlibModuleId(2),
        "Text" => StdlibModuleId(3),
        "List" => StdlibModuleId(4),
        "Map" => StdlibModuleId(5),
        "Set" => StdlibModuleId(6),
        "Option" => StdlibModuleId(7),
        "Result" => StdlibModuleId(8),
        "Io" => StdlibModuleId(9),
        "Fs" => StdlibModuleId(10),
        "Time" => StdlibModuleId(11),
        "Random" => StdlibModuleId(12),
        "Env" => StdlibModuleId(13),
        "Cli" => StdlibModuleId(14),
        "Proc" => StdlibModuleId(15),
        "Json" => StdlibModuleId(16),
        "NetHttp" => StdlibModuleId(17),
        _ => return None,
    };
    stdlib_signature(mid, name, b)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Ident, Span};
    use ridge_types::{CapRow, CapabilitySet, TyConArena, TyVid};

    fn dummy_span() -> Span {
        Span::point(0)
    }

    fn make_ident(text: &str) -> Ident {
        Ident {
            text: text.to_string(),
            span: dummy_span(),
        }
    }

    fn make_builtins() -> BuiltinTyCons {
        let mut arena = TyConArena::new();
        BuiltinTyCons::allocate(&mut arena)
    }

    // ── Literal inference ─────────────────────────────────────────────────────

    /// Test 1
    #[test]
    fn infer_literal_int() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let lit = Expr::Literal(Literal::IntDec {
            raw: "42".to_string(),
            span: dummy_span(),
        });
        let ty = infer_expr(&mut ctx, &b, &lit);
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "expected Int, got {ty:?}"
        );
    }

    /// Test 2
    #[test]
    fn infer_literal_float() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let lit = Expr::Literal(Literal::Float {
            raw: "3.14".to_string(),
            span: dummy_span(),
        });
        let ty = infer_expr(&mut ctx, &b, &lit);
        assert!(matches!(ty, Type::Con(id, _) if id == b.float));
    }

    /// Test 3
    #[test]
    fn infer_literal_bool() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let lit = Expr::Literal(Literal::Bool {
            value: true,
            span: dummy_span(),
        });
        let ty = infer_expr(&mut ctx, &b, &lit);
        assert!(matches!(ty, Type::Con(id, _) if id == b.bool));
    }

    /// Test 4
    #[test]
    fn infer_literal_text() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let lit = Expr::Literal(Literal::Text {
            raw: r#""hello""#.to_string(),
            span: dummy_span(),
        });
        let ty = infer_expr(&mut ctx, &b, &lit);
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    /// Test 5 — Timestamp is not a literal kind in the AST; it's a stdlib type.
    /// We test it via the prelude tycon lookup instead.
    #[test]
    fn infer_literal_int_hex() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let lit = Expr::Literal(Literal::IntHex {
            raw: "0xFF".to_string(),
            span: dummy_span(),
        });
        let ty = infer_expr(&mut ctx, &b, &lit);
        assert!(matches!(ty, Type::Con(id, _) if id == b.int));
    }

    // ── Ident: local env lookup ───────────────────────────────────────────────

    /// Test 6
    #[test]
    fn infer_ident_local() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let int_ty = Type::Con(b.int, vec![]);
        ctx.env.push_frame();
        ctx.env.bind("x".to_string(), Scheme::mono(int_ty));

        let expr = Expr::Ident(make_ident("x"));
        let ty = infer_expr(&mut ctx, &b, &expr);
        assert!(matches!(ty, Type::Con(id, _) if id == b.int));
    }

    // ── Ident: stdlib polymorphic lookup ─────────────────────────────────────

    /// Test 7 — `List.map` instantiates fresh vars each call.
    #[test]
    fn infer_ident_polymorphic_stdlib() {
        let b = make_builtins();
        // List.map : ∀ a b c. (fn c (a -> b)) -> List a -> List b
        let scheme = lookup_stdlib_by_segments(&b, "List", "map").expect("List.map must exist");
        let mut ctx = InferCtx::new();

        let t1 = instantiate(&mut ctx, &scheme);
        let t2 = instantiate(&mut ctx, &scheme);

        // The two instantiations must produce distinct type vars.
        let fresh_from = |t: &Type| -> TyVid {
            match t {
                Type::Fn { params, .. } => {
                    // First param is the callback fn. Its param is ?a.
                    match &params[0] {
                        Type::Fn {
                            params: cb_params, ..
                        } => match &cb_params[0] {
                            Type::Var(v) => *v,
                            other => panic!("expected Var in callback param, got {other:?}"),
                        },
                        other => panic!("expected Fn callback, got {other:?}"),
                    }
                }
                other => panic!("expected Fn from List.map, got {other:?}"),
            }
        };
        let v1 = fresh_from(&t1);
        let v2 = fresh_from(&t2);
        assert_ne!(v1, v2, "each instantiation must produce fresh vars");
    }

    // ── Qualified: module symbol lookup ──────────────────────────────────────

    /// Test 8 — `List.length` resolves to a scheme and instantiates.
    #[test]
    fn infer_qualified_module_symbol() {
        let b = make_builtins();
        let scheme =
            lookup_stdlib_by_segments(&b, "List", "length").expect("List.length must exist");
        let mut ctx = InferCtx::new();
        let ty = instantiate(&mut ctx, &scheme);
        // List.length : List a -> Int — result is a Fn type.
        assert!(
            matches!(ty, Type::Fn { .. }),
            "List.length must be a function type"
        );
    }

    // ── Lambda ────────────────────────────────────────────────────────────────

    /// Test 9 — `fn x -> x` infers as `?a -> ?a`
    #[test]
    fn infer_lambda_identity() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // fn x -> x
        let x_pat = Pattern::Var {
            name: make_ident("x"),
            span: dummy_span(),
        };
        let body = Expr::Ident(make_ident("x"));
        let lambda = Expr::Lambda {
            params: vec![LambdaParam::Pattern(x_pat)],
            body: Box::new(body),
            span: dummy_span(),
        };

        ctx.env.push_frame(); // outer scope for env
        let ty = infer_expr(&mut ctx, &b, &lambda);
        ctx.env.pop_frame();

        // Should be Fn { params: [Var(_)], ret: Var(_), .. }
        match ty {
            Type::Fn { params, ret, .. } => {
                assert_eq!(params.len(), 1);
                let p = ctx.shallow_resolve(&params[0]);
                let r = ctx.shallow_resolve(&ret);
                assert!(matches!(p, Type::Var(_)), "param should be a Var");
                assert!(matches!(r, Type::Var(_)), "ret should be a Var");
                // The param and ret should be the same variable (identity fn).
                if let (Type::Var(pv), Type::Var(rv)) = (&p, &r) {
                    assert_eq!(pv, rv, "identity fn: param and ret must be same var");
                }
            }
            other => panic!("expected Fn, got {other:?}"),
        }
    }

    // ── Lambda applied ────────────────────────────────────────────────────────

    /// Test 10 — `(fn x -> x) 5` types as Int
    #[test]
    fn infer_lambda_call_unifies_param() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // fn x -> x
        let x_pat = Pattern::Var {
            name: make_ident("x"),
            span: dummy_span(),
        };
        let lambda = Expr::Lambda {
            params: vec![LambdaParam::Pattern(x_pat)],
            body: Box::new(Expr::Ident(make_ident("x"))),
            span: dummy_span(),
        };
        let five = Expr::Literal(Literal::IntDec {
            raw: "5".to_string(),
            span: dummy_span(),
        });
        let call = Expr::Call {
            callee: Box::new(lambda),
            args: vec![five],
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &call);
        let resolved = ctx.shallow_resolve(&ty);
        assert!(
            matches!(resolved, Type::Con(id, _) if id == b.int),
            "expected Int, got {resolved:?}"
        );
        ctx.env.pop_frame();
    }

    // ── Let (monomorphic only — T7 needed for full polymorphism) ─────────────

    /// Test 11 — `let f = fn x -> x; f 5` types as Int (without generalisation).
    ///
    /// This is the plan's key `DoD` requirement: even without full generalisation
    /// the monomorphic let-binding + call sequence must type as Int.
    #[test]
    fn infer_let_then_apply_to_int() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // let f = fn x -> x
        let x_pat = Pattern::Var {
            name: make_ident("x"),
            span: dummy_span(),
        };
        let lambda = Expr::Lambda {
            params: vec![LambdaParam::Pattern(x_pat)],
            body: Box::new(Expr::Ident(make_ident("x"))),
            span: dummy_span(),
        };
        let let_f = Expr::Let {
            pat: Pattern::Var {
                name: make_ident("f"),
                span: dummy_span(),
            },
            ty: None,
            value: Box::new(lambda),
            span: dummy_span(),
        };

        // Simulate block: [let f = fn x -> x, f 5]
        let five = Expr::Literal(Literal::IntDec {
            raw: "5".to_string(),
            span: dummy_span(),
        });
        let call_f = Expr::Call {
            callee: Box::new(Expr::Ident(make_ident("f"))),
            args: vec![five],
            span: dummy_span(),
        };

        let block = Expr::Block(ridge_ast::Block {
            stmts: vec![let_f, call_f],
            span: dummy_span(),
        });

        let ty = infer_expr(&mut ctx, &b, &block);
        let resolved = ctx.shallow_resolve(&ty);
        assert!(
            matches!(resolved, Type::Con(id, _) if id == b.int),
            "let f = fn x -> x; f 5 must type as Int, got {resolved:?}"
        );
        assert!(
            ctx.errors.is_empty(),
            "no errors expected, got: {:?}",
            ctx.errors
        );
        ctx.env.pop_frame();
    }

    // ── If ────────────────────────────────────────────────────────────────────

    /// Test 12
    #[test]
    fn infer_if_branches_unify() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let cond = Expr::Literal(Literal::Bool {
            value: true,
            span: dummy_span(),
        });
        let then_br = Expr::Literal(Literal::IntDec {
            raw: "1".to_string(),
            span: dummy_span(),
        });
        let else_br = Expr::Literal(Literal::IntDec {
            raw: "2".to_string(),
            span: dummy_span(),
        });
        let if_expr = Expr::If {
            cond: Box::new(cond),
            then_branch: Box::new(then_br),
            else_branch: Some(Box::new(else_br)),
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &if_expr);
        assert!(matches!(ty, Type::Con(id, _) if id == b.int));
        assert!(ctx.errors.is_empty());
        ctx.env.pop_frame();
    }

    // ── Match ─────────────────────────────────────────────────────────────────

    /// Test 13
    #[test]
    fn infer_match_arms_unify() {
        use ridge_ast::MatchArm;
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let scrutinee = Expr::Literal(Literal::IntDec {
            raw: "5".to_string(),
            span: dummy_span(),
        });
        // arm1: literal `5` matches the specific int value
        let arm1 = MatchArm {
            pattern: Pattern::Literal {
                lit: Literal::IntDec {
                    raw: "5".to_string(),
                    span: dummy_span(),
                },
                span: dummy_span(),
            },
            guard: None,
            body: Expr::Literal(Literal::IntDec {
                raw: "1".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        };
        // arm2: wildcard `_` covers all other cases (not redundant after arm1)
        let arm2 = MatchArm {
            pattern: Pattern::Wildcard { span: dummy_span() },
            guard: None,
            body: Expr::Literal(Literal::IntDec {
                raw: "0".to_string(),
                span: dummy_span(),
            }),
            span: dummy_span(),
        };
        let match_expr = Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms: vec![arm1, arm2],
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &match_expr);
        assert!(matches!(ty, Type::Con(id, _) if id == b.int));
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Call: arity mismatch ──────────────────────────────────────────────────

    /// Test 14 — calling a unary fn with 2 args fires T003
    #[test]
    fn infer_call_arity_mismatch_fires_t003() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // fn x -> x — unary
        let x_pat = Pattern::Var {
            name: make_ident("x"),
            span: dummy_span(),
        };
        let lambda = Expr::Lambda {
            params: vec![LambdaParam::Pattern(x_pat)],
            body: Box::new(Expr::Ident(make_ident("x"))),
            span: dummy_span(),
        };
        // Call with 2 args
        let call = Expr::Call {
            callee: Box::new(lambda),
            args: vec![
                Expr::Literal(Literal::IntDec {
                    raw: "1".to_string(),
                    span: dummy_span(),
                }),
                Expr::Literal(Literal::IntDec {
                    raw: "2".to_string(),
                    span: dummy_span(),
                }),
            ],
            span: dummy_span(),
        };

        infer_expr(&mut ctx, &b, &call);
        let has_t003 = ctx.errors.iter().any(|e| e.code() == "T003");
        assert!(has_t003, "expected T003, errors: {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Call: type mismatch ───────────────────────────────────────────────────

    /// Test 15 — calling Int->Int with a Text arg fires T001
    #[test]
    fn infer_call_param_type_mismatch_fires_t001() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Bind `neg` as Int -> Int
        let neg_ty = Type::Fn {
            params: vec![Type::Con(b.int, vec![])],
            ret: Box::new(Type::Con(b.int, vec![])),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        ctx.env.bind("neg".to_string(), Scheme::mono(neg_ty));

        let call = Expr::Call {
            callee: Box::new(Expr::Ident(make_ident("neg"))),
            args: vec![Expr::Literal(Literal::Text {
                raw: r#""hello""#.to_string(),
                span: dummy_span(),
            })],
            span: dummy_span(),
        };

        infer_expr(&mut ctx, &b, &call);
        let has_t001 = ctx.errors.iter().any(|e| e.code() == "T001");
        assert!(has_t001, "expected T001, errors: {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Return ────────────────────────────────────────────────────────────────

    /// Test 16 — return inside a fn unifies with the fn's return type
    #[test]
    fn infer_return_unifies_with_enclosing_fn_ret() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Set the enclosing fn return type to Int.
        ctx.current_fn_ret = Some(Type::Con(b.int, vec![]));

        let ret_expr = Expr::Return {
            value: Box::new(Expr::Literal(Literal::IntDec {
                raw: "42".to_string(),
                span: dummy_span(),
            })),
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &ret_expr);
        // Return itself has type Unit.
        assert!(matches!(ty, Type::Con(id, _) if id == b.unit));
        // No errors — the return value (Int) unifies with the declared ret (Int).
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Return outside fn ─────────────────────────────────────────────────────

    /// Test 17 — return with no enclosing fn return type: no error (parser
    ///            allows it; type checker just skips unification).
    #[test]
    fn infer_return_outside_fn_no_error() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // current_fn_ret is None (outside any fn).
        let ret_expr = Expr::Return {
            value: Box::new(Expr::Literal(Literal::IntDec {
                raw: "1".to_string(),
                span: dummy_span(),
            })),
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &ret_expr);
        assert!(matches!(ty, Type::Con(id, _) if id == b.unit));
        // No T### fires when there's no fn context (parser-level issue, not type-level).
        assert!(ctx.errors.is_empty(), "unexpected errors: {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Pattern: wildcard ─────────────────────────────────────────────────────

    /// Test 18
    #[test]
    fn infer_pattern_wildcard() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let int_ty = Type::Con(b.int, vec![]);
        let pat = Pattern::Wildcard { span: dummy_span() };
        infer_pattern(&mut ctx, &b, &pat, &int_ty);
        // No bindings added, no errors.
        assert!(ctx.errors.is_empty());
        assert!(ctx.env.lookup("_").is_none(), "wildcard must not bind '_'");
        ctx.env.pop_frame();
    }

    // ── Pattern: ident binds ──────────────────────────────────────────────────

    /// Test 19
    #[test]
    fn infer_pattern_ident_binds() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let int_ty = Type::Con(b.int, vec![]);
        let pat = Pattern::Var {
            name: make_ident("n"),
            span: dummy_span(),
        };
        infer_pattern(&mut ctx, &b, &pat, &int_ty);
        // `n` should now be bound in env.
        let scheme = ctx.env.lookup("n").expect("n must be bound");
        assert!(matches!(&scheme.ty, Type::Con(id, _) if *id == b.int));
        ctx.env.pop_frame();
    }

    // ── Pattern: literal unifies ──────────────────────────────────────────────

    /// Test 20
    #[test]
    fn infer_pattern_literal_unifies() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let int_ty = Type::Con(b.int, vec![]);
        let pat = Pattern::Literal {
            lit: Literal::IntDec {
                raw: "5".to_string(),
                span: dummy_span(),
            },
            span: dummy_span(),
        };
        infer_pattern(&mut ctx, &b, &pat, &int_ty);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    /// Test 20b — literal pattern type mismatch fires T001
    #[test]
    fn infer_pattern_literal_mismatch_fires_t001() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let text_ty = Type::Con(b.text, vec![]);
        let pat = Pattern::Literal {
            lit: Literal::IntDec {
                raw: "5".to_string(),
                span: dummy_span(),
            },
            span: dummy_span(),
        };
        infer_pattern(&mut ctx, &b, &pat, &text_ty);
        let has_t001 = ctx.errors.iter().any(|e| e.code() == "T001");
        assert!(has_t001, "expected T001, errors: {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Pattern: tuple ────────────────────────────────────────────────────────

    /// Test 21
    #[test]
    fn infer_pattern_tuple() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        let int_ty = Type::Con(b.int, vec![]);
        let text_ty = Type::Con(b.text, vec![]);
        let tuple_ty = Type::Tuple(vec![int_ty, text_ty]);
        let pat = Pattern::Tuple {
            elems: vec![
                Pattern::Var {
                    name: make_ident("a"),
                    span: dummy_span(),
                },
                Pattern::Var {
                    name: make_ident("b"),
                    span: dummy_span(),
                },
            ],
            span: dummy_span(),
        };
        infer_pattern(&mut ctx, &b, &pat, &tuple_ty);
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        let a_scheme = ctx.env.lookup("a").expect("a must be bound");
        assert!(matches!(&a_scheme.ty, Type::Con(id, _) if *id == b.int));
        let b_scheme = ctx.env.lookup("b").expect("b must be bound");
        assert!(matches!(&b_scheme.ty, Type::Con(id, _) if *id == b.text));
        ctx.env.pop_frame();
    }

    // ── Block ─────────────────────────────────────────────────────────────────

    /// Test 22 — block returns last statement's type
    #[test]
    fn infer_block_returns_last_stmt_type() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let block = ridge_ast::Block {
            stmts: vec![
                Expr::Literal(Literal::IntDec {
                    raw: "1".to_string(),
                    span: dummy_span(),
                }),
                Expr::Literal(Literal::Text {
                    raw: r#""hello""#.to_string(),
                    span: dummy_span(),
                }),
            ],
            span: dummy_span(),
        };

        let ty = infer_block(&mut ctx, &b, &block);
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
        ctx.env.pop_frame();
    }

    /// Test 23 — empty block returns Unit
    #[test]
    fn infer_block_empty_unit() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let block = ridge_ast::Block {
            stmts: vec![],
            span: dummy_span(),
        };
        let ty = infer_block(&mut ctx, &b, &block);
        assert!(matches!(ty, Type::Con(id, _) if id == b.unit));
    }

    // ── InnerFn ───────────────────────────────────────────────────────────────

    /// Test 24 — basic inner fn declaration binds the name in outer scope
    #[test]
    fn infer_inner_fn_basic() {
        use ridge_ast::{FnDecl, PrimitiveType, Visibility};

        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // inner fn add (x: Int) -> Int = x
        let decl = FnDecl {
            attrs: vec![],
            vis: Visibility::Private,
            caps: vec![],
            name: make_ident("add"),
            params: vec![ridge_ast::Param::Annotated {
                name: make_ident("x"),
                ty: ridge_ast::Type::Primitive {
                    name: PrimitiveType::Int,
                    span: dummy_span(),
                },
                span: dummy_span(),
            }],
            ret: Some(ridge_ast::Type::Primitive {
                name: PrimitiveType::Int,
                span: dummy_span(),
            }),
            constraints: vec![],
            body: Body::Expr(Expr::Ident(make_ident("x"))),
            span: dummy_span(),
            doc: None,
        };
        let inner_fn = Expr::InnerFn {
            decl: Box::new(decl),
            span: dummy_span(),
        };

        infer_expr(&mut ctx, &b, &inner_fn);
        // The inner fn name `add` should now be bound in outer scope.
        let scheme = ctx
            .env
            .lookup("add")
            .expect("add must be bound after InnerFn");
        assert!(
            matches!(&scheme.ty, Type::Fn { .. }),
            "add must be a Fn type"
        );
        ctx.env.pop_frame();
    }

    // ── Assign ────────────────────────────────────────────────────────────────

    /// Test 25 — assign returns Unit
    #[test]
    fn infer_assign_returns_unit() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Bind `x` as Int
        ctx.env
            .bind("x".to_string(), Scheme::mono(Type::Con(b.int, vec![])));

        let assign = Expr::Assign {
            target: Box::new(Expr::Ident(make_ident("x"))),
            value: Box::new(Expr::Literal(Literal::IntDec {
                raw: "5".to_string(),
                span: dummy_span(),
            })),
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &assign);
        assert!(matches!(ty, Type::Con(id, _) if id == b.unit));
        assert!(ctx.errors.is_empty(), "errors: {:?}", ctx.errors);
        ctx.env.pop_frame();
    }

    // ── Var (mutable binding) ─────────────────────────────────────────────────

    /// Test 26 — var binding is treated like let (monomorphic)
    #[test]
    fn infer_var_mutable_binding() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let var_expr = Expr::Var {
            name: make_ident("count"),
            ty: None,
            value: Box::new(Expr::Literal(Literal::IntDec {
                raw: "0".to_string(),
                span: dummy_span(),
            })),
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &var_expr);
        assert!(matches!(ty, Type::Con(id, _) if id == b.unit));
        // count should be bound to Int
        let scheme = ctx.env.lookup("count").expect("count must be bound");
        assert!(matches!(&scheme.ty, Type::Con(id, _) if *id == b.int));
        ctx.env.pop_frame();
    }

    // ── Env: frame push/pop ───────────────────────────────────────────────────

    /// Test 27 — env.lookup respects lexical scoping (inner hides outer)
    #[test]
    fn env_inner_scope_hides_outer() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        ctx.env.push_frame();
        ctx.env
            .bind("x".to_string(), Scheme::mono(Type::Con(b.int, vec![])));

        ctx.env.push_frame();
        ctx.env
            .bind("x".to_string(), Scheme::mono(Type::Con(b.text, vec![])));
        // Inner binding takes precedence.
        let inner = ctx.env.lookup("x").expect("x must be in scope");
        assert!(matches!(&inner.ty, Type::Con(id, _) if *id == b.text));
        ctx.env.pop_frame();

        // After pop, outer binding is visible.
        let outer = ctx.env.lookup("x").expect("x must still be in scope");
        assert!(matches!(&outer.ty, Type::Con(id, _) if *id == b.int));
        ctx.env.pop_frame();
    }

    // ── Tuple expression ──────────────────────────────────────────────────────

    /// Test 28
    #[test]
    fn infer_tuple_expr() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let tup = Expr::Tuple {
            elems: vec![
                Expr::Literal(Literal::IntDec {
                    raw: "1".to_string(),
                    span: dummy_span(),
                }),
                Expr::Literal(Literal::Bool {
                    value: false,
                    span: dummy_span(),
                }),
            ],
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &tup);
        match ty {
            Type::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(&elems[0], Type::Con(id, _) if *id == b.int));
                assert!(matches!(&elems[1], Type::Con(id, _) if *id == b.bool));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
        ctx.env.pop_frame();
    }

    // ── List expression ───────────────────────────────────────────────────────

    /// Test 29
    #[test]
    fn infer_list_homogeneous() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let list = Expr::List {
            elems: vec![
                Expr::Literal(Literal::IntDec {
                    raw: "1".to_string(),
                    span: dummy_span(),
                }),
                Expr::Literal(Literal::IntDec {
                    raw: "2".to_string(),
                    span: dummy_span(),
                }),
            ],
            span: dummy_span(),
        };

        let ty = infer_expr(&mut ctx, &b, &list);
        assert!(
            matches!(&ty, Type::Con(id, args) if *id == b.list && args.len() == 1),
            "expected List Int, got {ty:?}"
        );
        ctx.env.pop_frame();
    }

    // ── Phase 4.5 T3: node_types write-back tests ─────────────────────────────

    fn make_node_id_map_for_expr(span: Span) -> ridge_resolve::NodeIdMap {
        let mut map = ridge_resolve::NodeIdMap::default();
        // Stamp NodeKind::Expr at the given span.
        map.assign(span, ridge_resolve::NodeKind::Expr)
            .expect("assign ok");
        map
    }

    /// T3-1: literal expression — `node_types_accum` populated with Int type.
    #[test]
    fn t3_literal_node_type_written() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let sp = Span::new(0, 2);
        ctx.node_id_map = Some(make_node_id_map_for_expr(sp));

        let lit = Expr::Literal(Literal::IntDec {
            raw: "42".to_string(),
            span: sp,
        });
        infer_expr(&mut ctx, &b, &lit);

        // NodeId 0 was stamped for sp with NodeKind::Expr.
        assert_eq!(
            ctx.node_types_accum.len(),
            1,
            "accumulator must have 1 entry"
        );
        assert!(
            matches!(&ctx.node_types_accum[0], Some(Type::Con(id, _)) if *id == b.int),
            "expected Int in node_types_accum[0], got {:?}",
            ctx.node_types_accum[0]
        );
    }

    /// T3-2: ident expression — `node_types_accum` populated with the bound type.
    #[test]
    fn t3_ident_node_type_written() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        let sp = Span::new(0, 3);
        ctx.node_id_map = Some(make_node_id_map_for_expr(sp));
        ctx.env.push_frame();
        // Bind `x` to Int mono-scheme.
        ctx.env.bind(
            "x".to_string(),
            crate::instantiate::monoscheme(Type::Con(b.int, vec![])),
        );

        let ident = Expr::Ident(Ident {
            text: "x".to_string(),
            span: sp,
        });
        infer_expr(&mut ctx, &b, &ident);
        ctx.env.pop_frame();

        assert_eq!(ctx.node_types_accum.len(), 1);
        assert!(
            matches!(&ctx.node_types_accum[0], Some(Type::Con(id, _)) if *id == b.int),
            "expected Int, got {:?}",
            ctx.node_types_accum[0]
        );
    }

    /// T3-3: call expression — `node_types_accum` populated with return type.
    #[test]
    fn t3_call_node_type_written() {
        use ridge_ast::Span;
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // Set up: fn_span for callee, arg_span for arg, call_span for the call.
        let fn_span = Span::new(0, 3);
        let arg_span = Span::new(4, 6);
        let call_span = Span::new(0, 6);

        let mut map = ridge_resolve::NodeIdMap::default();
        map.assign(fn_span, ridge_resolve::NodeKind::Expr)
            .expect("fn");
        map.assign(arg_span, ridge_resolve::NodeKind::Expr)
            .expect("arg");
        map.assign(call_span, ridge_resolve::NodeKind::Expr)
            .expect("call");
        ctx.node_id_map = Some(map);

        ctx.env.push_frame();
        // Bind `f` to `Int -> Text` scheme.
        ctx.env.bind(
            "f".to_string(),
            crate::instantiate::monoscheme(Type::Fn {
                params: vec![Type::Con(b.int, vec![])],
                ret: Box::new(Type::Con(b.text, vec![])),
                caps: ridge_types::CapRow::Concrete(CapabilitySet::PURE),
            }),
        );

        let call = Expr::Call {
            callee: Box::new(Expr::Ident(Ident {
                text: "f".to_string(),
                span: fn_span,
            })),
            args: vec![Expr::Literal(Literal::IntDec {
                raw: "1".to_string(),
                span: arg_span,
            })],
            span: call_span,
        };
        let ty = infer_expr(&mut ctx, &b, &call);
        ctx.env.pop_frame();

        assert!(
            matches!(ty, Type::Con(id, _) if id == b.text),
            "call should return Text, got {ty:?}"
        );
        // Find the call's NodeId (it was assigned index 2 since fn=0, arg=1, call=2).
        assert!(ctx.node_types_accum.len() >= 3, "need 3 entries");
        assert!(
            matches!(&ctx.node_types_accum[2], Some(Type::Con(id, _)) if *id == b.text),
            "expected Text at call node, got {:?}",
            ctx.node_types_accum[2]
        );
    }

    /// T3-4: if-then-else — the if expression's type is written back.
    #[test]
    fn t3_if_node_type_written() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        let cond_sp = Span::new(0, 4);
        let then_sp = Span::new(5, 6);
        let else_sp = Span::new(7, 8);
        let if_sp = Span::new(0, 8);

        let mut map = ridge_resolve::NodeIdMap::default();
        map.assign(cond_sp, ridge_resolve::NodeKind::Ident).ok(); // cond ident
        map.assign(cond_sp, ridge_resolve::NodeKind::Expr)
            .expect("cond");
        map.assign(then_sp, ridge_resolve::NodeKind::Expr)
            .expect("then");
        map.assign(else_sp, ridge_resolve::NodeKind::Expr)
            .expect("else");
        map.assign(if_sp, ridge_resolve::NodeKind::Expr)
            .expect("if");
        ctx.node_id_map = Some(map);

        ctx.env.push_frame();
        ctx.env.bind(
            "b".to_string(),
            crate::instantiate::monoscheme(Type::Con(b.bool, vec![])),
        );

        let if_expr = Expr::If {
            cond: Box::new(Expr::Ident(Ident {
                text: "b".to_string(),
                span: cond_sp,
            })),
            then_branch: Box::new(Expr::Literal(Literal::IntDec {
                raw: "1".to_string(),
                span: then_sp,
            })),
            else_branch: Some(Box::new(Expr::Literal(Literal::IntDec {
                raw: "2".to_string(),
                span: else_sp,
            }))),
            span: if_sp,
        };
        let ty = infer_expr(&mut ctx, &b, &if_expr);
        ctx.env.pop_frame();

        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "if should return Int, got {ty:?}"
        );
        assert!(
            ctx.node_types_accum.len() >= 4,
            "need entries for cond, then, else, if"
        );
    }

    /// T3-5: match expression — the match's result type is written back.
    #[test]
    fn t3_match_node_type_written() {
        use ridge_ast::{MatchArm, Pattern};
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        let scrut_sp = Span::new(0, 4);
        let arm_sp = Span::new(5, 6);
        let match_sp = Span::new(0, 6);

        let mut map = ridge_resolve::NodeIdMap::default();
        map.assign(scrut_sp, ridge_resolve::NodeKind::Ident).ok();
        map.assign(scrut_sp, ridge_resolve::NodeKind::Expr)
            .expect("scrut");
        map.assign(arm_sp, ridge_resolve::NodeKind::Expr)
            .expect("arm");
        map.assign(match_sp, ridge_resolve::NodeKind::Expr)
            .expect("match");
        ctx.node_id_map = Some(map);

        ctx.env.push_frame();
        ctx.env.bind(
            "x".to_string(),
            crate::instantiate::monoscheme(Type::Con(b.int, vec![])),
        );
        // Need tycon_decls and user_tycon_names for exhaustiveness check.
        let mut arena = ridge_types::TyConArena::new();
        let builtins = BuiltinTyCons::allocate(&mut arena);
        ctx.tycon_decls = arena.all().to_vec();

        let match_expr = Expr::Match {
            scrutinee: Box::new(Expr::Ident(Ident {
                text: "x".to_string(),
                span: scrut_sp,
            })),
            arms: vec![MatchArm {
                pattern: Pattern::Wildcard { span: arm_sp },
                guard: None,
                body: Expr::Literal(Literal::IntDec {
                    raw: "0".to_string(),
                    span: arm_sp,
                }),
                span: arm_sp,
            }],
            span: match_sp,
        };
        let ty = infer_expr(&mut ctx, &builtins, &match_expr);
        ctx.env.pop_frame();

        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "match should return Int, got {ty:?}"
        );
        assert!(
            ctx.node_types_accum.len() >= 2,
            "need at least arm and match entries"
        );
    }

    /// T3-6: lambda expression — the lambda's Fn type is written back.
    #[test]
    fn t3_lambda_node_type_written() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        let param_sp = Span::new(0, 1);
        let body_sp = Span::new(2, 3);
        let lambda_sp = Span::new(0, 3);

        let mut map = ridge_resolve::NodeIdMap::default();
        map.assign(param_sp, ridge_resolve::NodeKind::Ident).ok();
        map.assign(body_sp, ridge_resolve::NodeKind::Ident).ok();
        map.assign(body_sp, ridge_resolve::NodeKind::Expr)
            .expect("body");
        map.assign(lambda_sp, ridge_resolve::NodeKind::Expr)
            .expect("lambda");
        ctx.node_id_map = Some(map);

        ctx.env.push_frame();
        let lambda = Expr::Lambda {
            params: vec![LambdaParam::Pattern(Pattern::Var {
                name: Ident {
                    text: "x".to_string(),
                    span: param_sp,
                },
                span: param_sp,
            })],
            body: Box::new(Expr::Ident(Ident {
                text: "x".to_string(),
                span: body_sp,
            })),
            span: lambda_sp,
        };
        infer_expr(&mut ctx, &b, &lambda);
        ctx.env.pop_frame();

        // The lambda node (at lambda_sp) should have a Fn type.
        // lambda_sp was assigned last, so its NodeId index is the highest assigned.
        let has_fn_type = ctx
            .node_types_accum
            .iter()
            .any(|t| matches!(t, Some(Type::Fn { .. })));
        assert!(
            has_fn_type,
            "expected at least one Fn type in node_types_accum"
        );
    }
}
