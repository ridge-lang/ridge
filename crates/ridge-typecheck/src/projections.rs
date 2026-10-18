//! Well-formedness of written type projections, before normalisation erases them.

use ridge_ast::Span;
use ridge_types::{TyConArena, TyConId, TyConKind, TyVid, Type};
use rustc_hash::FxHashSet;

use crate::class_env::{ClassTable, InstanceEnv};
use crate::ctx::InferCtx;
use crate::error::{TypeDesc, TypeError};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Source,
    Constraint,
}

pub struct Site {
    ty: Type,
    span: Span,
    origin: Origin,
}

fn domain(ctx: &InferCtx, id: TyConId) -> Option<(&'static str, usize)> {
    let b = &ctx.builtins;
    if id == b.ret {
        Some(("function", 1))
    } else if id == b.rows {
        Some(("Fetchable", 1))
    } else if id == b.insert_shape {
        Some(("HasSchema", 1))
    } else if id == b.joincond || id == b.joinresult {
        Some(("Joinable", 2))
    } else if id == b.left_joinresult {
        Some(("LeftJoinable", 2))
    } else if id == b.right_joinresult {
        Some(("RightJoinable", 2))
    } else if id == b.full_joinresult {
        Some(("FullJoinable", 2))
    } else {
        None
    }
}

/// Record just a written head. Children already pass through the AST converter;
/// alias expansion is the exception, because its substituted body is not an AST.
pub fn remember_head(ctx: &mut InferCtx, ty: &Type, span: Span) {
    match ty {
        Type::Con(id, _) if needs_check(ctx, *id) => {
            ctx.projection_sites.push(Site {
                ty: ty.clone(),
                span,
                origin: Origin::Source,
            });
        }
        Type::Alias { body, .. } => remember_type(ctx, body, span),
        _ => {}
    }
}

/// Expressions and substituted schemas can pin an open projection at a use site.
pub fn remember_type(ctx: &mut InferCtx, ty: &Type, span: Span) {
    remember_type_at(ctx, ty, span, Origin::Source);
}

fn remember_type_at(ctx: &mut InferCtx, ty: &Type, span: Span, origin: Origin) {
    visit_type(ty, &mut |node| {
        remember_head_unaliased(ctx, node, span, origin);
    });
}

fn visit_type(ty: &Type, visit: &mut impl FnMut(&Type)) {
    visit(ty);
    match ty {
        Type::Con(_, args) | Type::Tuple(args) => {
            for arg in args {
                visit_type(arg, visit);
            }
        }
        Type::Fn { params, ret, .. } => {
            for param in params {
                visit_type(param, visit);
            }
            visit_type(ret, visit);
        }
        Type::Record { fields, .. } => {
            for (_, field) in fields {
                visit_type(field, visit);
            }
        }
        Type::Alias { body, .. } => visit_type(body, visit),
        _ => {}
    }
}

fn remember_head_unaliased(ctx: &mut InferCtx, ty: &Type, span: Span, origin: Origin) {
    if let Type::Con(id, _) = ty {
        if needs_check(ctx, *id) {
            ctx.projection_sites.push(Site {
                ty: ty.clone(),
                span,
                origin,
            });
        }
    }
}

fn needs_check(ctx: &InferCtx, id: TyConId) -> bool {
    domain(ctx, id).is_some()
        || ctx.projection_aliases.contains(&id)
        || ctx
            .tycon_decls
            .get(id.0 as usize)
            .is_some_and(|d| matches!(d.kind, TyConKind::Alias { .. }))
}

