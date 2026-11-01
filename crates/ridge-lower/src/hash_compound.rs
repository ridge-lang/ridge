//! Hash composition calls checked component dictionaries in canonical order.

use ridge_ast::Span;
use ridge_ir::{CtorKind, IrArm, IrExpr, IrLit, IrParam, IrPat, StdlibKind, SymbolRef};
use ridge_typecheck::derive::{ComparisonShape, ComparisonVariant};
use ridge_typecheck::DictPlan;
use ridge_types::{CapabilitySet, TyConId, Type, HASH_CLASS};

use crate::ctx::LowerCtx;

fn local(ctx: &mut LowerCtx<'_>, name: &str, span: Span) -> IrExpr {
    IrExpr::Local {
        id: ctx.fresh_id(None),
        name: name.into(),
        span,
    }
}

fn binding(name: &str, span: Span) -> IrPat {
    IrPat::Bind {
        name: name.into(),
        inner: None,
        span,
    }
}

fn call(ctx: &mut LowerCtx<'_>, callee: IrExpr, args: Vec<IrExpr>, span: Span) -> IrExpr {
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(callee),
        args,
        span,
    }
}

fn tag(
    ctx: &mut LowerCtx<'_>,
    kind: &str,
    labels: &[String],
    rank: usize,
    arity: usize,
    state: IrExpr,
    span: Span,
) -> IrExpr {
    let kind = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(kind.into()),
        span,
    };
    let rank = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Int(i64::try_from(rank).unwrap_or(i64::MAX)),
        span,
    };
    let arity = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Int(i64::try_from(arity).unwrap_or(i64::MAX)),
        span,
    };
    let names = labels
        .iter()
        .map(|label| IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Text(label.clone()),
            span,
        })
        .collect();
    let names = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: names,
        span,
    };
    let token = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![kind, rank, arity, names],
        span,
    };
    let callee = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.op".into(),
            name: "hashTag".into(),
            kind: StdlibKind::Function,
        },
        span,
    };
    call(ctx, callee, vec![token, state], span)
}

fn component(
    ctx: &mut LowerCtx<'_>,
    evidence: DictPlan,
    value: IrExpr,
    state: IrExpr,
    span: Span,
) -> IrExpr {
    let dictionary = crate::core::dict_plan_to_expr(ctx, HASH_CLASS, evidence, "Hash", span);
    let callee = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dictionary),
        field: "hash".into(),
        span,
    };
    call(ctx, callee, vec![value, state], span)
}

fn parameters(value: String, state: String, span: Span) -> Vec<IrParam> {
    vec![
        IrParam {
            name: value,
            ty: Type::Error,
            span,
        },
        IrParam {
            name: state,
            ty: Type::Error,
            span,
        },
    ]
}

fn dictionary(ctx: &mut LowerCtx<'_>, body: IrExpr, params: Vec<IrParam>, span: Span) -> IrExpr {
    let method = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params,
        body: Box::new(body),
        caps: CapabilitySet::PURE,
        span,
    };
    method_dictionary(ctx, method, span)
}

fn method_dictionary(ctx: &mut LowerCtx<'_>, method: IrExpr, span: Span) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: None,
            name: "$compound_hash".into(),
            variant: 0,
        },
        fields: vec![("hash".into(), method)],
        span,
    }
}

pub fn structural_dict(
    ctx: &mut LowerCtx<'_>,
    fields: Option<&[String]>,
    children: Vec<DictPlan>,
    span: Span,
) -> IrExpr {
    let value = ctx.fresh_local("__hash_value");
    let state = ctx.fresh_local("__hash_state");
    let body = structural_body(ctx, fields, children, &value, &state, span);
    dictionary(ctx, body, parameters(value, state, span), span)
}

