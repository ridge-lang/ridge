//! Top-level item driver — §10 T11.
//!
//! Dispatches over AST [`Item`] variants to produce [`IrItem`]s, then
//! collects them into the `LoweredModule` that `lower_module` returns.
//!
//! # What this module does
//!
//! - `lower_item`  — the top-level dispatcher; returns `None` for erased items
//!   (`Item::Import`, `Item::Type`).
//! - `lower_fn`    — converts a [`FnDecl`] to an [`IrFn`].
//! - `lower_const` — converts a [`ConstDecl`] to an [`IrConst`].
//!
//! Actor lowering is delegated to [`crate::actor_lower::lower_actor`] which was
//! already implemented in T10.
//!
//! # Type / capability / scheme wiring
//!
//! - `IrFn.caps` / `IrInit.caps` / `IrHandler.caps` / Lambda caps are looked up
//!   via [`crate::ctx::LowerCtx::lookup_inferred_caps`] (proxy `NodeId(span.start)`
//!   contract shared with `ridge-typecheck`).
//! - `IrFn.ret_ty` / `IrParam.ty` / `IrConst.ty` / state-field `ty` are lowered
//!   from the AST `Type` annotations via `crate::ast_type::lower_ast_type`.
//! - Record/actor `TyConId`s resolve via
//!   [`crate::ctx::LowerCtx::lookup_tycon_by_name`].
//!
//! Placeholders resolved in the Phase 4.5 sweep (`PHASE45-T3+T4`): bare param
//! types are now looked up from `node_types`; `IrFn.scheme` is now looked up
//! from `TypedModule.schemes` keyed by body `NodeId`.
//!
//! # `is_main` detection
//!
//! A top-level `fn main` with no parameters (after the resolver strips any
//! capability annotations) is marked `is_main = true`.  The resolver already
//! validated that at most one such `fn` exists; the lowerer simply reflects the
//! marker.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{
    decl::{ConstDecl, FnDecl},
    module::Item,
    Body, Expr, Param, Visibility,
};
use ridge_ir::{IrConst, IrFfiFn, IrFn, IrItem, IrParam};
use ridge_resolve::{NodeId, NodeKind};
use ridge_types::{Scheme, Type};

use crate::actor_lower::lower_actor;
use crate::ast_type::lower_ast_type;
use crate::core::lower_expr;
use crate::ctx::LowerCtx;

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower a single top-level AST [`Item`] to an [`IrItem`], or `None` if the
/// item kind is erased at the IR level.
///
/// - `Item::Fn`    → `Some(IrItem::Fn(...))`
/// - `Item::Actor` → `Some(IrItem::Actor(...))`
/// - `Item::Const` → `Some(IrItem::Const(...))`
/// - `Item::Type`  → `None`  (type decls live in `TypedWorkspace.tycons`)
/// - `Item::Import`→ `None`  (fully resolved into the per-NodeId `BindingMap`)
pub fn lower_item(ctx: &mut LowerCtx<'_>, item: &Item) -> Option<IrItem> {
    match item {
        Item::Fn(decl) => {
            // @ffi-decorated functions have no Ridge body to lower — the
            // codegen layer emits a thin wrapper that calls the BEAM target
            // directly.  Emit IrItem::Ffi so that the wrapper function IS
            // defined in the Core Erlang module (fixes E004 "undefined function"
            // when same-module pure-Ridge functions reference the stub via
            // SymbolRef::Local).
            if let Body::Ffi {
                module: ffi_module,
                name: ffi_fn,
                arity: ffi_arity,
            } = &decl.body
            {
                // Synthesise parameter names p0, p1, … for the wrapper arity.
                //
                // Ridge call convention for 0-arity foreign functions: callers
                // always pass one extra unit argument (e.g. `_mapsNew ()`).
                // So when ffi_arity == 0, the wrapper must accept 1 param
                // (the dummy unit) but not forward it to the foreign call.
                // When ffi_arity > 0, the wrapper takes exactly ffi_arity
                // params and forwards all of them.
                let wrapper_arity = if *ffi_arity == 0 {
                    1usize
                } else {
                    *ffi_arity as usize
                };
                let params: Vec<String> = (0..wrapper_arity).map(|i| format!("p{i}")).collect();
                return Some(IrItem::Ffi(IrFfiFn {
                    name: decl.name.text.clone(),
                    ffi_module: ffi_module.clone(),
                    ffi_fn: ffi_fn.clone(),
                    ffi_call_arity: *ffi_arity,
                    params,
                    is_pub: matches!(decl.vis, Visibility::Pub),
                    span: decl.span,
                }));
            }
            Some(IrItem::Fn(lower_fn(ctx, decl)))
        }
        Item::Actor(decl) => Some(IrItem::Actor(lower_actor(ctx, decl))),
        Item::Const(decl) => Some(IrItem::Const(lower_const(ctx, decl))),
        Item::Type(_) | Item::Import(_) => None,
    }
}

