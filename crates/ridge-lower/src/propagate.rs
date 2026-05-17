//! `?` propagation desugaring rule — §4.2.
//!
//! Converts `Expr::Propagate { inner, span }` into an `IrExpr::Match` that
//! inspects whether the inner value is `Ok`/`Err` (Result context) or
//! `Some`/`None` (Option context).
//!
//! # Propagation scope
//!
//! The enclosing scope type is read from `ctx.current_propagation_scope()`.
//! An absent scope emits `L003` (`PropagateOutsideScope`) and returns a
//! `Unit` stub.  A double-`?` on the inner expression emits `L004`
//! (`DoublePropagate`) and then proceeds with lowering the inner.
//!
//! # Type-constructor dispatch
//!
//! The built-in `TyCon` indices are stable (assigned in `BuiltinTyCons::allocate`):
//! - `Option` → `TyConId(9)`
//! - `Result` → `TyConId(10)`
//!
//! We match on `Type::Con(id, _)` using these constants.  An unrecognised
//! head emits `L999` and returns a `Unit` stub.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{Expr, Span};
use ridge_ir::{IrArm, IrExpr, IrLit, IrPat, SymbolRef};
use ridge_types::{TyConId, Type};

use crate::core::lower_expr;
use crate::ctx::LowerCtx;
use crate::error::LowerError;

// ── TyCon id constants (must match BuiltinTyCons::allocate order) ─────────────

/// `Option a` — `TyConId` assigned at index 9.
const OPTION_TYCON: TyConId = TyConId(9);
/// `Result a e` — `TyConId` assigned at index 10.
const RESULT_TYCON: TyConId = TyConId(10);

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower `Expr::Propagate { inner, span }` to `IrExpr::Match`.
///
/// Reads the enclosing propagation scope from `ctx`; emits `L003` if absent.
/// Emits `L004` if `inner` is itself a `Propagate` (double-`?`).
/// Never panics.
pub fn lower_propagate(ctx: &mut LowerCtx<'_>, inner: &Expr, span: Span) -> IrExpr {
    // ── L004: double-propagate check ─────────────────────────────────────────
    if let Expr::Propagate { .. } = inner {
        ctx.errors.push(LowerError::DoublePropagate { span });
        // Continue — still lower the inner expression to keep the tree valid.
    }

    // ── L003: scope check ─────────────────────────────────────────────────────
    let scope_ty = if let Some(ty) = ctx.current_propagation_scope() {
        ty.clone()
    } else {
        ctx.errors.push(LowerError::PropagateOutsideScope { span });
        let id = ctx.fresh_id(None);
        return IrExpr::Lit {
            id,
            value: IrLit::Unit,
            span,
        };
    };

    // ── Dispatch on the head type-constructor ─────────────────────────────────
    match &scope_ty {
        Type::Con(id, _) if *id == RESULT_TYCON => lower_propagate_result(ctx, inner, span),
        Type::Con(id, _) if *id == OPTION_TYCON => lower_propagate_option(ctx, inner, span),
        _other => {
            ctx.errors.push(LowerError::InternalLoweringError {
                span,
                message: "propagation scope is neither Result nor Option".into(),
            });
            let id = ctx.fresh_id(None);
            IrExpr::Lit {
                id,
                value: IrLit::Unit,
                span,
            }
        }
    }
}

// ── Result context ────────────────────────────────────────────────────────────

/// Build the `Match` for a Result-context `?`.
///
/// ```text
/// match inner {
///   Ok  __prop_ok_N  → __prop_ok_N
///   Err __prop_err_N → return Err __prop_err_N
/// }
/// ```
fn lower_propagate_result(ctx: &mut LowerCtx<'_>, inner: &Expr, span: Span) -> IrExpr {
    let id = ctx.fresh_id(None);
    let scrutinee = Box::new(lower_expr(ctx, inner));

    let x_name = ctx.fresh_local("__prop_ok");
    let e_name = ctx.fresh_local("__prop_err");

    // arm 0: Ok __prop_ok_N → __prop_ok_N
    let ok_arm = {
        let local_id = ctx.fresh_id(None);
        IrArm {
            pat: IrPat::Ctor {
                sym: SymbolRef::Prelude { name: "Ok".into() },
                fields: vec![],
                args: vec![IrPat::Bind {
                    name: x_name.clone(),
                    inner: None,
                    span,
                }],
                span,
            },
            when: None,
            body: IrExpr::Local {
                id: local_id,
                name: x_name,
                span,
            },
            span,
        }
    };

    // arm 1: Err __prop_err_N → return Err __prop_err_N
    let err_arm = {
        let local_id = ctx.fresh_id(None);
        let construct_id = ctx.fresh_id(None);
        let return_id = ctx.fresh_id(None);
        IrArm {
            pat: IrPat::Ctor {
                sym: SymbolRef::Prelude { name: "Err".into() },
                fields: vec![],
                args: vec![IrPat::Bind {
                    name: e_name.clone(),
                    inner: None,
                    span,
                }],
                span,
            },
            when: None,
            body: IrExpr::Return {
                id: return_id,
                value: Box::new(IrExpr::Construct {
                    id: construct_id,
                    ctor: SymbolRef::Prelude { name: "Err".into() },
                    fields: vec![(
                        "$0".into(),
                        IrExpr::Local {
                            id: local_id,
                            name: e_name,
                            span,
                        },
                    )],
                    span,
                }),
                span,
            },
            span,
        }
    };

    IrExpr::Match {
        id,
        scrutinee,
        arms: vec![ok_arm, err_arm],
        span,
    }
}

