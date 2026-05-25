//! Binary and unary operator lowering — §4.11.
//!
//! Implements the static op-to-symbol table for `Expr::Binary` and `Expr::Unary`.
//!
//! # Design notes
//!
//! - All binary operators (except `::` Cons) lower to `IrExpr::Call` with a
//!   `SymbolRef::Stdlib` callee — see §4.11 for the rationale.
//! - `BinOp::Cons` lowers to the dedicated `IrExpr::Cons` variant.
//! - Arithmetic type dispatch (`Int` vs `Float` family) requires upstream
//!   `node_types` wiring (Phase 4 left it empty; not currently scheduled).
//!   All arithmetic ops default to the Int family; see `op_to_symbol`.
//! - `BinOp::Pipe` cannot appear here — the parser emits `Expr::Pipe` for `|>`,
//!   not `Expr::Binary { op: BinOp::Pipe }`.  A defensive stub is provided.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{expr::BinOp, expr::UnaryOp, Expr, Span};
use ridge_ir::{IrExpr, IrLit, SymbolRef};
use ridge_resolve::NodeKind;
use ridge_types::Type;

use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower `lhs op rhs` to an [`IrExpr`].
///
/// `BinOp::Cons` → `IrExpr::Cons { head, tail }`.
/// All other binary ops → `IrExpr::Call { callee: Symbol(stdlib), args: [lhs', rhs'] }`.
/// `BinOp::Pipe` → defensive `Unit` stub with `InternalLoweringError` (L999);
/// the parser never emits it as `Expr::Binary`.
///
/// Never panics on any input.
pub fn lower_binary(
    ctx: &mut LowerCtx<'_>,
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
) -> IrExpr {
    // ── BinOp::Cons — dedicated IR variant ────────────────────────────────────
    if matches!(op, BinOp::Cons) {
        let id = ctx.fresh_id(None);
        let head = Box::new(lower_expr(ctx, lhs));
        let tail = Box::new(lower_expr(ctx, rhs));
        return IrExpr::Cons {
            id,
            head,
            tail,
            span,
        };
    }

    // ── BinOp::Pipe — defensive: parser never emits this ─────────────────────
    if matches!(op, BinOp::Pipe) {
        let id = ctx.fresh_id(None);
        ctx.errors.push(LowerError::InternalLoweringError {
            span,
            message: "BinOp::Pipe encountered in lower_binary; the parser emits Expr::Pipe, not Binary{Pipe}".into(),
        });
        return IrExpr::Lit {
            id,
            value: IrLit::Unit,
            span,
        };
    }

    // ── All other ops — lower to stdlib Call ──────────────────────────────────
    // PHASE45-T3: resolve the LHS type from node_types for type-driven dispatch
    // (Float vs Int arithmetic; Text vs List concatenation).  RHS is consulted
    // as a fallback when the LHS lookup misses, which happens inside contexts
    // where node_types is not populated for sub-expressions (notably actor
    // handler bodies; see the structural-Float-hint fallback below).
    let lhs_ty = resolve_lhs_type(ctx, lhs);
    let rhs_ty = resolve_lhs_type(ctx, rhs);
    let (module, name) = op_to_symbol_with_fallback(op, &lhs_ty, &rhs_ty, lhs, rhs, ctx);
    let callee_id = ctx.fresh_id(None);
    let call_id = ctx.fresh_id(None);

    let callee = Box::new(IrExpr::Symbol {
        id: callee_id,
        sym: SymbolRef::Stdlib {
            module: module.into(),
            name: name.into(),
        },
        span,
    });

    let lhs_ir = lower_expr(ctx, lhs);
    let rhs_ir = lower_expr(ctx, rhs);

    IrExpr::Call {
        id: call_id,
        callee,
        args: vec![lhs_ir, rhs_ir],
        span,
    }
}

