//! Typed composition of comparison dictionaries in backend-neutral IR.

use ridge_ast::Span;
use ridge_ir::{CtorKind, IrArm, IrExpr, IrLit, IrParam, IrPat, SymbolRef};
use ridge_types::{CapabilitySet, ClassId, Type, EQ_CLASS};

use crate::ctx::LowerCtx;

/// Compose an `Eq` or `Ord` dictionary from its component evidence. Tuple
/// positions retain their order; closed record labels arrive sorted by type.
pub fn structural_dict(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    fields: Option<&[String]>,
    children: Vec<ridge_typecheck::DictPlan>,
    span: Span,
) -> IrExpr {
    if class == ridge_types::HASH_CLASS {
        return crate::hash_compound::structural_dict(ctx, fields, children, span);
    }
    let (body, params) = structural_method(ctx, class, fields, children, span);
    let method = if class == EQ_CLASS { "eq" } else { "compare" };
    method_dictionary(ctx, method, params, body, span)
}

fn structural_method(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    fields: Option<&[String]>,
    children: Vec<ridge_typecheck::DictPlan>,
    span: Span,
) -> (IrExpr, Vec<IrParam>) {
    let equality = class == EQ_CLASS;
    let class_name = if equality { "Eq" } else { "Ord" };
    let children: Vec<_> = children
        .into_iter()
        .map(|plan| crate::core::dict_plan_to_expr(ctx, class, plan, class_name, span))
        .collect();
    let method = if equality { "eq" } else { "compare" };
    let left = ctx.fresh_local("__comparison_left");
    let right = ctx.fresh_local("__comparison_right");
    let pairs: Vec<_> = children
        .iter()
        .map(|_| {
            (
                ctx.fresh_local("__comparison_a"),
                ctx.fresh_local("__comparison_b"),
            )
        })
        .collect();
    let mut body = if equality {
        boolean(ctx, true, span)
    } else {
        IrExpr::Construct {
            id: ctx.fresh_id(None),
            ctor: equal_constructor(ctx),
            fields: vec![],
            span,
        }
    };
    for (index, child) in children.into_iter().enumerate().rev() {
        let (a, b) = if let Some(names) = fields {
            let a = local(ctx, &left, span);
            let b = local(ctx, &right, span);
            (
                field(ctx, a, &names[index], span),
                field(ctx, b, &names[index], span),
            )
        } else {
            (
                local(ctx, &pairs[index].0, span),
                local(ctx, &pairs[index].1, span),
            )
        };
        let callee = field(ctx, child, method, span);
        let compared = IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(callee),
            args: vec![a, b],
            span,
        };
        let (continue_pat, stop_pat, stop) = if equality {
            (
                IrPat::Lit {
                    value: IrLit::Bool(true),
                    span,
                },
                IrPat::Wild { span },
                boolean(ctx, false, span),
            )
        } else {
            let result = ctx.fresh_local("__comparison_result");
            (
                IrPat::Ctor {
                    sym: equal_constructor(ctx),
                    fields: vec![],
                    args: vec![],
                    span,
                },
                binding(&result, span),
                local(ctx, &result, span),
            )
        };
        body = IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(compared),
            arms: vec![arm(continue_pat, body, span), arm(stop_pat, stop, span)],
            span,
        };
    }
    if fields.is_none() {
        let right_pattern = IrPat::Tuple {
            elems: pairs.iter().map(|(_, b)| binding(b, span)).collect(),
            span,
        };
        body = bind_operand(ctx, &right, right_pattern, body, span);
        let left_pattern = IrPat::Tuple {
            elems: pairs.iter().map(|(a, _)| binding(a, span)).collect(),
            span,
        };
        body = bind_operand(ctx, &left, left_pattern, body, span);
    }
    (body, parameters(left, right, span))
}

fn bind_operand(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    pat: IrPat,
    body: IrExpr,
    span: Span,
) -> IrExpr {
    let value = local(ctx, name, span);
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(value),
        arms: vec![arm(pat, body, span)],
        span,
    }
}

pub fn method_dictionary(
    ctx: &mut LowerCtx<'_>,
    method: &str,
    params: Vec<IrParam>,
    body: IrExpr,
    span: Span,
) -> IrExpr {
    let method_fn = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params,
        body: Box::new(body),
        caps: CapabilitySet::PURE,
        span,
    };
    dictionary(ctx, method, method_fn, span)
}

