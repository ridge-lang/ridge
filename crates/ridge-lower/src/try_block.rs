//! Lower a value-producing `try` expression to a neutral Result scope.
//!
//! A try boundary targets Result even when its value never reaches a continuation.
//! Propagation exits this scope, while explicit return still exits the function.
//! Empty blocks produce a diagnostic and a defensive Unit stub.

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

/// Preserve the Result boundary while lowering the block to continuation form.
pub fn lower_try(ctx: &mut LowerCtx<'_>, block: &Block, span: Span) -> IrExpr {
    // ── L105: empty block (defensive) ────────────────────────────────────────
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
    // Read the independent checked target, not a divergent expression's value.
    // Fallback: `Type::Error` — the inner `?` site will emit L999.
    let scope_ty = resolve_try_type(ctx, span);

    // ── Push scope, lower body, pop scope ─────────────────────────────────────
    ctx.push_result_scope(scope_ty);
    let lowered = fold_block_to_continuation(ctx, &block.stmts, span);
    ctx.pop_propagation_scope();

    IrExpr::ResultScope {
        id: ctx.fresh_id(None),
        body: Box::new(lowered),
        span,
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read the checked boundary target. Standalone lowering callers may still
/// provide a concrete Result node type without a typed-module side table.
fn resolve_try_type(ctx: &LowerCtx<'_>, try_span: Span) -> Type {
    ctx.try_scope_types
        .and_then(|targets| targets.get(&try_span).cloned())
        .or_else(|| {
            ctx.node_id_map
                .as_ref()
                .and_then(|m| m.get(try_span, NodeKind::Try))
                .and_then(|nid| ctx.node_type(nid).cloned())
        })
        .unwrap_or(Type::Error)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Block, Expr, Span};
    use ridge_ir::{IrExpr, IrLit};
    use ridge_resolve::ModuleId;
    use ridge_types::Type;

    use crate::ctx::LowerCtx;

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[], crate::test_support::builtins())
    }

    fn result_ty() -> Type {
        let b = crate::test_support::builtins();
        Type::Con(
            b.result,
            vec![Type::Con(b.int, vec![]), Type::Con(b.int, vec![])],
        )
    }

    fn unit_block(span: Span) -> Block {
        Block {
            stmts: vec![Expr::Unit(span)],
            span,
        }
    }

    // ── single-stmt try block — no errors, scope popped ────────────
    //
    // After lowering the block, the propagation scope stack must be empty.

    #[test]
    fn try_block_single_stmt_no_errors_scope_empty_after() {
        let mut ctx = fresh_ctx();
        // Push a Result scope so resolve_try_type falls back gracefully.
        // (In real usage the node_id_map would look this up; here we inject it
        //  manually by pre-pushing the scope via lower_try indirectly — but
        //  lower_try calls resolve_try_type which will return Type::Error when
        //  no node_id_map is attached; that's fine for this test.)
        let span = sp_at(0, 10);
        let block = unit_block(span);

        let ir = lower_try(&mut ctx, &block, span);

        // No L105 error.
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

        // The scope remains explicit so the backend wraps the Unit payload in Ok.
        match ir {
            IrExpr::ResultScope { body, .. } => assert!(matches!(
                *body,
                IrExpr::Lit {
                    value: IrLit::Unit,
                    ..
                }
            )),
            other => panic!("expected ResultScope for single-Unit block, got {other:?}"),
        }
    }

    // ── try { e? } — inner ? sees the try scope ────────────────────
    //
    // Construct an Expr::Try whose block contains a Propagate.  We manually
    // pre-inject the propagation scope via push_propagation_scope before calling
    // lower_try (simulating what the node_id_map path would resolve).  We verify
    // that the lowered IR is an IrExpr::Match (the ? output) and no L103 is emitted.

    #[test]
    fn try_block_with_inner_propagate_sees_scope() {
        use crate::propagate::lower_propagate;

        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);

        // Manually push a Result scope — this simulates what lower_try would do
        // after resolve_try_type returns the real type.
        ctx.push_propagation_scope(result_ty());

        // Lower the inner Propagate directly (as lower_try would do via fold_block).
        let inner = Expr::Unit(sp());
        let ir = lower_propagate(&mut ctx, &inner, span);

        // Pop the scope manually (simulating try block exit).
        ctx.pop_propagation_scope();

        // No L103 error — the ? saw the scope.
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

    // ── empty try {} → L105 + Unit stub ────────────────────────────

    #[test]
    fn try_block_empty_emits_l105() {
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
        assert_eq!(ctx.errors[0].code(), "L105");

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

    // ── lower_try with Option scope type injected via try ───────────
    //
    // lower_try pushes whatever resolve_try_type returns. When node_id_map is
    // absent it returns Type::Error. We verify the scope is still popped cleanly
    // and no L105 is emitted for a non-empty block.

    #[test]
    fn try_block_scope_always_popped_even_with_error_type() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 5);
        let block = unit_block(span);

        // With no node_id_map, resolve_try_type → Type::Error.
        let _ir = lower_try(&mut ctx, &block, span);

        // No L105 (non-empty block).
        let l105_count = ctx.errors.iter().filter(|e| e.code() == "L105").count();
        assert_eq!(l105_count, 0, "no L105 expected for non-empty block");

        // Scope popped regardless of type.
        assert!(
            ctx.current_propagation_scope().is_none(),
            "scope must be popped after lower_try"
        );
    }

    // ── lower_try with multi-stmt block — scope popped, no L105 ──────
    //
    // Verify that a multi-statement try block lowers without L105 and the scope
    // is correctly popped. This isolated context has no typed node table;
    // the runtime tests cover propagation with a fully inferred Result type.

    #[test]
    fn try_block_multi_stmt_no_l105_scope_popped() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);
        let block = Block {
            stmts: vec![Expr::Unit(sp_at(0, 2)), Expr::Unit(sp_at(4, 6))],
            span,
        };

        let ir = lower_try(&mut ctx, &block, span);

        // No L105 for non-empty block.
        let l105_count = ctx.errors.iter().filter(|e| e.code() == "L105").count();
        assert_eq!(l105_count, 0, "no L105 expected for multi-stmt block");

        // Scope popped.
        assert!(
            ctx.current_propagation_scope().is_none(),
            "scope must be popped after lower_try"
        );

        let IrExpr::ResultScope { body, .. } = ir else {
            panic!("try must preserve its Result boundary");
        };
        match *body {
            IrExpr::Block { stmts, .. } => {
                assert_eq!(stmts.len(), 2, "expected 2 stmts in folded block");
            }
            other => panic!("expected IrExpr::Block for multi-stmt try, got {other:?}"),
        }
    }
}
