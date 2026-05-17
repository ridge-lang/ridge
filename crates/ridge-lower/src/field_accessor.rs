//! Field-accessor shorthand lowering `(.name)` — §4.10.
//!
//! `Expr::FieldAccessorFn { field }` lowers to an `IrExpr::Lambda` that
//! takes one parameter and projects the named field from it.
//!
//! # Rule (§4.10)
//!
//! ```text
//! lower_field_accessor(field, span) =
//!     let x = fresh_local("__field_arg")
//!     IrExpr::Lambda {
//!         params: [IrParam { name: x, ty: Type::Error, span }],
//!         body:   IrExpr::Field {
//!                     base:  IrExpr::Local { name: x, span },
//!                     field: field.text,
//!                     span,
//!                 },
//!         caps:   CapabilitySet::PURE,
//!         span,
//!     }
//! ```
//!
//! `ty: Type::Error` is a placeholder because `node_types` is not wired yet
//! (T17).  The correct type is `Fn { params: [record_ty], ret: field_ty, caps: ∅ }`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{Ident, Span};
use ridge_ir::{IrExpr, IrParam};
use ridge_resolve::NodeKind;
use ridge_types::{CapabilitySet, Type};

use crate::ctx::LowerCtx;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower `(.field)` to `IrExpr::Lambda { params: [x], body: x.field }`.
///
/// The synthesised parameter name is `__field_arg_N` (via `ctx.fresh_local`),
/// where `N` is the module-wide synthetic-name counter (R6 — globally unique
/// within the module).
///
/// # Type placeholder
///
/// The parameter type is `Type::Error` until `ridge-typecheck`'s `node_types`
/// side-table is wired (T17).
///
/// # Capability
///
/// Field projection is pure (`CapabilitySet::PURE`).
pub fn lower_field_accessor(ctx: &mut LowerCtx<'_>, field: &Ident, span: Span) -> IrExpr {
    // Allocate a globally-unique synthetic parameter name.
    let param_name = ctx.fresh_local("__field_arg");

    // Allocate IR node IDs for: Lambda, Field, Local (inside Field), IrParam.
    let lambda_id = ctx.fresh_id(None);
    let local_id = ctx.fresh_id(None);
    let field_id = ctx.fresh_id(None);

    // `IrExpr::Local { name: param_name, .. }` — the lambda parameter reference.
    let base = Box::new(IrExpr::Local {
        id: local_id,
        name: param_name.clone(),
        span,
    });

    // `IrExpr::Field { base, field: field.text }` — the projection body.
    let body = Box::new(IrExpr::Field {
        id: field_id,
        base,
        field: field.text.clone(),
        span,
    });

    // PHASE45-T3: look up the field-accessor lambda type from node_types
    // via `(span, NodeKind::Expr)` and lift `params[0]` as the synthetic
    // parameter type. Falls back to `Type::Error` when the mapping is absent
    // (test scaffolding, or no node_id_map attached).
    let param_ty = ctx
        .node_id_map
        .as_ref()
        .and_then(|m| m.get(span, NodeKind::Expr))
        .and_then(|nid| ctx.node_type(nid).cloned())
        .and_then(|ty| {
            if let Type::Fn { params, .. } = ty {
                params.into_iter().next()
            } else {
                None
            }
        })
        .unwrap_or(Type::Error);
    let param = IrParam {
        name: param_name,
        ty: param_ty,
        span,
    };

    IrExpr::Lambda {
        id: lambda_id,
        params: vec![param],
        body,
        caps: CapabilitySet::PURE,
        span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Ident, Span};
    use ridge_ir::IrExpr;
    use ridge_resolve::ModuleId;
    use ridge_types::CapabilitySet;

    fn sp() -> Span {
        Span::point(0)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn field_ident(name: &str) -> Ident {
        Ident {
            text: name.into(),
            span: sp(),
        }
    }

    // ── T4-fa-1: (.name) lowers to Lambda { params: [__field_arg_0], body: .name } ──

    #[test]
    fn field_accessor_lambda_shape() {
        let mut ctx = fresh_ctx();
        let span = sp();
        let field = field_ident("name");

        let ir = lower_field_accessor(&mut ctx, &field, span);

        match ir {
            IrExpr::Lambda {
                ref params,
                ref body,
                caps,
                span: s,
                ..
            } => {
                assert_eq!(s, span, "span must be preserved");
                assert_eq!(caps, CapabilitySet::PURE, "accessor must be pure");

                // Exactly one parameter named `__field_arg_0`.
                assert_eq!(params.len(), 1, "must have exactly one param");
                assert_eq!(
                    params[0].name, "__field_arg_0",
                    "param name must be __field_arg_0"
                );
                assert!(
                    matches!(params[0].ty, ridge_types::Type::Error),
                    "param type is Error placeholder until T17; got {:?}",
                    params[0].ty
                );

                // Body is a Field projection with base = Local(__field_arg_0).
                match body.as_ref() {
                    IrExpr::Field {
                        base, field: fname, ..
                    } => {
                        assert_eq!(fname, "name", "field name must match");
                        match base.as_ref() {
                            IrExpr::Local { name, .. } => {
                                assert_eq!(
                                    name, "__field_arg_0",
                                    "base must be Local(__field_arg_0)"
                                );
                            }
                            other => panic!("expected Local base, got {other:?}"),
                        }
                    }
                    other => panic!("expected Field body, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Lambda, got {other:?}"),
        }

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── T4-fa-2: two successive (.name) calls produce unique counter values ────
    //
    // Verifies that `ctx.fresh_local` increments the shared module counter so
    // that both `__field_arg_0` and `__field_arg_1` are produced.

    #[test]
    fn field_accessor_unique_per_call() {
        let mut ctx = fresh_ctx();
        let field = field_ident("name");

        let ir0 = lower_field_accessor(&mut ctx, &field, sp());
        let ir1 = lower_field_accessor(&mut ctx, &field, sp());

        let name0 = match &ir0 {
            IrExpr::Lambda { params, .. } => params[0].name.clone(),
            other => panic!("expected Lambda, got {other:?}"),
        };
        let name1 = match &ir1 {
            IrExpr::Lambda { params, .. } => params[0].name.clone(),
            other => panic!("expected Lambda, got {other:?}"),
        };

        assert_eq!(name0, "__field_arg_0");
        assert_eq!(name1, "__field_arg_1");
        assert_ne!(name0, name1, "each call must produce a distinct name");
    }

    // ── T4-fa-3: field name is faithfully preserved ───────────────────────────

    #[test]
    fn field_accessor_field_name_preserved() {
        let mut ctx = fresh_ctx();
        let field = field_ident("email");

        let ir = lower_field_accessor(&mut ctx, &field, sp());

        match ir {
            IrExpr::Lambda { body, .. } => match body.as_ref() {
                IrExpr::Field { field: fname, .. } => {
                    assert_eq!(fname, "email");
                }
                other => panic!("expected Field body, got {other:?}"),
            },
            other => panic!("expected Lambda, got {other:?}"),
        }
    }
}
