//! Eq, Ord and Hash derivation uses the ordinary solver for every component.

use ridge_ast::{Constructor, Span, Type as AstType, TypeBody};
use ridge_types::{BuiltinTyCons, Constraint, TyConDecl, TyConId, TyVid, Type};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::class_env::{ClassTable, InstanceEnv};
use crate::ctx::{InferCtx, TyValue};
use crate::derive::{DerivedInstance, DerivedMethodBody};
use crate::error::{TypeDesc, TypeError};
use crate::solve::{solve_constraints, DictPlan};

/// A value-contract body whose component evidence has already been checked.
#[derive(Debug, Clone)]
pub enum ComparisonShape {
    /// Compare record fields in declaration order.
    Record {
        /// Declared labels, paired with their dictionaries.
        fields: Vec<(String, DictPlan)>,
    },
    /// Compare declaration ranks, then same-variant payloads.
    Union {
        /// Variants in source order.
        variants: Vec<ComparisonVariant>,
    },
    /// Delegate an alias to its expanded type's instance.
    Alias {
        /// Evidence for the expanded type.
        evidence: DictPlan,
    },
}

/// One derived comparison variant and its payload evidence.
#[derive(Debug, Clone)]
pub struct ComparisonVariant {
    /// Source constructor name.
    pub name: String,
    /// Record labels, or `None` for positional payloads.
    pub fields: Option<Vec<String>>,
    /// Payload dictionaries in declaration order.
    pub evidence: Vec<DictPlan>,
}

/// Workspace inputs shared by all derived comparison proofs.
pub struct ComparisonContext<'a> {
    pub templates: &'a crate::type_template::TypeTemplates<'a>,
    pub rows_tycons: Option<crate::ctx::RowsTycons>,
    pub scopes: &'a [FxHashMap<String, TyConId>],
    pub tycons: &'a [TyConDecl],
    pub classes: &'a ClassTable,
    pub builtins: &'a BuiltinTyCons,
    pub origins: &'a crate::cross_module::TyConOrigins,
}

struct Proof {
    requirements: Vec<Constraint>,
    positions: Vec<Vec<usize>>,
    plans: Vec<DictPlan>,
    errors: Vec<TypeError>,
    rows: FxHashSet<TyConId>,
}

fn requirement_positions(requirements: &mut Vec<Constraint>) -> Vec<Vec<usize>> {
    requirements.sort_by_key(|constraint| {
        (
            constraint.class.0,
            constraint.tys.iter().map(|var| var.0).collect::<Vec<_>>(),
        )
    });
    requirements.dedup();
    requirements
        .iter()
        .map(|constraint| constraint.tys.iter().map(|var| var.0 as usize).collect())
        .collect()
}

fn canonical_constraint(
    constraint: &Constraint,
    variables: &FxHashMap<TyVid, TyVid>,
) -> Option<Constraint> {
    Some(Constraint::new(
        constraint.class,
        constraint
            .tys
            .iter()
            .map(|var| variables.get(var).copied())
            .collect::<Option<_>>()?,
    ))
}

fn canonical_plan(plan: &mut DictPlan, variables: &FxHashMap<TyVid, TyVid>) -> Option<()> {
    match plan {
        DictPlan::Forward(constraint) => *constraint = canonical_constraint(constraint, variables)?,
        DictPlan::Static { args, .. } | DictPlan::Structural { args, .. } => {
            for arg in args {
                canonical_plan(arg, variables)?;
            }
        }
    }
    Some(())
}

fn requested_plan(
    ctx: &mut InferCtx,
    resolutions: &crate::solve::DictResolution,
    request: &Constraint,
) -> Option<DictPlan> {
    let canonical = match ctx.deep_resolve(&Type::Var(request.sole_ty())) {
        Type::Var(var) => Some(Constraint::single(request.class, var)),
        _ => None,
    };
    resolutions
        .get(request)
        .or_else(|| canonical.as_ref().and_then(|key| resolutions.get(key)))
        .cloned()
}

fn fields(body: &TypeBody) -> Vec<&AstType> {
    match body {
        TypeBody::Record(body) => body.fields.iter().map(|field| &field.ty).collect(),
        TypeBody::Union(body) => body
            .alternatives
            .iter()
            .flat_map(|variant| match variant {
                Constructor::Positional { args, .. } => args.iter().collect::<Vec<_>>(),
                Constructor::Record { body, .. } => {
                    body.fields.iter().map(|field| &field.ty).collect()
                }
            })
            .collect(),
        TypeBody::Alias(body) => vec![body],
    }
}

