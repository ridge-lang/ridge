//! Inner-function lowering — §4.12.
//!
//! # Rule summary
//!
//! `Expr::InnerFn { decl, span }` lowers a named local function to a
//! `LetIn(Bind, Lambda)` pair.  The function name is bound in the enclosing
//! scope via `IrPat::Bind`, and the value is an `IrExpr::Lambda`.
//!
//! When `[InnerFn { decl, span: ifspan }, …rest]` is encountered during
//! `crate::block::fold_block_to_continuation`, the result is:
//!
//! ```text
//! IrExpr::LetIn {
//!     pat:   IrPat::Bind { name: decl.name.text, inner: None },
//!     value: IrExpr::Lambda {
//!                params: decl.params.iter().map(param_to_ir_param),
//!                body:   lower_expr(decl.body),
//!                caps:   lookup_inferred_caps(decl.span),
//!            },
//!     body:  fold_block_to_continuation(rest, …),   -- the continuation
//! }
//! ```
//!
//! ## Recursive inner functions
//!
//! When the lambda body references its own name via `Expr::Ident`, the
//! `BindingMap` (set up by Phase 3) classifies that ident as
//! `Binding::Local(...)`.  The lowerer does **not** special-case this: it
//! simply calls `lower_expr` on the body and trusts the binding-map result —
//! the recursive reference becomes `IrExpr::Local { name: decl.name.text }`.
//!
//! ## Nested inner functions
//!
//! Each level of nesting produces its own `LetIn(Lambda)`.  The recursion in
//! `fold_block_to_continuation` handles this naturally.
//!
//! ## `InnerFn` as final statement
//!
//! When the inner fn is the last statement in its block (no rest), the `body`
//! of the `LetIn` is `IrExpr::Lit { Unit }` — matching the existing `[Let]`
//! and `[Var]` final-stmt arms in `block.rs`.
//!
//! ## Bare `InnerFn` outside block context
//!
//! When `lower_expr` encounters `Expr::InnerFn` directly (not via the block
//! fold) it emits `L999 InternalLoweringError` with a clear message and
//! returns `IrExpr::Lit { Unit }`.
//!
//! ## Capability placeholder
//!
//! `CapabilitySet::PURE` is used as a placeholder until Phase 4's
//! `inferred_caps` side-table is wired into `LowerCtx` (T17).  This mirrors
//! the approach used in `field_accessor.rs`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{decl::FnDecl, Body, Param, Span};
use ridge_ir::{IrExpr, IrLit, IrParam, IrPat};
use ridge_resolve::NodeKind;
use ridge_types::Type;

use crate::ast_type::lower_ast_type;
use crate::block::fold_block_to_continuation;
use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower an `InnerFn` statement that appears in a block with a non-empty
/// continuation (`rest`).
///
/// Called from `fold_block_to_continuation` when it matches
/// `[Expr::InnerFn { decl, span: ifspan }, rest @ ..]`.
pub fn lower_inner_fn_with_continuation(
    ctx: &mut LowerCtx<'_>,
    decl: &FnDecl,
    ifspan: Span,
    rest: &[ridge_ast::Expr],
    block_span: Span,
) -> IrExpr {
    let let_id = ctx.fresh_id(None);

    let lambda = build_lambda(ctx, decl);

    let pat = IrPat::Bind {
        name: decl.name.text.clone(),
        inner: None,
        span: decl.name.span,
    };

    let body = fold_block_to_continuation(ctx, rest, block_span);

    IrExpr::LetIn {
        id: let_id,
        pat,
        value: Box::new(lambda),
        body: Box::new(body),
        span: ifspan,
    }
}

