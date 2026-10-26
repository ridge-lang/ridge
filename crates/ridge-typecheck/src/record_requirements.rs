//! Deferred field obligations for receivers whose nominal type is not known yet.

use ridge_ast::{Ident, Span};
use ridge_types::{RecordRequirement, TyVid, Type};
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

use crate::{ctx::InferCtx, instantiate::collect_free_vars, records, unify::unify};

pub struct Site {
    pub(crate) requirement: RecordRequirement,
    span: Span,
}

pub fn remember(ctx: &mut InferCtx, requirement: RecordRequirement, span: Span) {
    ctx.record_requirements.push(Site { requirement, span });
}

/// Resolve concrete receivers and equate repeated selections before exporting
/// any schemes. A fixed point also handles chains such as `record.inner.value`.
pub fn solve(ctx: &mut InferCtx) {
    if ctx.record_requirements.is_empty() {
        return;
    }
    let builtins = ctx.builtins;
    let mut tycons = None;
    loop {
        let pending = std::mem::take(&mut ctx.record_requirements);
        let mut fields = FxHashMap::default();
        let mut visibility = FxHashSet::default();
        let mut progress = false;
        for mut site in pending {
            let receiver = ctx.shallow_resolve(&site.requirement.receiver);
            if let Type::Var(var) = receiver {
                let name = site
                    .requirement
                    .field
                    .as_ref()
                    .map(|(name, _)| name.clone());
                let key = (var, name);
                if let Some((_, ty)) = &site.requirement.field {
                    if let Some(previous) = fields.get(&key) {
                        if let Err(error) = unify(ctx, previous, ty) {
                            ctx.errors.push(records::attach_span_pub(error, site.span));
                        }
                    } else {
                        fields.insert(key.clone(), ty.clone());
                    }
                }
                if visibility.insert((key, site.requirement.access_module)) {
                    site.requirement.receiver = receiver;
                    ctx.record_requirements.push(site);
                } else {
                    progress = true;
                }
                continue;
            }
            progress = true;
            let tycons = tycons.get_or_insert_with(|| ctx.tycon_decls.clone());
            let module = ctx.current_module_raw;
            ctx.current_module_raw = site.requirement.access_module;
            if let Some((name, expected)) = &site.requirement.field {
                let field = Ident {
                    text: name.clone(),
                    span: site.span,
                };
                let ty = records::infer_field_access(
                    ctx, &builtins, &receiver, &field, site.span, tycons,
                );
                if let Err(error) = unify(ctx, &ty, expected) {
                    ctx.errors.push(records::attach_span_pub(error, site.span));
                }
            } else {
                records::infer_record_with(ctx, &builtins, &receiver, &[], site.span, tycons);
            }
            ctx.current_module_raw = module;
        }
        if !progress {
            break;
        }
    }
}

/// Follow both ends of each obligation. In particular, a local value selected
/// from an enclosing receiver cannot generalise its field independently.
pub fn connected_types(ctx: &mut InferCtx, roots: &FxHashSet<TyVid>) -> Vec<Type> {
    if ctx.record_requirements.is_empty() || roots.is_empty() {
        return Vec::new();
    }
    let roots: FxHashSet<_> = roots
        .iter()
        .flat_map(|var| collect_free_vars(&ctx.deep_resolve(&Type::Var(*var))).0)
        .collect();
    let candidates: Vec<_> = ctx
        .record_requirements
        .iter()
        .map(|site| site.requirement.types().cloned().collect::<Vec<_>>())
        .collect();
    let candidates: Vec<Vec<_>> = candidates
        .iter()
        .map(|types| types.iter().map(|ty| ctx.deep_resolve(ty)).collect())
        .collect();
    let variables: Vec<FxHashSet<_>> = candidates
        .iter()
        .map(|types| {
            types
                .iter()
                .flat_map(|ty| collect_free_vars(ty).0)
                .collect()
        })
        .collect();
    let mut by_var: FxHashMap<TyVid, Vec<usize>> = FxHashMap::default();
    for (index, vars) in variables.iter().enumerate() {
        for var in vars {
            by_var.entry(*var).or_default().push(index);
        }
    }
    let mut queue: Vec<_> = roots.into_iter().collect();
    let mut included = FxHashSet::default();
    while let Some(var) = queue.pop() {
        if let Some(indices) = by_var.remove(&var) {
            for index in indices {
                if included.insert(index) {
                    queue.extend(variables[index].iter().copied());
                }
            }
        }
    }
    candidates
        .iter()
        .enumerate()
        .filter(|(index, _)| included.contains(index))
        .flat_map(|(_, types)| types.iter().cloned())
        .collect()
}

pub fn for_export(
    ctx: &mut InferCtx,
    roots: &FxHashSet<TyVid>,
) -> Option<Arc<Vec<RecordRequirement>>> {
    if ctx.record_requirements.is_empty() || roots.is_empty() {
        return None;
    }
    let connected = connected_types(ctx, roots);
    let mut free = roots.clone();
    for ty in &connected {
        free.extend(collect_free_vars(ty).0);
    }
    let requirements: Vec<_> = ctx
        .record_requirements
        .iter()
        .map(|site| site.requirement.clone())
        .collect();
    let mut exported = Vec::new();
    for requirement in requirements {
        let requirement = requirement.map_types(|ty| ctx.resolve_for_export(ty));
        let vars: FxHashSet<_> = requirement
            .types()
            .flat_map(|ty| collect_free_vars(ty).0)
            .collect();
        if !vars.is_disjoint(&free) {
            exported.push(requirement);
        }
    }
    (!exported.is_empty()).then(|| Arc::new(exported))
}

/// Schemes now own generalised obligations. Keep only obligations still tied
/// to a monomorphic enclosing binding, rather than rescanning old declarations.
pub fn release_generalised(ctx: &mut InferCtx) {
    if ctx.record_requirements.is_empty() {
        return;
    }
    let roots = ctx.env_free_tyvids();
    let connected = connected_types(ctx, &roots);
    let free: FxHashSet<_> = connected
        .iter()
        .flat_map(|ty| collect_free_vars(ty).0)
        .collect();
    let sites = std::mem::take(&mut ctx.record_requirements);
    for site in sites {
        let related = site.requirement.types().any(|ty| {
            !collect_free_vars(&ctx.deep_resolve(ty))
                .0
                .is_disjoint(&free)
        });
        if related {
            ctx.record_requirements.push(site);
        }
    }
}
