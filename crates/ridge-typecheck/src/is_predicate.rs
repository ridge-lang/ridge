//! Checked, non-binding pattern predicates. Constructor payload elision is
//! confined to the root of `is`; ordinary and nested patterns keep their arity.

use ridge_ast::{Expr, Ident, Pattern, QualifiedName, Span};
use ridge_types::{BuiltinTyCons, TyConId, TyConKind, Type, VariantPayload};

use crate::{
    ctx::InferCtx,
    error::TypeError,
    infer::{infer_expr, infer_pattern},
};

pub fn infer(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    subject: &Expr,
    pattern: &Pattern,
    span: Span,
) -> Type {
    let subject_ty = infer_expr(ctx, b, subject);
    let mut source = pattern;
    while let Pattern::Paren { inner, .. } = source {
        source = inner;
    }
    let mut checked = source.clone();
    let root = root_pattern(&mut checked);
    if let Pattern::Constructor {
        qualifier,
        name,
        args,
        fields,
        has_rest,
        span,
    } = root
    {
        if args.is_empty() && fields.is_none() {
            if let Some((_, kind, variant)) = constructor(ctx, b, qualifier.as_ref(), name) {
                match kind {
                    TyConKind::Union(schema) => match &schema.variants[variant].kind {
                        VariantPayload::Nullary => {}
                        VariantPayload::Positional(types) => {
                            args.extend(types.iter().map(|_| Pattern::Wildcard { span: *span }));
                        }
                        VariantPayload::Record(_) => {
                            *fields = Some(vec![]);
                            *has_rest = true;
                        }
                    },
                    TyConKind::Record(_) => {
                        *fields = Some(vec![]);
                        *has_rest = true;
                    }
                    _ => {}
                }
            } else {
                ctx.errors.push(TypeError::NotAConstructor {
                    name: name.text.clone(),
                    hint:
                        "use a declared constructor or a pattern; `is` does not test runtime types"
                            .into(),
                    span: *span,
                });
            }
        }
    }
    ctx.env.push_frame();
    infer_pattern(ctx, b, &checked, &subject_ty);
    ctx.env.pop_frame();
    ctx.is_patterns.insert(span, checked);
    Type::Con(b.bool, vec![])
}

/// Infer a constructor using its resolved owner rather than its spelling.
/// Return false when it has no retained constructor identity.
pub fn infer_constructor(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    pat: &Pattern,
    expected: &Type,
) -> bool {
    let Pattern::Constructor {
        qualifier,
        name,
        fields,
        args,
        has_rest,
        span,
    } = pat
    else {
        return false;
    };
    let Some((owner, kind, index)) = constructor(ctx, b, qualifier.as_ref(), name) else {
        return false;
    };
    match (kind, fields) {
        (TyConKind::Union(schema), Some(fields)) => {
            if let VariantPayload::Record(record) = &schema.variants[index].kind {
                crate::records::infer_record_variant_pattern(
                    ctx,
                    b,
                    &schema.params,
                    record,
                    owner,
                    &name.text,
                    fields,
                    *has_rest,
                    expected,
                    *span,
                );
            } else {
                ctx.errors.push(TypeError::NotAConstructor {
                    name: name.text.clone(),
                    hint: "use positional payload patterns for this constructor".into(),
                    span: *span,
                });
            }
        }
        (TyConKind::Union(schema), None) => {
            if matches!(schema.variants[index].kind, VariantPayload::Record(_)) {
                ctx.errors.push(TypeError::NotAConstructor {
                    name: name.text.clone(),
                    hint: "use a record payload pattern with braces".into(),
                    span: *span,
                });
            } else {
                crate::unions::infer_variant_pattern(
                    ctx, b, &schema, owner, index, args, expected, *span,
                );
            }
        }
        (TyConKind::Record(schema), Some(fields)) => crate::records::infer_record_pattern(
            ctx, b, &schema, owner, &name.text, fields, *has_rest, expected, *span,
        ),
        _ => ctx.errors.push(TypeError::NotAConstructor {
            name: name.text.clone(),
            hint: "use a record pattern with braces".into(),
            span: *span,
        }),
    }
    true
}

fn root_pattern(pattern: &mut Pattern) -> &mut Pattern {
    match pattern {
        Pattern::Paren { inner, .. } => root_pattern(inner),
        _ => pattern,
    }
}

