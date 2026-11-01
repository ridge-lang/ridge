//! One declared method contract for callers and instance implementations.

use ridge_ast::{InstanceDecl, Span};
use ridge_types::{
    CapRow, CapabilitySet, ClassId, Constraint, RigidId, Row, RowTail, RowVid, Scheme, Subst,
    TyConArena, TyConId, TyVid, Type,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::class_env::{ClassTable, InstanceEnv, MethodSig};
use crate::ctx::{InferCtx, RigidInfo, RowsTycons};
use crate::error::{TypeDesc, TypeError};
use crate::type_template::TypeTemplates;

pub type MethodContracts = FxHashMap<(ClassId, String), Scheme>;

pub fn prepare(
    templates: &TypeTemplates<'_>,
    classes: &ClassTable,
    instances: &mut InstanceEnv,
    rows: Option<RowsTycons>,
) -> Vec<(ridge_resolve::ModuleId, TypeError)> {
    instances.method_schemes.clear();
    let mut errors = Vec::new();
    let mut ctx = InferCtx::new(templates.builtins);
    ctx.tycon_decls = templates.tycons.to_vec();
    let mut arena = TyConArena::new();
    for decl in templates.tycons {
        arena.intern(decl.clone());
    }
    for (class, info) in classes.iter() {
        let Some(module) = info.def_module else {
            continue;
        };
        for sig in &info.method_sigs {
            if sig.ast_ret_type.is_none() {
                continue;
            }
            let catalog = std::mem::take(&mut ctx.tycon_decls);
            ctx = InferCtx::new(templates.builtins);
            ctx.tycon_decls = catalog;
            ctx.current_module_raw = Some(module);
            ctx.class_ids.clone_from(classes.names_to_ids());
            ctx.rows_tycons = rows;
            ctx.user_tycon_names = templates
                .scopes
                .get(module as usize)
                .cloned()
                .unwrap_or_default();
            let scheme = source_scheme(&mut ctx, templates, module, class, sig);
            crate::projections::finish(&mut ctx, classes, instances, &arena);
            if ctx.errors.is_empty() {
                if let Some(scheme) = scheme {
                    instances
                        .method_schemes
                        .insert((class, sig.name.clone()), scheme);
                }
            }
            errors.extend(
                ctx.errors
                    .drain(..)
                    .map(|error| (ridge_resolve::ModuleId(module), error)),
            );
        }
    }
    errors
}

fn source_scheme(
    ctx: &mut InferCtx,
    templates: &TypeTemplates<'_>,
    module: u32,
    class: ClassId,
    sig: &MethodSig,
) -> Option<Scheme> {
    let ret = sig.ast_ret_type.as_ref()?;
    let mut annotations = sig.ast_param_types.clone();
    annotations.push(ret.clone());
    let mut names = sig.class_ty_vars.clone();
    for name in crate::solve::collect_head_vars(&annotations) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let variables: FxHashMap<_, _> = names
        .iter()
        .map(|name| (name.as_str(), ctx.fresh_tyvid()))
        .collect();
    let class_variables = sig
        .class_ty_vars
        .iter()
        .filter_map(|name| variables.get(name.as_str()).copied())
        .collect();
    let params = sig
        .ast_param_types
        .iter()
        .map(|ty| annotation(ctx, templates, module, &variables, ty))
        .collect();
    let ret = annotation(ctx, templates, module, &variables, ret);
    let mut scheme = Scheme::mono(Type::Fn {
        params,
        ret: Box::new(ret),
        caps: CapRow::Concrete(CapabilitySet::PURE),
    });
    scheme
        .constraints
        .push(Constraint::new(class, class_variables));
    let (mut vars, caps) = scheme.free_vars();
    vars.extend(variables.values().copied());
    scheme.vars = vars.into_iter().collect();
    scheme.vars.sort_by_key(|v| v.0);
    scheme.cap_vars = caps.into_iter().collect();
    scheme.cap_vars.sort_by_key(|v| v.0);
    scheme.row_vars = scheme.free_row_vars().into_iter().collect();
    scheme.row_vars.sort_by_key(|v| v.0);
    scheme.projections =
        crate::projections::for_export(ctx, &scheme.vars.iter().copied().collect());
    Some(scheme)
}

fn annotation(
    ctx: &mut InferCtx,
    templates: &TypeTemplates<'_>,
    module: u32,
    variables: &FxHashMap<&str, TyVid>,
    ty: &ridge_ast::Type,
) -> Type {
    // Collection precedes alias elaboration. Expand source aliases in their
    // declaring scope, then retain their domains through the common checker.
    let parameters = variables
        .iter()
        .map(|(name, var)| ((*name).to_owned(), Type::Var(*var)))
        .collect();
    if let Some(converted) = templates.convert(module, ty, &parameters, &mut FxHashSet::default()) {
        crate::projections::remember_type(ctx, &converted, ty.span());
        return converted;
    }
    let b = ctx.builtins;
    let names = ctx.user_tycon_names.clone();
    crate::tycon_collect::ast_type_to_ridge_type(&b, ctx, ty, &names, variables)
}

/// Snapshot intrinsic seeds before ordinary functions can shadow their names.
pub fn capture(
    ctx: &InferCtx,
    classes: &ClassTable,
    instances: &InstanceEnv,
    reconciled: &FxHashMap<String, TyConId>,
) -> MethodContracts {
    let mut contracts = instances.method_schemes.clone();
    let intrinsic = classes
        .iter()
        .flat_map(|(class, info)| {
            info.method_sigs.iter().filter_map(move |sig| {
                let belongs = |scheme: &Scheme| scheme.constraints.iter().any(|c| c.class == class);
                let scheme = ctx
                    .env
                    .lookup(&sig.name)
                    .filter(|s| belongs(s))
                    .cloned()
                    .or_else(|| {
                        ridge_resolve::BUILTINS
                            .iter()
                            .filter(|module| module.exports.contains(&sig.name.as_str()))
                            .find_map(|module| {
                                crate::stdlib_signatures::stdlib_signature(
                                    module.id,
                                    &sig.name,
                                    &ctx.builtins,
                                )
                                .or_else(|| {
                                    crate::stdlib_types::reconciled_fn_scheme(
                                        module.name,
                                        &sig.name,
                                        reconciled,
                                        &ctx.builtins,
                                        Some(classes),
                                    )
                                })
                                .filter(belongs)
                            })
                    })?;
                Some(((class, sig.name.clone()), scheme))
            })
        })
        .collect::<MethodContracts>();
    for (key, scheme) in intrinsic {
        contracts.entry(key).or_insert(scheme);
    }
    contracts
}

pub fn check_method_set(ctx: &mut InferCtx, decl: &InstanceDecl, classes: &ClassTable) {
    let Some(info) = classes
        .id_by_name(&decl.class.text)
        .and_then(|id| classes.get(id))
    else {
        return;
    };
    let expected: FxHashSet<_> = info
        .method_sigs
        .iter()
        .map(|sig| sig.name.as_str())
        .collect();
    let found: FxHashSet<_> = decl
        .methods
        .iter()
        .map(|method| method.name.text.as_str())
        .collect();
    if expected != found || found.len() != decl.methods.len() {
        mismatch(
            ctx,
            decl.span,
            TypeDesc::Text(format!("the required methods of {}", info.name)),
            TypeDesc::Text(
                decl.methods
                    .iter()
                    .map(|m| m.name.text.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        );
    }
}

pub fn rigid_head(
    ctx: &mut InferCtx,
    decl: &InstanceDecl,
    classes: &ClassTable,
    variables: &FxHashMap<&str, TyVid>,
) -> Subst {
    let givens: Vec<_> = decl
        .constraints
        .iter()
        .filter_map(|given| {
            let class = classes.id_by_name(&given.class.text)?;
            let tys: Option<smallvec::SmallVec<[TyVid; 1]>> = given
                .ty_vars
                .iter()
                .map(|v| variables.get(v.text.as_str()).copied())
                .collect();
            Some(Constraint::new(class, tys?))
        })
        .collect();
    let mut subst = Subst::empty();
    for (name, vid) in variables {
        let promises: Vec<_> = givens
            .iter()
            .filter(|c| c.tys.contains(vid))
            .cloned()
            .collect();
        let id = RigidId(vid.0);
        ctx.rigids.insert(
            id,
            RigidInfo {
                decl: format!("{} instance", decl.class.text),
                span: decl.span,
                givens: promises.iter().map(|c| c.class).collect(),
                given_constraints: promises,
                complete: true,
            },
        );
        subst.ty.insert(
            *vid,
            Type::Rigid {
                id,
                name: (*name).into(),
            },
        );
    }
    subst
}

pub struct MethodCheck<'a> {
    pub decl: &'a InstanceDecl,
    pub method: &'a str,
    pub contracts: &'a MethodContracts,
    pub classes: &'a ClassTable,
    pub variables: &'a FxHashMap<&'a str, TyVid>,
    pub rigid: &'a Subst,
}

pub fn prepare_method(
    ctx: &mut InferCtx,
    check: &MethodCheck<'_>,
    params: &mut [Type],
    ret: &mut Type,
) -> Vec<RowVid> {
    let MethodCheck {
        decl,
        method,
        contracts,
        classes,
        variables,
        rigid,
    } = *check;
    for param in params.iter_mut() {
        *param = rigid.apply_to_ty(param);
    }
    *ret = rigid.apply_to_ty(ret);
    let Some(class) = classes.id_by_name(&decl.class.text) else {
        return vec![];
    };
    let Some(scheme) = contracts.get(&(class, method.into())) else {
        mismatch(
            ctx,
            decl.span,
            TypeDesc::Text("a declared class method contract".into()),
            TypeDesc::Text(method.into()),
        );
        return vec![];
    };
    let Some(formals) = scheme.constraints.iter().find(|c| c.class == class) else {
        return vec![];
    };
    let names = ctx.user_tycon_names.clone();
    let b = ctx.builtins;
    let heads: Vec<_> = decl
        .head
        .iter()
        .map(|head| {
            rigid.apply_to_ty(&crate::tycon_collect::ast_type_to_ridge_type(
                &b, ctx, head, &names, variables,
            ))
        })
        .collect();
    let mut subst = Subst::empty();
    for (formal, head) in formals.tys.iter().zip(heads) {
        subst.ty.insert(*formal, head);
    }
    for var in &scheme.vars {
        if !subst.ty.contains_key(var) {
            let id = RigidId(ctx.fresh_tyvid().0);
            ctx.rigids.insert(
                id,
                RigidInfo {
                    decl: format!("{}.{}", decl.class.text, method),
                    span: decl.span,
                    givens: vec![],
                    given_constraints: vec![],
                    complete: true,
                },
            );
            subst.ty.insert(
                *var,
                Type::Rigid {
                    id,
                    name: "method parameter".into(),
                },
            );
        }
    }
    for row in &scheme.row_vars {
        let fresh = ctx.fresh_rowvid();
        subst
            .row
            .insert(*row, Row::new(vec![], RowTail::Open(fresh)));
    }
    let expected = subst.apply_to_ty(&scheme.ty);
    let rows = crate::instantiate::collect_free_row_vars(&expected)
        .into_iter()
        .collect();
    let found = Type::Fn {
        params: params.to_vec(),
        ret: Box::new(ret.clone()),
        caps: CapRow::Concrete(CapabilitySet::PURE),
    };
    if !crate::unify::try_unify(ctx, &expected, &found) {
        mismatch(ctx, decl.span, TypeDesc::ty(expected), TypeDesc::ty(found));
    }
    rows
}

pub fn check_rows(ctx: &mut InferCtx, rows: &[RowVid], span: Span) {
    for &row in rows {
        let resolved = ctx.deep_resolve(&Type::record(vec![], RowTail::Open(row)));
        if !matches!(&resolved, Type::Record { fields, tail: RowTail::Open(_)} if fields.is_empty())
        {
            mismatch(
                ctx,
                span,
                TypeDesc::Text("an unrestricted method row".into()),
                TypeDesc::ty(resolved),
            );
        }
    }
}

fn mismatch(ctx: &mut InferCtx, span: Span, expected: TypeDesc, found: TypeDesc) {
    ctx.errors.push(TypeError::TypeMismatch {
        expected, found, span,
        hint: Some("an instance method must implement its class's declared type without specializing generic parameters".into()),
    });
}
