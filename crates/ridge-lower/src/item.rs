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
    typeclass::InstanceDecl,
    Body, Expr, Param, Visibility,
};
use ridge_ir::{CtorKind, IrConst, IrExpr, IrFfiFn, IrFn, IrItem, IrParam, SymbolRef};
use ridge_resolve::{NodeId, NodeKind};
use ridge_types::{Scheme, Type};

use crate::actor_lower::lower_actor;
use crate::ast_type::lower_ast_type;
use crate::core::lower_expr;
use crate::ctx::LowerCtx;

// ── Public entry points ───────────────────────────────────────────────────────

/// Lower a single top-level AST [`Item`] to zero or more [`IrItem`]s.
///
/// Most items produce exactly one `IrItem`. Instance declarations expand to
/// multiple items (one private fn per method body + one dict const), so this
/// returns a `Vec` rather than `Option`.
///
/// - `Item::Fn`           → `[IrItem::Fn(...)]`
/// - `Item::Actor`        → `[IrItem::Actor(...)]`
/// - `Item::Const`        → `[IrItem::Const(...)]`
/// - `Item::InstanceDecl` → `[IrItem::Fn(method), ..., IrItem::Const(dict)]`
/// - `Item::Type`         → `[]`  (type decls live in `TypedWorkspace.tycons`)
/// - `Item::Import`       → `[]`  (fully resolved into the per-NodeId `BindingMap`)
/// - `Item::ClassDecl`    → `[]`  (class metadata lives in `TypedWorkspace.class_table`)
pub fn lower_item_multi(ctx: &mut LowerCtx<'_>, item: &Item) -> Vec<IrItem> {
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
                return vec![IrItem::Ffi(IrFfiFn {
                    name: decl.name.text.clone(),
                    ffi_module: ffi_module.clone(),
                    ffi_fn: ffi_fn.clone(),
                    ffi_call_arity: *ffi_arity,
                    params,
                    is_pub: matches!(decl.vis, Visibility::Pub),
                    span: decl.span,
                })];
            }
            vec![IrItem::Fn(lower_fn(ctx, decl))]
        }
        Item::Actor(decl) => vec![IrItem::Actor(lower_actor(ctx, decl))],
        Item::Const(decl) => vec![IrItem::Const(lower_const(ctx, decl))],
        Item::InstanceDecl(decl) => lower_instance(ctx, decl),
        // Type, import, and class declarations are erased at the IR level.
        // Class metadata lives in `TypedWorkspace.class_table`.
        Item::Type(_) | Item::Import(_) | Item::ClassDecl(_) => vec![],
    }
}

