//! Block-sequencing and let/var continuation lowering rules — §4.9.
//!
//! # `lower_block`
//!
//! Lowers `Expr::Block(Block { stmts, span })` via a right-fold
//! (`fold_block_to_continuation`) that converts consecutive `let`/`var`
//! binding stmts into nested `IrExpr::LetIn` / `IrExpr::VarIn` nodes
//! (continuation form).
//!
//! # `lower_assign`
//!
//! Lowers `Expr::Assign { target, value, span }` to `IrExpr::Assign`.
//! The target must be an `Expr::Ident`; any other target shape is defensive
//! (emits `L999`, returns a `Unit` stub).
//! Target classification:
//! - When `ctx.in_actor_body == true` and the ident appears in
//!   `ctx.current_state_fields`, the target is `AssignTarget::StateField`.
//! - Otherwise, the target is `AssignTarget::Local`.
//!
//! # Fold algorithm (§4.9)
//!
//! ```text
//! fold_block_to_continuation([], span)       → Lit Unit           (defensive)
//! fold_block_to_continuation([last], span)   → lower_expr(last)
//! fold_block_to_continuation([Let{..}, rest], span)
//!     → LetIn { id, pat, value, body: fold(rest, span), span: let_span }
//! fold_block_to_continuation([Var{..}, rest], span)
//!     → VarIn { id, name, ty: Error, value, body: fold(rest, span), span: var_span }
//! fold_block_to_continuation([Guard{..}, rest], span)
//!     → guard::lower_guard_with_continuation(cond, else_branch, gspan, rest, span)
//! fold_block_to_continuation([InnerFn{..}, rest], span)
//!     → inner_fn::lower_inner_fn_with_continuation(decl, ifspan, rest, span)
//! fold_block_to_continuation([other, rest], span)
//!     → Block { id, stmts: [lower_expr(other), fold(rest, span)], span }
//! ```

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{Block, Expr, Span};
use ridge_ir::{AssignTarget, IrExpr, IrLit};
use ridge_resolve::{imports::Binding, NodeKind};
use ridge_types::Type;

use crate::core::{lower_expr, lower_pattern};
use crate::ctx::LowerCtx;
use crate::error::LowerError;
use crate::guard::{lower_guard_final, lower_guard_with_continuation};
use crate::inner_fn::{lower_inner_fn_final, lower_inner_fn_with_continuation};

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower an AST [`Block`] to its [`IrExpr`] equivalent using continuation form.
///
/// Delegates to `fold_block_to_continuation`.  If the block is empty
/// (defensive — Phase 4 should have rejected empty blocks with `P014`) a
/// `IrExpr::Lit { Unit }` is returned and no error is emitted (the parse
/// error already covers this).
pub fn lower_block(ctx: &mut LowerCtx<'_>, block: &Block) -> IrExpr {
    fold_block_to_continuation(ctx, &block.stmts, block.span)
}

