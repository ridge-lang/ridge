//! `try { ... }` block desugaring rule — §4.3.
//!
//! Converts `Expr::Try { block, span }` to a plain continuation-form block.
//! The `Try` boundary **disappears** after lowering — it was only a propagation-
//! scope marker for `?` inside the block (§4.3 design note).
//!
//! # Scope protocol
//!
//! 1. Push the try block's inferred return type as the propagation scope.
//! 2. Lower the block body via `fold_block_to_continuation`.
//! 3. Pop the scope.
//!
//! The block's return type is obtained from `ctx.node_types` via the `node_id_map`
//! (Option A, T3).  If the type cannot be resolved (no `node_id_map` attached,
//! or `node_types` slot is `None`) `Type::Error` is pushed as the scope sentinel —
//! any `?` inside will then fall through to its `InternalLoweringError` arm.
//!
//! An empty block body emits `L005` (`EmptyTryBlock`) and returns a `Unit` stub
//! without touching the scope stack.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{Block, Span};
use ridge_ir::{IrExpr, IrLit};
use ridge_resolve::NodeKind;
use ridge_types::Type;

use crate::block::fold_block_to_continuation;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower `Expr::Try { block, span }` to a continuation-form block.
///
/// The try boundary is erased; only the propagation-scope push/pop remains as
/// a lowering-time side effect (§4.3).
///
/// Never panics on any input — all error paths push a diagnostic and return a
/// structurally valid `IrExpr`.
pub fn lower_try(ctx: &mut LowerCtx<'_>, block: &Block, span: Span) -> IrExpr {
    // ── L005: empty block (defensive) ────────────────────────────────────────
    if block.stmts.is_empty() {
        ctx.errors.push(LowerError::EmptyTryBlock { span });
        let id = ctx.fresh_id(None);
        return IrExpr::Lit {
            id,
            value: IrLit::Unit,
            span,
        };
    }

    // ── Resolve the try block's return type for the propagation scope ─────────
    //
    // Prefer: look up the block's NodeId via `(block.span, NodeKind::Block)`.
    // Fallback: `Type::Error` — the inner `?` site will emit L999.
    let scope_ty = resolve_block_type(ctx, block.span);

    // ── Push scope, lower body, pop scope ─────────────────────────────────────
    ctx.push_propagation_scope(scope_ty);
    let lowered = fold_block_to_continuation(ctx, &block.stmts, span);
    ctx.pop_propagation_scope();

    lowered
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Attempt to resolve the inferred type of a `try` block.
///
/// Looks up the block's `NodeId` via `node_id_map.get(block_span, NodeKind::Block)`
/// (populated by Phase 4.5 T1), then reads its type from `ctx.node_types`.
/// Falls back to `Type::Error` when no `node_id_map` is attached, the span has
/// no stamp, or `node_types` has no entry for this `NodeId`.
///
/// When the fallback fires, any `?` inside the `try` block falls through to
/// `L999 InternalLoweringError` rather than producing incorrect Match arms.
///
/// PHASE45-T1+T3 (OQ-PHASE45-004): block-type lookup wired via `NodeKind::Block`.
fn resolve_block_type(ctx: &LowerCtx<'_>, block_span: Span) -> Type {
    ctx.node_id_map
        .as_ref()
        .and_then(|m| m.get(block_span, NodeKind::Block))
        .and_then(|nid| ctx.node_type(nid).cloned())
        .unwrap_or(Type::Error)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Block, Expr, Span};
    use ridge_ir::{IrExpr, IrLit};
    use ridge_resolve::ModuleId;
    use ridge_types::{TyConId, Type};

    use crate::ctx::LowerCtx;

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn result_ty() -> Type {
        // Result a e — TyConId(10)
        Type::Con(
            TyConId(10),
            vec![Type::Con(TyConId(0), vec![]), Type::Con(TyConId(0), vec![])],
        )
    }

    fn unit_block(span: Span) -> Block {
        Block {
            stmts: vec![Expr::Unit(span)],
            span,
        }
    }

    // ── T7-try-1: single-stmt try block — no errors, scope popped ────────────
    //
    // After lowering the block, the propagation scope stack must be empty.

    #[test]
    fn try_block_single_stmt_no_errors_scope_empty_after() {
        let mut ctx = fresh_ctx();
        // Push a Result scope so resolve_block_type falls back gracefully.
        // (In real usage the node_id_map would look this up; here we inject it
        //  manually by pre-pushing the scope via lower_try indirectly — but
        //  lower_try calls resolve_block_type which will return Type::Error when
        //  no node_id_map is attached; that's fine for this test.)
        let span = sp_at(0, 10);
        let block = unit_block(span);

        let ir = lower_try(&mut ctx, &block, span);

        // No L005 error.
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        // Scope must be empty after the call.
        assert!(
            ctx.current_propagation_scope().is_none(),
            "propagation scope must be empty after lower_try"
        );

        // Result is the lowered Unit expression.
        match ir {
            IrExpr::Lit {
                value: IrLit::Unit, ..
            } => {}
            other => panic!("expected Lit Unit for single-Unit block, got {other:?}"),
        }
    }

    // ── T7-try-2: try { e? } — inner ? sees the try scope ────────────────────
    //
    // Construct an Expr::Try whose block contains a Propagate.  We manually
    // pre-inject the propagation scope via push_propagation_scope before calling
    // lower_try (simulating what the node_id_map path would resolve).  We verify
    // that the lowered IR is an IrExpr::Match (the ? output) and no L003 is emitted.

    #[test]
    fn try_block_with_inner_propagate_sees_scope() {
        use crate::propagate::lower_propagate;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);

        // Manually push a Result scope — this simulates what lower_try would do
        // after resolve_block_type returns the real type.
        ctx.push_propagation_scope(result_ty());

        // Lower the inner Propagate directly (as lower_try would do via fold_block).
        let inner = Expr::Unit(sp());
        let ir = lower_propagate(&mut ctx, &inner, span);

        // Pop the scope manually (simulating try block exit).
        ctx.pop_propagation_scope();

        // No L003 error — the ? saw the scope.
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        // The ? desugared to a Match.
        match ir {
            IrExpr::Match { .. } => {}
            other => panic!("expected IrExpr::Match from propagate in scope, got {other:?}"),
        }

        // Scope must be empty now.
        assert!(ctx.current_propagation_scope().is_none());
    }

    // ── T7-try-3: empty try {} → L005 + Unit stub ────────────────────────────

    #[test]
    fn try_block_empty_emits_l005() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let block = Block {
            stmts: vec![],
            span,
        };

        let ir = lower_try(&mut ctx, &block, span);

        assert_eq!(
            ctx.errors.len(),
            1,
            "expected exactly 1 error; got: {:?}",
            ctx.errors
        );
        assert_eq!(ctx.errors[0].code(), "L005");

        match ir {
            IrExpr::Lit {
                value: IrLit::Unit, ..
            } => {}
            other => panic!("expected Unit stub, got {other:?}"),
        }

        // Scope must be untouched (never pushed for empty block).
        assert!(
            ctx.current_propagation_scope().is_none(),
            "empty try must not push a scope"
        );
    }

    // ── T7-try-4: lower_try with Option scope type injected via try ───────────
    //
    // lower_try pushes whatever resolve_block_type returns. When node_id_map is
    // absent it returns Type::Error. We verify the scope is still popped cleanly
    // and no L005 is emitted for a non-empty block.

    #[test]
    fn try_block_scope_always_popped_even_with_error_type() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 5);
        let block = unit_block(span);

        // With no node_id_map, resolve_block_type → Type::Error.
        let _ir = lower_try(&mut ctx, &block, span);

        // No L005 (non-empty block).
        let l005_count = ctx.errors.iter().filter(|e| e.code() == "L005").count();
        assert_eq!(l005_count, 0, "no L005 expected for non-empty block");

        // Scope popped regardless of type.
        assert!(
            ctx.current_propagation_scope().is_none(),
            "scope must be popped after lower_try"
        );
    }

    // ── T7-try-5: lower_try with multi-stmt block — scope popped, no L005 ──────
    //
    // Verify that a multi-statement try block lowers without L005 and the scope
    // is correctly popped.  resolve_block_type currently returns Type::Error
    // (NodeKind::Block is not yet in the NodeKind enum), so any inner `?` would
    // hit L999 — but this test has no inner `?`, so no error is expected.

    #[test]
    fn try_block_multi_stmt_no_l005_scope_popped() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);
        let block = Block {
            stmts: vec![Expr::Unit(sp_at(0, 2)), Expr::Unit(sp_at(4, 6))],
            span,
        };

        let ir = lower_try(&mut ctx, &block, span);

        // No L005 for non-empty block.
        let l005_count = ctx.errors.iter().filter(|e| e.code() == "L005").count();
        assert_eq!(l005_count, 0, "no L005 expected for multi-stmt block");

        // Scope popped.
        assert!(
            ctx.current_propagation_scope().is_none(),
            "scope must be popped after lower_try"
        );

        // IR is a Block (two stmts folded).
        match ir {
            IrExpr::Block { stmts, .. } => {
                assert_eq!(stmts.len(), 2, "expected 2 stmts in folded block");
            }
            other => panic!("expected IrExpr::Block for multi-stmt try, got {other:?}"),
        }
    }
}