/// Lower `-expr` to `Call(std.int.neg, [expr'])` or `Call(std.float.neg, [expr'])`.
///
/// PHASE45-T3: type-driven dispatch — reads the operand's resolved type from
/// `node_types` via `node_id_map`; dispatches to `std.float.neg` when the
/// operand is `Float`; falls back to `std.int.neg` on miss.
///
/// Never panics on any input.
pub fn lower_unary(ctx: &mut LowerCtx<'_>, op: UnaryOp, expr: &Expr, span: Span) -> IrExpr {
    match op {
        UnaryOp::Neg => {
            // PHASE45-T3: resolve the operand type for Float/Int dispatch.
            let operand_ty = resolve_lhs_type(ctx, expr);
            let (neg_module, neg_name) = if is_float(ctx, &operand_ty) {
                ("std.float", "neg")
            } else {
                ("std.int", "neg")
            };
            let callee_id = ctx.fresh_id(None);
            let call_id = ctx.fresh_id(None);
            let callee = Box::new(IrExpr::Symbol {
                id: callee_id,
                sym: SymbolRef::Stdlib {
                    module: neg_module.into(),
                    name: neg_name.into(),
                },
                span,
            });
            let operand = lower_expr(ctx, expr);
            IrExpr::Call {
                id: call_id,
                callee,
                args: vec![operand],
                span,
            }
        }
    }
}

// ── Op-to-symbol table ────────────────────────────────────────────────────────

/// Map a [`BinOp`] to `(stdlib_module, stdlib_fn_name)`.
///
/// Called only for non-`Cons` and non-`Pipe` operators (those two are handled
/// separately by the caller).
///
/// # Type dispatch (PHASE45-T3)
///
/// Arithmetic ops (`Add`, `Sub`, `Mul`, `Div`, `Pow`) dispatch to `std.float.*`
/// when `lhs_ty` resolves to the workspace's `Float` tycon; otherwise they fall
/// back to `std.int.*`.  `BinOp::Concat` dispatches to `std.list.concat` when
/// `lhs_ty` resolves to `List`; otherwise falls back to `std.text.concat`.
/// `BinOp::Mod` and `BinOp::Pow` have no Float counterpart and remain Int-only.
fn op_to_symbol(op: BinOp, lhs_ty: &Type, ctx: &LowerCtx<'_>) -> (&'static str, &'static str) {
    match op {
        // ── Arithmetic — Float/Int dispatch via node_types ────────────────────
        // PHASE45-T3: dispatches to std.float.* when LHS resolves to Float.
        BinOp::Add => {
            if is_float(ctx, lhs_ty) {
                ("std.float", "add")
            } else {
                ("std.int", "add")
            }
        }
        BinOp::Sub => {
            if is_float(ctx, lhs_ty) {
                ("std.float", "sub")
            } else {
                ("std.int", "sub")
            }
        }
        BinOp::Mul => {
            if is_float(ctx, lhs_ty) {
                ("std.float", "mul")
            } else {
                ("std.int", "mul")
            }
        }
        BinOp::Div => {
            if is_float(ctx, lhs_ty) {
                ("std.float", "div")
            } else {
                ("std.int", "div")
            }
        }
        // PHASE45-T3: Mod and Pow are Int-only (no Float counterpart).
        BinOp::Mod => ("std.int", "mod"),
        BinOp::Pow => ("std.int", "pow"),

        // OQ-L010: polymorphic == and other comparison ops lower to std.op.eq/ne/lt/…
        // (not type-specific), relying on runtime dispatch for type-directed equality.
        // ── Polymorphic comparison ─────────────────────────────────────────────
        BinOp::Eq => ("std.op", "eq"),
        BinOp::Ne => ("std.op", "ne"),
        BinOp::Lt => ("std.op", "lt"),
        BinOp::Gt => ("std.op", "gt"),
        BinOp::Le => ("std.op", "le"),
        BinOp::Ge => ("std.op", "ge"),

        // ── Boolean logic ──────────────────────────────────────────────────────
        BinOp::And => ("std.bool", "and"),
        BinOp::Or => ("std.bool", "or"),

        // ── Concatenation — Text vs List dispatch via node_types ──────────────
        // PHASE45-T3: dispatches to std.list.concat when LHS resolves to List.
        BinOp::Concat => {
            if is_list(ctx, lhs_ty) {
                ("std.list", "concat")
            } else {
                ("std.text", "concat")
            }
        }

        // ── Handled before this function is called; unreachable ───────────────
        BinOp::Cons | BinOp::Pipe => {
            // Defensive: these two are handled before `op_to_symbol` is called.
            // Return a placeholder — the caller guards against this.
            ("std.op", "unreachable_op")
        }
    }
}