/// Validate after receiver reconciliation and inference. An open variable is
/// deliberately deferred; failure to reduce alone is not proof of invalidity.
pub fn finish(ctx: &mut InferCtx, ct: &ClassTable, ie: &InstanceEnv, arena: &TyConArena) {
    // Reuse the cycle-safe alias resolver now that every body is available.
    // In particular, a forward `Wrap Int` can first reveal `Rows Int` here.
    for site in std::mem::take(&mut ctx.projection_sites) {
        let expanded =
            crate::tycon_collect::chase_alias_chain(arena.all(), &site.ty, &mut Vec::new());
        remember_type_at(ctx, &expanded, site.span, site.origin);
    }
    let sites = std::mem::take(&mut ctx.projection_sites);
    let source_domains: FxHashSet<_> = sites
        .iter()
        .filter(|site| site.origin == Origin::Source)
        .filter_map(|site| resolved_domain_key(ctx, &site.ty))
        .collect();
    let mut seen = FxHashSet::default();
    for Site { ty, span, origin } in sites {
        if origin == Origin::Constraint
            && resolved_domain_key(ctx, &ty).is_some_and(|key| source_domains.contains(&key))
        {
            continue;
        }
        let Type::Con(id, args) = ty else { continue };
        if seen.contains(&(span, id)) {
            continue;
        }
        let errors = check_site(ctx, ct, ie, id, &args, span);
        if !errors.is_empty() {
            // An open/valid sibling must not hide an invalid application at
            // the same use; deduplicate only diagnostics actually produced.
            seen.insert((span, id));
            ctx.errors.extend(errors);
        }
    }
}

fn resolved_domain_key(ctx: &mut InferCtx, ty: &Type) -> Option<(TyConId, ridge_types::TyKey)> {
    let Type::Con(id, args) = ty else { return None };
    let (_, arity) = domain(ctx, *id)?;
    if args.len() != arity {
        return None;
    }
    let receiver = ctx.shallow_resolve(&args[0]);
    let receiver = ctx.deep_resolve(&receiver);
    Some((*id, ridge_types::type_to_key(&receiver)))
}

/// A successful dictionary requirement includes its intrinsic type domain.
/// Both unary and binary projections have their domain in the first argument;
/// Unit is a closed witness for the otherwise unrestricted right-hand type.
pub fn remember_class_domain(
    ctx: &mut InferCtx,
    ct: &ClassTable,
    wanted: &ridge_types::Constraint,
    span: Span,
) {
    if !ctx.capture_projection_domains || wanted.tys.len() != 1 {
        return;
    }
    let Some(class) = ct.get(wanted.class) else {
        return;
    };
    let b = ctx.builtins;
    for id in [
        b.rows,
        b.insert_shape,
        b.joincond,
        b.joinresult,
        b.left_joinresult,
        b.right_joinresult,
        b.full_joinresult,
    ] {
        let Some((owner, arity)) = domain(ctx, id) else {
            continue;
        };
        if owner != class.name {
            continue;
        }
        let mut args = vec![Type::Var(wanted.sole_ty())];
        if arity == 2 {
            args.push(Type::Con(b.unit, vec![]));
        }
        remember_type_at(ctx, &Type::Con(id, args), span, Origin::Constraint);
    }
}

/// Preserve the domain before resolving an exported type erases it. Only
/// requirements connected to this binding's type escape its inference scope.
/// This also carries requirements introduced by an intermediary's body.
pub fn for_export(
    ctx: &mut InferCtx,
    free: &FxHashSet<TyVid>,
) -> Option<std::sync::Arc<Vec<Type>>> {
    if free.is_empty() {
        return None;
    }
    let scope = &ctx.projection_sites[ctx.projection_scope_start..];
    let sites: Vec<_> = scope
        .iter()
        .filter(|site| site.origin == Origin::Source)
        .chain(
            scope
                .iter()
                .filter(|site| site.origin == Origin::Constraint),
        )
        .map(|site| site.ty.clone())
        .collect();
    let mut requirements = Vec::new();
    let mut seen = FxHashSet::default();
    for ty in sites {
        let expanded =
            crate::tycon_collect::chase_alias_chain(&ctx.tycon_decls, &ty, &mut Vec::new());
        visit_type(&expanded, &mut |node| {
            let Type::Con(id, args) = node else { return };
            if domain(ctx, *id).is_none() {
                return;
            }
            // Resolve arguments without reducing the projection itself.
            let args: Vec<_> = args.iter().map(|arg| ctx.resolve_for_export(arg)).collect();
            let Some(receiver) = args.first() else { return };
            let (vars, _) = crate::instantiate::collect_free_vars(receiver);
            let key = (*id, ridge_types::type_to_key(receiver));
            let projection = Type::Con(*id, args);
            if !vars.is_disjoint(free) && seen.insert(key) {
                requirements.push(projection);
            }
        });
    }
    (!requirements.is_empty()).then(|| std::sync::Arc::new(requirements))
}

