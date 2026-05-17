//! Guard-expression lowering — §4.4.
//!
//! # Rule summary
//!
//! `Expr::Guard { cond, else_branch, span }` is a **block-level statement**.
//! It cannot be lowered in isolation (as a bare expression); it must be
//! encountered during the block continuation fold so that the statements that
//! follow it can be captured as the "true continuation".
//!
//! When `[Guard { cond, else_branch, span: gspan }, …rest]` is encountered
//! during `crate::block::fold_block_to_continuation`, the result is:
//!
//! ```text
//! IrExpr::Match {
//!     scrutinee: lower_expr(cond),               -- typed Bool
//!     arms: [
//!         IrArm { pat: Lit(Bool(true)),  body: fold(rest, …) },  -- continuation
//!         IrArm { pat: Lit(Bool(false)), body: lower_block(else_branch) },
//!     ],
//! }
//! ```
//!
//! ## Multiple guards — right-fold
//!
//! `[Guard c1 e1, Guard c2 e2, …rest]` right-folds naturally because the
//! recursive call to `fold_block_to_continuation` inside the "true" arm sees
//! `[Guard c2 e2, …rest]` as its input and applies this rule again.
//!
//! ## Guard as final statement
//!
//! When the guard is the last statement in its block (no continuation), the
//! "true" arm body is `IrExpr::Lit { Unit }` — the block evaluates to `()`.
//!
//! ## Bare `Guard` outside block context
//!
//! When `lower_expr` encounters `Expr::Guard` directly (not via the block fold)
//! it emits `L006 BareGuardExpr` and returns a `Unit` literal.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{Block, Expr, Span};
use ridge_ir::{IrArm, IrExpr, IrLit, IrPat};

use crate::block::{fold_block_to_continuation, lower_block};
use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower a `Guard` statement that appears in a block with a non-empty
/// continuation (`rest`).
///
/// Called from `fold_block_to_continuation` when it matches
/// `[Expr::Guard { cond, else_branch, span: gspan }, rest @ ..]`.
///
/// The "true" arm recursively folds the remaining statements;
/// the "false" arm lowers the else branch as a complete block.
pub fn lower_guard_with_continuation(
    ctx: &mut LowerCtx<'_>,
    cond: &Expr,
    else_branch: &Block,
    gspan: Span,
    rest: &[Expr],
    block_span: Span,
) -> IrExpr {
    let id = ctx.fresh_id(None);

    let scrutinee = Box::new(lower_expr(ctx, cond));

    // "true" arm: the continuation — whatever follows this guard in the block.
    let true_body = fold_block_to_continuation(ctx, rest, block_span);
    let true_arm = IrArm {
        pat: IrPat::Lit {
            value: IrLit::Bool(true),
            span: gspan,
        },
        when: None,
        body: true_body,
        span: gspan,
    };

    // "false" arm: the guard's else branch.
    let false_body = lower_block(ctx, else_branch);
    let false_arm = IrArm {
        pat: IrPat::Lit {
            value: IrLit::Bool(false),
            span: gspan,
        },
        when: None,
        body: false_body,
        span: gspan,
    };

    IrExpr::Match {
        id,
        scrutinee,
        arms: vec![true_arm, false_arm],
        span: gspan,
    }
}

/// Lower a `Guard` that is the **final** statement in its block (no rest).
///
/// The "true" arm body is a synthesised `Unit` literal (the block evaluates
/// to `()` when the guard passes and there is nothing left to evaluate).
pub fn lower_guard_final(
    ctx: &mut LowerCtx<'_>,
    cond: &Expr,
    else_branch: &Block,
    gspan: Span,
) -> IrExpr {
    // Delegate to the general form with an empty rest slice.
    lower_guard_with_continuation(ctx, cond, else_branch, gspan, &[], gspan)
}

