//! Pipe-forward desugaring rule (`|>`) — §4.1.
//!
//! Implements the single rule: `lhs |> rhs` → flat `IrExpr::Call` where the
//! piped `lhs` is appended as the last argument.
//!
//! # Rule summary (§4.1)
//!
//! | RHS shape | IR result |
//! |---|---|
//! | `Call { callee, args }` | `Call(lower(callee), lower(args) ++ [lhs'])` |
//! | `Ident _ \| Qualified _ \| FieldAccess _ \| Lambda _ \| FieldAccessorFn _` | `Call(lower(rhs), [lhs'])` |
//! | `Paren { inner }` | peel paren, re-dispatch on `inner` |
//! | `Pipe { .. }` | L001 — parser is left-assoc, this should never fire |
//! | any other | L002 — defensive; Phase 4 should have rejected |

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{Expr, Span};
use ridge_ir::{IrExpr, IrLit};

use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower `lhs |> rhs` to a flat `IrExpr::Call`.
///
/// `rhs` controls the output shape:
/// - If `rhs` is `Call { callee, args }`, the piped `lhs` is appended to the
///   end of the argument list: `Call(callee, args ++ [lhs'])`.
/// - If `rhs` is a bare callable (`Ident`, `Qualified`, `FieldAccess`,
///   `Lambda`, `FieldAccessorFn`), emit `Call(rhs', [lhs'])`.
/// - `Paren { inner }` is peeled first (paren erasure — §1.3, §4.1).
/// - `Pipe` as RHS (should never occur — parser is left-assoc): emits `L001`
///   and returns a `Unit` stub preserving `span`.
/// - Any other shape: emits `L002` and returns a `Unit` stub.
///
/// Never panics on any input — all error paths push a [`LowerError`] and
/// return a structurally valid stub.
pub fn lower_pipe(ctx: &mut LowerCtx<'_>, lhs: &Expr, rhs: &Expr, span: Span) -> IrExpr {
    // Lower the LHS first (strict LTR order per IR invariant §4, point 4).
    let lhs_ir = lower_expr(ctx, lhs);

    // Peel any number of parentheses from the RHS before dispatching.
    let rhs_inner = peel_paren(rhs);

    match rhs_inner {
        // ── Call RHS: `xs |> f a b` → `Call(f, [a, b, xs])` ─────────────────
        Expr::Call { callee, args, .. } => {
            let id = ctx.fresh_id(None);
            let callee_ir = Box::new(lower_expr(ctx, callee));
            let mut args_ir: Vec<IrExpr> = args.iter().map(|a| lower_expr(ctx, a)).collect();
            args_ir.push(lhs_ir);
            IrExpr::Call {
                id,
                callee: callee_ir,
                args: args_ir,
                span,
            }
        }

        // ── Bare callable RHS: `xs |> f` → `Call(f, [xs])` ──────────────────
        Expr::Ident(_)
        | Expr::Qualified(_)
        | Expr::FieldAccess { .. }
        | Expr::Lambda { .. }
        | Expr::FieldAccessorFn { .. } => {
            let id = ctx.fresh_id(None);
            let callee_ir = Box::new(lower_expr(ctx, rhs_inner));
            IrExpr::Call {
                id,
                callee: callee_ir,
                args: vec![lhs_ir],
                span,
            }
        }

        // ── Pipe as RHS: defensive — parser is left-assoc, should never fire ─
        Expr::Pipe { span: rhs_span, .. } => {
            ctx.errors
                .push(LowerError::MalformedPipeRhs { span: *rhs_span });
            let id = ctx.fresh_id(None);
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }

        // ── Unknown RHS shape: defensive — Phase 4 should have rejected ──────
        other => {
            let rhs_span = expr_span(other);
            ctx.errors
                .push(LowerError::UnknownPipeRhsShape { span: rhs_span });
            let id = ctx.fresh_id(None);
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Peel one or more `Paren { inner }` wrappers from an expression (paren
/// erasure — §1.3, §4.1).
///
/// Returns the innermost non-paren expression.  If `expr` is not a `Paren`
/// node this returns `expr` unchanged.
fn peel_paren(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren { inner, .. } => peel_paren(inner),
        other => other,
    }
}

/// Extract the source span from an [`Expr`] for use in error reporting.
///
/// Covers every variant that can legally appear as a pipe RHS (or as an
/// unexpected shape) so that error messages carry a precise source location.
const fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::Literal(lit) => lit.span(),
        Expr::Unit(s)
        | Expr::List { span: s, .. }
        | Expr::Tuple { span: s, .. }
        | Expr::Paren { span: s, .. }
        | Expr::FieldAccessorFn { span: s, .. }
        | Expr::Binary { span: s, .. }
        | Expr::Unary { span: s, .. }
        | Expr::Call { span: s, .. }
        | Expr::FieldAccess { span: s, .. }
        | Expr::Pipe { span: s, .. }
        | Expr::Lambda { span: s, .. }
        | Expr::InnerFn { span: s, .. }
        | Expr::Record { span: s, .. }
        | Expr::With { span: s, .. }
        | Expr::Ask { span: s, .. }
        | Expr::Send { span: s, .. }
        | Expr::Spawn { span: s, .. }
        | Expr::Propagate { span: s, .. }
        | Expr::If { span: s, .. }
        | Expr::Match { span: s, .. }
        | Expr::Try { span: s, .. }
        | Expr::Guard { span: s, .. }
        | Expr::Return { span: s, .. }
        | Expr::Let { span: s, .. }
        | Expr::Var { span: s, .. }
        | Expr::Assign { span: s, .. }
        | Expr::Interp { span: s, .. }
        | Expr::RecordLit { span: s, .. } => *s,
        Expr::Ident(i) => i.span,
        Expr::Qualified(q) => q.span,
        Expr::Block(b) => b.span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Ident, Literal, Span};
    use ridge_ir::{IrExpr, IrLit};
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

    fn unit_expr() -> Expr {
        Expr::Unit(sp())
    }

    fn int_expr(n: &str) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.into(),
            span: sp(),
        })
    }

    fn ident_expr(name: &str) -> Expr {
        Expr::Ident(Ident {
            text: name.into(),
            span: sp(),
        })
    }

    // ── T4-pipe-1: bare callable RHS (Ident `f`) ─────────────────────────────
    //
    // `xs |> f` → `Call(<f-local>, [xs'])`
    // The ident has no binding in the test scaffold, so lower_ident emits a
    // defensive L999 and returns Local("f") — but the structural shape is what
    // we care about.

    #[test]
    fn pipe_bare_callable() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 10);
        let lhs = unit_expr();
        let rhs = ident_expr("f");
        let ir = lower_pipe(&mut ctx, &lhs, &rhs, span);

        match ir {
            IrExpr::Call {
                callee,
                args,
                span: s,
                ..
            } => {
                assert_eq!(s, span, "pipe span must be preserved");
                assert_eq!(args.len(), 1, "bare callable pipe must have exactly 1 arg");
                match args[0] {
                    IrExpr::Lit {
                        value: IrLit::Unit, ..
                    } => {}
                    ref other => panic!("expected Unit arg for lhs, got {other:?}"),
                }
                match *callee {
                    IrExpr::Local { ref name, .. } => {
                        assert_eq!(name, "f", "callee must be Local(f)");
                    }
                    ref other => panic!("expected Local callee, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }
    }

    // ── T4-pipe-2: call RHS — `xs |> f y` → `Call(f, [y, xs])` ─────────────
    //
    // Uses integer literals for lhs and arg to avoid binding wiring.

    #[test]
    fn pipe_call_rhs() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);

        let lhs = int_expr("1"); // xs
        let y = int_expr("2"); // y
        let callee = ident_expr("f");
        let rhs = Expr::Call {
            callee: Box::new(callee),
            args: vec![y],
            span: sp_at(5, 15),
        };

        let ir = lower_pipe(&mut ctx, &lhs, &rhs, span);

        match ir {
            IrExpr::Call { args, .. } => {
                assert_eq!(args.len(), 2, "call-rhs pipe: 1 pre-arg + 1 lhs");
                // First arg is `y` (Int 2), second is `lhs` (Int 1).
                match args[0] {
                    IrExpr::Lit {
                        value: IrLit::Int(2),
                        ..
                    } => {}
                    ref other => panic!("expected Int(2) as first arg, got {other:?}"),
                }
                match args[1] {
                    IrExpr::Lit {
                        value: IrLit::Int(1),
                        ..
                    } => {}
                    ref other => panic!("expected Int(1) as last arg, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }
    }

    // ── T4-pipe-3: chained pipe `xs |> f |> g` ───────────────────────────────
    //
    // Left-associative so the AST is `Pipe(Pipe(xs, f), g)`.
    // Inner pipe `xs |> f` lowers first → Call(f, [xs]).
    // Outer pipe takes that as lhs → Call(g, [Call(f, [xs])]).

    #[test]
    fn pipe_chained() {
        let mut ctx = fresh_ctx();
        let inner_span = sp_at(0, 10);
        let outer_span = sp_at(0, 15);

        let xs = unit_expr();
        let f = ident_expr("f");
        let g = ident_expr("g");

        // Build `xs |> f` first as a nested pipe (AST, not yet lowered).
        let inner_pipe = Expr::Pipe {
            lhs: Box::new(xs),
            rhs: Box::new(f),
            span: inner_span,
        };

        // `(xs |> f) |> g`
        let ir = lower_pipe(&mut ctx, &inner_pipe, &g, outer_span);

        match ir {
            IrExpr::Call {
                callee,
                args,
                span: s,
                ..
            } => {
                assert_eq!(s, outer_span);
                assert_eq!(args.len(), 1, "outer pipe: 1 arg (the inner Call)");
                // The single arg must itself be a Call (the inner pipe's result).
                match args[0] {
                    IrExpr::Call { .. } => {}
                    ref other => panic!("expected inner IrExpr::Call as arg, got {other:?}"),
                }
                // Outer callee is Local("g") — no binding → Local via defensive path.
                match *callee {
                    IrExpr::Local { ref name, .. } => assert_eq!(name, "g"),
                    ref other => panic!("expected Local(g), got {other:?}"),
                }
            }
            other => panic!("expected outer IrExpr::Call, got {other:?}"),
        }
    }

    // ── T4-pipe-4: paren-wrapped RHS `xs |> (f)` → `Call(f, [xs])` ──────────

    #[test]
    fn pipe_paren_rhs_peeled() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 12);

        let lhs = unit_expr();
        let inner_ident = ident_expr("f");
        let rhs = Expr::Paren {
            inner: Box::new(inner_ident),
            span: sp_at(5, 8),
        };

        let ir = lower_pipe(&mut ctx, &lhs, &rhs, span);

        match ir {
            IrExpr::Call { args, .. } => {
                assert_eq!(args.len(), 1, "paren-peeled bare callable: 1 arg");
            }
            other => panic!("expected IrExpr::Call after paren peel, got {other:?}"),
        }
    }

    // ── T4-pipe-5: defensive — Pipe as RHS emits L001 ────────────────────────

    #[test]
    fn pipe_rhs_pipe_emits_l001() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 20);
        let rhs_inner_span = sp_at(10, 20);

        let lhs = unit_expr();
        let rhs = Expr::Pipe {
            lhs: Box::new(unit_expr()),
            rhs: Box::new(ident_expr("f")),
            span: rhs_inner_span,
        };

        let ir = lower_pipe(&mut ctx, &lhs, &rhs, span);

        // Must emit exactly one L001 error.
        assert_eq!(ctx.errors.len(), 1, "expected 1 error");
        assert_eq!(
            ctx.errors[0].code(),
            "L001",
            "expected L001 MalformedPipeRhs"
        );

        // Must return a Unit stub with the outer pipe's span.
        match ir {
            IrExpr::Lit {
                value: IrLit::Unit,
                span: s,
                ..
            } => assert_eq!(s, span),
            other => panic!("expected Unit stub, got {other:?}"),
        }
    }

    // ── T4-pipe-6: defensive — unknown RHS shape emits L002 ──────────────────

    #[test]
    fn pipe_rhs_unknown_emits_l002() {
        let mut ctx = fresh_ctx();
        let span = sp_at(0, 25);

        let lhs = unit_expr();
        // `Expr::Unit` is not a valid pipe RHS shape.
        let rhs = Expr::Unit(sp_at(15, 17));

        let ir = lower_pipe(&mut ctx, &lhs, &rhs, span);

        assert_eq!(ctx.errors.len(), 1, "expected 1 error");
        assert_eq!(
            ctx.errors[0].code(),
            "L002",
            "expected L002 UnknownPipeRhsShape"
        );

        match ir {
            IrExpr::Lit {
                value: IrLit::Unit,
                span: s,
                ..
            } => assert_eq!(s, span),
            other => panic!("expected Unit stub, got {other:?}"),
        }
    }

    // ── T4-pipe-7: IrNodeId counter advances correctly across a pipe ──────────

    #[test]
    fn pipe_id_counter_advances() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let lhs = int_expr("5");
        let rhs = ident_expr("g");
        let _ir = lower_pipe(&mut ctx, &lhs, &rhs, span);
        // After one pipe with one literal lhs + one ident rhs (emitting L999)
        // + one Call node: counter >= 3.
        assert!(
            ctx.ir_node_id_counter >= 3,
            "expected at least 3 IR nodes allocated; got {}",
            ctx.ir_node_id_counter
        );
    }
}