/// Compare the formation contracts exported by two checks of a module.
/// Normalised types alone do not retain these requirements.
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "workspace scheme maps use the canonical hasher"
)]
pub fn contracts_equal(
    first: &rustc_hash::FxHashMap<String, ridge_types::Scheme>,
    second: &rustc_hash::FxHashMap<String, ridge_types::Scheme>,
) -> bool {
    formation_surface(first) == formation_surface(second)
}

type FormationSurface = Vec<(String, ridge_types::TyKey, Vec<ridge_types::TyKey>)>;

/// Canonicalise the requirements and body with one substitution. Local IDs
/// alone do not describe a requirement's relation to the parameters.
fn formation_surface(
    schemes: &rustc_hash::FxHashMap<String, ridge_types::Scheme>,
) -> FormationSurface {
    let mut surface = Vec::new();
    for (name, scheme) in schemes {
        if scheme.projections.as_ref().is_none_or(|p| p.is_empty()) {
            continue;
        }
        let (mut ty, mut cap, mut row) = (0, 0, 0);
        let (body, requirements) = scheme.instantiate_with_projections(
            &mut || {
                let id = ridge_types::TyVid(ty);
                ty += 1;
                id
            },
            &mut || {
                let id = ridge_types::CapVid(cap);
                cap += 1;
                id
            },
            &mut || {
                let id = ridge_types::RowVid(row);
                row += 1;
                id
            },
        );
        surface.push((
            name.clone(),
            ridge_types::type_to_key(&body),
            requirements.iter().map(ridge_types::type_to_key).collect(),
        ));
    }
    surface.sort_by(|a, b| a.0.cmp(&b.0));
    surface
}

/// A recursive complete signature is instantiated before its body is checked.
/// Its formation contract must therefore be available at that boundary.
pub fn check_recursive_contract(
    ctx: &mut InferCtx,
    ct: &ClassTable,
    ie: &InstanceEnv,
    signature: &Type,
    scheme: &ridge_types::Scheme,
    span: Span,
) {
    let Some(requirements) = &scheme.projections else {
        return;
    };
    let signature =
        crate::tycon_collect::chase_alias_chain(&ctx.tycon_decls, signature, &mut Vec::new());
    let mut written = FxHashSet::default();
    let mut rigids = ridge_types::Subst::default();
    visit_type(&signature, &mut |node| {
        if let Type::Rigid { id, name } = ctx.shallow_resolve(node) {
            rigids.ty.insert(TyVid(id.0), Type::Rigid { id, name });
        }
        if let Type::Con(id, args) = node {
            if domain(ctx, *id).is_some() {
                let args = args.iter().map(|a| ctx.resolve_for_export(a)).collect();
                written.insert(ridge_types::type_to_key(&Type::Con(*id, args)));
            }
        }
    });
    let promised = rigids.ty.keys().copied().collect();
    for requirement in requirements.iter() {
        if written.contains(&ridge_types::type_to_key(requirement)) {
            continue;
        }
        let requirement = rigids.apply_to_ty(requirement);
        let Type::Con(id, args) = &requirement else {
            continue;
        };
        let Some((class, arity)) = domain(ctx, *id) else {
            continue;
        };
        if args.len() != arity {
            continue;
        }
        let args: Vec<_> = args
            .iter()
            .map(|a| {
                let a = ctx.shallow_resolve(a);
                ctx.deep_resolve(&a)
            })
            .collect();
        if matches!(args[0], Type::Error) {
            continue;
        }
        if *id == ctx.builtins.ret && matches!(args[0], Type::Fn { .. }) {
            continue;
        }
        if *id != ctx.builtins.ret {
            if let (Some(owner), Type::Rigid { id, .. }) = (ct.id_by_name(class), &args[0]) {
                if ctx.rigids.get(id).is_some_and(|info| {
                    info.givens.iter().any(|given| {
                        ct.get(*given).is_some_and(|class| class.arity == 1)
                            && crate::solve::givens_cover(&[*given], owner, ct)
                    })
                }) {
                    continue;
                }
            }
        }
        if *id != ctx.builtins.insert_shape && !reduces(ctx, *id, &args) {
            ctx.errors.push(invalid(requirement, span,
                "this recursive generic requires the projection in its signature; annotate a parameter or result with it".to_owned()));
            continue;
        }
        if let Some(class) = ct.id_by_name(class) {
            let errors = crate::solve::check_promised_projection_requirement(
                ctx, ie, ct, class, &args[0], span, &promised,
            );
            ctx.errors.extend(errors);
        }
    }
}