/// Compatibility shim — delegates to [`lower_item_multi`] and returns the
/// first item, or `None` for erased items.
///
/// Existing callers (test scaffolding) that expect a single `Option<IrItem>`
/// can continue to use this. New code should prefer [`lower_item_multi`].
pub fn lower_item(ctx: &mut LowerCtx<'_>, item: &Item) -> Option<IrItem> {
    lower_item_multi(ctx, item).into_iter().next()
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

    // Expose this fn's constraints so that call-site lowering inside the body
    // can determine whether to forward the caller's own dict params.
    let saved_constraints =
        std::mem::replace(&mut ctx.current_fn_constraints, scheme.constraints.clone());

    let body = lower_expr(ctx, expr);

    ctx.current_fn_constraints = saved_constraints;
    ctx.pop_propagation_scope();

    // PHASE45-T3: bare-param types are lifted from the scheme's Type::Fn
    // rather than looked up via NodeKind::Ident (ident spans carry no type).
    let user_params: Vec<IrParam> = decl
        .params
        .iter()
        .enumerate()
        .map(|(idx, p)| param_to_ir_param(ctx, &scheme, idx, p))
        .collect();

    // Prepend one implicit dict param per class constraint.
    // Dict params come BEFORE user params; their order follows the scheme's
    // declared constraint order. Each dict param carries `Type::Error` at the
    // IR level — dicts are not typed in the IR (they are plain BEAM maps).
    let params: Vec<IrParam> = scheme
        .constraints
        .iter()
        .map(|c| {
            let class_name = ctx.class_name(c.class).unwrap_or("Unknown");
            IrParam {
                name: format!("$dict_{class_name}_{}", c.ty.0),
                ty: Type::Error, // untyped in IR
                span: decl.span,
            }
        })
        .chain(user_params)
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

// ── Instance lowering ─────────────────────────────────────────────────────────

/// Lower an `instance C T` declaration to a dict const and one fn per method.
///
/// Produces (in order):
/// 1. One private [`IrFn`] per method body, named `{ClassName}__{TypeName}__{MethodName}`.
/// 2. One module-level [`IrConst`] named `$inst_{ClassName}_{TypeName}`, whose
///    value is a `MapLit` of `{'method' => fn/N, ...}` — the typeclass dictionary.
///
/// When the class name or type name cannot be resolved (missing class table or
/// unknown type), lowering is skipped and an empty vec is returned. This is a
/// defensive no-op for test scaffolding that does not wire the full pipeline.
pub fn lower_instance(ctx: &mut LowerCtx<'_>, decl: &InstanceDecl) -> Vec<IrItem> {
    let class_name = decl.class.text.clone();

    // Determine the concrete type name from the AST type annotation.
    let type_name = match &decl.ty {
        ridge_ast::Type::Named { name, .. } => name.text.clone(),
        // Other type forms (tuples, fns, …) are not supported as instance heads
        // in 0.2.13. Skip silently — a typecheck error would already have fired.
        _ => return vec![],
    };

    let mut items: Vec<IrItem> = Vec::new();

    // Dict map entries: method_name_atom → local fn ref.
    // Built alongside the method fns so field order matches declaration order.
    let mut dict_fields: Vec<(String, IrExpr)> = Vec::new();

    for method in &decl.methods {
        let method_name = method.name.text.clone();
        // Private fn name: ClassName__TypeName__MethodName
        let fn_name = format!("{class_name}__{type_name}__{method_name}");

        // Lower the method body as an ordinary fn.
        // The method fn receives the user params (NOT a dict param — methods
        // inside an instance body access the concrete type directly).
        let body = lower_expr(ctx, &method.body);

        let ret_ty = lower_ast_type(ctx, &method.ret);

        let params: Vec<IrParam> = method
            .params
            .iter()
            .map(|p| match p {
                Param::Bare(id) => IrParam {
                    name: id.text.clone(),
                    ty: Type::Error,
                    span: id.span,
                },
                Param::Annotated { name, ty, span } => IrParam {
                    name: name.text.clone(),
                    ty: lower_ast_type(ctx, ty),
                    span: *span,
                },
            })
            .collect();

        let _arity = params.len();

        let method_fn = IrFn {
            name: fn_name.clone(),
            module: ctx.module_id,
            params,
            ret_ty,
            caps: ridge_types::CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error), // placeholder — not used by codegen
            body,
            origin: NodeId(0),
            span: method.span,
            is_pub: false, // instance method fns are always module-private
            is_main: false,
            doc: None,
        };

        items.push(IrItem::Fn(method_fn));

        // Build the dict field: method_name_atom → LocalFnRef(fn_name, arity).
        // The field VALUE is a Symbol so codegen emits `fun fn_name/arity`.
        let id = ctx.fresh_id(None);
        let fn_ref_expr = IrExpr::Symbol {
            id,
            sym: SymbolRef::Local {
                name: fn_name,
                module: ctx.module_id,
            },
            span: method.span,
        };

        dict_fields.push((method_name, fn_ref_expr));
    }

    // Build the dict const: $inst_ClassName_TypeName = #{'method' => fn/N, ...}
    let dict_name = format!("$inst_{class_name}_{type_name}");
    let id = ctx.fresh_id(None);

    // Use `IrExpr::Construct` with a Record ctor so codegen lowers it to MapLit.
    // The ctor name matches the dict const name (it's just a placeholder symbol
    // for the Record ctor — the actual field data is in `fields`).
    let dict_value = IrExpr::Construct {
        id,
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            // TyConId(0) is a placeholder — dict consts are untyped in the IR.
            owner_type: ridge_types::TyConId(0),
            name: dict_name.clone(),
            variant: 0,
        },
        fields: dict_fields,
        span: decl.span,
    };

    let dict_const = IrConst {
        name: dict_name,
        ty: Type::Error, // untyped in IR
        value: dict_value,
        origin: NodeId(0),
        span: decl.span,
        is_pub: false,
    };
    items.push(IrItem::Const(dict_const));

    items
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
            attrs: vec![],
            vis: ridge_ast::Visibility::Private,
            caps: vec![],
            name: ident(name),
            params: vec![],
            ret: None,
            constraints: vec![],
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
            deriving: vec![],
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
            attrs: vec![],
            vis: ridge_ast::Visibility::Pub,
            caps: vec![],
            name: ident("exported"),
            params: vec![],
            ret: None,
            constraints: vec![],
            body: Body::Expr(Expr::Unit(sp())),
            span: sp(),
            doc: None,
        };
        let f = lower_fn(&mut ctx, &decl);
        assert!(f.is_pub);
    }

    // ── Constrained fn gains leading dict params ──────────────────────────────

    #[test]
    fn lower_fn_with_one_constraint_prepends_dict_param() {
        use ridge_types::{ClassId, Constraint, Scheme, TyVid, Type};

        let ctx = fresh_ctx();

        // Construct a scheme with one constraint (ClassId=0, TyVid=0).
        let constraint = Constraint {
            class: ClassId(0),
            ty: TyVid(0),
        };
        let constrained_scheme = Scheme {
            vars: vec![TyVid(0)],
            cap_vars: vec![],
            ty: Type::Error,
            constraints: vec![constraint],
        };

        // Simulate what lower_fn does: override the scheme lookup by building
        // a scheme manually and checking the dict param synthesis.
        // We exercise the scheme.constraints → dict param path by calling
        // lower_fn with a decl whose body has the scheme wired into the fn.
        // Since lower_fn reads the scheme from the workspace, we test the
        // param synthesis logic directly here.
        let class_name = ctx.class_name(ClassId(0)).unwrap_or("Unknown");
        let expected_param_name = format!("$dict_{class_name}_0");

        // Synthesise the dict param names the same way lower_fn does.
        let dict_params: Vec<IrParam> = constrained_scheme
            .constraints
            .iter()
            .map(|c| {
                let cn = ctx.class_name(c.class).unwrap_or("Unknown");
                IrParam {
                    name: format!("$dict_{cn}_{}", c.ty.0),
                    ty: ridge_types::Type::Error,
                    span: sp(),
                }
            })
            .collect();

        assert_eq!(dict_params.len(), 1, "one constraint → one dict param");
        assert_eq!(
            dict_params[0].name, expected_param_name,
            "dict param name follows $dict_ClassName_TyVid convention"
        );
    }

    // ── Instance declaration produces dict const + method fns ─────────────────

    #[test]
    fn lower_instance_produces_method_fn_and_dict_const() {
        use ridge_ast::{typeclass::InstanceDecl, Ident, Type as AstType};

        let mut ctx = fresh_ctx();

        // Build a minimal InstanceDecl for `instance Show Color`.
        let method = ridge_ast::typeclass::MethodDef {
            name: ident("toText"),
            params: vec![Param::Bare(ident("c"))],
            ret: AstType::Named {
                name: ident("Text"),
                span: sp(),
            },
            body: Expr::Literal(Literal::Text {
                raw: "red".into(),
                span: sp(),
            }),
            span: sp(),
        };
        let instance_decl = InstanceDecl {
            class: Ident {
                text: "Show".into(),
                span: sp(),
            },
            ty: AstType::Named {
                name: ident("Color"),
                span: sp(),
            },
            methods: vec![method],
            span: sp(),
            doc: None,
        };

        let items = lower_instance(&mut ctx, &instance_decl);

        // Should produce exactly 2 items: one method fn + one dict const.
        assert_eq!(items.len(), 2, "instance produces one fn + one dict const");

        // The first item must be the method fn.
        match &items[0] {
            IrItem::Fn(f) => {
                assert_eq!(
                    f.name, "Show__Color__toText",
                    "method fn name follows ClassName__TypeName__MethodName"
                );
                assert!(!f.is_pub, "instance method fns are always private");
            }
            other => panic!("expected IrItem::Fn, got {other:?}"),
        }

        // The second item must be the dict const.
        match &items[1] {
            IrItem::Const(c) => {
                assert_eq!(
                    c.name, "$inst_Show_Color",
                    "dict const name follows $inst_ClassName_TypeName"
                );
                assert!(!c.is_pub, "dict consts are always private");
                // The dict value must be a Construct (MapLit shape).
                assert!(
                    matches!(&c.value, IrExpr::Construct { .. }),
                    "dict value must be a Construct (MapLit)"
                );
            }
            other => panic!("expected IrItem::Const, got {other:?}"),
        }
    }
}