/// Lower a top-level [`FnDecl`] to an [`IrFn`].
///
/// # Type and capability wiring
///
/// - `caps` — read from Phase 4's `inferred_caps` side-table via the proxy
///   `NodeId(decl.span.start)` (see [`LowerCtx::lookup_inferred_caps`]).
/// - `ret_ty` — lowered from the declared AST `Type` annotation via
///   `lower_ast_type`.  Falls back to `Type::Error` when no annotation is
///   present (inferred-only return type; cannot be resolved without `node_types`).
/// - `scheme` — looked up from `TypedModule.schemes` keyed by the fn body's
///   `NodeId` (resolved via `node_id_map.get(body_span, body_kind)`).  Falls back
///   to `Scheme::mono(Type::Error)` when no workspace or scheme entry is present.
///   PHASE45-T4: scheme lookup wired from TypedModule.schemes.
/// - param `ty` — lowered from the declared AST annotation; for bare (unannotated)
///   parameters the type is looked up from `node_types` via `node_id_map`.
///   PHASE45-T3: bare param types looked up from `node_types` via `node_id_map`.
///
/// # Propagation scope
///
/// Per §4.2, the fn's return type is pushed onto `propagation_scope_stack`
/// before lowering the body, and popped after.
///
/// # `is_main`
///
/// A fn named `"main"` at module top level is marked `is_main = true`.
pub fn lower_fn(ctx: &mut LowerCtx<'_>, decl: &FnDecl) -> IrFn {
    // PHASE45-T4: look up the generalised scheme from TypedModule.schemes early
    // so that bare-param types can be extracted from it (see param_to_ir_param).
    // The scheme is keyed by the body's NodeId; the body_kind mirrors the
    // logic in ridge-typecheck/src/scc.rs:309-312 (Block/Try/Expr).
    // Body::Ffi has no expression to lower — its codegen is handled in T3+ by
    // the codegen layer that consumes Body::Ffi directly.
    // TODO(T3): lower_fn must be skipped / re-routed for Body::Ffi; for now,
    // treat it as Body::Expr with a Type::Error body to keep the workspace green.
    let expr = match &decl.body {
        Body::Expr(e) => e,
        Body::Ffi { .. } => {
            // TODO(T3): codegen for @ffi bodies is wired in T3.
            // Returning a dummy IrFn is not possible here without an expression,
            // so we fall back to an early return with a placeholder.
            // This path is unreachable until T3 introduces stdlib compilation.
            unreachable!(
                "Body::Ffi encountered in lower_fn — T3 must re-route @ffi decls before lowering"
            )
        }
    };

    let scheme = lookup_fn_scheme(ctx, expr);

    // Resolve ret_ty from the declared annotation when present.
    // When absent, read the body's inferred type from node_types (PHASE45-T3+OQ-004).
    // The body NodeId is keyed by (body.span(), body_node_kind(body)) — the same
    // logic used by ridge-typecheck/scc.rs to key scheme write-back.
    let ret_ty = if let Some(ast_ty) = &decl.ret {
        lower_ast_type(ctx, ast_ty)
    } else {
        // PHASE45-T3+OQ-004: read body's inferred return type from node_types.
        let bkind = body_node_kind(expr);
        let bspan = match expr {
            Expr::Block(b) => b.span,
            Expr::Try { span, .. } => *span,
            other => other.span(),
        };
        ctx.node_id_map
            .as_ref()
            .and_then(|m| m.get(bspan, bkind))
            .and_then(|nid| ctx.node_type(nid).cloned())
            .unwrap_or(Type::Error)
    };

    // Push a propagation scope for `?` desugaring inside the body (§4.2).
    ctx.push_propagation_scope(ret_ty.clone());

    let body = lower_expr(ctx, expr);

    ctx.pop_propagation_scope();

    // PHASE45-T3: bare-param types are lifted from the scheme's Type::Fn
    // rather than looked up via NodeKind::Ident (ident spans carry no type).
    let params: Vec<IrParam> = decl
        .params
        .iter()
        .enumerate()
        .map(|(idx, p)| param_to_ir_param(ctx, &scheme, idx, p))
        .collect();

    let is_main = decl.name.text == "main";

    // Read the effective capability set from Phase 4's inferred_caps side-table.
    let caps = ctx.lookup_inferred_caps(decl.span);

    IrFn {
        name: decl.name.text.clone(),
        module: ctx.module_id,
        params,
        ret_ty,
        caps,
        scheme,
        body,
        // FnDecl items have no NodeId in the origin side-table; NodeId(0) is the
        // canonical placeholder (same as actor_lower uses for ActorDecl.origin).
        origin: NodeId(0),
        span: decl.span,
        is_pub: matches!(decl.vis, Visibility::Pub),
        is_main,
        doc: decl.doc.as_ref().map(|d| d.text.clone()),
    }
}