/// Lower `Expr::Assign { target, value, span }` to `IrExpr::Assign`.
///
/// The `target` must resolve to an `Expr::Ident`.  Any other target shape
/// is defensive: a `L999` error is pushed and a `Unit` literal is returned.
///
/// Target classification (R8 / §4.14):
/// - `AssignTarget::StateField` — when `ctx.in_actor_body == true` and the
///   ident appears in `ctx.current_state_fields`.  `ridge-resolve` has no
///   `Binding::StateField`; the set is populated by `actor_lower`.
/// - `AssignTarget::Local` — all other mutable locals.
pub fn lower_assign(ctx: &mut LowerCtx<'_>, target: &Expr, value: &Expr, span: Span) -> IrExpr {
    // The target must be a bare ident; anything else is rejected defensively.
    let ident = match target {
        Expr::Ident(id) => id,
        other => {
            let id = ctx.fresh_id(None);
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: format!(
                    "assignment target is not a bare ident (got {:?}); \
                     only `var`-bound locals are valid `<-` targets",
                    other.span()
                ),
            });
            return IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            };
        }
    };

    let target_span = ident.span;

    // Resolve the target ident through the binding map (same pattern as
    // `core::lower_ident`).
    let node_id = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(target_span, NodeKind::Ident));

    let binding = node_id.and_then(|nid| {
        ctx.binding_map
            .and_then(|bm| bm.get(nid.0 as usize).and_then(Option::as_ref))
    });

    // Classify the target.
    //
    // When inside an actor handler/init body (`ctx.in_actor_body == true`) and
    // the ident name appears in `ctx.current_state_fields`, this is a state-field
    // assignment (`<-` targeting a declared `state` field).  `ridge-resolve` has
    // no `Binding::StateField` variant; the classification is done here using the
    // actor-body context populated by `actor_lower` (R8 / §4.14).
    //
    // For all other cases, fall back to `AssignTarget::Local`.
    let is_state_field = ctx.in_actor_body
        && ctx
            .current_state_fields
            .as_ref()
            .is_some_and(|s| s.contains(ident.text.as_str()));

    if is_state_field {
        // State-field assignment: `field <- expr` inside an actor body.
        let assign_target = AssignTarget::StateField {
            name: ident.text.clone(),
            span: target_span,
        };
        let value_ir = lower_expr(ctx, value);
        let id = ctx.fresh_id(None);
        return IrExpr::Assign {
            id,
            target: assign_target,
            value: Box::new(value_ir),
            span,
        };
    }

    let assign_target = match binding {
        Some(Binding::Local(_)) => AssignTarget::Local {
            name: ident.text.clone(),
            span: target_span,
        },
        None => {
            // No binding map or NodeId missing — defensive, treat as Local and
            // emit an error so the issue is traceable.
            ctx.errors.push(LowerError::InternalLoweringError {
                span: target_span,
                message: format!(
                    "no binding found for assignment target `{}` at {target_span:?}; \
                     binding map absent or NodeId missing",
                    ident.text
                ),
            });
            AssignTarget::Local {
                name: ident.text.clone(),
                span: target_span,
            }
        }
        Some(_other) => {
            // Any non-Local binding in an assignment position is unexpected —
            // Phase 4 should have rejected this.  Emit defensive error.
            ctx.errors.push(LowerError::InternalLoweringError {
                span: target_span,
                message: format!(
                    "assignment target `{}` resolves to a non-mutable binding; \
                     Phase 4 should have rejected this",
                    ident.text
                ),
            });
            AssignTarget::Local {
                name: ident.text.clone(),
                span: target_span,
            }
        }
    };

    let value_ir = lower_expr(ctx, value);
    let id = ctx.fresh_id(None);
    IrExpr::Assign {
        id,
        target: assign_target,
        value: Box::new(value_ir),
        span,
    }
}

// ── Core fold ─────────────────────────────────────────────────────────────────