/// Resolve the type of the constructor through the same imported environment
/// used for expressions, rather than picking an unrelated same-spelled variant.
pub fn constructor(
    ctx: &InferCtx,
    b: &BuiltinTyCons,
    qualifier: Option<&QualifiedName>,
    name: &Ident,
) -> Option<(TyConId, TyConKind, usize)> {
    if let Some(&(owner, variant)) = ctx.resolved_constructors.get(&name.span) {
        return Some((
            owner,
            ctx.tycon_decls.get(owner.0 as usize)?.kind.clone(),
            variant,
        ));
    }
    let key = qualifier.map_or_else(
        || name.text.clone(),
        |q| {
            q.segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(".")
        },
    );
    let owner = ctx
        .env
        .lookup(&key)
        .and_then(|scheme| {
            let ret = match &scheme.ty {
                Type::Fn { ret, .. } => ret.as_ref(),
                ty => ty,
            };
            if let Type::Con(owner, _) = ret {
                Some(*owner)
            } else {
                None
            }
        })
        .or_else(|| ctx.user_tycon_names.get(&key).copied())
        .or_else(|| {
            qualifier
                .is_none()
                .then(|| crate::unions::resolve_prelude_ctor(b, &name.text).map(|(owner, _)| owner))
                .flatten()
        })?;
    let kind = ctx.tycon_decls.get(owner.0 as usize)?.kind.clone();
    match &kind {
        TyConKind::Union(schema) => {
            let index = schema.variants.iter().position(|v| v.name == name.text)?;
            Some((owner, kind, index))
        }
        TyConKind::Record(_) => Some((owner, kind, 0)),
        _ => None,
    }
}

pub fn resolved_constructors(
    b: &BuiltinTyCons,
    nodes: Option<&ridge_resolve::NodeIdMap>,
    bindings: &ridge_resolve::BindingMap,
    symbols: &[&ridge_resolve::SymbolTable],
    tycons: &[ridge_types::TyConDecl],
    origins: &crate::cross_module::TyConOrigins,
) -> rustc_hash::FxHashMap<Span, (TyConId, usize)> {
    use ridge_resolve::{imports::Binding, NodeKind};
    let mut result = rustc_hash::FxHashMap::default();
    let owners: rustc_hash::FxHashMap<_, _> = tycons
        .iter()
        .filter_map(|decl| {
            Some((
                (
                    origins.declaring_module(decl.id)?,
                    origins.type_name(decl.id),
                ),
                decl.id,
            ))
        })
        .collect();
    if let Some(nodes) = nodes {
        for (span, kind, id) in nodes.iter() {
            if kind != NodeKind::Ident {
                continue;
            }
            if let Some(Some(Binding::StdlibSymbol { module, name })) = bindings.get(id.0 as usize)
            {
                let manifest = &ridge_resolve::stdlib_builtin::BUILTINS[module.0 as usize];
                let constructor = manifest
                    .declared_types
                    .iter()
                    .find_map(|type_name| {
                        let owner = origins.resolve_compiler_name(type_name)?;
                        match &tycons.get(owner.0 as usize)?.kind {
                            TyConKind::Union(schema) => Some((
                                owner,
                                schema
                                    .variants
                                    .iter()
                                    .position(|variant| &variant.name == name)?,
                            )),
                            TyConKind::Record(_) if type_name == name => Some((owner, 0)),
                            _ => None,
                        }
                    })
                    .or_else(|| crate::unions::resolve_prelude_ctor(b, name));
                if let Some(constructor) = constructor {
                    result.insert(span, constructor);
                }
            }
            if let Some(Some(Binding::Constructor {
                owner_type,
                owner_module,
                variant,
                is_record,
            })) = bindings.get(id.0 as usize)
            {
                if let Some(symbol) = symbols
                    .get(owner_module.0 as usize)
                    .and_then(|table| table.entries.get(owner_type.0 as usize))
                {
                    if let Some(&owner) = owners.get(&(owner_module.0, symbol.name.clone())) {
                        // The resolver can provisionally classify a type used
                        // with braces as a record constructor. Only a real
                        // record schema may confirm that classification.
                        let valid = match &tycons[owner.0 as usize].kind {
                            TyConKind::Record(_) => *is_record,
                            TyConKind::Union(schema) => {
                                !*is_record && schema.variants.get(*variant as usize).is_some()
                            }
                            _ => false,
                        };
                        if valid {
                            result.insert(span, (owner, *variant as usize));
                        }
                    }
                }
            }
        }
    }
    result
}
