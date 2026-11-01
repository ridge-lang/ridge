//! Providers belong to declared instances or to the compiler's private library.

use ridge_ast::Span;
use ridge_ir::{IrExpr, IrItem, StdlibKind, SymbolRef};
use ridge_resolve::ModuleId;
use ridge_typecheck::DictPlan;
use ridge_types::{Constraint, TyVid};

use crate::ctx::LowerCtx;

pub const HOME: &str = "std._evidence";

pub fn intrinsic(ctx: &mut LowerCtx<'_>, plan: &DictPlan) -> bool {
    let DictPlan::Static { class, tycon, .. } = plan else {
        return false;
    };
    let b = ctx.builtins;
    crate::value_comparison::is_decimal(b, *class, *tycon)
        || ctx
            .instance_env
            .is_some_and(|env| env.structural_comparisons.contains_key(&(*class, *tycon)))
        || crate::hash::is_primitive(b, *class, *tycon)
        || crate::comparison::is_composed(b, *class, *tycon)
        || crate::prelude_dict::is_prelude_codec_instance(b, *class, *tycon)
        || crate::prelude_dict::is_prelude_eq_instance(b, *class, *tycon)
        || crate::prelude_dict::is_prelude_ord_instance(b, *class, *tycon)
        || crate::prelude_dict::is_prelude_totext_instance(ctx, *class, *tycon)
}

pub fn provider(ctx: &mut LowerCtx<'_>, plan: &DictPlan) -> SymbolRef {
    let name = "$ridge_evidence".to_owned();
    if intrinsic(ctx, plan) {
        return SymbolRef::Stdlib {
            module: HOME.into(),
            name,
            kind: StdlibKind::Function,
        };
    }
    if let DictPlan::Static { info, class, .. } = plan {
        if let Some(module) = info.def_module.map(ModuleId) {
            if let Some(fqn) = ctx.stdlib_fqn(module) {
                return SymbolRef::Stdlib {
                    module: fqn.into(),
                    name,
                    kind: StdlibKind::Function,
                };
            }
            return if module == ctx.module_id {
                SymbolRef::Local { module, name }
            } else {
                SymbolRef::External { module, name }
            };
        }
        if let Some(home) = ctx
            .class_name(*class)
            .and_then(crate::core::stdlib_class_home_module)
        {
            return SymbolRef::Stdlib {
                module: home.into(),
                name,
                kind: StdlibKind::Function,
            };
        }
    }
    SymbolRef::Local {
        module: ctx.module_id,
        name,
    }
}

pub fn owns(ctx: &LowerCtx<'_>, provider: &SymbolRef) -> bool {
    match provider {
        SymbolRef::Local { module, .. } => *module == ctx.module_id,
        SymbolRef::Stdlib { module, .. } => module == &ctx.module_fqn,
        _ => false,
    }
}

pub fn register(ctx: &mut LowerCtx<'_>, items: &[IrItem]) {
    let Some(workspace) = ctx.workspace else {
        return;
    };
    let mut instances: Vec<_> = workspace
        .instance_env
        .instances
        .iter()
        .map(|(key, info)| (key.clone(), info.clone()))
        .collect();
    if ctx.module_fqn == HOME {
        // The stdlib build gates source-declared primitive codecs out of its
        // checking registry. The intrinsic provider must nevertheless publish
        // the complete prelude ABI used by ordinary consumer workspaces.
        let mut prelude = ridge_typecheck::class_env::InstanceEnv::new();
        ridge_typecheck::class_env::register_prelude_instances(&mut prelude, ctx.builtins);
        instances.extend(
            prelude
                .instances
                .into_iter()
                .filter(|(key, _)| !workspace.instance_env.instances.contains_key(key)),
        );
    }
    let names: std::collections::BTreeSet<_> = items
        .iter()
        .filter_map(|item| match item {
            IrItem::Fn(function) => Some(function.name.as_str()),
            IrItem::Const(value) => Some(value.name.as_str()),
            _ => None,
        })
        .collect();
    for ((class, heads), info) in instances {
        let Some(tycon) = heads.first().copied() else {
            continue;
        };
        let class_name = ctx.class_name(class).unwrap_or("Unknown").to_owned();
        let mut symbol = format!(
            "$inst_{class_name}_{}",
            crate::core::instance_head_name(ctx, tycon, &heads[1..])
        );
        // Composite query terminals deliberately emit a receiver-only body:
        // their predicate arity grows with join depth. Retain the complete
        // declaration recipe while linking its actual source dictionary.
        if ctx.module_fqn == "std.repo"
            && heads.len() > 1
            && !names.contains(symbol.as_str())
            && workspace.tycons.get(tycon.0 as usize).is_some_and(|decl| {
                matches!(
                    decl.name.as_str(),
                    "Joined" | "LeftJoined" | "RightJoined" | "FullJoined"
                )
            })
        {
            symbol = format!(
                "$inst_{class_name}_{}",
                crate::core::instance_head_name(ctx, tycon, &[])
            );
        }
        let declared = info.def_module == Some(ctx.module_id.0) && names.contains(symbol.as_str());
        let args: Vec<_> = info
            .ctx_constraints
            .iter()
            .enumerate()
            .map(|(index, constraint)| {
                DictPlan::Forward(Constraint::single(
                    constraint.class,
                    TyVid(u32::try_from(index).unwrap_or(u32::MAX)),
                ))
            })
            .collect();
        let span = info.span;
        let plan = DictPlan::Static {
            class,
            tycon,
            extra_head: heads[1..].iter().copied().collect(),
            info: Box::new(info),
            args,
        };
        let is_intrinsic = intrinsic(ctx, &plan);
        if !(declared && !is_intrinsic || ctx.module_fqn == HOME && is_intrinsic) {
            continue;
        }
        let owner = provider(ctx, &plan);
        if !owns(ctx, &owner) {
            continue;
        }
        let identity = crate::evidence_identity::recipe(ctx, &plan);
        let _ = crate::evidence::dictionary(ctx, plan, &class_name, span);
        if declared && !is_intrinsic {
            if let Some(identity) = identity {
                register_declared_body(ctx, &identity, &symbol, span);
            }
        }
    }
}

fn register_declared_body(ctx: &mut LowerCtx<'_>, identity: &str, symbol: &str, span: Span) {
    let Some((params, _, _)) = ctx.evidence_factories.get(identity) else {
        return;
    };
    let names: Vec<_> = params.iter().map(|param| param.name.clone()).collect();
    let value = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Local {
            module: ctx.module_id,
            name: symbol.into(),
        },
        span,
    };
    let body = if names.is_empty() {
        value
    } else {
        let args = names
            .into_iter()
            .map(|name| IrExpr::Local {
                id: ctx.fresh_id(None),
                name,
                span,
            })
            .collect();
        IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(value),
            args,
            span,
        }
    };
    let none = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "None".into(),
        },
        fields: vec![],
        span,
    };
    let body = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![body, none],
        span,
    };
    if let Some((_, current, _)) = ctx.evidence_factories.get_mut(identity) {
        *current = body;
    }
}
