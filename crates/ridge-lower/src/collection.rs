//! Collection equality composes checked key and value evidence.

use ridge_ast::Span;
use ridge_ir::{IrExpr, IrLit, IrParam, StdlibKind, SymbolRef};
use ridge_typecheck::DictPlan;
use ridge_types::{TyConId, Type, EQ_CLASS};

use crate::ctx::LowerCtx;

pub fn equality_dictionary(
    ctx: &mut LowerCtx<'_>,
    tycon: TyConId,
    plans: Vec<DictPlan>,
    span: Span,
) -> IrExpr {
    let is_map = tycon == ctx.builtins.map;
    if plans.len() != if is_map { 3 } else { 2 } {
        // Missing component evidence is already diagnosed by type checking.
        // Retain an editor hole; never manufacture a valid comparison method.
        return IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Unit,
            span,
        };
    }
    let mut args = Vec::new();
    for (index, plan) in plans.into_iter().enumerate() {
        let dict = crate::core::dict_plan_to_expr(ctx, EQ_CLASS, plan, "Eq", span);
        args.push(IrExpr::Field {
            id: ctx.fresh_id(None),
            base: Box::new(dict),
            field: if index == 0 { "hash" } else { "eq" }.into(),
            span,
        });
    }
    let mut params = Vec::new();
    for prefix in ["__collection_left", "__collection_right"] {
        let name = ctx.fresh_local(prefix);
        args.push(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: name.clone(),
            span,
        });
        params.push(IrParam {
            name,
            ty: Type::Error,
            span,
        });
    }
    let callee = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.op".into(),
            name: if is_map { "mapEq" } else { "setEq" }.into(),
            kind: StdlibKind::Function,
        },
        span,
    };
    let body = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(callee),
        args,
        span,
    };
    crate::comparison::method_dictionary(ctx, "eq", params, body, span)
}