fn dictionary(ctx: &mut LowerCtx<'_>, method: &str, method_fn: IrExpr, span: Span) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: None,
            name: format!("$synth_comparison_{method}"),
            variant: 0,
        },
        fields: vec![(method.into(), method_fn)],
        span,
    }
}

/// Prelude comparisons composed from element evidence or declared variant ranks.
pub fn is_composed(
    b: &ridge_types::BuiltinTyCons,
    class: ClassId,
    tycon: ridge_types::TyConId,
) -> bool {
    ((class == EQ_CLASS || class == ridge_types::ORD_CLASS || class == ridge_types::HASH_CLASS)
        && [b.list, b.option, b.result].contains(&tycon))
        || (class == ridge_types::ORD_CLASS && tycon == b.ordering)
        || (class == EQ_CLASS && [b.map, b.set].contains(&tycon))
}

/// Synthesize comparisons from checked plans and the canonical sum schema.
pub fn composed_dict(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    tycon: ridge_types::TyConId,
    args: Vec<ridge_typecheck::DictPlan>,
    span: Span,
) -> IrExpr {
    use ridge_typecheck::derive::{ComparisonShape, ComparisonVariant};
    if class == EQ_CLASS && [ctx.builtins.map, ctx.builtins.set].contains(&tycon) {
        return crate::collection::equality_dictionary(ctx, tycon, args, span);
    }
    if class == ridge_types::HASH_CLASS {
        return crate::hash_compound::composed_dict(ctx, tycon, args, span);
    }
    if tycon == ctx.builtins.list {
        return list_dict(ctx, class, args, span);
    }
    let Some(workspace) = ctx.workspace else {
        unreachable!("container comparison needs its typed constructor schema")
    };
    let ridge_types::TyConKind::Union(schema) = &workspace.tycons[tycon.0 as usize].kind else {
        unreachable!("comparison sum must have a union schema")
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
                            unreachable!("prelude comparison payload must name its parameter")
                        };
                        args[slot.0 as usize].clone()
                    })
                    .collect(),
                ridge_types::VariantPayload::Record(_) => {
                    unreachable!("prelude comparison sums have positional payloads")
                }
            };
            ComparisonVariant {
                name: variant.name.clone(),
                fields: None,
                evidence,
            }
        })
        .collect();
    let (body, params) = derived_method(
        ctx,
        class,
        tycon,
        &ComparisonShape::Union { variants },
        span,
    );
    method_dictionary(
        ctx,
        if class == EQ_CLASS { "eq" } else { "compare" },
        params,
        body,
        span,
    )
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

fn list_dict(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    args: Vec<ridge_typecheck::DictPlan>,
    span: Span,
) -> IrExpr {
    let mut args = args.into_iter();
    let Some(element) = args.next() else {
        unreachable!("list comparison requires element evidence")
    };
    let element_name = ctx.fresh_local("__list_element_dictionary");
    let class_name = if class == EQ_CLASS { "Eq" } else { "Ord" };
    let element = crate::core::dict_plan_to_expr(ctx, class, element, class_name, span);
    let loop_name = ctx.fresh_local("__list_compare_loop");
    let left = ctx.fresh_local("__list_left");
    let right = ctx.fresh_local("__list_right");
    let body = list_body(ctx, class, &element_name, &loop_name, &left, &right, span);
    let method = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params: parameters(left, right, span),
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
    let dictionary = dictionary(
        ctx,
        if class == EQ_CLASS { "eq" } else { "compare" },
        method,
        span,
    );
    IrExpr::LetIn {
        id: ctx.fresh_id(None),
        pat: binding(&element_name, span),
        value: Box::new(element),
        body: Box::new(dictionary),
        span,
    }
}