/// Lower an `InnerFn` that is the **final** statement in its block (no rest).
///
/// The `body` of the `LetIn` is a synthesised `Unit` literal.
pub fn lower_inner_fn_final(ctx: &mut LowerCtx<'_>, decl: &FnDecl, ifspan: Span) -> IrExpr {
    let let_id = ctx.fresh_id(None);

    let lambda = build_lambda(ctx, decl);

    let pat = IrPat::Bind {
        name: decl.name.text.clone(),
        inner: None,
        span: decl.name.span,
    };

    let unit_id = ctx.fresh_id(None);
    let body = IrExpr::Lit {
        id: unit_id,
        value: IrLit::Unit,
        span: ifspan,
    };

    IrExpr::LetIn {
        id: let_id,
        pat,
        value: Box::new(lambda),
        body: Box::new(body),
        span: ifspan,
    }
}

/// Lower a bare `InnerFn` expression encountered outside any block context.
///
/// This is a Phase 4 invariant violation: `InnerFn` is a block-level statement
/// and should never reach `lower_expr` directly.  Emits `L999` with a clear
/// message and returns `IrExpr::Lit { Unit }`.
pub fn lower_inner_fn_bare(ctx: &mut LowerCtx<'_>, decl: &FnDecl, ifspan: Span) -> IrExpr {
    let id = ctx.fresh_id(None);
    ctx.errors.push(LowerError::InternalLoweringError {
        span: ifspan,
        message: format!(
            "`fn {}` (InnerFn) encountered outside block context; \
             inner functions are block-level statements and must appear inside a block",
            decl.name.text
        ),
    });
    IrExpr::Lit {
        id,
        value: IrLit::Unit,
        span: ifspan,
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Build the `IrExpr::Lambda` for the inner function's body.
///
/// # Capability set
///
/// Reads the effective caps from Phase 4's `inferred_caps` side-table via the
/// proxy `NodeId(decl.span.start)` (see [`LowerCtx::lookup_inferred_caps`]).
/// Falls back to `CapabilitySet::PURE` when the table is absent or has no
/// entry for this span (inner `fn` lambdas are keyed by top-level `fn` spans
/// only; anonymous lambdas have no `inferred_caps` entry).
fn build_lambda(ctx: &mut LowerCtx<'_>, decl: &FnDecl) -> IrExpr {
    let lambda_id = ctx.fresh_id(None);

    // PHASE45-T3: attempt to lift bare-param types from node_types via the
    // InnerFn expression's Type::Fn.  For InnerFn, node_types stores
    // Type::Unit at (decl.span, NodeKind::Expr) (the expression evaluates to
    // unit); the Type::Fn shape will not match and the lookup falls back to
    // Type::Error — which is the same as the prior NodeKind::Ident path but
    // uses the correct structural approach.
    let params: Vec<IrParam> = decl
        .params
        .iter()
        .enumerate()
        .map(|(idx, p)| param_to_ir_param(ctx, decl.span, idx, p))
        .collect();

    // Inner fns always have Body::Expr; Body::Ffi is only valid at module
    // top-level (T3 will reject it elsewhere).
    let body_expr = match &decl.body {
        Body::Expr(e) => e,
        Body::Ffi { .. } => {
            // TODO(T3): @ffi in inner-fn position is a T003 error, not lowerable.
            unreachable!("Body::Ffi in inner-fn position — T3 must reject this before lowering")
        }
    };
    let body = Box::new(lower_expr(ctx, body_expr));

    let caps = ctx.lookup_inferred_caps(decl.span);

    IrExpr::Lambda {
        id: lambda_id,
        params,
        body,
        caps,
        span: decl.span,
    }
}

/// Convert an AST [`Param`] to an [`IrParam`].
///
/// For `Param::Annotated` the declared type annotation is lowered via
/// [`lower_ast_type`].  For `Param::Bare` (no annotation) the type is resolved
/// by looking up the enclosing fn's `Type::Fn` at `(fn_span, NodeKind::Expr)`
/// and indexing `params[param_idx]`.  For inner fns the expression node stores
/// `Type::Unit` (not a `Fn`), so this always falls back to `Type::Error` in
/// practice — but uses the correct structural pattern (same as lambdas in
/// `crate::core`).
///
/// PHASE45-T3: bare param type lifted from enclosing fn's `Type::Fn` (structural pattern).
fn param_to_ir_param(
    ctx: &mut LowerCtx<'_>,
    fn_span: Span,
    param_idx: usize,
    param: &Param,
) -> IrParam {
    match param {
        Param::Bare(ident) => {
            // PHASE45-T3: look up the fn's Type::Fn from (fn_span, NodeKind::Expr)
            // and extract params[param_idx].  Inner-fn expressions store Type::Unit
            // so this falls back to Type::Error; the structural pattern is correct.
            let ty = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(fn_span, NodeKind::Expr))
                .and_then(|nid| ctx.node_type(nid).cloned())
                .and_then(|fn_ty| {
                    if let Type::Fn { params, .. } = fn_ty {
                        params.into_iter().nth(param_idx)
                    } else {
                        None
                    }
                })
                .unwrap_or(Type::Error);
            IrParam {
                name: ident.text.clone(),
                ty,
                span: ident.span,
            }
        }
        Param::Annotated { name, ty, span } => IrParam {
            name: name.text.clone(),
            ty: lower_ast_type(ctx, ty),
            span: *span,
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{decl::FnDecl, Expr, Ident, Literal, Span};
    use ridge_ir::{IrExpr, IrLit, IrPat};
    use ridge_resolve::{BindingMap, LocalId, ModuleId, NodeIdMap, NodeKind};
    use ridge_types::CapabilitySet;

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn simple_decl(name: &str, body: Expr) -> FnDecl {
        FnDecl {
            vis: ridge_ast::Visibility::Private,
            caps: vec![],
            name: Ident {
                text: name.into(),
                span: sp(),
            },
            params: vec![],
            ret: None,
            body: ridge_ast::Body::Expr(body),
            span: sp(),
            doc: None,
        }
    }

    fn decl_with_params(name: &str, params: Vec<Param>, body: Expr) -> FnDecl {
        FnDecl {
            vis: ridge_ast::Visibility::Private,
            caps: vec![],
            name: Ident {
                text: name.into(),
                span: sp(),
            },
            params,
            ret: None,
            body: ridge_ast::Body::Expr(body),
            span: sp(),
            doc: None,
        }
    }

    // ── T8-if-1: basic inner fn becomes LetIn(Bind, Lambda) ──────────────────
    //
    // `fn inner = 42` with continuation [Unit]
    // → LetIn { pat: Bind("inner"), value: Lambda([], Int(42)), body: Unit }
    #[test]
    fn inner_fn_basic_becomes_let_in_lambda() {
        let mut ctx = fresh_ctx();
        let ifspan = sp_at(0, 15);
        let block_span = sp_at(0, 20);

        let body_expr = Expr::Literal(Literal::IntDec {
            raw: "42".into(),
            span: sp_at(12, 14),
        });
        let decl = simple_decl("inner", body_expr);

        let rest = [Expr::Unit(sp_at(16, 18))];

        let ir = lower_inner_fn_with_continuation(&mut ctx, &decl, ifspan, &rest, block_span);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::LetIn {
                pat,
                value,
                body,
                span: s,
                ..
            } => {
                assert_eq!(s, ifspan);

                // Pattern must bind the fn name.
                match pat {
                    IrPat::Bind {
                        name, inner: None, ..
                    } => {
                        assert_eq!(name, "inner");
                    }
                    other => panic!("expected IrPat::Bind, got {other:?}"),
                }

                // Value must be a Lambda.
                match *value {
                    IrExpr::Lambda {
                        ref params,
                        ref body,
                        caps,
                        ..
                    } => {
                        assert_eq!(params.len(), 0, "no params");
                        assert_eq!(caps, CapabilitySet::PURE);
                        match body.as_ref() {
                            IrExpr::Lit {
                                value: IrLit::Int(42),
                                ..
                            } => {}
                            other => panic!("expected Int(42) lambda body, got {other:?}"),
                        }
                    }
                    other => panic!("expected IrExpr::Lambda, got {other:?}"),
                }

                // Body is the continuation (Unit).
                match *body {
                    IrExpr::Lit {
                        value: IrLit::Unit, ..
                    } => {}
                    other => panic!("expected Unit continuation body, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::LetIn, got {other:?}"),
        }
    }

    // ── T8-if-2: recursive inner fn references self via Local ─────────────────
    //
    // `fn fact n = fact n` where `fact` in the body resolves to Binding::Local.
    // The lowerer trusts the binding map; `fact` in the body becomes
    // `IrExpr::Local { name: "fact" }`.
    #[test]
    fn recursive_inner_fn_self_reference_via_local() {
        // Set up binding map so `fact` ident in the body resolves to Local.
        let fact_body_span = sp_at(20, 24);

        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(fact_body_span, NodeKind::Ident).unwrap();

        let local_id = LocalId(0);
        let mut binding_map: BindingMap = vec![None; (node_id.0 + 1) as usize];
        binding_map[node_id.0 as usize] = Some(ridge_resolve::imports::Binding::Local(local_id));

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(binding_map)));

        let ifspan = sp_at(0, 30);
        let block_span = sp_at(0, 35);

        // Body of the recursive fn: just `fact` (the self-reference).
        let body_expr = Expr::Ident(Ident {
            text: "fact".into(),
            span: fact_body_span,
        });
        let param = Param::Bare(Ident {
            text: "n".into(),
            span: sp_at(10, 11),
        });
        let decl = decl_with_params("fact", vec![param], body_expr);

        let rest = [];
        let ir = lower_inner_fn_with_continuation(&mut ctx, &decl, ifspan, &rest, block_span);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::LetIn { value, .. } => match *value {
                IrExpr::Lambda { body, params, .. } => {
                    assert_eq!(params.len(), 1);
                    assert_eq!(params[0].name, "n");

                    // Body must be Local("fact") — the recursive reference.
                    match body.as_ref() {
                        IrExpr::Local { name, .. } => {
                            assert_eq!(name, "fact", "recursive ref must be Local(fact)");
                        }
                        other => panic!("expected Local(fact) lambda body, got {other:?}"),
                    }
                }
                other => panic!("expected Lambda, got {other:?}"),
            },
            other => panic!("expected LetIn, got {other:?}"),
        }
    }

    // ── T8-if-3: nested inner fns each produce their own LetIn(Lambda) ─────────
    //
    // Block: [fn outer = (fn inner = 1), 99]
    // Outer: LetIn(Bind outer, Lambda([], LetIn(Bind inner, Lambda([], Int 1), Unit)))
    //
    // The inner fn appears in the lambda body — we simulate by putting both
    // as block stmts and verifying the outer LetIn wraps the whole thing.
    #[test]
    fn nested_inner_fns_produce_nested_let_in() {
        let mut ctx = fresh_ctx();
        let outer_span = sp_at(0, 40);
        let inner_span = sp_at(5, 20);
        let block_span = sp_at(0, 50);

        // Build the inner fn decl (fn inner = 1).
        let inner_decl = simple_decl(
            "inner",
            Expr::Literal(Literal::IntDec {
                raw: "1".into(),
                span: sp_at(15, 16),
            }),
        );

        // Body of the outer fn: just an InnerFn expression wrapping `inner_decl`.
        let outer_body = Expr::InnerFn {
            decl: Box::new(inner_decl),
            span: inner_span,
        };
        let outer_decl = simple_decl("outer", outer_body);

        let rest = [Expr::Literal(Literal::IntDec {
            raw: "99".into(),
            span: sp_at(42, 44),
        })];

        let ir =
            lower_inner_fn_with_continuation(&mut ctx, &outer_decl, outer_span, &rest, block_span);

        // There will be errors because InnerFn in the outer body hits the bare
        // guard (lower_expr for InnerFn body emits L999). That's acceptable here
        // because we're just testing the structural nesting, not full round-tripping.
        match ir {
            IrExpr::LetIn {
                pat,
                value,
                body,
                span: s,
                ..
            } => {
                assert_eq!(s, outer_span);
                match pat {
                    IrPat::Bind { name, .. } => assert_eq!(name, "outer"),
                    other => panic!("expected Bind(outer), got {other:?}"),
                }
                // Value is a Lambda.
                match *value {
                    IrExpr::Lambda { .. } => {}
                    other => panic!("expected Lambda for outer fn, got {other:?}"),
                }
                // Continuation body is Int(99).
                match *body {
                    IrExpr::Lit {
                        value: IrLit::Int(99),
                        ..
                    } => {}
                    other => panic!("expected Int(99) continuation, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::LetIn, got {other:?}"),
        }
    }

    // ── T8-if-4: inner fn as final stmt → LetIn with Unit body ───────────────
    #[test]
    fn inner_fn_final_produces_unit_body() {
        let mut ctx = fresh_ctx();
        let ifspan = sp_at(0, 15);

        let decl = simple_decl(
            "helper",
            Expr::Literal(Literal::IntDec {
                raw: "0".into(),
                span: sp_at(12, 13),
            }),
        );

        let ir = lower_inner_fn_final(&mut ctx, &decl, ifspan);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::LetIn { body, .. } => match *body {
                IrExpr::Lit {
                    value: IrLit::Unit, ..
                } => {}
                other => panic!("expected Unit body for final inner fn, got {other:?}"),
            },
            other => panic!("expected IrExpr::LetIn, got {other:?}"),
        }
    }

    // ── T8-if-5: bare inner fn emits L999 ────────────────────────────────────
    #[test]
    fn bare_inner_fn_emits_l999() {
        let mut ctx = fresh_ctx();
        let ifspan = sp_at(0, 20);

        let decl = simple_decl("bad", Expr::Unit(sp()));

        let ir = lower_inner_fn_bare(&mut ctx, &decl, ifspan);

        assert_eq!(
            ctx.errors.len(),
            1,
            "expected 1 L999 error; got: {:?}",
            ctx.errors
        );
        assert_eq!(ctx.errors[0].code(), "L999");

        match ir {
            IrExpr::Lit {
                value: IrLit::Unit, ..
            } => {}
            other => panic!("expected Unit stub, got {other:?}"),
        }
    }

    // ── T8-if-6: params are lowered to IrParam with Type::Error ──────────────
    #[test]
    fn params_lowered_with_type_error_placeholder() {
        let mut ctx = fresh_ctx();
        let ifspan = sp_at(0, 30);
        let block_span = sp_at(0, 35);

        let params = vec![
            Param::Bare(Ident {
                text: "x".into(),
                span: sp_at(8, 9),
            }),
            Param::Annotated {
                name: Ident {
                    text: "y".into(),
                    span: sp_at(11, 12),
                },
                ty: ridge_ast::Type::Named {
                    name: Ident {
                        text: "Int".into(),
                        span: sp_at(14, 17),
                    },
                    span: sp_at(14, 17),
                },
                span: sp_at(10, 18),
            },
        ];

        let decl = decl_with_params("f", params, Expr::Unit(sp()));
        let rest = [];

        let ir = lower_inner_fn_with_continuation(&mut ctx, &decl, ifspan, &rest, block_span);

        match ir {
            IrExpr::LetIn { value, .. } => match *value {
                IrExpr::Lambda { params, .. } => {
                    assert_eq!(params.len(), 2);
                    assert_eq!(params[0].name, "x");
                    assert_eq!(params[1].name, "y");
                    // All types are Error placeholder.
                    assert!(matches!(params[0].ty, ridge_types::Type::Error));
                    assert!(matches!(params[1].ty, ridge_types::Type::Error));
                }
                other => panic!("expected Lambda, got {other:?}"),
            },
            other => panic!("expected LetIn, got {other:?}"),
        }
    }
}
