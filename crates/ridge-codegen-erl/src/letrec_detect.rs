//! Recursive inner-fn detection for `LetIn` → `letrec` promotion (OQ-L012).
//!
//! Phase 5 emits recursive inner fns as `LetIn(Bind(name, None), Lambda, body)`.
//! During Phase 6 lowering, the `LetIn` arm calls [`body_references_local`] on
//! the lambda body to decide whether to emit `letrec` (self-referencing) or a
//! plain `let` (non-recursive).

// letrec_detect is consumed from expr.rs and from the module-level entry points.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]

use ridge_ir::IrExpr;

/// Walk `body` and return `true` iff any `IrExpr::Local { name }` matching
/// `target_name` appears anywhere in the tree.
///
/// Used by the `LetIn` arm of [`crate::expr::lower_expr_in_scope`] to detect
/// self-references so it can emit `letrec` instead of `let`.
///
/// Per OQ-L012 (Phase 5): "Phase 5 emits recursive inner fns as
/// `LetIn(Bind, Lambda)` — the recursion is handled by the binding map".
/// Phase 6 detects self-reference by walking the lambda body.
pub(crate) fn body_references_local(body: &IrExpr, target_name: &str) -> bool {
    match body {
        IrExpr::Local { name, .. } => name == target_name,
        IrExpr::Block { stmts, .. } => stmts.iter().any(|s| body_references_local(s, target_name)),
        IrExpr::LetIn { value, body, .. } | IrExpr::VarIn { value, body, .. } => {
            body_references_local(value, target_name) || body_references_local(body, target_name)
        }
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            body_references_local(scrutinee, target_name)
                || arms.iter().any(|arm| {
                    arm.when
                        .as_ref()
                        .is_some_and(|w| body_references_local(w, target_name))
                        || body_references_local(&arm.body, target_name)
                })
        }
        IrExpr::Call { callee, args, .. } => {
            body_references_local(callee, target_name)
                || args.iter().any(|a| body_references_local(a, target_name))
        }
        IrExpr::Lambda { body, .. } => body_references_local(body, target_name),
        IrExpr::Return { value, .. } | IrExpr::Assign { value, .. } => {
            body_references_local(value, target_name)
        }
        IrExpr::Construct { fields, .. } => fields
            .iter()
            .any(|(_, v)| body_references_local(v, target_name)),
        IrExpr::Field { base, .. } => body_references_local(base, target_name),
        IrExpr::Tuple { elems, .. } | IrExpr::ListLit { elems, .. } => {
            elems.iter().any(|e| body_references_local(e, target_name))
        }
        IrExpr::Cons { head, tail, .. } => {
            body_references_local(head, target_name) || body_references_local(tail, target_name)
        }
        IrExpr::Send { handle, args, .. }
        | IrExpr::Ask { handle, args, .. }
        | IrExpr::TryAsk { handle, args, .. } => {
            body_references_local(handle, target_name)
                || args.iter().any(|a| body_references_local(a, target_name))
        }
        IrExpr::Spawn { args, .. } | IrExpr::ChildSpec { args, .. } => {
            args.iter().any(|a| body_references_local(a, target_name))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::Span;
    use ridge_ir::{IrExpr, IrLit, IrNodeId, IrPat};

    fn sp() -> Span {
        Span::point(0)
    }

    fn node() -> IrNodeId {
        IrNodeId(0)
    }

    fn local(name: &str) -> IrExpr {
        IrExpr::Local {
            id: node(),
            name: name.into(),
            span: sp(),
        }
    }

    fn lit_int(n: i64) -> IrExpr {
        IrExpr::Lit {
            id: node(),
            value: IrLit::Int(n),
            span: sp(),
        }
    }

    #[test]
    fn detects_direct_local_ref() {
        let body = IrExpr::Block {
            id: node(),
            stmts: vec![local("f")],
            span: sp(),
        };
        assert!(body_references_local(&body, "f"));
        assert!(!body_references_local(&body, "g"));
    }

    #[test]
    fn detects_in_nested_letin() {
        let body = IrExpr::LetIn {
            id: node(),
            pat: IrPat::Bind {
                name: "tmp".into(),
                inner: None,
                span: sp(),
            },
            value: Box::new(lit_int(1)),
            body: Box::new(local("f")),
            span: sp(),
        };
        assert!(body_references_local(&body, "f"));
    }
}