fn list_body(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    element: &str,
    loop_name: &str,
    left: &str,
    right: &str,
    span: Span,
) -> IrExpr {
    let equal = if class == EQ_CLASS {
        boolean(ctx, true, span)
    } else {
        ordering(ctx, "Equal", 1, span)
    };
    let less = if class == EQ_CLASS {
        boolean(ctx, false, span)
    } else {
        ordering(ctx, "Less", 0, span)
    };
    let greater = if class == EQ_CLASS {
        boolean(ctx, false, span)
    } else {
        ordering(ctx, "Greater", 2, span)
    };
    let a_head = ctx.fresh_local("__list_a_head");
    let a_tail = ctx.fresh_local("__list_a_tail");
    let b_head = ctx.fresh_local("__list_b_head");
    let b_tail = ctx.fresh_local("__list_b_tail");
    let cons = |head: &str, tail: &str| IrPat::Cons {
        head: Box::new(binding(head, span)),
        tail: Box::new(binding(tail, span)),
        span,
    };
    let pair = |a, b| IrPat::Tuple {
        elems: vec![a, b],
        span,
    };
    let method = local(ctx, element, span);
    let callee = field(
        ctx,
        method,
        if class == EQ_CLASS { "eq" } else { "compare" },
        span,
    );
    let a = local(ctx, &a_head, span);
    let b = local(ctx, &b_head, span);
    let compared = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(callee),
        args: vec![a, b],
        span,
    };
    let callee = local(ctx, loop_name, span);
    let a = local(ctx, &a_tail, span);
    let b = local(ctx, &b_tail, span);
    let recurse = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(callee),
        args: vec![a, b],
        span,
    };
    let continue_pat = if class == EQ_CLASS {
        IrPat::Lit {
            value: IrLit::Bool(true),
            span,
        }
    } else {
        IrPat::Ctor {
            sym: equal_constructor(ctx),
            fields: vec![],
            args: vec![],
            span,
        }
    };
    let result = ctx.fresh_local("__list_ordering");
    let (stop_pat, stop) = if class == EQ_CLASS {
        (IrPat::Wild { span }, boolean(ctx, false, span))
    } else {
        (binding(&result, span), local(ctx, &result, span))
    };
    let head_result = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(compared),
        arms: vec![arm(continue_pat, recurse, span), arm(stop_pat, stop, span)],
        span,
    };
    let arms = vec![
        arm(pair(IrPat::Nil { span }, IrPat::Nil { span }), equal, span),
        arm(pair(IrPat::Nil { span }, IrPat::Wild { span }), less, span),
        arm(
            pair(IrPat::Wild { span }, IrPat::Nil { span }),
            greater,
            span,
        ),
        arm(
            pair(cons(&a_head, &a_tail), cons(&b_head, &b_tail)),
            head_result,
            span,
        ),
    ];
    let scrutinee = tuple(ctx, &[left.into(), right.into()], span);
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(scrutinee),
        arms,
        span,
    }
}

fn parameters(left: String, right: String, span: Span) -> Vec<IrParam> {
    [left, right]
        .into_iter()
        .map(|name| IrParam {
            name,
            ty: Type::Error,
            span,
        })
        .collect()
}

/// Lower the already checked field plans of a derived comparison.
pub fn derived_method(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    owner: ridge_types::TyConId,
    shape: &ridge_typecheck::derive::ComparisonShape,
    span: Span,
) -> (IrExpr, Vec<IrParam>) {
    use ridge_typecheck::derive::ComparisonShape;
    if let ComparisonShape::Record { fields } = shape {
        let (names, plans): (Vec<_>, Vec<_>) = fields.iter().cloned().unzip();
        return structural_method(ctx, class, Some(&names), plans, span);
    }
    let left = ctx.fresh_local("__derived_left");
    let right = ctx.fresh_local("__derived_right");
    let body = match shape {
        ComparisonShape::Alias { evidence } => {
            let a = local(ctx, &left, span);
            let b = local(ctx, &right, span);
            comparison_call(ctx, class, evidence.clone(), a, b, span)
        }
        ComparisonShape::Union { variants } => {
            union_body(ctx, class, owner, variants, &left, &right, span)
        }
        ComparisonShape::Record { .. } => unreachable!("record comparison handled above"),
    };
    (body, parameters(left, right, span))
}

fn comparison_call(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    plan: ridge_typecheck::DictPlan,
    left: IrExpr,
    right: IrExpr,
    span: Span,
) -> IrExpr {
    let class_name = if class == EQ_CLASS { "Eq" } else { "Ord" };
    let dictionary = crate::core::dict_plan_to_expr(ctx, class, plan, class_name, span);
    let callee = field(
        ctx,
        dictionary,
        if class == EQ_CLASS { "eq" } else { "compare" },
        span,
    );
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(callee),
        args: vec![left, right],
        span,
    }
}

pub fn variant_pattern(
    builtins: &ridge_types::BuiltinTyCons,
    owner: ridge_types::TyConId,
    variant: &ridge_typecheck::derive::ComparisonVariant,
    index: usize,
    binds: Vec<IrPat>,
    span: Span,
) -> IrPat {
    let symbol = if owner == builtins.option || owner == builtins.result {
        SymbolRef::Prelude {
            name: variant.name.clone(),
        }
    } else {
        SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: Some(owner),
            name: variant.name.clone(),
            variant: u32::try_from(index).unwrap_or(u32::MAX),
        }
    };
    let args = if let Some(names) = &variant.fields {
        vec![IrPat::Ctor {
            sym: SymbolRef::Constructor {
                ctor_kind: CtorKind::Record,
                owner_type: Some(owner),
                name: variant.name.clone(),
                variant: 0,
            },
            fields: names.iter().cloned().zip(binds).collect(),
            args: vec![],
            span,
        }]
    } else {
        binds
    };
    IrPat::Ctor {
        sym: symbol,
        args,
        fields: vec![],
        span,
    }
}

