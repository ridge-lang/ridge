//! Solve nested declarations at their own generalisation boundary.

use std::{cell::OnceCell, sync::Arc};

use ridge_ast::{visit::Visit, Body, Expr, Span};
use ridge_types::{BuiltinTyCons, Scheme, TyVid};
use rustc_hash::FxHashSet;

use crate::{
    class_env::{ClassTable, InstanceEnv},
    ctx::InferCtx,
};

#[derive(Debug)]
pub struct Registries {
    classes: ClassTable,
    instances: InstanceEnv,
}

pub fn for_module(
    module: &ridge_ast::Module,
    registries: Option<(&ClassTable, &InstanceEnv)>,
    shared: &OnceCell<Arc<Registries>>,
) -> Option<Arc<Registries>> {
    struct Inner(bool);
    impl<'ast> Visit<'ast> for Inner {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            if matches!(expr, Expr::InnerFn { .. }) {
                self.0 = true;
            }
            if !self.0 {
                ridge_ast::visit::walk_expr(self, expr);
            }
        }
    }
    let mut inner = Inner(false);
    inner.visit_module(module);
    if !inner.0 {
        return None;
    }
    let (classes, instances) = registries?;
    Some(Arc::clone(shared.get_or_init(|| {
        Arc::new(Registries {
            classes: classes.clone(),
            instances: instances.clone(),
        })
    })))
}

/// Captured constraints remain with the enclosing declaration. Constraints on
/// the helper's own variables are instantiated independently at each call.
pub struct Solution {
    registries: Arc<Registries>,
    retained: Vec<ridge_types::Constraint>,
    plans: crate::solve::DictResolution,
}

/// Instance solving precedes generalisation, as at the module SCC boundary.
pub fn solve(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    constraint_start: usize,
    span: Span,
) -> Option<Solution> {
    let registries = ctx.local_registries.clone()?;
    let initial = ctx.deferred_constraints.split_off(constraint_start);
    let (retained, plans) = crate::solve::solve_work(
        ctx,
        &registries.instances,
        &registries.classes,
        &FxHashSet::default(),
        span,
        Some(b),
        initial,
    );
    Some(Solution {
        registries,
        retained,
        plans,
    })
}

pub fn finish(
    ctx: &mut InferCtx,
    scheme: &mut Scheme,
    solution: Option<Solution>,
    outer: &FxHashSet<TyVid>,
    span: Span,
) {
    let Some(Solution {
        registries,
        retained,
        plans,
    }) = solution
    else {
        return;
    };
    let own: FxHashSet<_> = scheme.vars.iter().copied().collect();
    crate::solve::report_ambiguous_element_dicts(
        ctx,
        &registries.classes,
        &plans,
        &own,
        outer,
        span,
    );
    for constraint in retained {
        if constraint.tys.iter().any(|var| own.contains(var)) {
            if !scheme.constraints.contains(&constraint) {
                scheme.constraints.push(constraint);
            }
        } else {
            ctx.deferred_constraints.push(constraint);
        }
    }
    ctx.dict_resolution_accum.extend(plans);
}

pub fn remember_scheme(ctx: &mut InferCtx, declaration: &ridge_ast::FnDecl, scheme: &Scheme) {
    ctx.write_node_type(
        declaration.name.span,
        ridge_resolve::NodeKind::Ident,
        &scheme.ty,
    );
    let Body::Expr(body) = &declaration.body else {
        return;
    };
    let (span, kind) = match body {
        Expr::Block(block) => (block.span, ridge_resolve::NodeKind::Block),
        Expr::Try { span, .. } => (*span, ridge_resolve::NodeKind::Try),
        expr => (expr.span(), ridge_resolve::NodeKind::Expr),
    };
    if let Some(node) = ctx
        .node_id_map
        .as_ref()
        .and_then(|nodes| nodes.get(span, kind))
    {
        ctx.schemes_accum.insert(node, scheme.clone());
    }
}