// OQ-L008: the block fold assumes stmts are already flat (pre-flattened by the
// parser); nested Block exprs inside a stmt list are not recursively flattened here.
/// Right-fold a statement list into continuation form.
///
/// See the module-level documentation for the full algorithm (§4.9).
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive match over all statement shapes with defensive fallbacks (§4.9)"
)]
pub(crate) fn fold_block_to_continuation(
    ctx: &mut LowerCtx<'_>,
    stmts: &[Expr],
    span: Span,
) -> IrExpr {
    match stmts {
        // ── Empty block (defensive) ───────────────────────────────────────────
        // Phase 4 enforces non-empty blocks (P014); if we see one anyway,
        // return a Unit literal rather than panicking.
        [] => {
            let id = ctx.fresh_id(None);
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        // ── Single statement — the block's value ──────────────────────────────
        // Edge case: if the last stmt is a `let` or `var`, Phase 4 types the
        // block as `Unit`, so we emit the binding with a `Unit` body.
        [Expr::Let {
            pat,
            value,
            span: lspan,
            ..
        }] => {
            let id = ctx.fresh_id(None);
            let pat_ir = lower_pattern(ctx, pat);
            let value_ir = lower_expr(ctx, value);
            let unit_id = ctx.fresh_id(None);
            let body = Box::new(IrExpr::Lit {
                id: unit_id,
                value: IrLit::Unit,
                span: *lspan,
            });
            IrExpr::LetIn {
                id,
                pat: pat_ir,
                value: Box::new(value_ir),
                body,
                span: *lspan,
            }
        }

        [Expr::Var {
            name,
            value,
            span: vspan,
            ..
        }] => {
            let id = ctx.fresh_id(None);
            let value_ir = lower_expr(ctx, value);
            let unit_id = ctx.fresh_id(None);
            let body = Box::new(IrExpr::Lit {
                id: unit_id,
                value: IrLit::Unit,
                span: *vspan,
            });
            IrExpr::VarIn {
                id,
                name: name.text.clone(),
                ty: Type::Error,
                value: Box::new(value_ir),
                body,
                span: *vspan,
            }
        }

        // ── Single Guard stmt — true arm = Unit (empty continuation) ─────────
        [Expr::Guard {
            cond,
            else_branch,
            span: gspan,
        }] => lower_guard_final(ctx, cond, else_branch, *gspan),

        // ── Single InnerFn stmt — LetIn with Unit body ────────────────────────
        [Expr::InnerFn { decl, span: ifspan }] => lower_inner_fn_final(ctx, decl, *ifspan),

        // ── Single non-binding stmt — just lower it ───────────────────────────
        [last] => lower_expr(ctx, last),

        // ── Let binding with continuation ─────────────────────────────────────
        [Expr::Let {
            pat,
            value,
            span: lspan,
            ..
        }, rest @ ..] => {
            let id = ctx.fresh_id(None);
            let pat_ir = lower_pattern(ctx, pat);
            let value_ir = lower_expr(ctx, value);
            let body = fold_block_to_continuation(ctx, rest, span);
            IrExpr::LetIn {
                id,
                pat: pat_ir,
                value: Box::new(value_ir),
                body: Box::new(body),
                span: *lspan,
            }
        }

        // ── Var binding with continuation ─────────────────────────────────────
        [Expr::Var {
            name,
            value,
            span: vspan,
            ..
        }, rest @ ..] => {
            let id = ctx.fresh_id(None);
            let value_ir = lower_expr(ctx, value);
            let body = fold_block_to_continuation(ctx, rest, span);
            IrExpr::VarIn {
                id,
                name: name.text.clone(),
                // The resolved type lives in the typecheck side-table (node_types),
                // not on the AST node.  `Type::Error` is the correct sentinel for
                // Phase 5 — the codegen will look up the real type from the
                // `LoweredModule.node_types` table indexed by `IrNodeId`.
                ty: Type::Error,
                value: Box::new(value_ir),
                body: Box::new(body),
                span: *vspan,
            }
        }

        // ── Guard stmt with continuation (§4.4) ──────────────────────────────
        [Expr::Guard {
            cond,
            else_branch,
            span: gspan,
        }, rest @ ..] => lower_guard_with_continuation(ctx, cond, else_branch, *gspan, rest, span),

        // ── InnerFn stmt with continuation (§4.12) ───────────────────────────
        [Expr::InnerFn { decl, span: ifspan }, rest @ ..] => {
            lower_inner_fn_with_continuation(ctx, decl, *ifspan, rest, span)
        }

        // ── Generic expression stmt with continuation ─────────────────────────
        //
        // This arm covers all other non-binding stmts in a non-final position:
        // `[other_stmt, rest @ ..]`.  Rust requires all arms in an OR-pattern
        // to bind the same variables, so we use a catch-all here and index
        // `stmts` directly.
        _ => {
            // Invariant: reached only when `stmts.len() >= 2` and the first
            // stmt is neither `Let`, `Var`, `Guard`, nor `InnerFn`
            // (those are handled above).
            // The `[]` and `[last]` arms guarantee at least 2 elements here.
            let other_stmt = &stmts[0];
            let rest = &stmts[1..];

            let id = ctx.fresh_id(None);
            let first_ir = lower_expr(ctx, other_stmt);
            let rest_ir = fold_block_to_continuation(ctx, rest, span);
            IrExpr::Block {
                id,
                stmts: vec![first_ir, rest_ir],
                span,
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Block, Ident, Literal, Pattern, Span};
    use ridge_ir::{IrExpr, IrLit, IrPat};
    use ridge_resolve::{BindingMap, LocalId, ModuleId, NodeIdMap, NodeKind};

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    // ── Simple let-then-expr ─────────────────────────────────────────────────────
    //
    // Block { stmts: [Let { pat: x, value: 1 }, Ident(x)] }
    // → LetIn { pat: Bind(x), value: Lit(Int 1), body: Local(x) }
    //
    // Assert structure and that the LetIn's span equals the `Let` stmt's span.
    #[test]
    fn block_let_then_expr_produces_let_in() {
        let let_span = sp_at(0, 10);
        let ident_span = sp_at(12, 13);

        // Set up a binding map so the trailing `Ident(x)` resolves to a Local.
        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(ident_span, NodeKind::Ident).unwrap();

        let local_id = LocalId(0);
        let mut binding_map: BindingMap = vec![None; (node_id.0 + 1) as usize];
        binding_map[node_id.0 as usize] = Some(Binding::Local(local_id));

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(binding_map)));

        let stmts = vec![
            Expr::Let {
                pat: Pattern::Var {
                    name: Ident {
                        text: "x".into(),
                        span: let_span,
                    },
                    span: let_span,
                },
                ty: None,
                value: Box::new(Expr::Literal(Literal::IntDec {
                    raw: "1".into(),
                    span: sp_at(6, 7),
                })),
                span: let_span,
            },
            Expr::Ident(Ident {
                text: "x".into(),
                span: ident_span,
            }),
        ];

        let block = Block {
            stmts,
            span: sp_at(0, 13),
        };

        let ir = lower_block(&mut ctx, &block);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::LetIn {
                pat: IrPat::Bind {
                    name, inner: None, ..
                },
                value,
                body,
                span: s,
                ..
            } => {
                assert_eq!(name, "x");
                assert_eq!(s, let_span, "LetIn span must equal the Let stmt span");
                match *value {
                    IrExpr::Lit {
                        value: IrLit::Int(1),
                        ..
                    } => {}
                    other => panic!("expected Lit Int 1, got {other:?}"),
                }
                match *body {
                    IrExpr::Local { name, .. } => assert_eq!(name, "x"),
                    other => panic!("expected Local(x), got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::LetIn, got {other:?}"),
        }
    }

    // ── Let-only block (final stmt is Let) → LetIn with Unit body ───────────────
    #[test]
    fn block_let_only_produces_let_in_with_unit_body() {
        let let_span = sp_at(0, 10);
        let mut ctx = fresh_ctx();

        let stmts = vec![Expr::Let {
            pat: Pattern::Var {
                name: Ident {
                    text: "x".into(),
                    span: let_span,
                },
                span: let_span,
            },
            ty: None,
            value: Box::new(Expr::Literal(Literal::IntDec {
                raw: "42".into(),
                span: sp_at(6, 8),
            })),
            span: let_span,
        }];

        let block = Block {
            stmts,
            span: let_span,
        };

        let ir = lower_block(&mut ctx, &block);

        match ir {
            IrExpr::LetIn { body, span: s, .. } => {
                assert_eq!(s, let_span);
                match *body {
                    IrExpr::Lit {
                        value: IrLit::Unit, ..
                    } => {}
                    other => panic!("expected Unit body, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::LetIn, got {other:?}"),
        }
    }

    // ── Var-then-expr → VarIn ────────────────────────────────────────────────────
    #[test]
    fn block_var_then_expr_produces_var_in() {
        let var_span = sp_at(0, 10);
        let unit_span = sp_at(12, 14);
        let mut ctx = fresh_ctx();

        let stmts = vec![
            Expr::Var {
                name: Ident {
                    text: "count".into(),
                    span: var_span,
                },
                ty: None,
                value: Box::new(Expr::Literal(Literal::IntDec {
                    raw: "0".into(),
                    span: sp_at(6, 7),
                })),
                span: var_span,
            },
            Expr::Unit(unit_span),
        ];

        let block = Block {
            stmts,
            span: sp_at(0, 14),
        };

        let ir = lower_block(&mut ctx, &block);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::VarIn {
                name,
                value,
                body,
                span: s,
                ..
            } => {
                assert_eq!(name, "count");
                assert_eq!(s, var_span);
                match *value {
                    IrExpr::Lit {
                        value: IrLit::Int(0),
                        ..
                    } => {}
                    other => panic!("expected Lit Int 0, got {other:?}"),
                }
                match *body {
                    IrExpr::Lit {
                        value: IrLit::Unit, ..
                    } => {}
                    other => panic!("expected Unit body, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::VarIn, got {other:?}"),
        }
    }

    // ── Multi-stmt non-let → right-folded Block ──────────────────────────────────
    //
    // Block { stmts: [Unit, Unit] }
    // → IrExpr::Block { stmts: [Lit Unit, Lit Unit], .. }
    // Assert stmts.len() == 2.
    #[test]
    fn block_multi_non_let_stmts_produces_ir_block() {
        let mut ctx = fresh_ctx();
        let block_span = sp_at(0, 20);

        let stmts = vec![Expr::Unit(sp_at(0, 2)), Expr::Unit(sp_at(4, 6))];

        let block = Block {
            stmts,
            span: block_span,
        };

        let ir = lower_block(&mut ctx, &block);

        match ir {
            IrExpr::Block { stmts, .. } => {
                assert_eq!(stmts.len(), 2, "expected 2 stmts in Block");
            }
            other => panic!("expected IrExpr::Block, got {other:?}"),
        }
    }

    // ── Mixed: [Let, expr_stmt, Ident] → LetIn { body: Block { ... } } ──────────
    #[test]
    fn block_mixed_let_then_two_stmts() {
        let let_span = sp_at(0, 10);
        let mut ctx = fresh_ctx();

        let stmts = vec![
            Expr::Let {
                pat: Pattern::Var {
                    name: Ident {
                        text: "x".into(),
                        span: let_span,
                    },
                    span: let_span,
                },
                ty: None,
                value: Box::new(Expr::Unit(sp_at(6, 8))),
                span: let_span,
            },
            Expr::Unit(sp_at(11, 13)),
            Expr::Unit(sp_at(15, 17)),
        ];

        let block = Block {
            stmts,
            span: sp_at(0, 17),
        };

        let ir = lower_block(&mut ctx, &block);

        match ir {
            IrExpr::LetIn { body, .. } => match *body {
                IrExpr::Block { stmts, .. } => {
                    assert_eq!(stmts.len(), 2, "continuation should be Block with 2 stmts");
                }
                other => panic!("expected IrExpr::Block body, got {other:?}"),
            },
            other => panic!("expected IrExpr::LetIn, got {other:?}"),
        }
    }

    // ── Empty block (defensive) → IrShapeMalformed error, no panic ──────────────
    //
    // Phase 4 rejects empty blocks (P014); we must not panic if one arrives.
    #[test]
    fn block_empty_defensive_returns_unit() {
        let mut ctx = fresh_ctx();
        let block = Block {
            stmts: vec![],
            span: sp(),
        };

        let ir = lower_block(&mut ctx, &block);

        match ir {
            IrExpr::Lit {
                value: IrLit::Unit, ..
            } => {}
            other => panic!("expected Lit Unit for empty block, got {other:?}"),
        }
        // No panic, no assertion about errors — the parse-level error already covers this.
    }

    // ── Assign on Local → promoted to Let binding in block sequence ──────────────
    //
    // Build a Local binding for ident `x`, then
    // Expr::Assign { target: Ident(x), value: Lit(2) }
    // → IrExpr::Assign { target: AssignTarget::Local { name: "x" }, value: Lit Int 2 }
    #[test]
    fn lower_assign_local_binding() {
        let target_span = sp_at(0, 1);
        let value_span = sp_at(5, 6);

        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(target_span, NodeKind::Ident).unwrap();

        let local_id = LocalId(0);
        let mut binding_map: BindingMap = vec![None; (node_id.0 + 1) as usize];
        binding_map[node_id.0 as usize] = Some(Binding::Local(local_id));

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(binding_map)));

        let target = Expr::Ident(Ident {
            text: "x".into(),
            span: target_span,
        });
        let value = Expr::Literal(Literal::IntDec {
            raw: "2".into(),
            span: value_span,
        });

        let ir = lower_assign(&mut ctx, &target, &value, sp_at(0, 6));

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Assign { target, value, .. } => {
                match target {
                    AssignTarget::Local { name, .. } => assert_eq!(name, "x"),
                    AssignTarget::StateField { name, .. } => {
                        panic!("expected AssignTarget::Local, got StateField({name:?})")
                    }
                }
                match *value {
                    IrExpr::Lit {
                        value: IrLit::Int(2),
                        ..
                    } => {}
                    other => panic!("expected Lit Int 2, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Assign, got {other:?}"),
        }
    }

    // ── Bare Expr::Let outside Block → defensive error in dispatcher ─────────────
    //
    // A top-level `Expr::Let { .. }` passed to `lower_expr` should emit a
    // `L999` error and return a `Unit` stub (see core.rs defensive arm).
    #[test]
    fn bare_let_outside_block_emits_error() {
        use crate::core::lower_expr;

        let mut ctx = fresh_ctx();
        let expr = Expr::Let {
            pat: Pattern::Var {
                name: Ident {
                    text: "x".into(),
                    span: sp(),
                },
                span: sp(),
            },
            ty: None,
            value: Box::new(Expr::Unit(sp())),
            span: sp(),
        };

        let ir = lower_expr(&mut ctx, &expr);

        // Must emit exactly one L999 error.
        assert_eq!(
            ctx.errors.len(),
            1,
            "expected 1 L999 error for bare let; got: {:?}",
            ctx.errors
        );
        assert_eq!(ctx.errors[0].code(), "L999");

        // Must return Unit stub.
        match ir {
            IrExpr::Lit {
                value: IrLit::Unit, ..
            } => {}
            other => panic!("expected Unit stub, got {other:?}"),
        }
    }
}