/// Lower a bare `Guard` expression encountered outside any block context.
///
/// This is a Phase 4 invariant violation: `guard` is a block-level statement
/// and should never reach `lower_expr` directly.  Emits `L006 BareGuardExpr`
/// and returns `IrExpr::Lit { Unit }` so the surrounding expression tree
/// remains structurally valid.
pub fn lower_guard_bare(ctx: &mut LowerCtx<'_>, span: Span) -> IrExpr {
    let id = ctx.fresh_id(None);
    ctx.errors.push(LowerError::BareGuardExpr { span });
    IrExpr::Lit {
        id,
        value: IrLit::Unit,
        span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Block, Expr, Ident, Literal, Span};
    use ridge_ir::{IrExpr, IrLit, IrPat};
    use ridge_resolve::ModuleId;

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn bool_cond(value: bool) -> Expr {
        Expr::Literal(Literal::Bool { value, span: sp() })
    }

    fn unit_block() -> Block {
        Block {
            stmts: vec![Expr::Unit(sp())],
            span: sp(),
        }
    }

    fn int_block(n: &str) -> Block {
        Block {
            stmts: vec![Expr::Literal(Literal::IntDec {
                raw: n.into(),
                span: sp(),
            })],
            span: sp(),
        }
    }

    // ── T8-g-1: single guard with continuation ────────────────────────────────
    //
    // Block: [guard true else { 42 }, 99]
    // → Match { scrutinee: Bool(true), arms: [true → Int(99), false → Int(42)] }
    #[test]
    fn guard_with_continuation_produces_match() {
        let mut ctx = fresh_ctx();
        let gspan = sp_at(0, 10);
        let block_span = sp_at(0, 20);

        let cond = bool_cond(true);
        let else_branch = int_block("42");
        let rest = [Expr::Literal(Literal::IntDec {
            raw: "99".into(),
            span: sp_at(12, 14),
        })];

        let ir =
            lower_guard_with_continuation(&mut ctx, &cond, &else_branch, gspan, &rest, block_span);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Match {
                scrutinee,
                arms,
                span: s,
                ..
            } => {
                assert_eq!(s, gspan);
                assert_eq!(arms.len(), 2);

                // Scrutinee is the Bool(true) literal.
                match *scrutinee {
                    IrExpr::Lit {
                        value: IrLit::Bool(true),
                        ..
                    } => {}
                    other => panic!("expected Bool(true) scrutinee, got {other:?}"),
                }

                // arm 0: true → continuation (Int 99)
                assert!(matches!(
                    &arms[0].pat,
                    IrPat::Lit {
                        value: IrLit::Bool(true),
                        ..
                    }
                ));
                match &arms[0].body {
                    IrExpr::Lit {
                        value: IrLit::Int(99),
                        ..
                    } => {}
                    other => panic!("expected Int(99) true-arm body, got {other:?}"),
                }

                // arm 1: false → else block (Int 42)
                assert!(matches!(
                    &arms[1].pat,
                    IrPat::Lit {
                        value: IrLit::Bool(false),
                        ..
                    }
                ));
                match &arms[1].body {
                    IrExpr::Lit {
                        value: IrLit::Int(42),
                        ..
                    } => {}
                    other => panic!("expected Int(42) false-arm body, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T8-g-2: multiple guards right-fold ────────────────────────────────────
    //
    // Block: [guard c1 else { 1 }, guard c2 else { 2 }, 99]
    // → Match(c1, [true → Match(c2, [true → Int(99), false → Int(2)]), false → Int(1)])
    #[test]
    fn multiple_guards_right_fold() {
        let mut ctx = fresh_ctx();
        let gspan1 = sp_at(0, 10);
        let gspan2 = sp_at(11, 21);
        let block_span = sp_at(0, 30);

        let cond1 = bool_cond(true);
        let else1 = int_block("1");

        let cond2 = bool_cond(false);
        let else2 = int_block("2");

        // rest after c1: [guard c2 else { 2 }, Int(99)]
        let rest = vec![
            Expr::Guard {
                cond: Box::new(cond2),
                else_branch: else2,
                span: gspan2,
            },
            Expr::Literal(Literal::IntDec {
                raw: "99".into(),
                span: sp_at(22, 24),
            }),
        ];

        let ir = lower_guard_with_continuation(&mut ctx, &cond1, &else1, gspan1, &rest, block_span);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        // Outer match.
        match ir {
            IrExpr::Match {
                scrutinee: _,
                arms,
                span: s,
                ..
            } => {
                assert_eq!(s, gspan1);
                assert_eq!(arms.len(), 2);

                // true arm is itself a Match (the inner guard).
                match &arms[0].body {
                    IrExpr::Match {
                        arms: inner_arms,
                        span: inner_s,
                        ..
                    } => {
                        assert_eq!(*inner_s, gspan2);
                        assert_eq!(inner_arms.len(), 2);

                        // inner true → Int(99)
                        match &inner_arms[0].body {
                            IrExpr::Lit {
                                value: IrLit::Int(99),
                                ..
                            } => {}
                            other => panic!("expected Int(99) inner-true body, got {other:?}"),
                        }
                        // inner false → Int(2)
                        match &inner_arms[1].body {
                            IrExpr::Lit {
                                value: IrLit::Int(2),
                                ..
                            } => {}
                            other => panic!("expected Int(2) inner-false body, got {other:?}"),
                        }
                    }
                    other => panic!("expected inner Match in true arm, got {other:?}"),
                }

                // false arm → Int(1)
                match &arms[1].body {
                    IrExpr::Lit {
                        value: IrLit::Int(1),
                        ..
                    } => {}
                    other => panic!("expected Int(1) false-arm body, got {other:?}"),
                }
            }
            other => panic!("expected outer IrExpr::Match, got {other:?}"),
        }
    }

    // ── T8-g-3: guard with multi-stmt else block ──────────────────────────────
    //
    // Block: [guard true else { unit; 42 }, unit]
    // The else block has 2 stmts; it becomes an IrExpr::Block inside the false arm.
    #[test]
    fn guard_with_multi_stmt_else() {
        let mut ctx = fresh_ctx();
        let gspan = sp_at(0, 20);
        let block_span = sp_at(0, 30);

        let cond = bool_cond(true);
        let else_branch = Block {
            stmts: vec![
                Expr::Unit(sp_at(10, 12)),
                Expr::Literal(Literal::IntDec {
                    raw: "42".into(),
                    span: sp_at(14, 16),
                }),
            ],
            span: sp_at(8, 18),
        };
        let rest = [Expr::Unit(sp_at(22, 24))];

        let ir =
            lower_guard_with_continuation(&mut ctx, &cond, &else_branch, gspan, &rest, block_span);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Match { arms, .. } => {
                // false arm body: multi-stmt block lowers to IrExpr::Block.
                match &arms[1].body {
                    IrExpr::Block { stmts, .. } => {
                        assert_eq!(
                            stmts.len(),
                            2,
                            "multi-stmt else should produce Block with 2 stmts"
                        );
                    }
                    other => panic!("expected IrExpr::Block for multi-stmt else, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T8-g-4: guard as final statement (no continuation) → true arm = Unit ──
    #[test]
    fn guard_as_final_stmt_produces_unit_true_arm() {
        let mut ctx = fresh_ctx();
        let gspan = sp_at(0, 10);

        let cond = bool_cond(true);
        let else_branch = unit_block();

        let ir = lower_guard_final(&mut ctx, &cond, &else_branch, gspan);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Match { arms, .. } => {
                assert_eq!(arms.len(), 2);

                // true arm body must be Unit (empty continuation).
                match &arms[0].body {
                    IrExpr::Lit {
                        value: IrLit::Unit, ..
                    } => {}
                    other => panic!("expected Unit true-arm body, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T8-g-5: bare guard outside block context emits L006 ──────────────────
    #[test]
    fn bare_guard_emits_l006() {
        let mut ctx = fresh_ctx();
        let span = sp_at(5, 15);

        let ir = lower_guard_bare(&mut ctx, span);

        // Must emit exactly one L006 error.
        assert_eq!(
            ctx.errors.len(),
            1,
            "expected 1 L006 error; got: {:?}",
            ctx.errors
        );
        assert_eq!(ctx.errors[0].code(), "L006");
        assert_eq!(ctx.errors[0].span(), span);

        // Must return Unit stub.
        match ir {
            IrExpr::Lit {
                value: IrLit::Unit,
                span: s,
                ..
            } => {
                assert_eq!(s, span);
            }
            other => panic!("expected Unit stub, got {other:?}"),
        }
    }

    // ── T8-g-6: guard span is preserved on IrExpr::Match ─────────────────────
    #[test]
    fn guard_span_preserved_on_match() {
        let mut ctx = fresh_ctx();
        let gspan = sp_at(100, 200);
        let cond = bool_cond(false);
        let else_branch = unit_block();

        let ir = lower_guard_final(&mut ctx, &cond, &else_branch, gspan);

        match ir {
            IrExpr::Match { span: s, .. } => {
                assert_eq!(s, gspan, "Match span must equal guard span");
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T8-g-7: guard cond is an ident reference (lower_expr integration) ─────
    //
    // Uses Expr::Ident as the condition to verify that lower_expr is invoked on
    // the condition (it will emit L999 because no binding map is attached, but
    // the structure is still a Match).
    #[test]
    fn guard_ident_cond_produces_match_with_ident_scrutinee() {
        let mut ctx = fresh_ctx();
        let gspan = sp_at(0, 10);

        let cond = Expr::Ident(Ident {
            text: "ok".into(),
            span: sp_at(6, 8),
        });
        let else_branch = unit_block();

        let ir = lower_guard_final(&mut ctx, &cond, &else_branch, gspan);

        // There will be an L999 from the missing binding map — that's acceptable.
        match ir {
            IrExpr::Match { .. } => {}
            other => panic!("expected IrExpr::Match regardless of binding errors, got {other:?}"),
        }
    }
}