fn structural_body(
    ctx: &mut LowerCtx<'_>,
    fields: Option<&[String]>,
    children: Vec<DictPlan>,
    value: &str,
    state: &str,
    span: Span,
) -> IrExpr {
    let (fields, children) = if let Some(labels) = fields {
        let mut parts = labels.iter().cloned().zip(children).collect::<Vec<_>>();
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        let (labels, children): (Vec<_>, Vec<_>) = parts.into_iter().unzip();
        (Some(labels), children)
    } else {
        (None, children)
    };
    let fields = fields.as_deref();
    let input = local(ctx, state, span);
    let mut state = tag(
        ctx,
        if fields.is_some() { "record" } else { "tuple" },
        fields.unwrap_or_default(),
        0,
        children.len(),
        input,
        span,
    );
    let names: Vec<_> = children
        .iter()
        .map(|_| ctx.fresh_local("__hash_part"))
        .collect();
    for (index, child) in children.into_iter().enumerate() {
        let part = if let Some(labels) = fields {
            let base = local(ctx, value, span);
            IrExpr::Field {
                id: ctx.fresh_id(None),
                base: Box::new(base),
                field: labels[index].clone(),
                span,
            }
        } else {
            local(ctx, &names[index], span)
        };
        state = component(ctx, child, part, state, span);
    }
    if fields.is_some() {
        return state;
    }
    let tuple = local(ctx, value, span);
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(tuple),
        arms: vec![IrArm {
            pat: IrPat::Tuple {
                elems: names.iter().map(|name| binding(name, span)).collect(),
                span,
            },
            when: None,
            body: state,
            span,
        }],
        span,
    }
}

pub fn derived_method(
    ctx: &mut LowerCtx<'_>,
    owner: TyConId,
    shape: &ComparisonShape,
    span: Span,
) -> (IrExpr, Vec<IrParam>) {
    let value = ctx.fresh_local("__hash_value");
    let state = ctx.fresh_local("__hash_state");
    let body = match shape {
        ComparisonShape::Record { fields } => {
            let (labels, evidence): (Vec<_>, Vec<_>) = fields.iter().cloned().unzip();
            structural_body(ctx, Some(&labels), evidence, &value, &state, span)
        }
        ComparisonShape::Alias { evidence } => {
            let input = local(ctx, &value, span);
            let state = local(ctx, &state, span);
            component(ctx, evidence.clone(), input, state, span)
        }
        ComparisonShape::Union { variants } => {
            union_body(ctx, owner, variants, &value, &state, span)
        }
    };
    (body, parameters(value, state, span))
}

fn union_body(
    ctx: &mut LowerCtx<'_>,
    owner: TyConId,
    variants: &[ComparisonVariant],
    value: &str,
    state: &str,
    span: Span,
) -> IrExpr {
    let arms = variants
        .iter()
        .enumerate()
        .map(|(rank, variant)| {
            let names: Vec<_> = variant
                .evidence
                .iter()
                .map(|_| ctx.fresh_local("__hash_payload"))
                .collect();
            let pattern = crate::comparison::variant_pattern(
                ctx.builtins,
                owner,
                variant,
                rank,
                names.iter().map(|name| binding(name, span)).collect(),
                span,
            );
            let mut parts: Vec<_> = variant
                .evidence
                .iter()
                .cloned()
                .zip(&names)
                .enumerate()
                .collect();
            if let Some(labels) = &variant.fields {
                parts.sort_by(|a, b| labels[a.0].cmp(&labels[b.0]));
            }
            let labels = parts
                .iter()
                .filter_map(|(i, _)| variant.fields.as_ref().map(|labels| labels[*i].clone()))
                .collect::<Vec<_>>();
            let input = local(ctx, state, span);
            let mut output = tag(
                ctx,
                "union",
                &labels,
                rank,
                variant.evidence.len(),
                input,
                span,
            );
            for (_, (evidence, name)) in parts {
                let part = local(ctx, name, span);
                output = component(ctx, evidence, part, output, span);
            }
            IrArm {
                pat: pattern,
                when: None,
                body: output,
                span,
            }
        })
        .collect();
    let input = local(ctx, value, span);
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(input),
        arms,
        span,
    }
}

