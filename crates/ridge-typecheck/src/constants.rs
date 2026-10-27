//! Check a constant's value against the contract visible to its callers.

use ridge_ast::{Item, Module};
use ridge_types::BuiltinTyCons;

use crate::class_env::{ClassTable, InstanceEnv};
use crate::ctx::InferCtx;

pub fn seed(ctx: &mut InferCtx, b: &BuiltinTyCons, ast: &Module) {
    for item in &ast.items {
        let Item::Const(c) = item else { continue };
        let mut variables = rustc_hash::FxHashMap::default();
        crate::scc::collect_tyvars_from_ast_type(&c.ty, &mut variables, ctx);
        let names = ctx.user_tycon_names.clone();
        let ty = crate::tycon_collect::ast_type_to_ridge_type(b, ctx, &c.ty, &names, &variables);
        let scheme = crate::instantiate::monoscheme(ty);
        ctx.name_schemes_accum
            .insert(c.name.text.clone(), scheme.clone());
        ctx.env.bind(c.name.text.clone(), scheme);
    }
}

pub fn check(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    ast: &Module,
    classes: &ClassTable,
    instances: &InstanceEnv,
) {
    for item in &ast.items {
        let Item::Const(c) = item else { continue };
        let expected = ctx.name_schemes_accum[&c.name.text].ty.clone();
        let value = crate::infer::infer_expr(ctx, b, &c.value);
        if let Err(error) = crate::unify::check_value(ctx, &expected, &value) {
            ctx.errors
                .push(crate::infer::attach_span(error, c.value.span()));
        }
        // Producing a value must be pure. A lambda's latent capabilities live
        // on its type; merely constructing it does not execute its body.
        crate::caps_check::check_caps_decl_kind(
            ctx,
            b,
            &c.name.text,
            Some(ridge_types::CapabilitySet::PURE),
            &c.value,
            c.span,
            crate::error::CapDeclKind::Const,
        );
        crate::record_requirements::solve(ctx);
        let (_, plans) = crate::solve::solve_constraints(
            ctx,
            instances,
            classes,
            &rustc_hash::FxHashSet::default(),
            c.span,
            Some(b),
        );
        ctx.dict_resolution_accum.extend(plans);
    }
}
