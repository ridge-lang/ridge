//! Close implicit constraint evidence when a named function becomes a value.

use ridge_ast::Expr;
use ridge_ir::{IrExpr, IrParam};
use ridge_types::{CapRow, CapabilitySet, Type};

use crate::core::{build_dict_args, lookup_callee_type};
use crate::ctx::LowerCtx;

/// The stored callable accepts only source parameters. Its adapter supplies the
/// same evidence a direct call would pass, including enclosing generic evidence.
pub fn close_constraints(ctx: &mut LowerCtx<'_>, source: &Expr, callee: IrExpr) -> IrExpr {
    let Some(Type::Fn { params, ret, caps }) = lookup_callee_type(ctx, source) else {
        return callee;
    };
    let span = source.span();
    let arg_types: Vec<_> = params.iter().cloned().map(Some).collect();
    let mut args = build_dict_args(ctx, &callee, &arg_types, Some(&ret), span);
    if args.is_empty() {
        return callee;
    }
    let mut parameters = Vec::with_capacity(params.len());
    for ty in params {
        let name = ctx.fresh_local("__constrained");
        args.push(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: name.clone(),
            span,
        });
        parameters.push(IrParam { name, ty, span });
    }
    let body = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(callee),
        args,
        span,
    };
    IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params: parameters,
        body: Box::new(body),
        caps: match caps {
            CapRow::Concrete(caps) => caps,
            _ => CapabilitySet::PURE,
        },
        span,
    }
}