fn proof_error(class: &str, name: &str, span: Span) -> TypeError {
    TypeError::NoInstance {
        class: class.into(), ty: TypeDesc::Text(name.into()), span,
        fix_hint: "every component needs supported instance evidence; write an explicit instance when its contract differs from fieldwise derivation".into(),
    }
}

fn reset_proof_context(ctx: &mut InferCtx, context: &ComparisonContext<'_>, module: u32) {
    let catalogue = std::mem::take(&mut ctx.tycon_decls);
    let origins = std::mem::take(&mut ctx.tycon_origins);
    *ctx = InferCtx::new(context.builtins);
    ctx.tycon_decls = catalogue;
    ctx.tycon_origins = origins;
    ctx.current_module_raw = Some(module);
    ctx.rows_tycons = context.rows_tycons;
    ctx.user_tycon_names
        .clone_from(&context.scopes[module as usize]);
}

fn prove(
    context: &ComparisonContext<'_>,
    instances: &InstanceEnv,
    derived: &DerivedInstance,
    ctx: &mut InferCtx,
) -> Proof {
    let module = derived.instance_info.def_module.unwrap_or(u32::MAX);
    let class = derived.key.0;
    let class_name = context
        .classes
        .get(class)
        .map_or("?", |info| info.name.as_str());
    let mut result = Proof {
        requirements: vec![],
        positions: vec![],
        plans: vec![],
        errors: vec![],
        rows: FxHashSet::default(),
    };
    let Some(decl) = context.templates.declaration(module, derived.key.1) else {
        return result;
    };
    reset_proof_context(ctx, context, module);
    let parameters: Vec<_> = decl.params.iter().map(|_| ctx.fresh_tyvid()).collect();
    let params = decl
        .params
        .iter()
        .zip(&parameters)
        .map(|(name, &var)| (name.text.clone(), Type::Var(var)))
        .collect();
    let mut requests = Vec::new();
    for field in fields(&decl.body) {
        let Some(ty) = context
            .templates
            .convert(module, field, &params, &mut FxHashSet::default())
        else {
            result
                .errors
                .push(proof_error(class_name, &decl.name.text, field.span()));
            return result;
        };
        let key = ctx.tyvids.new_key(TyValue(Some(ty)));
        let request = Constraint::single(class, TyVid(key.0));
        requests.push(request.clone());
        ctx.deferred_constraints.push(request);
    }
    let (requirements, resolutions) = solve_constraints(
        ctx,
        instances,
        context.classes,
        &FxHashSet::default(),
        derived.instance_info.span,
        Some(context.builtins),
    );
    let variables: FxHashMap<_, _> = parameters
        .iter()
        .enumerate()
        .filter_map(|(index, var)| {
            let Type::Var(root) = ctx.deep_resolve(&Type::Var(*var)) else {
                return None;
            };
            Some((root, TyVid(u32::try_from(index).ok()?)))
        })
        .collect();
    if variables.len() != parameters.len() {
        result.errors.push(proof_error(
            class_name,
            &decl.name.text,
            derived.instance_info.span,
        ));
        return result;
    }
    for requirement in requirements {
        let Some(requirement) = canonical_constraint(&requirement, &variables) else {
            result.errors.push(proof_error(
                class_name,
                &decl.name.text,
                derived.instance_info.span,
            ));
            return result;
        };
        result.requirements.push(requirement);
    }
    result.positions = requirement_positions(&mut result.requirements);
    for request in requests {
        if let Some(mut plan) = requested_plan(ctx, &resolutions, &request) {
            if canonical_plan(&mut plan, &variables).is_some() {
                result.plans.push(plan);
            } else {
                result.errors.push(proof_error(
                    class_name,
                    &decl.name.text,
                    derived.instance_info.span,
                ));
            }
        }
    }
    result.errors.append(&mut ctx.errors);
    result.rows = std::mem::take(&mut ctx.demanded_rows);
    result
}

fn shape(body: &TypeBody, plans: Vec<DictPlan>) -> Option<ComparisonShape> {
    let mut plans = plans.into_iter();
    match body {
        TypeBody::Record(body) => Some(ComparisonShape::Record {
            fields: body
                .fields
                .iter()
                .map(|field| Some((field.name.text.clone(), plans.next()?)))
                .collect::<Option<_>>()?,
        }),
        TypeBody::Union(body) => Some(ComparisonShape::Union {
            variants: body
                .alternatives
                .iter()
                .map(|variant| {
                    let (name, fields, count) = match variant {
                        Constructor::Positional { name, args, .. } => (name, None, args.len()),
                        Constructor::Record { name, body, .. } => (
                            name,
                            Some(
                                body.fields
                                    .iter()
                                    .map(|field| field.name.text.clone())
                                    .collect(),
                            ),
                            body.fields.len(),
                        ),
                    };
                    Some(ComparisonVariant {
                        name: name.text.clone(),
                        fields,
                        evidence: (0..count).map(|_| plans.next()).collect::<Option<_>>()?,
                    })
                })
                .collect::<Option<_>>()?,
        }),
        TypeBody::Alias(_) => Some(ComparisonShape::Alias {
            evidence: plans.next()?,
        }),
    }
}