// ── Option context ────────────────────────────────────────────────────────────

/// Build the `Match` for an Option-context `?`.
///
/// ```text
/// match inner {
///   Some __prop_some_N → __prop_some_N
///   None               → return None
/// }
/// ```
fn lower_propagate_option(ctx: &mut LowerCtx<'_>, inner: &Expr, span: Span) -> IrExpr {
    let id = ctx.fresh_id(None);
    let scrutinee = Box::new(lower_expr(ctx, inner));

    let x_name = ctx.fresh_local("__prop_some");

    // arm 0: Some __prop_some_N → __prop_some_N
    let some_arm = {
        let local_id = ctx.fresh_id(None);
        IrArm {
            pat: IrPat::Ctor {
                sym: SymbolRef::Prelude {
                    name: "Some".into(),
                },
                fields: vec![],
                args: vec![IrPat::Bind {
                    name: x_name.clone(),
                    inner: None,
                    span,
                }],
                span,
            },
            when: None,
            body: IrExpr::Local {
                id: local_id,
                name: x_name,
                span,
            },
            span,
        }
    };

    // arm 1: None → return None
    let none_arm = {
        let construct_id = ctx.fresh_id(None);
        let return_id = ctx.fresh_id(None);
        IrArm {
            pat: IrPat::Ctor {
                sym: SymbolRef::Prelude {
                    name: "None".into(),
                },
                fields: vec![],
                args: vec![],
                span,
            },
            when: None,
            body: IrExpr::Return {
                id: return_id,
                value: Box::new(IrExpr::Construct {
                    id: construct_id,
                    ctor: SymbolRef::Prelude {
                        name: "None".into(),
                    },
                    fields: vec![],
                    span,
                }),
                span,
            },
            span,
        }
    };

    IrExpr::Match {
        id,
        scrutinee,
        arms: vec![some_arm, none_arm],
        span,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Literal, Span};
    use ridge_ir::{IrExpr, IrLit, IrPat, SymbolRef};
    use ridge_resolve::ModuleId;
    use ridge_types::{TyConId, Type};

    use crate::ctx::LowerCtx;

    fn sp() -> Span {
        Span::point(0)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn unit_expr() -> Expr {
        Expr::Unit(sp())
    }

    fn result_ty() -> Type {
        // Result a e — TyConId(10)
        Type::Con(
            TyConId(10),
            vec![Type::Con(TyConId(0), vec![]), Type::Con(TyConId(0), vec![])],
        )
    }

    fn option_ty() -> Type {
        // Option a — TyConId(9)
        Type::Con(TyConId(9), vec![Type::Con(TyConId(0), vec![])])
    }

    // ── T7-prop-1: Result scope → Match with Ok and Err arms ─────────────────

    #[test]
    fn propagate_result_scope_produces_match_ok_err() {
        let mut ctx = fresh_ctx();
        ctx.push_propagation_scope(result_ty());

        let inner = unit_expr();
        let ir = lower_propagate(&mut ctx, &inner, sp());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Match { arms, .. } => {
                assert_eq!(arms.len(), 2, "expected exactly 2 arms");

                // arm 0: Ok pattern
                match &arms[0].pat {
                    IrPat::Ctor {
                        sym, args, fields, ..
                    } => {
                        assert_eq!(fields.len(), 0);
                        assert_eq!(args.len(), 1);
                        match sym {
                            SymbolRef::Prelude { name } => {
                                assert_eq!(name, "Ok", "arm 0 must be Ok");
                            }
                            other => panic!("expected Prelude sym for arm 0, got {other:?}"),
                        }
                    }
                    other => panic!("expected IrPat::Ctor for arm 0, got {other:?}"),
                }

                // arm 1: Err pattern
                match &arms[1].pat {
                    IrPat::Ctor {
                        sym, args, fields, ..
                    } => {
                        assert_eq!(fields.len(), 0);
                        assert_eq!(args.len(), 1);
                        match sym {
                            SymbolRef::Prelude { name } => {
                                assert_eq!(name, "Err", "arm 1 must be Err");
                            }
                            other => panic!("expected Prelude sym for arm 1, got {other:?}"),
                        }
                    }
                    other => panic!("expected IrPat::Ctor for arm 1, got {other:?}"),
                }

                // Both arms have no guard
                assert!(arms[0].when.is_none());
                assert!(arms[1].when.is_none());
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T7-prop-2: Option scope → Match with Some and None arms ──────────────

    #[test]
    fn propagate_option_scope_produces_match_some_none() {
        let mut ctx = fresh_ctx();
        ctx.push_propagation_scope(option_ty());

        let inner = unit_expr();
        let ir = lower_propagate(&mut ctx, &inner, sp());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Match { arms, .. } => {
                assert_eq!(arms.len(), 2, "expected exactly 2 arms");

                // arm 0: Some
                match &arms[0].pat {
                    IrPat::Ctor { sym, .. } => match sym {
                        SymbolRef::Prelude { name } => {
                            assert_eq!(name, "Some", "arm 0 must be Some");
                        }
                        other => panic!("expected Prelude(Some), got {other:?}"),
                    },
                    other => panic!("expected IrPat::Ctor for arm 0, got {other:?}"),
                }

                // arm 1: None
                match &arms[1].pat {
                    IrPat::Ctor { sym, args, .. } => {
                        assert_eq!(args.len(), 0, "None has no payload");
                        match sym {
                            SymbolRef::Prelude { name } => {
                                assert_eq!(name, "None", "arm 1 must be None");
                            }
                            other => panic!("expected Prelude(None), got {other:?}"),
                        }
                    }
                    other => panic!("expected IrPat::Ctor for arm 1, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }

    // ── T7-prop-3: no scope → L003 + Unit stub ───────────────────────────────

    #[test]
    fn propagate_outside_scope_emits_l003() {
        let mut ctx = fresh_ctx();
        // No scope pushed.

        let inner = unit_expr();
        let ir = lower_propagate(&mut ctx, &inner, sp());

        assert_eq!(
            ctx.errors.len(),
            1,
            "expected exactly 1 error; got: {:?}",
            ctx.errors
        );
        assert_eq!(ctx.errors[0].code(), "L003");

        match ir {
            IrExpr::Lit {
                value: IrLit::Unit, ..
            } => {}
            other => panic!("expected Unit stub, got {other:?}"),
        }
    }

    // ── T7-prop-4: Err arm body is Return { Construct { Err, [($0, Local)] } } ─

    #[test]
    fn propagate_result_err_arm_body_shape() {
        let mut ctx = fresh_ctx();
        ctx.push_propagation_scope(result_ty());

        let inner = Expr::Literal(Literal::IntDec {
            raw: "42".into(),
            span: sp(),
        });
        let ir = lower_propagate(&mut ctx, &inner, sp());

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Match { arms, .. } => {
                // arm 1 is the Err arm
                match &arms[1].body {
                    IrExpr::Return { value, .. } => match value.as_ref() {
                        IrExpr::Construct { ctor, fields, .. } => {
                            match ctor {
                                SymbolRef::Prelude { name } => {
                                    assert_eq!(name, "Err", "Construct ctor must be Err");
                                }
                                other => panic!("expected Prelude(Err) ctor, got {other:?}"),
                            }
                            assert_eq!(fields.len(), 1, "Construct must have 1 field");
                            assert_eq!(fields[0].0, "$0", "field name must be $0");
                            match &fields[0].1 {
                                IrExpr::Local { name, .. } => {
                                    assert!(
                                        name.starts_with("__prop_err"),
                                        "local name must start with __prop_err; got {name}"
                                    );
                                }
                                other => panic!("expected Local(__prop_err_N), got {other:?}"),
                            }
                        }
                        other => panic!("expected Construct inside Return, got {other:?}"),
                    },
                    other => panic!("expected IrExpr::Return for Err arm body, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Match, got {other:?}"),
        }
    }
}