fn check_site(
    ctx: &mut InferCtx,
    ct: &ClassTable,
    ie: &InstanceEnv,
    id: TyConId,
    args: &[Type],
    span: Span,
) -> Vec<TypeError> {
    let Some((class, arity)) = domain(ctx, id) else {
        return Vec::new();
    };
    let args: Vec<_> = args
        .iter()
        .map(|a| {
            let a = ctx.shallow_resolve(a);
            ctx.deep_resolve(&a)
        })
        .collect();
    let projection = Type::Con(id, args.clone());
    if args.len() != arity {
        return vec![invalid(
            projection,
            span,
            format!(
                "this projection takes {arity} type argument(s), but {} were written",
                args.len()
            ),
        )];
    }
    let receiver = &args[0];
    if matches!(receiver, Type::Var(_) | Type::Rigid { .. } | Type::Error)
        || (matches!(receiver, Type::Con(head, _) if domain(ctx, *head).is_some())
            && has_open_type(receiver))
    {
        return Vec::new();
    }
    if id == ctx.builtins.ret {
        return if matches!(receiver, Type::Fn { .. }) {
            Vec::new()
        } else {
            vec![invalid(
                projection,
                span,
                "`Ret` extracts the result of a function type; write `Ret (fn ... -> result)`"
                    .to_owned(),
            )]
        };
    }
    let instance = ct.id_by_name(class).and_then(|cid| {
        if let Type::Con(tid, _) = receiver {
            ie.get((cid, *tid)).map(|info| (cid, info))
        } else {
            None
        }
    });
    let supported =
        id == ctx.builtins.insert_shape || reduces(ctx, id, &args) || can_defer(ctx, id, receiver);
    if let Some((cid, info)) = instance {
        if supported {
            let has_requirements = !info.ctx_constraints.is_empty()
                || ct.get(cid).is_some_and(|c| !c.superclasses.is_empty());
            if has_requirements {
                return crate::solve::check_projection_requirement(
                    ctx, ie, ct, cid, receiver, span,
                );
            }
            return Vec::new();
        }
    }
    // Other projections have a closed receiver domain: a dictionary alone
    // cannot supply a missing type-level reduction equation.
    if supported && id != ctx.builtins.insert_shape {
        return Vec::new();
    }
    if instance.is_some() {
        return vec![invalid(
            projection,
            span,
            "this receiver has no reduction rule for the type projection".to_owned(),
        )];
    }
    vec![TypeError::NoInstance {
        class: class.to_owned(),
        ty: TypeDesc::ty(receiver.clone()),
        span,
        fix_hint: if id == ctx.builtins.insert_shape {
            "`InsertShape` requires an entity with `HasSchema`; derive `Schema` on the entity or use its actual input type".to_owned()
        } else {
            format!("this type projection requires a supported `{class}` receiver; use a sequence, query or join type supported by that class")
        },
    }]
}

fn reduces(ctx: &InferCtx, id: TyConId, args: &[Type]) -> bool {
    let b = ctx.builtins;
    if id == b.rows {
        ctx.reduce_rows_arg(&args[0]).is_some()
    } else if id == b.joincond {
        ctx.reduce_joincond_arg(&args[0], &args[1]).is_some()
    } else if id == b.joinresult {
        ctx.reduce_joinresult_arg(&args[0], &args[1]).is_some()
    } else if id == b.left_joinresult {
        ctx.reduce_leftjoinresult_arg(&args[0], &args[1]).is_some()
    } else if id == b.right_joinresult {
        ctx.reduce_rightjoinresult_arg(&args[0], &args[1]).is_some()
    } else if id == b.full_joinresult {
        ctx.reduce_fulljoinresult_arg(&args[0], &args[1]).is_some()
    } else {
        false
    }
}