/// Lower a top-level [`ConstDecl`] to an [`IrConst`].
///
/// `ty` is lowered from the required AST type annotation via `lower_ast_type`.
pub fn lower_const(ctx: &mut LowerCtx<'_>, decl: &ConstDecl) -> IrConst {
    let value = lower_expr(ctx, &decl.value);
    let ty = lower_ast_type(ctx, &decl.ty);

    IrConst {
        name: decl.name.text.clone(),
        ty,
        value,
        // ConstDecl items have no NodeId in the origin side-table; placeholder.
        origin: NodeId(0),
        span: decl.span,
        is_pub: matches!(decl.vis, Visibility::Pub),
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Return the [`NodeKind`] used to key `body` in the `NodeIdMap`.
///
/// Mirrors the keying logic from `ridge-typecheck/src/scc.rs:309-312`:
/// - `Expr::Block` → `NodeKind::Block`
/// - `Expr::Try`   → `NodeKind::Try`
/// - anything else → `NodeKind::Expr`
///
/// Used by both [`lookup_fn_scheme`] and [`lower_fn`] (for body-based `ret_ty`).
const fn body_node_kind(body: &Expr) -> NodeKind {
    match body {
        Expr::Block(_) => NodeKind::Block,
        Expr::Try { .. } => NodeKind::Try,
        _ => NodeKind::Expr,
    }
}

/// Look up the generalised [`Scheme`] for a top-level `fn` body.
///
/// Mirrors the keying logic from `ridge-typecheck/src/scc.rs:309-312`:
/// `body_kind` is `NodeKind::Block` for `Expr::Block`, `NodeKind::Try` for
/// `Expr::Try`, and `NodeKind::Expr` for all other shapes.  The scheme is then
/// retrieved from the current `TypedModule.schemes` table (accessed via
/// `ctx.workspace.modules[ctx.module_id.0].schemes`).
///
/// Falls back to `Scheme::mono(Type::Error)` when the workspace is absent, the
/// module index is out of range, or no scheme entry exists for this body.
///
/// PHASE45-T4: scheme lookup wired from TypedModule.schemes.
fn lookup_fn_scheme(ctx: &LowerCtx<'_>, body: &Expr) -> Scheme {
    let body_kind = body_node_kind(body);
    let body_span = match body {
        Expr::Block(b) => b.span,
        Expr::Try { span, .. } => *span,
        other => other.span(),
    };

    ctx.node_id_map
        .as_ref()
        .and_then(|m| m.get(body_span, body_kind))
        .and_then(|nid| {
            ctx.workspace
                .and_then(|ws| ws.modules.get(ctx.module_id.0 as usize))
                .and_then(|tmod| tmod.schemes.get(&nid).cloned())
        })
        .unwrap_or_else(|| Scheme::mono(Type::Error))
}

/// Convert an AST [`Param`] to an [`IrParam`].
///
/// For `Param::Annotated` the declared type annotation is lowered via
/// [`lower_ast_type`].  For `Param::Bare` (no annotation) the type is lifted
/// from `scheme.ty` — the generalised [`Scheme`] for the enclosing fn (keyed
/// by body [`NodeId`], looked up from [`TypedModule::schemes`]).  The scheme's
/// inner `Type::Fn { params }` is indexed by `param_idx`.  Falls back to
/// `Type::Error` when the scheme is absent or the Fn shape doesn't match
/// (test scaffolding).
///
/// PHASE45-T3: bare param type lifted from the enclosing fn's scheme.
fn param_to_ir_param(
    ctx: &mut LowerCtx<'_>,
    scheme: &Scheme,
    param_idx: usize,
    param: &Param,
) -> IrParam {
    match param {
        Param::Bare(ident) => {
            // PHASE45-T3: lift param type from the enclosing fn's scheme.
            // The scheme's Type::Fn { params } carries the fully-generalised
            // parameter types resolved after SCC constraint solving.
            let ty = if let Type::Fn { params, .. } = &scheme.ty {
                params.get(param_idx).cloned().unwrap_or(Type::Error)
            } else {
                Type::Error
            };
            IrParam {
                name: ident.text.clone(),
                ty,
                span: ident.span,
            }
        }
        Param::Annotated { name, ty, span } => IrParam {
            name: name.text.clone(),
            ty: lower_ast_type(ctx, ty),
            span: *span,
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{decl::FnDecl, Expr, Ident, Literal, Span};
    use ridge_ir::{IrExpr, IrItem, IrLit};
    use ridge_resolve::ModuleId;

    fn sp() -> Span {
        Span::point(0)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn ident(text: &str) -> Ident {
        Ident {
            text: text.into(),
            span: sp(),
        }
    }

    fn int_lit(n: &str) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.into(),
            span: sp(),
        })
    }

    fn simple_fn_decl(name: &str, body: Expr) -> FnDecl {
        FnDecl {
            vis: ridge_ast::Visibility::Private,
            caps: vec![],
            name: ident(name),
            params: vec![],
            ret: None,
            body: Body::Expr(body),
            span: sp(),
            doc: None,
        }
    }

    // ── item-1: lower_fn produces IrFn with correct name ─────────────────────

    #[test]
    fn lower_fn_name_and_body() {
        let mut ctx = fresh_ctx();
        let decl = simple_fn_decl("hello", int_lit("42"));
        let f = lower_fn(&mut ctx, &decl);

        assert_eq!(f.name, "hello");
        assert!(!f.is_pub);
        assert!(!f.is_main);
        assert_eq!(f.module, ModuleId(0));
        assert!(f.params.is_empty());
        match &f.body {
            IrExpr::Lit {
                value: IrLit::Int(42),
                ..
            } => {}
            other => panic!("expected Int(42), got {other:?}"),
        }
    }

    // ── item-2: lower_fn marks main correctly ─────────────────────────────────

    #[test]
    fn lower_fn_marks_main() {
        let mut ctx = fresh_ctx();
        let decl = simple_fn_decl("main", Expr::Unit(sp()));
        let f = lower_fn(&mut ctx, &decl);
        assert!(f.is_main, "fn main must have is_main = true");
    }

    // ── item-3: lower_fn propagation scope is balanced ────────────────────────

    #[test]
    fn lower_fn_propagation_scope_balanced() {
        let mut ctx = fresh_ctx();
        assert!(ctx.current_propagation_scope().is_none());
        let decl = simple_fn_decl("f", Expr::Unit(sp()));
        let _ = lower_fn(&mut ctx, &decl);
        assert!(
            ctx.current_propagation_scope().is_none(),
            "propagation scope stack must be balanced after lower_fn"
        );
    }

    // ── item-4: lower_const produces IrConst with correct name ────────────────

    #[test]
    fn lower_const_name_and_value() {
        use ridge_ast::decl::ConstDecl;

        let mut ctx = fresh_ctx();
        let decl = ConstDecl {
            vis: ridge_ast::Visibility::Pub,
            name: ident("MAX_RETRIES"),
            ty: ridge_ast::Type::Named {
                name: ident("Int"),
                span: sp(),
            },
            value: int_lit("3"),
            span: sp(),
            doc: None,
        };
        let c = lower_const(&mut ctx, &decl);

        assert_eq!(c.name, "MAX_RETRIES");
        assert!(c.is_pub);
        match &c.value {
            IrExpr::Lit {
                value: IrLit::Int(3),
                ..
            } => {}
            other => panic!("expected Int(3), got {other:?}"),
        }
    }

    // ── item-5: lower_item dispatches to None for Type and Import ─────────────

    #[test]
    fn lower_item_erases_type_and_import() {
        use ridge_ast::{
            decl::{ImportDecl, ModulePath, TypeDecl},
            module::Item,
            TypeBody,
        };

        let mut ctx = fresh_ctx();

        let type_item = Item::Type(TypeDecl {
            vis: ridge_ast::Visibility::Private,
            name: ident("MyType"),
            params: vec![],
            body: TypeBody::Alias(ridge_ast::Type::Named {
                name: ident("Int"),
                span: sp(),
            }),
            span: sp(),
            doc: None,
        });
        assert!(lower_item(&mut ctx, &type_item).is_none());

        let import_item = Item::Import(ImportDecl {
            path: ModulePath {
                segments: vec![ident("std"), ident("list")],
                span: sp(),
            },
            alias: None,
            items: None,
            span: sp(),
            doc: None,
        });
        assert!(lower_item(&mut ctx, &import_item).is_none());
    }

    // ── item-6: lower_item dispatches Fn to IrItem::Fn ───────────────────────

    #[test]
    fn lower_item_fn_dispatches_correctly() {
        use ridge_ast::module::Item;

        let mut ctx = fresh_ctx();
        let item = Item::Fn(simple_fn_decl("my_fn", Expr::Unit(sp())));
        let ir = lower_item(&mut ctx, &item);
        assert!(
            matches!(ir, Some(IrItem::Fn(ref f)) if f.name == "my_fn"),
            "expected IrItem::Fn, got {ir:?}"
        );
    }

    // ── item-7: pub fn is_pub = true ─────────────────────────────────────────

    #[test]
    fn lower_fn_pub_flag() {
        let mut ctx = fresh_ctx();
        let decl = FnDecl {
            vis: ridge_ast::Visibility::Pub,
            caps: vec![],
            name: ident("exported"),
            params: vec![],
            ret: None,
            body: Body::Expr(Expr::Unit(sp())),
            span: sp(),
            doc: None,
        };
        let f = lower_fn(&mut ctx, &decl);
        assert!(f.is_pub);
    }
}