pub fn composed_dict(
    ctx: &mut LowerCtx<'_>,
    tycon: TyConId,
    args: Vec<DictPlan>,
    span: Span,
) -> IrExpr {
    // A rejected editor program can retain a partially resolved static plan.
    // Leave a hole instead of constructing Hash evidence without its children.
    // The type error prevents this incomplete expression from being emitted.
    let required = if tycon == ctx.builtins.result { 2 } else { 1 };
    if args.len() != required {
        return IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Unit,
            span,
        };
    }
    if tycon == ctx.builtins.list {
        return list_dictionary(ctx, args, span);
    }
    let Some(workspace) = ctx.workspace else {
        unreachable!("sum hashing needs its typed schema")
    };
    let schema = match &workspace.tycons[tycon.0 as usize].kind {
        ridge_types::TyConKind::Union(schema) => schema.clone(),
        _ => unreachable!("hashing container must have a sum schema"),
    };
    let variants = schema
        .variants
        .iter()
        .map(|variant| {
            let evidence = match &variant.kind {
                ridge_types::VariantPayload::Nullary => vec![],
                ridge_types::VariantPayload::Positional(types) => types
                    .iter()
                    .map(|ty| {
                        let Type::Var(slot) = ty else {
                            unreachable!("container payload names its parameter")
                        };
                        args[slot.0 as usize].clone()
                    })
                    .collect(),
                ridge_types::VariantPayload::Record(_) => {
                    unreachable!("prelude sums have positional payloads")
                }
            };
            ComparisonVariant {
                name: variant.name.clone(),
                fields: None,
                evidence,
            }
        })
        .collect();
    let shape = ComparisonShape::Union { variants };
    let (body, params) = derived_method(ctx, tycon, &shape, span);
    dictionary(ctx, body, params, span)
}

fn list_dictionary(ctx: &mut LowerCtx<'_>, args: Vec<DictPlan>, span: Span) -> IrExpr {
    let Some(element) = args.into_iter().next() else {
        unreachable!("list hashing needs element evidence")
    };
    let element = crate::core::dict_plan_to_expr(ctx, HASH_CLASS, element, "Hash", span);
    let element_name = ctx.fresh_local("__hash_element_dictionary");
    let loop_name = ctx.fresh_local("__hash_list_loop");
    let list = ctx.fresh_local("__hash_list");
    let state = ctx.fresh_local("__hash_state");
    let head = ctx.fresh_local("__hash_head");
    let tail = ctx.fresh_local("__hash_tail");
    let input = local(ctx, &state, span);
    let empty = tag(ctx, "list.nil", &[], 0, 0, input, span);
    let input = local(ctx, &state, span);
    let input = tag(ctx, "list.cons", &[], 0, 2, input, span);
    let part = local(ctx, &head, span);
    let dictionary = local(ctx, &element_name, span);
    let callee = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dictionary),
        field: "hash".into(),
        span,
    };
    let next_state = call(ctx, callee, vec![part, input], span);
    let callee = local(ctx, &loop_name, span);
    let rest = local(ctx, &tail, span);
    let recurse = call(ctx, callee, vec![rest, next_state], span);
    let input = local(ctx, &list, span);
    let body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(input),
        arms: vec![
            IrArm {
                pat: IrPat::Nil { span },
                when: None,
                body: empty,
                span,
            },
            IrArm {
                pat: IrPat::Cons {
                    head: Box::new(binding(&head, span)),
                    tail: Box::new(binding(&tail, span)),
                    span,
                },
                when: None,
                body: recurse,
                span,
            },
        ],
        span,
    };
    let method = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params: parameters(list, state, span),
        body: Box::new(body),
        caps: CapabilitySet::PURE,
        span,
    };
    let method_value = local(ctx, &loop_name, span);
    let method = IrExpr::LetIn {
        id: ctx.fresh_id(None),
        pat: binding(&loop_name, span),
        value: Box::new(method),
        body: Box::new(method_value),
        span,
    };
    let dictionary = method_dictionary(ctx, method, span);
    IrExpr::LetIn {
        id: ctx.fresh_id(None),
        pat: binding(&element_name, span),
        value: Box::new(element),
        body: Box::new(dictionary),
        span,
    }
}
