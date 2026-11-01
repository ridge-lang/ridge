//! Comparison operators invoke checked evidence and consume the result once.

use ridge_ast::{BinOp, Expr, Span};
use ridge_ir::{CtorKind, IrArm, IrExpr, IrLit, IrPat, SymbolRef};
use ridge_types::{Type, EQ_CLASS, ORD_CLASS};

use crate::ctx::LowerCtx;

pub fn lower(
    ctx: &mut LowerCtx<'_>,
    op: BinOp,
    left: &Expr,
    right: &Expr,
    ty: &Type,
    span: Span,
) -> IrExpr {
    let equality = matches!(op, BinOp::Eq | BinOp::Ne);
    let class = if equality { EQ_CLASS } else { ORD_CLASS };
    let dictionary = crate::core::comparison_dictionary(ctx, class, ty, span);
    let callee = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dictionary),
        field: if equality { "eq" } else { "compare" }.into(),
        span,
    };
    let left = crate::core::lower_expr(ctx, left);
    let right = crate::core::lower_expr(ctx, right);
    let call = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(callee),
        args: vec![left, right],
        span,
    };
    if op == BinOp::Eq {
        return call;
    }
    let answers: Vec<_> = if equality {
        vec![
            (
                IrPat::Lit {
                    value: IrLit::Bool(true),
                    span,
                },
                false,
            ),
            (
                IrPat::Lit {
                    value: IrLit::Bool(false),
                    span,
                },
                true,
            ),
        ]
    } else {
        [("Less", 0), ("Equal", 1), ("Greater", 2)]
            .into_iter()
            .map(|(name, variant)| {
                let answer = match op {
                    BinOp::Lt => variant == 0,
                    BinOp::Le => variant != 2,
                    BinOp::Gt => variant == 2,
                    BinOp::Ge => variant != 0,
                    _ => false,
                };
                (
                    IrPat::Ctor {
                        sym: SymbolRef::Constructor {
                            ctor_kind: CtorKind::UnionVariant,
                            owner_type: Some(ctx.builtins.ordering),
                            name: name.into(),
                            variant,
                        },
                        args: vec![],
                        fields: vec![],
                        span,
                    },
                    answer,
                )
            })
            .collect()
    };
    let arms = answers
        .into_iter()
        .map(|(pat, answer)| IrArm {
            pat,
            when: None,
            body: boolean(ctx, answer, span),
            span,
        })
        .collect();
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(call),
        arms,
        span,
    }
}

fn boolean(ctx: &mut LowerCtx<'_>, value: bool, span: Span) -> IrExpr {
    IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Bool(value),
        span,
    }
}
