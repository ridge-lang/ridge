//! `if`-expression lowering rule — §4.7.
//!
//! Converts `Expr::If { cond, then_branch, else_branch }` into an
//! `IrExpr::Match` over a `Bool` scrutinee with two arms:
//!
//! - arm 0: `true  → lower(then_branch)`
//! - arm 1: `false → lower(else_branch)` — or `IrExpr::Lit { Unit }` when
//!   `else_branch` is `None` (the no-else form synthesises a unit arm so that
//!   the surrounding type tree remains correct).
//!
//! This is the only rule that produces a `Bool`-scrutinee `Match`; the codegen
//! or a later optimisation pass may specialise it, but lowering does not.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{Expr, Span};
use ridge_ir::{IrArm, IrExpr, IrLit, IrPat};

use crate::core::lower_expr;
use crate::ctx::LowerCtx;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower an `if`-expression to `IrExpr::Match`.
///
/// The scrutinee is the lowered `cond`; arms are always two: `true` and
/// `false`.  When `else_branch` is `None` the false arm body is synthesised
/// as `IrExpr::Lit { Unit }` with the same span as the whole `if`, preserving
/// type correctness (the expression evaluates to `()` when the condition is
/// false).
///
/// Never panics on any input — all error paths are delegated to
/// `lower_expr`.
pub fn lower_if(
    ctx: &mut LowerCtx<'_>,
    cond: &Expr,
    then_branch: &Expr,
    else_branch: Option<&Expr>,
    span: Span,
) -> IrExpr {
    let id = ctx.fresh_id(None);

    let scrutinee = Box::new(lower_expr(ctx, cond));

    let then_body = lower_expr(ctx, then_branch);

    let else_body = if let Some(e) = else_branch {
        lower_expr(ctx, e)
    } else {
        let unit_id = ctx.fresh_id(None);
        IrExpr::Lit {
            id: unit_id,
            value: IrLit::Unit,
            span,
        }
    };

    let true_arm = IrArm {
        pat: IrPat::Lit {
            value: IrLit::Bool(true),
            span,
        },
        when: None,
        body: then_body,
        span,
    };

    let false_arm = IrArm {
        pat: IrPat::Lit {
            value: IrLit::Bool(false),
            span,
        },
        when: None,
        body: else_body,
        span,
    };

    IrExpr::Match {
        id,
        scrutinee,
        arms: vec![true_arm, false_arm],
        span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Literal, Span};
    use ridge_ir::{IrExpr, IrLit, IrPat};
    use ridge_resolve::ModuleId;

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

    fn bool_expr(value: bool) -> Expr {
        Expr::Literal(Literal::Bool { value, span: sp() })
    }

    fn int_expr(n: &str) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.into(),
            span: sp(),
        })
    }

    fn unit_expr() -> Expr {
        Expr::Unit(sp())
    }

    // ── T5-if-1: if true then 1 else 2 ───────────────────────────────────────
    //
    // `if true then 1 else 2` lowers to a Match with two arms.
    // arm 0 pattern: IrPat::Lit { Bool(true) }, body: IrExpr::Lit { Int(1) }
    // arm 1 pattern: IrPat::Lit { Bool(false) }, body: IrExpr::Lit { Int(2) }

    #[test]
    fn lower_if_then_else() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);

        let cond = bool_expr(true);
        let then_b = int_expr("1");
        let else_b = int_expr("2");

        let ir = lower_if(&mut ctx, &cond, &then_b, Some(&else_b), span);

        match ir {
            IrExpr::Match {
                scrutinee, arms, ..
            } => {
                // Scrutinee is the lowered bool literal.
                match *scrutinee {
                    IrExpr::Lit {
                        value: IrLit::Bool(true),
                        ..
                    } => {}
                    ref other => panic!("expected Bool(true) scrutinee, got {other:?}"),
                }

                assert_eq!(arms.len(), 2, "expected exactly 2 arms");

                // arm 0: true → Int(1)
                match &arms[0].pat {
                    IrPat::Lit {
                        value: IrLit::Bool(b),
                        ..
                    } => assert!(*b, "arm 0 pattern must be Bool(true)"),
                    other => panic!("expected Lit(Bool(true)) for arm 0, got {other:?}"),
                }
                match &arms[0].body {
                    IrExpr::Lit {
                        value: IrLit::Int(n),
                        ..
                    } => assert_eq!(*n, 1),
                    other => panic!("expected Int(1) body for arm 0, got {other:?}"),
                }
                assert!(arms[0].when.is_none(), "arm 0 must have no guard");

                // arm 1: false → Int(2)
                match &arms[1].pat {
                    IrPat::Lit {
                        value: IrLit::Bool(b),
                        ..
                    } => assert!(!*b, "arm 1 pattern must be Bool(false)"),
                    other => panic!("expected Lit(Bool(false)) for arm 1, got {other:?}"),
                }
                match &arms[1].body {
                    IrExpr::Lit {
                        value: IrLit::Int(n),
                        ..
                    } => assert_eq!(*n, 2),
                    other => panic!("expected Int(2) body for arm 1, got {other:?}"),
                }
                assert!(arms[1].when.is_none(), "arm 1 must have no guard");
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T5-if-2: if true then () — no else branch ─────────────────────────────
    //
    // The synthesised false-arm body must be `IrExpr::Lit { Unit }`.

    #[test]
    fn lower_if_then_no_else() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 15);

        let cond = bool_expr(true);
        let then_b = unit_expr();

        let ir = lower_if(&mut ctx, &cond, &then_b, None, span);

        match ir {
            IrExpr::Match { arms, .. } => {
                assert_eq!(arms.len(), 2, "even no-else: 2 arms");

                // arm 1 body must be the synthesised Unit.
                match &arms[1].body {
                    IrExpr::Lit {
                        value: IrLit::Unit, ..
                    } => {}
                    other => panic!("expected synthesised Unit for no-else arm, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T5-if-3: nested if — if c1 then (if c2 then a else b) else c ─────────
    //
    // The inner `if` must appear as the then-arm body of the outer `Match`.

    #[test]
    fn lower_if_nested() {
        let mut ctx = fresh_ctx();
        let outer_span = sp_at(0, 40);
        let inner_span = sp_at(10, 30);

        let cond1 = bool_expr(true);
        let cond2 = bool_expr(false);
        let inner_then = int_expr("1");
        let inner_else = int_expr("2");
        let outer_else = int_expr("3");

        // Build the inner if as an AST node we call lower_if on directly,
        // but embed it as an Expr::If for the outer lower_if call.
        let inner_if = Expr::If {
            cond: Box::new(cond2),
            then_branch: Box::new(inner_then),
            else_branch: Some(Box::new(inner_else)),
            span: inner_span,
        };

        let ir = lower_if(&mut ctx, &cond1, &inner_if, Some(&outer_else), outer_span);

        match ir {
            IrExpr::Match {
                arms,
                span: match_span,
                ..
            } => {
                assert_eq!(match_span, outer_span, "outer span must match");
                assert_eq!(arms.len(), 2, "outer match: 2 arms");

                // The true-arm body of the outer match must itself be a Match
                // (the lowered inner if).
                match &arms[0].body {
                    IrExpr::Match {
                        arms: inner_arms, ..
                    } => {
                        assert_eq!(inner_arms.len(), 2, "inner match: 2 arms");
                    }
                    other => {
                        panic!("expected inner IrExpr::Match in outer true-arm, got {other:?}")
                    }
                }

                // The false-arm body is Int(3).
                match &arms[1].body {
                    IrExpr::Lit {
                        value: IrLit::Int(val),
                        ..
                    } => assert_eq!(*val, 3),
                    other => panic!("expected Int(3) for outer false-arm, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }
}