fn can_defer(ctx: &InferCtx, id: TyConId, receiver: &Type) -> bool {
    match receiver {
        Type::Var(_) | Type::Rigid { .. } | Type::Error => true,
        Type::Alias { body, .. } => can_defer(ctx, id, body),
        Type::Con(head, _) if domain(ctx, *head).is_some() => has_open_type(receiver),
        Type::Con(head, args)
            if (id == ctx.builtins.rows || id == ctx.builtins.joincond)
                && ctx.is_composite_join_tycon(*head) =>
        {
            args.first().is_some_and(|base| can_defer(ctx, id, base))
        }
        _ => false,
    }
}

fn has_open_type(ty: &Type) -> bool {
    match ty {
        Type::Con(_, args) | Type::Tuple(args) => args.iter().any(has_open_type),
        Type::Fn { params, ret, .. } => params.iter().any(has_open_type) || has_open_type(ret),
        Type::Alias { body, .. } => has_open_type(body),
        Type::Record { fields, .. } => fields.iter().any(|(_, ty)| has_open_type(ty)),
        _ => true,
    }
}

fn invalid(projection: Type, span: Span, detail: String) -> TypeError {
    TypeError::InvalidTypeProjection {
        projection: TypeDesc::ty(projection),
        detail,
        span,
    }
}

/// Substitute a record or union schema and retain its projection obligations.
pub fn substitute_schema(
    ctx: &mut InferCtx,
    span: Span,
    ty: &Type,
    params: &[TyVid],
    args: &[Type],
) -> Type {
    let ty = subst_type_inner(ty, params, args);
    crate::projections::remember_type(ctx, &ty, span);
    ty
}