fn rank(
    ctx: &mut LowerCtx<'_>,
    owner: ridge_types::TyConId,
    variants: &[ridge_typecheck::derive::ComparisonVariant],
    name: &str,
    span: Span,
) -> IrExpr {
    let scrutinee = local(ctx, name, span);
    let arms = variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let binds = variant
                .evidence
                .iter()
                .map(|_| IrPat::Wild { span })
                .collect();
            let pat = variant_pattern(ctx.builtins, owner, variant, index, binds, span);
            let body = IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Int(i64::try_from(index).unwrap_or(i64::MAX)),
                span,
            };
            arm(pat, body, span)
        })
        .collect();
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(scrutinee),
        arms,
        span,
    }
}

fn tuple(ctx: &mut LowerCtx<'_>, names: &[String], span: Span) -> IrExpr {
    IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: names.iter().map(|name| local(ctx, name, span)).collect(),
        span,
    }
}

fn union_body(
    ctx: &mut LowerCtx<'_>,
    class: ClassId,
    owner: ridge_types::TyConId,
    variants: &[ridge_typecheck::derive::ComparisonVariant],
    left: &str,
    right: &str,
    span: Span,
) -> IrExpr {
    let mut arms = Vec::new();
    for (index, variant) in variants.iter().enumerate() {
        let a: Vec<_> = variant
            .evidence
            .iter()
            .map(|_| ctx.fresh_local("__derived_a"))
            .collect();
        let b: Vec<_> = variant
            .evidence
            .iter()
            .map(|_| ctx.fresh_local("__derived_b"))
            .collect();
        let a_pattern = variant_pattern(
            ctx.builtins,
            owner,
            variant,
            index,
            a.iter().map(|name| binding(name, span)).collect(),
            span,
        );
        let b_pattern = variant_pattern(
            ctx.builtins,
            owner,
            variant,
            index,
            b.iter().map(|name| binding(name, span)).collect(),
            span,
        );
        let (body, params) = structural_method(ctx, class, None, variant.evidence.clone(), span);
        let callee = IrExpr::Lambda {
            id: ctx.fresh_id(None),
            params,
            body: Box::new(body),
            caps: CapabilitySet::PURE,
            span,
        };
        let a_tuple = tuple(ctx, &a, span);
        let b_tuple = tuple(ctx, &b, span);
        let compared = IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(callee),
            args: vec![a_tuple, b_tuple],
            span,
        };
        arms.push(arm(
            IrPat::Tuple {
                elems: vec![a_pattern, b_pattern],
                span,
            },
            compared,
            span,
        ));
    }
    let fallback = if class == EQ_CLASS {
        boolean(ctx, false, span)
    } else {
        let a_rank = rank(ctx, owner, variants, left, span);
        let b_rank = rank(ctx, owner, variants, right, span);
        let dictionary = crate::prelude_dict::synth_ord_dict(ctx, span);
        let callee = field(ctx, dictionary, "compare", span);
        IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(callee),
            args: vec![a_rank, b_rank],
            span,
        }
    };
    arms.push(arm(IrPat::Wild { span }, fallback, span));
    let scrutinee = tuple(ctx, &[left.into(), right.into()], span);
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(scrutinee),
        arms,
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

fn equal_constructor(ctx: &LowerCtx<'_>) -> SymbolRef {
    SymbolRef::Constructor {
        ctor_kind: CtorKind::UnionVariant,
        owner_type: Some(ctx.builtins.ordering),
        name: "Equal".into(),
        variant: 1,
    }
}

fn field(ctx: &mut LowerCtx<'_>, base: IrExpr, name: &str, span: Span) -> IrExpr {
    IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(base),
        field: name.into(),
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

fn binding(name: &str, span: Span) -> IrPat {
    IrPat::Bind {
        name: name.into(),
        inner: None,
        span,
    }
}

const fn arm(pat: IrPat, body: IrExpr, span: Span) -> IrArm {
    IrArm {
        pat,
        when: None,
        body,
        span,
    }
}
