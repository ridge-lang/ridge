//! Owned numerical value contracts whose order is returned as a signed Int.

use ridge_ast::Span;
use ridge_ir::{CtorKind, IrArm, IrExpr, IrLit, IrParam, IrPat, StdlibKind, SymbolRef};
use ridge_types::{CapabilitySet, ClassId, Type, EQ_CLASS, ORD_CLASS};

use crate::ctx::LowerCtx;

pub fn is_decimal(
    builtins: &ridge_types::BuiltinTyCons,
    class: ClassId,
    tycon: ridge_types::TyConId,
) -> bool {
    (class == EQ_CLASS || class == ORD_CLASS) && tycon == builtins.decimal
}

pub fn dictionary(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    tycon: ridge_types::TyConId,
    span: Span,
) -> Option<IrExpr> {
    if let Some(plan) = ctx
        .instance_env
        .and_then(|env| env.structural_comparisons.get(&(class, tycon)))
        .cloned()
    {
        let class_name = ctx.class_name(class).unwrap_or("?").to_owned();
        return Some(crate::core::dict_plan_to_expr_raw(
            ctx,
            class,
            plan,
            &class_name,
            span,
        ));
    }
    is_decimal(ctx.builtins, class, tycon).then(|| decimal_dictionary(ctx, class, span))
}

fn decimal_dictionary(ctx: &mut LowerCtx<'_>, class: ClassId, span: Span) -> IrExpr {
    let left = ctx.fresh_local("__decimal_left");
    let right = ctx.fresh_local("__decimal_right");
    let method = if class == EQ_CLASS { "eq" } else { "compare" };
    let arguments = vec![local(ctx, &left, span), local(ctx, &right, span)];
    let call = call(ctx, "std.decimal", method, arguments, span);
    let body = if class == EQ_CLASS {
        call
    } else {
        signed_ordering(ctx, call, span)
    };
    let method_fn = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params: vec![
            IrParam {
                name: left,
                ty: Type::Con(ctx.builtins.decimal, vec![]),
                span,
            },
            IrParam {
                name: right,
                ty: Type::Con(ctx.builtins.decimal, vec![]),
                span,
            },
        ],
        body: Box::new(body),
        caps: CapabilitySet::PURE,
        span,
    };
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: None,
            name: "$decimal_comparison".into(),
            variant: 0,
        },
        fields: vec![(method.into(), method_fn)],
        span,
    }
}

fn local(ctx: &mut LowerCtx<'_>, name: &str, span: Span) -> IrExpr {
    IrExpr::Local {
        id: ctx.fresh_id(None),
        name: name.into(),
        span,
    }
}

fn call(ctx: &mut LowerCtx<'_>, module: &str, name: &str, args: Vec<IrExpr>, span: Span) -> IrExpr {
    let callee = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: module.into(),
            name: name.into(),
            kind: StdlibKind::Function,
        },
        span,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(callee),
        args,
        span,
    }
}

fn ordering(ctx: &mut LowerCtx<'_>, name: &str, variant: u32, span: Span) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: Some(ctx.builtins.ordering),
            name: name.into(),
            variant,
        },
        fields: vec![],
        span,
    }
}

fn choose(
    ctx: &mut LowerCtx<'_>,
    condition: IrExpr,
    yes: IrExpr,
    no: IrExpr,
    span: Span,
) -> IrExpr {
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(condition),
        arms: vec![
            IrArm {
                pat: IrPat::Lit {
                    value: IrLit::Bool(true),
                    span,
                },
                when: None,
                body: yes,
                span,
            },
            IrArm {
                pat: IrPat::Wild { span },
                when: None,
                body: no,
                span,
            },
        ],
        span,
    }
}

fn signed_ordering(ctx: &mut LowerCtx<'_>, value: IrExpr, span: Span) -> IrExpr {
    let name = ctx.fresh_local("__decimal_order");
    let result = local(ctx, &name, span);
    let zero = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Int(0),
        span,
    };
    let less = call(
        ctx,
        "std.op",
        "lt",
        vec![result.clone(), zero.clone()],
        span,
    );
    let greater = call(ctx, "std.op", "gt", vec![result, zero], span);
    let equal = ordering(ctx, "Equal", 1, span);
    let greater_value = ordering(ctx, "Greater", 2, span);
    let less_value = ordering(ctx, "Less", 0, span);
    let otherwise = choose(ctx, greater, greater_value, equal, span);
    let body = choose(ctx, less, less_value, otherwise, span);
    IrExpr::LetIn {
        id: ctx.fresh_id(None),
        pat: IrPat::Bind {
            name,
            inner: None,
            span,
        },
        value: Box::new(value),
        body: Box::new(body),
        span,
    }
}