fn subst_type_inner(ty: &Type, params: &[TyVid], args: &[Type]) -> Type {
    match ty {
        Type::Var(v) => params
            .iter()
            .position(|p| p == v)
            .map_or_else(|| ty.clone(), |pos| args[pos].clone()),
        Type::Con(id, sub_args) => {
            let new_sub = sub_args
                .iter()
                .map(|a| subst_type_inner(a, params, args))
                .collect();
            Type::Con(*id, new_sub)
        }
        Type::Tuple(ts) => {
            let new_ts = ts
                .iter()
                .map(|t| subst_type_inner(t, params, args))
                .collect();
            Type::Tuple(new_ts)
        }
        Type::Fn {
            params: ps,
            ret,
            caps,
        } => Type::Fn {
            params: ps
                .iter()
                .map(|p| subst_type_inner(p, params, args))
                .collect(),
            ret: Box::new(subst_type_inner(ret, params, args)),
            caps: caps.clone(),
        },
        // Alias: walk body but preserve wrapper for diagnostic names.
        Type::Alias { name, body } => Type::Alias {
            name: *name,
            body: Box::new(subst_type_inner(body, params, args)),
        },
        Type::Record { fields, tail } => Type::Record {
            fields: fields
                .iter()
                .map(|(name, ty)| (name.clone(), subst_type_inner(ty, params, args)))
                .collect(),
            tail: tail.clone(),
        },
        Type::Error => Type::Error,
        // Non-exhaustive wildcard: future Type variants returned as-is.
        _ => ty.clone(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::class_env::{
        register_prelude_classes, register_stdlib_classes, InstanceInfo, InstanceOrigin,
    };
    use ridge_types::{BuiltinTyCons, Constraint, RecordSchema, TyConDecl};

    #[test]
    fn contract_keys_ignore_local_ids_but_preserve_parameter_roles() {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let make = |base: u32, swapped: bool| {
            let a = TyVid(base);
            let c = TyVid(base + 1);
            let params = if swapped {
                vec![Type::Var(c), Type::Var(a)]
            } else {
                vec![Type::Var(a), Type::Var(c)]
            };
            let mut scheme = ridge_types::Scheme::mono(Type::Fn {
                params,
                ret: Box::new(Type::Con(b.unit, vec![])),
                caps: ridge_types::CapRow::Concrete(ridge_types::CapabilitySet::PURE),
            });
            scheme.vars = vec![a, c];
            scheme.projections = Some(std::sync::Arc::new(vec![Type::Con(
                b.rows,
                vec![Type::Var(a)],
            )]));
            rustc_hash::FxHashMap::from_iter([("take".to_owned(), scheme)])
        };
        assert!(contracts_equal(&make(10, false), &make(90, false)));
        assert!(!contracts_equal(&make(10, false), &make(90, true)));
    }

    #[test]
    fn schema_projection_checks_context_without_demanding_runtime_dictionaries() {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let mut ct = ClassTable::new();
        register_prelude_classes(&mut ct);
        register_stdlib_classes(&mut ct);
        let schema = ct.id_by_name("HasSchema").unwrap();
        let row = ct.id_by_name("Row").unwrap();
        let mut record = |name: &str, params| {
            arena.intern(TyConDecl {
                id: TyConId(0),
                name: name.to_owned(),
                arity: params,
                kind: TyConKind::Record(RecordSchema::new(
                    (0..params).map(TyVid).collect(),
                    vec![],
                )),
                def_span: Some(Span::point(0)),
                def_module_raw: Some(0),
                opaque: false,
                is_anon: false,
            })
        };
        let boxed = record("Box", 1);
        let user = record("User", 0);
        let mut ie = InstanceEnv::new();
        let info = InstanceInfo {
            def_module: Some(0),
            methods: vec![],
            ctx_constraints: vec![Constraint::single(row, TyVid(0))],
            head_var_positions: vec![0],
            origin: InstanceOrigin::Explicit,
            span: Span::point(0),
        };
        ie.insert((schema, boxed), info.clone(), "HasSchema", "Box")
            .unwrap();
        ie.insert(
            (row, user),
            InstanceInfo {
                ctx_constraints: vec![],
                head_var_positions: vec![],
                ..info
            },
            "Row",
            "User",
        )
        .unwrap();
        for (element, valid) in [(b.int, false), (user, true)] {
            let mut ctx = InferCtx::new(&b);
            ctx.tycon_decls = arena.all().to_vec();
            let v = ctx.fresh_tyvid();
            let deferred = Constraint::single(ridge_types::TOTEXT_CLASS, v);
            ctx.deferred_constraints.push(deferred.clone());
            ctx.pending_error_propagations.push((
                Type::Con(b.int, vec![]),
                Type::Con(b.text, vec![]),
                Span::point(22),
            ));
            ctx.demanded_rows.insert(boxed);
            remember_type(
                &mut ctx,
                &Type::Con(
                    b.insert_shape,
                    vec![Type::Con(boxed, vec![Type::Con(element, vec![])])],
                ),
                Span::point(12),
            );
            finish(&mut ctx, &ct, &ie, &arena);
            if valid {
                assert!(ctx.errors.is_empty(), "{:?}", ctx.errors);
            } else {
                assert!(
                    ctx.errors.iter().any(
                        |e| matches!(e, TypeError::NoInstance { class, .. } if class == "Row")
                    ),
                    "{:?}",
                    ctx.errors
                );
            }
            assert_eq!(ctx.deferred_constraints, vec![deferred]);
            assert!(matches!(ctx.shallow_resolve(&Type::Var(v)), Type::Var(found) if found == v));
            assert!(
                matches!(ctx.pending_error_propagations.as_slice(), [(Type::Con(left, _), Type::Con(right, _), span)] if *left == b.int && *right == b.text && *span == Span::point(22))
            );
            assert_eq!(ctx.demanded_rows, FxHashSet::from_iter([boxed]));
            assert!(ctx.dict_resolution_accum.is_empty());
            assert!(ctx.capture_projection_domains);
            assert!(
                ctx.projection_sites.is_empty(),
                "a formation proof cannot regenerate its own work"
            );
        }
    }
}
