//! Retained dictionary recipes use source identities and explicit context.
//!
//! One exported dispatcher per module avoids identifiers derived from source
//! positions, arena indices, hash collisions, or arbitrarily long field names.

use ridge_ast::Span;
use ridge_ir::{IrArm, IrExpr, IrFn, IrItem, IrLit, IrParam, IrPat, StdlibKind, SymbolRef};
use ridge_resolve::NodeId;
use ridge_typecheck::DictPlan;
use ridge_types::{CapabilitySet, Constraint, Scheme, TyVid, Type};

use crate::ctx::LowerCtx;

const DISPATCHER: &str = "$ridge_evidence";

fn local(ctx: &mut LowerCtx<'_>, name: &str, span: Span) -> IrExpr {
    IrExpr::Local {
        id: ctx.fresh_id(None),
        name: name.into(),
        span,
    }
}

const fn parameter(name: String, span: Span) -> IrParam {
    IrParam {
        name,
        ty: Type::Error,
        span,
    }
}

pub fn dictionary(
    ctx: &mut LowerCtx<'_>,
    mut plan: DictPlan,
    fallback_class: &str,
    span: Span,
) -> IrExpr {
    let class = crate::core::plan_class(&plan);
    let provider = crate::evidence_registry::provider(ctx, &plan);
    let layout = structural_layout(ctx, &plan, span);
    let Some(identity) = crate::evidence_identity::recipe(ctx, &plan) else {
        ctx.errors
            .push(crate::error::LowerError::InternalLoweringError {
                span,
                message: "unsupported type in retained evidence identity".into(),
            });
        return IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Unit,
            span,
        };
    };
    let children = match &mut plan {
        DictPlan::Structural { args, .. } | DictPlan::Static { args, .. } => std::mem::take(args),
        DictPlan::Forward(_) => unreachable!("forwarded evidence is already retained"),
    };
    let mut parameters = Vec::new();
    let mut forwarded = Vec::new();
    let mut arguments = Vec::new();
    for (index, child) in children.into_iter().enumerate() {
        let child_class = crate::core::plan_class(&child);
        let constraint =
            Constraint::single(child_class, TyVid(u32::try_from(index).unwrap_or(u32::MAX)));
        let name =
            constraint.dict_param_name(ctx.class_name(child_class).unwrap_or(fallback_class));
        parameters.push(parameter(name, span));
        forwarded.push(DictPlan::Forward(constraint));
        arguments.push(crate::core::dict_plan_to_expr(
            ctx,
            child_class,
            child,
            fallback_class,
            span,
        ));
    }
    match &mut plan {
        DictPlan::Structural { args, .. } | DictPlan::Static { args, .. } => *args = forwarded,
        DictPlan::Forward(_) => unreachable!("forwarded evidence is already retained"),
    }
    if crate::evidence_registry::owns(ctx, &provider)
        && !ctx.evidence_factories.contains_key(&identity)
    {
        let body = crate::core::dict_plan_to_expr_raw(ctx, class, plan, fallback_class, span);
        let body = IrExpr::Tuple {
            id: ctx.fresh_id(None),
            elems: vec![body, layout],
            span,
        };
        ctx.evidence_factories
            .insert(identity.clone(), (parameters, body, span));
    }
    let factory = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: provider,
        span,
    };
    let identity = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(identity),
        span,
    };
    let arguments = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: arguments,
        span,
    };
    let operation = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.op".into(),
            name: "evidence".into(),
            kind: StdlibKind::Function,
        },
        span,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(operation),
        args: vec![factory, identity, arguments],
        span,
    }
}

fn option(ctx: &mut LowerCtx<'_>, value: Option<IrExpr>, span: Span) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: if value.is_some() { "Some" } else { "None" }.into(),
        },
        fields: value
            .into_iter()
            .map(|value| (String::new(), value))
            .collect(),
        span,
    }
}

fn structural_layout(ctx: &mut LowerCtx<'_>, plan: &DictPlan, span: Span) -> IrExpr {
    let DictPlan::Structural { class, fields, .. } = plan else {
        return option(ctx, None, span);
    };
    let method = if *class == ridge_types::EQ_CLASS {
        "eq"
    } else if *class == ridge_types::ORD_CLASS {
        "compare"
    } else if *class == ridge_types::HASH_CLASS {
        "hash"
    } else {
        ctx.errors
            .push(crate::error::LowerError::InternalLoweringError {
                span,
                message: "unsupported class in structural evidence recipe".into(),
            });
        return option(ctx, None, span);
    };
    let method = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(method.into()),
        span,
    };
    let record = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Bool(fields.is_some()),
        span,
    };
    let labels = fields
        .iter()
        .flatten()
        .map(|label| IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Text(label.clone()),
            span,
        })
        .collect();
    let labels = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: labels,
        span,
    };
    let layout = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![method, record, labels],
        span,
    };
    option(ctx, Some(layout), span)
}

pub fn dispatcher(ctx: &mut LowerCtx<'_>) -> Option<IrItem> {
    let factories = std::mem::take(&mut ctx.evidence_factories);
    let span = factories.values().next()?.2;
    let mut arms = Vec::new();
    for (identity, (parameters, body, span)) in factories {
        let context = parameters
            .into_iter()
            .rev()
            .fold(IrPat::Nil { span }, |tail, param| IrPat::Cons {
                head: Box::new(IrPat::Bind {
                    name: param.name,
                    inner: None,
                    span,
                }),
                tail: Box::new(tail),
                span,
            });
        let body = option(ctx, Some(body), span);
        arms.push(IrArm {
            pat: IrPat::Tuple {
                elems: vec![
                    IrPat::Lit {
                        value: IrLit::Text(identity),
                        span,
                    },
                    context,
                ],
                span,
            },
            when: None,
            body,
            span,
        });
    }
    let missing = option(ctx, None, span);
    arms.push(IrArm {
        pat: IrPat::Wild { span },
        when: None,
        body: missing,
        span,
    });
    let key = local(ctx, "$evidence_identity", span);
    let arguments = local(ctx, "$evidence_arguments", span);
    let scrutinee = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![key, arguments],
        span,
    };
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(scrutinee),
        arms,
        span,
    };
    Some(IrItem::Fn(IrFn {
        name: DISPATCHER.into(),
        module: ctx.module_id,
        params: vec![
            parameter("$evidence_identity".into(), span),
            parameter("$evidence_arguments".into(), span),
        ],
        ret_ty: Type::Error,
        caps: CapabilitySet::PURE,
        scheme: Scheme::mono(Type::Error),
        body,
        origin: NodeId(0),
        span,
        is_pub: true,
        is_main: false,
        doc: None,
    }))
}
