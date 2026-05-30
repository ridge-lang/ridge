//! `with`-update expression lowering — §4.5.
//!
//! `Expr::With { base, fields, span }` lowers to a partial record update:
//!
//! ```text
//! IrExpr::RecordUpdate {
//!     base:    lower_expr(base),
//!     updates: [(field, lower_expr(value)) for each touched field],
//! }
//! ```
//!
//! Only the **touched** fields are emitted; the backend's map update preserves
//! every other field of `base`.  Because no record schema is consulted, this
//! works even when the concrete record type of `base` is not statically known
//! at this point — for example an unannotated closure parameter, whose type is
//! fixed only when the closure is applied.
//!
//! # Edge cases
//!
//! - **Chained `with`:** `u with { a=1 } with { b=2 }` is left-associative; the
//!   outer call lowers the inner `RecordUpdate` as its `base` via recursion.
//! - **Shorthand field (`u with { name }`):** the shorthand pulls `name` from
//!   the local environment via `IrExpr::Local { name }`, NOT from the base.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{expr::FieldInit, Expr, Span};
use ridge_ir::IrExpr;

use crate::core::lower_expr;
use crate::ctx::LowerCtx;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower `base with { fields }` to `IrExpr::RecordUpdate { base, updates }`.
///
/// The update is partial: only the touched fields appear in `updates`. No record
/// schema is needed, so a `base` of statically-unknown concrete record type
/// (e.g. an unannotated closure parameter) lowers correctly.
pub fn lower_with(ctx: &mut LowerCtx<'_>, base: &Expr, fields: &[FieldInit], span: Span) -> IrExpr {
    // A `with` update is a *partial* map update over `base`: only the touched
    // fields are emitted, every other field is preserved by the backend's map
    // update.  This needs no record schema, so it works even when the concrete
    // record type of `base` is not statically known here — notably an
    // unannotated closure parameter, whose type is fixed only at the call site.
    let lowered_base = lower_expr(ctx, base);

    let updates: Vec<(String, IrExpr)> = fields
        .iter()
        .map(|f| {
            let value_ir = if let Some(v) = &f.value {
                lower_expr(ctx, v)
            } else {
                // Shorthand `with { name }` — pull `name` from the local
                // environment, not from the base.
                let id = ctx.fresh_id(None);
                IrExpr::Local {
                    id,
                    name: f.name.text.clone(),
                    span: f.span,
                }
            };
            (f.name.text.clone(), value_ir)
        })
        .collect();

    let id = ctx.fresh_id(None);
    IrExpr::RecordUpdate {
        id,
        base: Box::new(lowered_base),
        updates,
        span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Ident, Literal};
    use ridge_resolve::ModuleId;

    fn sp() -> Span {
        Span::point(0)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn int_lit_expr(n: i64) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.to_string(),
            span: sp(),
        })
    }

    fn make_field_init(name: &str, value: Option<Expr>) -> FieldInit {
        FieldInit {
            name: Ident {
                text: name.into(),
                span: sp(),
            },
            value,
            span: sp(),
        }
    }

    /// `with` lowers to a `RecordUpdate` carrying only the touched fields — and,
    /// crucially, does so with no workspace / schema and emits no error. This is
    /// the regression guard for the closure-`with` miscompile: the old path
    /// returned `Unit` (which became BEAM `'ok'`) whenever the schema could not
    /// be resolved.
    #[test]
    fn with_lowers_to_record_update_without_schema() {
        let mut ctx = fresh_ctx();
        let base = int_lit_expr(0);
        let fields = vec![make_field_init("v", Some(int_lit_expr(5)))];

        let ir = lower_with(&mut ctx, &base, &fields, sp());

        match ir {
            IrExpr::RecordUpdate { updates, .. } => {
                assert_eq!(updates.len(), 1, "one touched field");
                assert_eq!(updates[0].0, "v");
            }
            other => panic!("expected RecordUpdate, got {other:?}"),
        }
        assert!(
            ctx.errors.is_empty(),
            "lowering `with` must not need a schema or emit errors; got: {:?}",
            ctx.errors
        );
    }

    /// A shorthand field (`u with { name }`) pulls `name` from the local
    /// environment, emitted as `IrExpr::Local`.
    #[test]
    fn with_shorthand_pulls_from_local() {
        let mut ctx = fresh_ctx();
        let base = int_lit_expr(0);
        let fields = vec![make_field_init("name", None)];

        let ir = lower_with(&mut ctx, &base, &fields, sp());

        match ir {
            IrExpr::RecordUpdate { updates, .. } => {
                assert_eq!(updates.len(), 1);
                assert_eq!(updates[0].0, "name");
                match &updates[0].1 {
                    IrExpr::Local { name, .. } => assert_eq!(name, "name"),
                    other => panic!("shorthand must lower to Local, got {other:?}"),
                }
            }
            other => panic!("expected RecordUpdate, got {other:?}"),
        }
    }
}