/// Final derivation diagnostics and the registered implicit dictionaries used.
pub struct ComparisonResult {
    pub errors: Vec<(ridge_resolve::ModuleId, TypeError)>,
    pub rows: FxHashSet<TyConId>,
}

/// Fieldwise contracts cannot silently disagree with handwritten equality.
fn has_manual_eq(
    instances: &InstanceEnv,
    derived: &[DerivedInstance],
    item: &DerivedInstance,
) -> bool {
    matches!(item.key.0, ridge_types::ORD_CLASS | ridge_types::HASH_CLASS)
        && instances.get((ridge_types::EQ_CLASS, item.key.1)).is_some()
        && !derived
            .iter()
            .any(|other| other.key == (ridge_types::EQ_CLASS, item.key.1))
}

/// Infer recursive derived contexts, then retain the final component plans.
pub fn finish_comparisons(
    context: &ComparisonContext<'_>,
    instances: &mut InstanceEnv,
    derived: &mut [DerivedInstance],
) -> ComparisonResult {
    let mut ctx = InferCtx::new(context.builtins);
    ctx.tycon_decls = context.tycons.to_vec();
    ctx.tycon_origins = context.origins.clone();
    let comparisons: Vec<_> = derived
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            matches!(
                item.key.0,
                ridge_types::EQ_CLASS | ridge_types::ORD_CLASS | ridge_types::HASH_CLASS
            )
        })
        .map(|(index, _)| index)
        .collect();
    loop {
        let mut changed = false;
        for &index in &comparisons {
            let mut proof = prove(context, instances, &derived[index], &mut ctx);
            let info = &mut derived[index].instance_info;
            proof
                .requirements
                .extend(info.ctx_constraints.iter().cloned());
            proof.positions = requirement_positions(&mut proof.requirements);
            if info.ctx_constraints != proof.requirements {
                info.ctx_constraints = proof.requirements;
                info.head_var_positions = proof
                    .positions
                    .iter()
                    .filter_map(|positions| positions.first().copied())
                    .collect();
                let module = info.def_module.unwrap_or(u32::MAX);
                instances
                    .context_arg_positions
                    .insert((module, info.span), proof.positions);
                if let Some(stored) = instances.instances.get_mut(&(
                    derived[index].key.0,
                    smallvec::smallvec![derived[index].key.1],
                )) {
                    *stored = info.clone();
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut errors = Vec::new();
    let mut rows = FxHashSet::default();
    for &index in &comparisons {
        let proof = prove(context, instances, &derived[index], &mut ctx);
        let module = derived[index].instance_info.def_module.unwrap_or(u32::MAX);
        let Some(decl) = context.templates.declaration(module, derived[index].key.1) else {
            errors.push((
                ridge_resolve::ModuleId(module),
                proof_error(
                    "comparison",
                    "source declaration",
                    derived[index].instance_info.span,
                ),
            ));
            continue;
        };
        let manual_eq = has_manual_eq(instances, derived, &derived[index]);
        if manual_eq {
            let class = context
                .classes
                .get(derived[index].key.0)
                .map_or("?", |info| info.name.as_str());
            errors.push((ridge_resolve::ModuleId(module), TypeError::NoInstance {
                class: class.into(), ty: TypeDesc::Text(decl.name.text.clone()), span: derived[index].instance_info.span,
                fix_hint: format!("this type has a manual Eq instance; write a matching manual {class} instance instead of deriving it fieldwise"),
            }));
        }
        if proof.errors.is_empty() && !manual_eq {
            rows.extend(proof.rows);
            if let Some(shape) = shape(&decl.body, proof.plans) {
                derived[index].method_body = DerivedMethodBody::DerivedComparison { shape };
            } else {
                errors.push((
                    ridge_resolve::ModuleId(module),
                    proof_error(
                        "comparison",
                        &decl.name.text,
                        derived[index].instance_info.span,
                    ),
                ));
            }
        }
        errors.extend(
            proof
                .errors
                .into_iter()
                .map(|error| (ridge_resolve::ModuleId(module), error)),
        );
    }
    ComparisonResult { errors, rows }
}