/// Resolve the type of an AST expression by looking up its span in the
/// `node_id_map` and then in `node_types`.
///
/// Used by arithmetic and concat dispatch to determine Float/Int and Text/List
/// families.  Returns `Type::Error` when no mapping is found.
///
/// PHASE45-T3: type-driven dispatch helper.
fn resolve_lhs_type(ctx: &LowerCtx<'_>, expr: &Expr) -> Type {
    ctx.node_id_map
        .as_ref()
        .and_then(|m| m.get(expr.span(), NodeKind::Expr))
        .and_then(|nid| ctx.node_type(nid).cloned())
        .unwrap_or(Type::Error)
}

/// Op-to-symbol dispatch with RHS and structural fallbacks.
///
/// Most operators (`+`, `-`, `*`, polymorphic comparisons, boolean logic) lower
/// to the same Erlang BIF for both numeric families, so the Int default is
/// harmless.  `BinOp::Div` is the exception: `std.int.div` lowers to
/// `erlang:div/2`, which crashes on Float operands.  When neither side carries
/// a `node_types` entry (which is the case inside actor handler bodies — those
/// bodies are not visited by `infer_expr` today), fall back to a conservative
/// structural inspection of both operands for Float hints.
fn op_to_symbol_with_fallback(
    op: BinOp,
    lhs_ty: &Type,
    rhs_ty: &Type,
    lhs: &Expr,
    rhs: &Expr,
    ctx: &LowerCtx<'_>,
) -> (&'static str, &'static str) {
    if matches!(op, BinOp::Div)
        && !is_float(ctx, lhs_ty)
        && !is_float(ctx, rhs_ty)
        && (looks_like_float(lhs) || looks_like_float(rhs))
    {
        return ("std.float", "div");
    }
    let effective_lhs_ty = if matches!(lhs_ty, Type::Error) && is_float(ctx, rhs_ty) {
        rhs_ty
    } else {
        lhs_ty
    };
    op_to_symbol(op, effective_lhs_ty, ctx)
}

/// Best-effort structural check for "this expression evaluates to Float".
///
/// Only returns `true` for forms whose float-ness is locally evident — Float
/// literals and qualified calls into the `Float` module.  Recurses through
/// parens, binary/unary operators, and conditional branches; intentionally
/// conservative everywhere else (a bare local binding gives no signal here,
/// since lowering does not track binding types).
fn looks_like_float(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(lit) => matches!(lit, ridge_ast::Literal::Float { .. }),
        Expr::Paren { inner, .. } | Expr::Unary { expr: inner, .. } => looks_like_float(inner),
        Expr::Binary { lhs, rhs, .. } => looks_like_float(lhs) || looks_like_float(rhs),
        Expr::Call { callee, .. } => match callee.as_ref() {
            Expr::Qualified(q) => q.segments.first().is_some_and(|seg| seg.text == "Float"),
            Expr::FieldAccess { base, .. } => matches!(
                base.as_ref(),
                Expr::Ident(id) if id.text == "Float"
            ),
            _ => false,
        },
        Expr::Qualified(q) => q.segments.first().is_some_and(|seg| seg.text == "Float"),
        Expr::If {
            then_branch,
            else_branch,
            ..
        } => {
            looks_like_float(then_branch)
                || else_branch.as_ref().is_some_and(|e| looks_like_float(e))
        }
        Expr::Block(b) => b.stmts.last().is_some_and(looks_like_float),
        _ => false,
    }
}

/// Returns `true` if `ty` is the workspace's `Float` tycon.
///
/// Checks via the workspace's `builtins.float` id when a workspace is attached.
/// Falls back to `false` (Int default) when no workspace is present.
///
/// PHASE45-T3: used by arithmetic op dispatch.
fn is_float(ctx: &LowerCtx<'_>, ty: &Type) -> bool {
    let Some(ws) = ctx.workspace else {
        return false;
    };
    matches!(ty, Type::Con(id, _) if *id == ws.builtins.float)
}

/// Returns `true` if `ty` is the workspace's `List` tycon.
///
/// Checks via the workspace's `builtins.list` id when a workspace is attached.
/// Falls back to `false` (Text default) when no workspace is present.
///
/// PHASE45-T3: used by `++` concat dispatch.
fn is_list(ctx: &LowerCtx<'_>, ty: &Type) -> bool {
    let Some(ws) = ctx.workspace else {
        return false;
    };
    matches!(ty, Type::Con(id, _) if *id == ws.builtins.list)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{expr::BinOp, expr::UnaryOp, Literal, Span};
    use ridge_ir::{IrExpr, IrLit, SymbolRef};
    use ridge_resolve::ModuleId;

    fn sp() -> Span {
        Span::point(0)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn int_expr(n: i64) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.to_string(),
            span: sp(),
        })
    }

    fn list_expr() -> Expr {
        Expr::List {
            elems: vec![],
            span: sp(),
        }
    }

    // ── T4-op-1: Add lowers to std.int.add Call ───────────────────────────────

    #[test]
    fn binary_add_default_int() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let lhs = int_expr(1);
        let rhs = int_expr(2);

        let ir = lower_binary(&mut ctx, BinOp::Add, &lhs, &rhs, span);

        match ir {
            IrExpr::Call { callee, args, .. } => {
                assert_eq!(args.len(), 2);
                match *callee {
                    IrExpr::Symbol {
                        sym:
                            SymbolRef::Stdlib {
                                ref module,
                                ref name,
                            },
                        ..
                    } => {
                        assert_eq!(module, "std.int");
                        assert_eq!(name, "add");
                    }
                    ref other => panic!("expected Stdlib callee, got {other:?}"),
                }
                match (&args[0], &args[1]) {
                    (
                        IrExpr::Lit {
                            value: IrLit::Int(1),
                            ..
                        },
                        IrExpr::Lit {
                            value: IrLit::Int(2),
                            ..
                        },
                    ) => {}
                    _ => panic!("expected Int(1) and Int(2) as args"),
                }
            }
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── T4-op-2: Eq lowers to std.op.eq (polymorphic) ─────────────────────────

    #[test]
    fn binary_eq_polymorphic() {
        let mut ctx = fresh_ctx();
        let ir = lower_binary(&mut ctx, BinOp::Eq, &int_expr(1), &int_expr(2), sp());

        match ir {
            IrExpr::Call { callee, .. } => match *callee {
                IrExpr::Symbol {
                    sym:
                        SymbolRef::Stdlib {
                            ref module,
                            ref name,
                        },
                    ..
                } => {
                    assert_eq!(module, "std.op");
                    assert_eq!(name, "eq");
                }
                ref other => panic!("expected Stdlib(std.op.eq), got {other:?}"),
            },
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }
    }

    // ── T4-op-3: Cons lowers to IrExpr::Cons ──────────────────────────────────

    #[test]
    fn binary_cons_emits_cons() {
        let mut ctx = fresh_ctx();
        let lhs = int_expr(1);
        let rhs = list_expr(); // `[]` — lowers to Unit stub (T5 handles ListLit).

        let ir = lower_binary(&mut ctx, BinOp::Cons, &lhs, &rhs, sp());

        match ir {
            IrExpr::Cons { head, tail, .. } => {
                match *head {
                    IrExpr::Lit {
                        value: IrLit::Int(1),
                        ..
                    } => {}
                    ref other => panic!("expected Int(1) as head, got {other:?}"),
                }
                // Tail is whatever the stub produces (Unit for now — T5 will fix List).
                // The structural assertion is: we got IrExpr::Cons, not a Call.
                let _ = tail;
            }
            other => panic!("expected IrExpr::Cons, got {other:?}"),
        }
    }

    // ── T4-op-4: UnaryOp::Neg lowers to std.int.neg ───────────────────────────

    #[test]
    fn unary_neg_default_int() {
        let mut ctx = fresh_ctx();
        let operand = int_expr(42);

        let ir = lower_unary(&mut ctx, UnaryOp::Neg, &operand, sp());

        match ir {
            IrExpr::Call { callee, args, .. } => {
                assert_eq!(args.len(), 1);
                match *callee {
                    IrExpr::Symbol {
                        sym:
                            SymbolRef::Stdlib {
                                ref module,
                                ref name,
                            },
                        ..
                    } => {
                        assert_eq!(module, "std.int");
                        assert_eq!(name, "neg");
                    }
                    ref other => panic!("expected Stdlib(std.int.neg), got {other:?}"),
                }
                match args[0] {
                    IrExpr::Lit {
                        value: IrLit::Int(42),
                        ..
                    } => {}
                    ref other => panic!("expected Int(42) operand, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── T4-op-5: BinOp::Pipe as Binary emits L999 defensive error ────────────

    #[test]
    fn binary_pipe_binop_emits_internal_error() {
        let mut ctx = fresh_ctx();
        let ir = lower_binary(&mut ctx, BinOp::Pipe, &int_expr(1), &int_expr(2), sp());

        assert_eq!(ctx.errors.len(), 1, "expected 1 error");
        assert_eq!(ctx.errors[0].code(), "L999");

        match ir {
            IrExpr::Lit {
                value: IrLit::Unit, ..
            } => {}
            other => panic!("expected Unit stub, got {other:?}"),
        }
    }

    // ── T4-op-6: And lowers to std.bool.and ───────────────────────────────────

    #[test]
    fn binary_and_bool() {
        let mut ctx = fresh_ctx();
        let ir = lower_binary(&mut ctx, BinOp::And, &int_expr(1), &int_expr(2), sp());

        match ir {
            IrExpr::Call { callee, .. } => match *callee {
                IrExpr::Symbol {
                    sym:
                        SymbolRef::Stdlib {
                            ref module,
                            ref name,
                        },
                    ..
                } => {
                    assert_eq!(module, "std.bool");
                    assert_eq!(name, "and");
                }
                ref other => panic!("expected Stdlib(std.bool.and), got {other:?}"),
            },
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }
    }

    // ── T4-op-7: Concat lowers to std.text.concat (default) ──────────────────

    #[test]
    fn binary_concat_default_text() {
        let mut ctx = fresh_ctx();
        let ir = lower_binary(&mut ctx, BinOp::Concat, &int_expr(1), &int_expr(2), sp());

        match ir {
            IrExpr::Call { callee, .. } => match *callee {
                IrExpr::Symbol {
                    sym:
                        SymbolRef::Stdlib {
                            ref module,
                            ref name,
                        },
                    ..
                } => {
                    assert_eq!(module, "std.text");
                    assert_eq!(name, "concat");
                }
                ref other => panic!("expected Stdlib(std.text.concat), got {other:?}"),
            },
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }
    }

    // ── T4-op-8: Sub span is preserved on the Call node ───────────────────────

    #[test]
    fn binary_sub_span_preserved() {
        let mut ctx = fresh_ctx();
        let span = Span::new(5, 15);
        let ir = lower_binary(&mut ctx, BinOp::Sub, &int_expr(10), &int_expr(3), span);

        match ir {
            IrExpr::Call { span: s, .. } => assert_eq!(s, span, "span must be preserved"),
            other => panic!("expected IrExpr::Call, got {other:?}"),
        }
    }
}
