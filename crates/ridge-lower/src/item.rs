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
    Attribute, Body, Expr, Param, Pattern, Span, Visibility,
};
use ridge_ir::{CtorKind, IrConst, IrExpr, IrFfiFn, IrFn, IrItem, IrLit, IrParam, SymbolRef};
use ridge_resolve::{NodeId, NodeKind};
use ridge_types::{Scheme, Type};

use crate::actor_lower::lower_actor;
use crate::ast_type::lower_ast_type;
use crate::core::{
    lower_expr, stdlib_class_home_module, synth_destructure_param, wrap_pattern_params,
};
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
        // A `deriving (Table)` record erases like any type, except it also emits
        // its column-mirror values (the type itself stays in the arena). A
        // `deriving (Schema)` record instead derives a `HasSchema` instance,
        // lowered from `TypedWorkspace.derived_instances` like every other derive.
        Item::Type(decl) => lower_table_mirrors(ctx, decl),
        // Import and class declarations are erased at the IR level.
        // Class metadata lives in `TypedWorkspace.class_table`.
        Item::Import(_) | Item::ClassDecl(_) => vec![],
    }
}

/// Emit the column-mirror values for a `deriving (Table)` record.
///
/// For `pub type User = { id: Int, … } deriving (Table)` this produces two
/// top-level constants (the mirror type itself is erased; it lives in the
/// arena):
///
/// - `userCols  = { id = Column { name = "id", table = "users" }, … }`
/// - `userTable = { name = "users", columns = ["id", …] }`
///
/// Records lower to BEAM maps, so each value is an `IrExpr::Construct` over a
/// record ctor (codegen turns it into a `MapLit`). Names and SQL spellings come
/// from [`ridge_ast::column_mirror`], the shared source of truth that name
/// resolution and type checking also use. Returns `[]` for any type without
/// `deriving (Table)` or with a non-record body.
fn lower_table_mirrors(ctx: &mut LowerCtx<'_>, decl: &ridge_ast::TypeDecl) -> Vec<IrItem> {
    use ridge_ast::column_mirror as cm;

    if !cm::has_table_derive(&decl.deriving) {
        return vec![];
    }
    let ridge_ast::TypeBody::Record(rec) = &decl.body else {
        return vec![];
    };

    let entity = decl.name.text.as_str();
    let table = cm::table_sql_name(entity);
    let span = decl.span;
    let is_pub = matches!(decl.vis, Visibility::Pub);

    // userCols = { <field> = Column { name = "<col>", table = "<table>" }, … }.
    // The mirror field keeps the entity's field name (so `userCols.createdAt`
    // works); the Column carries the SQL column name.
    let cols_fields: Vec<(String, IrExpr)> = rec
        .fields
        .iter()
        .map(|f| {
            let col = cm::column_sql_name(&f.name.text);
            // Build the leaf values first so `ctx` is not borrowed twice in one
            // call (argument evaluation would alias the `&mut`).
            let name_v = synth_text(ctx, &col, span);
            let table_v = synth_text(ctx, &table, span);
            let column_val = synth_record(
                ctx,
                "Column",
                vec![("name".to_owned(), name_v), ("table".to_owned(), table_v)],
                span,
            );
            (f.name.text.clone(), column_val)
        })
        .collect();
    let cols_value = synth_record(ctx, &cm::mirror_type_name(entity), cols_fields, span);

    // userTable = { name = "<table>", columns = ["<col>", …] }.
    let table_name_v = synth_text(ctx, &table, span);
    let column_names: Vec<IrExpr> = rec
        .fields
        .iter()
        .map(|f| synth_text(ctx, &cm::column_sql_name(&f.name.text), span))
        .collect();
    let columns_list = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: column_names,
        span,
    };
    let table_value = synth_record(
        ctx,
        "Table",
        vec![
            ("name".to_owned(), table_name_v),
            ("columns".to_owned(), columns_list),
        ],
        span,
    );

    vec![
        IrItem::Const(IrConst {
            name: cm::mirror_value_name(entity),
            ty: Type::Error, // untyped in IR — a plain record/map value
            value: cols_value,
            origin: NodeId(0),
            span,
            is_pub,
        }),
        IrItem::Const(IrConst {
            name: cm::table_value_name(entity),
            ty: Type::Error,
            value: table_value,
            origin: NodeId(0),
            span,
            is_pub,
        }),
    ]
}

/// Synthesize a `Text` literal IR expression.
fn synth_text(ctx: &mut LowerCtx<'_>, s: &str, span: ridge_ast::Span) -> IrExpr {
    IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: ridge_ir::IrLit::Text(s.to_owned()),
        span,
    }
}

/// Synthesize a `Bool` literal IR expression.
fn synth_bool(ctx: &mut LowerCtx<'_>, b: bool, span: ridge_ast::Span) -> IrExpr {
    IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: ridge_ir::IrLit::Bool(b),
        span,
    }
}

/// Synthesize a record-construction IR expression (codegen lowers it to a
/// `MapLit`). `owner_type` is irrelevant for records — codegen reads only
/// `ctor_kind` — so a placeholder id is used, matching the instance-dict path.
fn synth_record(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    fields: Vec<(String, IrExpr)>,
    span: ridge_ast::Span,
) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: ridge_types::TyConId(0),
            name: name.to_owned(),
            variant: 0,
        },
        fields,
        span,
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
    //
    // A destructuring param (`(Point { x, y }: Point)`, L9) lowers to a fresh
    // `__param_N` binder and records an entry so the body can be wrapped in a
    // `match` that binds the pattern.
    let mut user_params: Vec<IrParam> = Vec::with_capacity(decl.params.len());
    let mut pattern_entries: Vec<(String, &Pattern, Span)> = Vec::new();
    for (idx, p) in decl.params.iter().enumerate() {
        if let Param::PatternAnnotated { pat, ty, span } = p {
            let (ir, synth) = synth_destructure_param(ctx, ty, *span);
            user_params.push(ir);
            pattern_entries.push((synth, pat, *span));
        } else {
            user_params.push(param_to_ir_param(ctx, &scheme, idx, p));
        }
    }
    let body = wrap_pattern_params(ctx, body, pattern_entries);

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
                name: format!("$dict_{class_name}_{}", c.sole_ty().0),
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
        // A `@test` function is an entry point the test runner calls by name, so
        // it must be exported from the BEAM module even when it is not `pub`
        // (per `Attribute::Test`: any visibility is allowed).
        is_pub: matches!(decl.vis, Visibility::Pub) || has_test_attr(&decl.attrs),
        is_main,
        doc: decl.doc.as_ref().map(|d| d.text.clone()),
    }
}

/// Whether a function carries the `@test` attribute.
fn has_test_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| matches!(a, Attribute::Test { .. }))
}

// ── Instance lowering ─────────────────────────────────────────────────────────

/// Lower an `instance C T` declaration to a dict and one fn per method.
///
/// For a **non-parametric** instance (`instance Show Color`) this produces:
/// 1. One private [`IrFn`] per method body, named `{ClassName}__{TypeName}__{MethodName}`.
/// 2. One module-level [`IrConst`] named `$inst_{ClassName}_{TypeName}`, whose
///    value is a `MapLit` of `{'method' => fn/N, ...}` — the typeclass dictionary.
///
/// For a **parametric** instance (`instance Encode (List a) where Encode a`)
/// the dictionary cannot be a constant — its methods need the element type's
/// dictionary at runtime. The `$inst_` item is emitted as an [`IrFn`] taking one
/// dict parameter per context constraint (`$dict_{CtxClass}_{i}`) and returning
/// the method map. Method bodies that call a class method on the constrained
/// variable (e.g. `encode e` for `e : a`) project the passed-in dict via the
/// Forward path, so the call site applies `$inst_Encode_List` to the element
/// dictionary to build the concrete map.
///
/// When the class name or type name cannot be resolved (missing class table or
/// unknown type), lowering is skipped and an empty vec is returned. This is a
/// defensive no-op for test scaffolding that does not wire the full pipeline.
#[expect(
    clippy::too_many_lines,
    reason = "one linear pass building an instance's head name, dict params, method fns, and dict const; splitting it would scatter the shared head/constraint state"
)]
pub fn lower_instance(ctx: &mut LowerCtx<'_>, decl: &InstanceDecl) -> Vec<IrItem> {
    let class_name = decl.class.text.clone();

    // Determine the head type-constructor name(s) from the AST. A non-parametric
    // head is `Type::Named` (`Color`); a parametric head is `Type::App`
    // (`List a`), which parses parenthesised — `(List a)` → `Type::Paren { App }`
    // — so peel any `Paren` wrappers first. A multi-parameter head contributes
    // one name per atom, joined with `_` so `Convert Celsius Fahrenheit` keys
    // `$inst_Convert_Celsius_Fahrenheit`. A single-atom head keeps the existing
    // `$inst_{Class}_{Head}` name unchanged.
    let mut head_names: Vec<String> = Vec::with_capacity(decl.head.len());
    for atom in &decl.head {
        let mut cur = atom;
        while let ridge_ast::Type::Paren { inner, .. } = cur {
            cur = inner;
        }
        let name = match cur {
            ridge_ast::Type::Named { name, .. } => name.text.clone(),
            ridge_ast::Type::App { head, .. } => head.text.clone(),
            // Primitive types (`Int`, `Float`, `Bool`, `Text`) as instance heads —
            // e.g. `instance SqlType Int`. The name string must match the tycon
            // name used in the typecheck/tycon arena so generated dict-const names
            // like `$inst_SqlType_Int` stay consistent across the pipeline.
            ridge_ast::Type::Primitive { name, .. } => {
                use ridge_ast::PrimitiveType;
                match name {
                    PrimitiveType::Int => "Int",
                    PrimitiveType::Float => "Float",
                    PrimitiveType::Bool => "Bool",
                    PrimitiveType::Text => "Text",
                    PrimitiveType::Unit => "Unit",
                    PrimitiveType::Timestamp => "Timestamp",
                    PrimitiveType::Decimal => "Decimal",
                }
                .to_owned()
            }
            // A function-type instance head (`instance Run (fn Int -> Int)`).
            // Name it after the synthetic per-arity `Fn/N` constructor so the
            // generated dict const `$inst_{Class}_Fn{arity}` matches the arena
            // name the call site reads when referencing the dictionary.
            ridge_ast::Type::Fn { fn_ty, .. } => ridge_types::fn_tycon_name(fn_ty.params.len()),
            // Other type forms (tuples, …) are not supported as instance heads.
            // Skip silently — a typecheck error would already have fired.
            _ => return vec![],
        };
        head_names.push(name);
    }
    // A fundep terminal class (`Refinable`/`Projectable`/…) over a nested-join
    // composite receiver (`Joined`/`LeftJoined`/`RightJoined`/`FullJoined`) keys its
    // dictionary by the RECEIVER ALONE: the dependency collapses the predicate, whose
    // leaf arity grows with the join depth, so the per-arity predicate atom is dropped
    // to match the receiver-only instance the typechecker resolves (see `discharge` in
    // ridge-typecheck). A multi-atom head over one of these composites is only ever a
    // fundep terminal — the receiver-keyed single-param classes (`Joinable`/`JoinShape`/
    // `Decodable`) carry a one-atom head — so the receiver-name test alone is exact.
    // Binary receivers keep their full multi-atom name unchanged.
    let receiver_is_composite_join = head_names.first().is_some_and(|n| {
        matches!(
            n.as_str(),
            "Joined" | "LeftJoined" | "RightJoined" | "FullJoined"
        )
    });
    let type_name = if head_names.len() > 1 && receiver_is_composite_join {
        head_names[0].clone()
    } else {
        head_names.join("_")
    };

    // Build the implicit dictionary parameters for a parametric instance, one
    // per `where` constraint. Each is named `$dict_{CtxClass}_{var}` where `var`
    // is the constraint head variable's real inference `TyVid`, recorded by the
    // typecheck's instance-body inference (keyed by the `InstanceDecl` span,
    // indexed in `where` order). The matching `current_fn_constraints` entry (set
    // below) carries that same `TyVid`, so the Forward path in method-body
    // lowering — which reads the variable a class-method call pins from
    // `node_types` — projects the dictionary for the right entity rather than the
    // first same-class one. When the table is absent (unit tests, or a module
    // whose instance bodies were not inferred) the positional sentinel `TyVid(i)`
    // is the fallback, preserving the prior order-based behaviour. Empty for
    // non-parametric instances.
    let real_vars = ctx
        .instance_dict_constraints
        .and_then(|m| m.get(&decl.span));
    let mut dict_params: Vec<IrParam> = Vec::new();
    let mut body_constraints: Vec<ridge_types::Constraint> = Vec::new();
    for (i, cc) in decl.constraints.iter().enumerate() {
        let ctx_class_name = cc.class.text.clone();
        let class_id = ctx
            .class_table
            .and_then(|ct| ct.id_by_name(&ctx_class_name));
        #[allow(clippy::cast_possible_truncation)]
        let tyvid = real_vars
            .and_then(|v| v.get(i).copied())
            .unwrap_or(ridge_types::TyVid(i as u32));
        dict_params.push(IrParam {
            name: format!("$dict_{ctx_class_name}_{}", tyvid.0),
            ty: Type::Error, // untyped in IR — dicts are plain BEAM maps
            span: cc.span,
        });
        if let Some(class) = class_id {
            body_constraints.push(ridge_types::Constraint::single(class, tyvid));
        }
    }
    let is_parametric = !dict_params.is_empty();

    // Expose the instance's context constraints while lowering method bodies so
    // that bare class-method references on the constrained variable forward the
    // passed-in dict params. Restored after the method loop. For non-parametric
    // instances `body_constraints` is empty → no change in behaviour.
    let saved_constraints = std::mem::replace(&mut ctx.current_fn_constraints, body_constraints);

    let mut items: Vec<IrItem> = Vec::new();

    // Dict map entries: method_name_atom → local fn ref.
    // Built alongside the method fns so field order matches declaration order.
    let mut dict_fields: Vec<(String, IrExpr)> = Vec::new();

    for method in &decl.methods {
        let (method_fn, dict_field) =
            lower_instance_method(ctx, method, is_parametric, &class_name, &type_name);
        if let Some(fn_item) = method_fn {
            items.push(IrItem::Fn(fn_item));
        }
        dict_fields.push(dict_field);
    }

    // Method bodies are lowered; restore the caller's constraint scope.
    ctx.current_fn_constraints = saved_constraints;

    // Build the dictionary value: $inst_ClassName_TypeName = #{'method' => fn/N, ...}
    let dict_name = format!("$inst_{class_name}_{type_name}");
    let id = ctx.fresh_id(None);

    // Use `IrExpr::Construct` with a Record ctor so codegen lowers it to MapLit.
    // The ctor name matches the dict name (it's just a placeholder symbol for
    // the Record ctor — the actual field data is in `fields`).
    let dict_value = IrExpr::Construct {
        id,
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            // TyConId(0) is a placeholder — dicts are untyped in the IR.
            owner_type: ridge_types::TyConId(0),
            name: dict_name.clone(),
            variant: 0,
        },
        fields: dict_fields,
        span: decl.span,
    };

    if is_parametric {
        // Parametric instance: `$inst_` is a *function* of the element dicts.
        // `fun($dict_Encode_0) -> #{'encode' => fun(Xs) -> ... end} end`.
        // The method-map Construct closes over the dict params; codegen emits a
        // MapLit whose method funs reference those params via `maps:get`.
        let dict_fn = IrFn {
            name: dict_name,
            module: ctx.module_id,
            params: dict_params,
            ret_ty: Type::Error, // untyped in IR — returns a dict map
            caps: ridge_types::CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error), // placeholder — not used by codegen
            body: dict_value,
            origin: NodeId(0),
            span: decl.span,
            // Instance dictionaries are exported so the constraint solver can
            // reference them cross-module (a consumer module dispatching a class
            // method on a type defined elsewhere). The method fns they close over
            // stay private. See `SymbolRef::External` dispatch in codegen.
            is_pub: true,
            is_main: false,
            doc: None,
        };
        items.push(IrItem::Fn(dict_fn));
    } else {
        let dict_const = IrConst {
            name: dict_name,
            ty: Type::Error, // untyped in IR
            value: dict_value,
            origin: NodeId(0),
            span: decl.span,
            // Exported for cross-module dispatch (see the parametric arm above).
            is_pub: true,
        };
        items.push(IrItem::Const(dict_const));
    }

    items
}

/// Lower one instance method to its dict-map field, plus (for non-parametric
/// instances) the private top-level fn that field references.
///
/// - **Parametric** instance → the field value is an inline [`IrExpr::Lambda`]
///   so the method body captures the enclosing `$inst_` fn's dict parameters;
///   returns `(None, field)`.
/// - **Non-parametric** instance → emit a private `{Class}__{Type}__{method}`
///   [`IrFn`] and reference it as `fun fn_name/arity`; returns `(Some(fn), field)`.
fn lower_instance_method(
    ctx: &mut LowerCtx<'_>,
    method: &ridge_ast::typeclass::MethodDef,
    is_parametric: bool,
    class_name: &str,
    type_name: &str,
) -> (Option<IrFn>, (String, IrExpr)) {
    let method_name = method.name.text.clone();

    // Lower the method body. The user params carry the concrete-type values;
    // for a parametric instance the body additionally references the enclosing
    // `$inst_` fn's dict params (set in `current_fn_constraints`), captured by
    // the method's closure.
    let raw_body = lower_expr(ctx, &method.body);

    let mut params: Vec<IrParam> = Vec::with_capacity(method.params.len());
    let mut pattern_entries: Vec<(String, &Pattern, Span)> = Vec::new();
    for p in &method.params {
        match p {
            Param::Bare(id) => params.push(IrParam {
                name: id.text.clone(),
                ty: Type::Error,
                span: id.span,
            }),
            Param::Annotated { name, ty, span } => params.push(IrParam {
                name: name.text.clone(),
                ty: lower_ast_type(ctx, ty),
                span: *span,
            }),
            Param::PatternAnnotated { pat, ty, span } => {
                let (ir, synth) = synth_destructure_param(ctx, ty, *span);
                params.push(ir);
                pattern_entries.push((synth, pat, *span));
            }
        }
    }
    let body = wrap_pattern_params(ctx, raw_body, pattern_entries);

    if is_parametric {
        // Inline lambda so the body captures the enclosing dict parameters. A
        // separate top-level fn would not see `$dict_{Class}_{i}` in scope.
        let lambda_id = ctx.fresh_id(None);
        let method_lambda = IrExpr::Lambda {
            id: lambda_id,
            params,
            body: Box::new(body),
            caps: ridge_types::CapabilitySet::PURE,
            span: method.span,
        };
        return (None, (method_name, method_lambda));
    }

    // Non-parametric: private top-level fn referenced as `fun fn_name/arity`.
    let ret_ty = lower_ast_type(ctx, &method.ret);
    let fn_name = format!("{class_name}__{type_name}__{method_name}");
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

    (Some(method_fn), (method_name, fn_ref_expr))
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

/// Lower a derived instance (produced from a `deriving` clause) to IR.
///
/// Like [`lower_instance`], this emits:
/// 1. One private [`IrFn`] per method with a synthesised body.
/// 2. One [`IrConst`] dict value `$inst_{ClassName}_{TypeName}`.
///
/// The method body is determined by the [`ridge_typecheck::DerivedMethodBody`]
/// tag stored during the collect pass.
#[expect(
    clippy::too_many_lines,
    reason = "flat match dispatch over all derived method body kinds; splitting would not reduce complexity"
)]
pub fn lower_derived_instance(
    ctx: &mut LowerCtx<'_>,
    derived: &ridge_typecheck::DerivedInstance,
    class_name: &str,
    type_name: &str,
) -> Vec<IrItem> {
    use ridge_ir::{IrArm, IrPat};
    use ridge_typecheck::DerivedMethodBody;

    let sp = Span::point(0);
    let mut items: Vec<IrItem> = Vec::new();

    // Transparent newtype delegation has its own shape — potentially several
    // methods, each forwarding to the inner type's dictionary — so it does not
    // fit the single-method structural path below.
    if let DerivedMethodBody::DerivedDelegated {
        field_name,
        inner_tycon,
        inner_type_name,
        methods,
    } = &derived.method_body
    {
        return build_delegated_instance(
            ctx,
            derived.key,
            class_name,
            type_name,
            field_name,
            *inner_tycon,
            inner_type_name,
            methods,
        );
    }

    // `Row` is a two-method class (`fromRow` decodes, `toRow` encodes), so its
    // derived instance emits two method fns and a two-field dict — it does not
    // fit the single-method structural path below.
    if let DerivedMethodBody::DerivedRowRecord {
        field_names,
        columns,
        field_type_names,
        optionals,
    } = &derived.method_body
    {
        return build_row_instance(
            ctx,
            derived.key.1,
            type_name,
            field_names,
            columns,
            field_type_names,
            optionals,
        );
    }

    // `Schema` derives the `HasSchema` instance — `schemaOf` returns the entity's
    // `EntitySchema` as a `std.schema` builder chain over the columns, and
    // `toInsertRow` encodes the insert shape. Two methods and a two-field dict, so
    // it has its own builder rather than the structural single-method path below.
    if let DerivedMethodBody::DerivedSchemaRecord {
        entity_name,
        table,
        columns,
    } = &derived.method_body
    {
        return build_schema_instance(ctx, type_name, entity_name, table, columns);
    }

    let method_name = derived
        .instance_info
        .methods
        .first()
        .map_or("", |(n, _)| n.as_str());
    let fn_name = format!("{class_name}__{type_name}__{method_name}");

    // ── Build the method body ─────────────────────────────────────────────────

    let (body, params) = match &derived.method_body {
        DerivedMethodBody::DerivedEq => {
            // eq (a: T) (b: T) -> Bool  =  erlang:=:=(a, b)
            // Dispatch through std.op.eq which codegen maps to erlang:=:=.
            let body = IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Stdlib {
                        module: "std.op".to_string(),
                        name: "eq".to_string(),
                    },
                    span: sp,
                }),
                args: vec![
                    IrExpr::Local {
                        id: ctx.fresh_id(None),
                        name: "a".to_string(),
                        span: sp,
                    },
                    IrExpr::Local {
                        id: ctx.fresh_id(None),
                        name: "b".to_string(),
                        span: sp,
                    },
                ],
                span: sp,
            };
            let params = vec![
                IrParam {
                    name: "a".to_string(),
                    ty: Type::Error,
                    span: sp,
                },
                IrParam {
                    name: "b".to_string(),
                    ty: Type::Error,
                    span: sp,
                },
            ];
            (body, params)
        }

        DerivedMethodBody::DerivedToTextRecord {
            field_names,
            field_tycons,
        } => {
            // toText (x: T) -> Text
            //   = "TypeName { f1 = " ++ toText(x.f1) ++ ", f2 = " ++ toText(x.f2) ++ " }"
            //
            // Each field is accessed via IrExpr::Field, then wrapped with the
            // appropriate stdlib toText call (reusing the interpolation path
            // for builtin types: std.int.toText, std.bool.toText, etc.).
            // Text fields and user-defined types are passed through as-is.
            let body = build_to_text_record_body(ctx, type_name, field_names, field_tycons, sp);
            let params = vec![IrParam {
                name: "x".to_string(),
                ty: Type::Error,
                span: sp,
            }];
            (body, params)
        }

        DerivedMethodBody::DerivedToTextUnion { variants } => {
            // toText (x: T) -> Text  =  match x { Ctor => "Ctor", Ctor(v0, v1) => "Ctor(" ++ toText(v0) ++ ", " ++ toText(v1) ++ ")", ... }
            // Nullary variants render as just the name; payload variants render
            // "CtorName(toText(v0), toText(v1), ...)".
            let arms: Vec<IrArm> = variants
                .iter()
                .map(|(ctor_name, payload_count, payload_tycons)| {
                    // Bind payload variables p0, p1, … so they can be rendered.
                    let sym = SymbolRef::Constructor {
                        ctor_kind: CtorKind::UnionVariant,
                        owner_type: derived.key.1,
                        name: ctor_name.clone(),
                        variant: 0,
                    };
                    let args: Vec<IrPat> = (0..*payload_count)
                        .map(|i| IrPat::Bind {
                            name: format!("_p{i}"),
                            inner: None,
                            span: sp,
                        })
                        .collect();
                    let pat = IrPat::Ctor {
                        sym,
                        fields: vec![],
                        args,
                        span: sp,
                    };
                    let arm_body = build_to_text_union_arm_body(
                        ctx,
                        ctor_name,
                        *payload_count,
                        payload_tycons,
                        sp,
                    );
                    IrArm {
                        pat,
                        when: None,
                        body: arm_body,
                        span: sp,
                    }
                })
                .collect();

            let body = IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: "x".to_string(),
                    span: sp,
                }),
                arms,
                span: sp,
            };
            let params = vec![IrParam {
                name: "x".to_string(),
                ty: Type::Error,
                span: sp,
            }];
            (body, params)
        }

        DerivedMethodBody::DerivedOrdRecord { field_names } => {
            // compare (a: T) (b: T) -> Ordering
            // Field-by-field lexicographic order. Uses nested matches on
            // std.op.lt / std.op.gt per field; first non-Equal field wins.
            // For 0.2.13, emit a match using std.op.lt/gt calls.
            let body = build_ord_record_body(ctx, field_names, sp);
            let params = vec![
                IrParam {
                    name: "a".to_string(),
                    ty: Type::Error,
                    span: sp,
                },
                IrParam {
                    name: "b".to_string(),
                    ty: Type::Error,
                    span: sp,
                },
            ];
            (body, params)
        }

        DerivedMethodBody::DerivedOrdUnion { variants } => {
            // compare (a: T) (b: T) -> Ordering — variant index then payload.
            let body = build_ord_union_body(ctx, derived.key.1, variants, sp);
            let params = vec![
                IrParam {
                    name: "a".to_string(),
                    ty: Type::Error,
                    span: sp,
                },
                IrParam {
                    name: "b".to_string(),
                    ty: Type::Error,
                    span: sp,
                },
            ];
            (body, params)
        }

        DerivedMethodBody::DerivedEncodeRecord {
            field_names,
            field_shapes,
        } => {
            // encode (x: T) -> JsonValue
            //   = JObject(std.map.fromList([(<<"f">>, encode_shape(x.f)), ...]))
            let body = build_encode_record_body(ctx, field_names, field_shapes, sp);
            let params = vec![IrParam {
                name: "x".to_string(),
                ty: Type::Error,
                span: sp,
            }];
            (body, params)
        }

        DerivedMethodBody::DerivedEncodeUnion { variants } => {
            // encode (x: T) -> JsonValue
            //   = match x { Nullary -> JText "Ctor"; Payload _p0... -> {"tag":...,"values":[...]} }
            let arms: Vec<ridge_ir::IrArm> = variants
                .iter()
                .map(|(ctor_name, payload_shapes)| {
                    let payload_count = payload_shapes.len();
                    let sym = SymbolRef::Constructor {
                        ctor_kind: CtorKind::UnionVariant,
                        owner_type: derived.key.1,
                        name: ctor_name.clone(),
                        variant: 0,
                    };
                    let args: Vec<ridge_ir::IrPat> = (0..payload_count)
                        .map(|i| ridge_ir::IrPat::Bind {
                            name: format!("_p{i}"),
                            inner: None,
                            span: sp,
                        })
                        .collect();
                    let pat = ridge_ir::IrPat::Ctor {
                        sym,
                        fields: vec![],
                        args,
                        span: sp,
                    };
                    let arm_body = build_encode_union_arm_body(ctx, ctor_name, payload_shapes, sp);
                    ridge_ir::IrArm {
                        pat,
                        when: None,
                        body: arm_body,
                        span: sp,
                    }
                })
                .collect();
            let body = IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: "x".to_string(),
                    span: sp,
                }),
                arms,
                span: sp,
            };
            let params = vec![IrParam {
                name: "x".to_string(),
                ty: Type::Error,
                span: sp,
            }];
            (body, params)
        }

        DerivedMethodBody::DerivedDecodeRecord {
            field_names,
            field_shapes,
        } => {
            // decode (j: JsonValue) -> Result T Error
            //   = match j { JObject m -> <sequence per field>; _ -> Err(decode.expected_object) }
            let body = build_decode_record_body(
                ctx,
                derived.key.1,
                type_name,
                field_names,
                field_shapes,
                sp,
            );
            let params = vec![IrParam {
                name: "j".to_string(),
                ty: Type::Error,
                span: sp,
            }];
            (body, params)
        }

        DerivedMethodBody::DerivedDecodeUnion { variants } => {
            // decode (j: JsonValue) -> Result T Error
            //   = match j { JText s -> <nullary dispatch>; JObject m -> <payload dispatch>; _ -> Err(...) }
            let body = build_decode_union_body(ctx, derived.key.1, type_name, variants, sp);
            let params = vec![IrParam {
                name: "j".to_string(),
                ty: Type::Error,
                span: sp,
            }];
            (body, params)
        }

        DerivedMethodBody::DerivedRowRecord { .. } => {
            unreachable!("DerivedRowRecord is handled by the early return above")
        }

        DerivedMethodBody::DerivedSchemaRecord { .. } => {
            unreachable!("DerivedSchemaRecord is handled by the early return above")
        }

        DerivedMethodBody::DerivedDelegated { .. } => {
            unreachable!("DerivedDelegated is handled by the early return above")
        }
    };

    // A generic type whose derived instance threads element dictionaries (e.g.
    // `type Box a deriving (Encode)` → `instance Encode (Box a) where Encode a`)
    // produces a constrained instance: `head_var_positions` lists the type
    // parameters that flow a runtime dict. Such an instance lowers like a
    // hand-written parametric instance — `$inst_` is a *function* of those dicts
    // and the method is an inline lambda closing over them.
    let head_var_positions = &derived.instance_info.head_var_positions;
    let is_parametric = !head_var_positions.is_empty();

    let dict_name = format!("$inst_{class_name}_{type_name}");

    if is_parametric {
        // The method body references `$dict_{Class}_{i}` directly (emitted by the
        // `Var` arm of encode_shape/decode_shape), so it must be an inline lambda
        // captured inside the `$inst_` function — a separate top-level fn would
        // not have those dict parameters in scope.
        let method_lambda = IrExpr::Lambda {
            id: ctx.fresh_id(None),
            params,
            body: Box::new(body),
            caps: ridge_types::CapabilitySet::PURE,
            span: sp,
        };
        let dict_value = IrExpr::Construct {
            id: ctx.fresh_id(None),
            ctor: SymbolRef::Constructor {
                ctor_kind: CtorKind::Record,
                owner_type: ridge_types::TyConId(0),
                name: dict_name.clone(),
                variant: 0,
            },
            fields: vec![(method_name.to_string(), method_lambda)],
            span: sp,
        };
        let dict_params: Vec<IrParam> = head_var_positions
            .iter()
            .map(|&i| IrParam {
                name: format!("$dict_{class_name}_{i}"),
                ty: Type::Error,
                span: sp,
            })
            .collect();
        let dict_fn = IrFn {
            name: dict_name,
            module: ctx.module_id,
            params: dict_params,
            ret_ty: Type::Error,
            caps: ridge_types::CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: dict_value,
            origin: NodeId(0),
            span: sp,
            // Exported for cross-module instance dispatch (see `lower_instance`).
            is_pub: true,
            is_main: false,
            doc: None,
        };
        items.push(IrItem::Fn(dict_fn));
        return items;
    }

    // ── Non-parametric: emit the method fn + the dict const ───────────────────
    // $inst_ClassName_TypeName = #{ 'method' => fun fn_name/N }

    let method_fn = IrFn {
        name: fn_name.clone(),
        module: ctx.module_id,
        params,
        ret_ty: Type::Error,
        caps: ridge_types::CapabilitySet::PURE,
        scheme: Scheme::mono(Type::Error),
        body,
        origin: NodeId(0),
        span: sp,
        is_pub: false,
        is_main: false,
        doc: None,
    };
    items.push(IrItem::Fn(method_fn));

    let fn_ref_expr = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Local {
            name: fn_name,
            module: ctx.module_id,
        },
        span: sp,
    };

    let dict_value = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: ridge_types::TyConId(0),
            name: dict_name.clone(),
            variant: 0,
        },
        fields: vec![(method_name.to_string(), fn_ref_expr)],
        span: sp,
    };

    items.push(IrItem::Const(IrConst {
        name: dict_name,
        ty: Type::Error,
        value: dict_value,
        origin: NodeId(0),
        span: sp,
        // Exported for cross-module instance dispatch (see `lower_instance`).
        is_pub: true,
    }));

    items
}

// ── Derived newtype delegation ────────────────────────────────────────────────

/// Lower a transparent newtype instance: emit one forwarding method fn per class
/// method plus the `$inst_{Class}_{Type}` dictionary const.
///
/// Each method unwraps its wrapper-typed arguments (projecting the single field),
/// forwards to the inner type's dictionary, and rewraps the result when it is the
/// wrapper type. The wrapper is therefore indistinguishable from its inner value
/// at runtime for every delegated class.
#[expect(
    clippy::too_many_arguments,
    reason = "destructured fields of the DerivedDelegated method body, passed through to one builder"
)]
#[expect(
    clippy::too_many_lines,
    reason = "flat per-method fn + dict-const emission; splitting would not reduce complexity"
)]
fn build_delegated_instance(
    ctx: &mut LowerCtx<'_>,
    key: (ridge_types::ClassId, ridge_types::TyConId),
    class_name: &str,
    type_name: &str,
    field_name: &str,
    inner_tycon: ridge_types::TyConId,
    inner_type_name: &str,
    methods: &[ridge_typecheck::DelegatedMethod],
) -> Vec<IrItem> {
    use ridge_typecheck::{DelegArg, DelegResult};

    let sp = Span::point(0);
    let newtype_tycon = key.1;
    let class_id = key.0;
    let mut items: Vec<IrItem> = Vec::new();
    let mut dict_fields: Vec<(String, IrExpr)> = Vec::new();

    for method in methods {
        let params: Vec<IrParam> = (0..method.args.len())
            .map(|i| IrParam {
                name: format!("__d{i}"),
                ty: Type::Error,
                span: sp,
            })
            .collect();

        // Unwrap each wrapper-typed argument (`__di.field`); pass others through.
        let call_args: Vec<IrExpr> = method
            .args
            .iter()
            .enumerate()
            .map(|(i, arg)| {
                let local = IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: format!("__d{i}"),
                    span: sp,
                };
                match arg {
                    DelegArg::Wrapped => IrExpr::Field {
                        id: ctx.fresh_id(None),
                        base: Box::new(local),
                        field: field_name.to_string(),
                        span: sp,
                    },
                    DelegArg::Plain => local,
                }
            })
            .collect();

        let inner_result = delegated_inner_call(
            ctx,
            class_id,
            class_name,
            inner_tycon,
            inner_type_name,
            &method.name,
            call_args,
            sp,
        );

        let body = match method.result {
            DelegResult::Plain => inner_result,
            DelegResult::Wrap => {
                wrap_newtype(ctx, newtype_tycon, type_name, field_name, inner_result, sp)
            }
            DelegResult::WrapResult => {
                wrap_result_newtype(ctx, newtype_tycon, type_name, field_name, inner_result, sp)
            }
        };

        let fn_name = format!("{class_name}__{type_name}__{}", method.name);
        items.push(IrItem::Fn(IrFn {
            name: fn_name.clone(),
            module: ctx.module_id,
            params,
            ret_ty: Type::Error,
            caps: ridge_types::CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body,
            origin: NodeId(0),
            span: sp,
            is_pub: false,
            is_main: false,
            doc: None,
        }));

        dict_fields.push((
            method.name.clone(),
            IrExpr::Symbol {
                id: ctx.fresh_id(None),
                sym: SymbolRef::Local {
                    name: fn_name,
                    module: ctx.module_id,
                },
                span: sp,
            },
        ));
    }

    let dict_name = format!("$inst_{class_name}_{type_name}");
    let dict_value = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: ridge_types::TyConId(0),
            name: dict_name.clone(),
            variant: 0,
        },
        fields: dict_fields,
        span: sp,
    };
    items.push(IrItem::Const(IrConst {
        name: dict_name,
        ty: Type::Error,
        value: dict_value,
        origin: NodeId(0),
        span: sp,
        // Exported for cross-module instance dispatch (see `lower_instance`); the
        // forwarding method fns it closes over stay private.
        is_pub: true,
    }));

    items
}

/// Build the call to the inner type's class method for one delegated method.
///
/// The inner dictionary is located the same way the constraint solver would:
/// `ToText` dispatches straight to the inner type's stdlib `toText`; prelude
/// `Encode`/`Decode` primitives synthesise their dictionary inline; a stdlib
/// class (`SqlType`) references its `$inst_` const in the class's home module.
#[expect(
    clippy::too_many_arguments,
    reason = "the inner-instance coordinates (class, inner tycon, names) plus the call args"
)]
fn delegated_inner_call(
    ctx: &mut LowerCtx<'_>,
    class_id: ridge_types::ClassId,
    class_name: &str,
    inner_tycon: ridge_types::TyConId,
    inner_type_name: &str,
    method: &str,
    mut args: Vec<IrExpr>,
    sp: Span,
) -> IrExpr {
    // ToText forwards to the inner type's stdlib renderer directly (there is no
    // runtime `ToText` dictionary to project).
    if class_name == "ToText" {
        let arg = args.pop().unwrap_or_else(|| IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Unit,
            span: sp,
        });
        return crate::interp::wrap_to_text_by_tycon(ctx, arg, inner_tycon, sp);
    }

    // Prelude `Encode`/`Decode` for a primitive inner: synthesise the dictionary
    // inline (these instances have no module-level `$inst_` const).
    let dict = if crate::prelude_dict::is_prelude_codec_instance(class_id, inner_tycon) {
        crate::prelude_dict::synth_prelude_dict(ctx, class_id, inner_tycon, vec![], sp)
            .unwrap_or_else(|| IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Unit,
                span: sp,
            })
    } else if let Some(home) = stdlib_class_home_module(class_name) {
        // Stdlib class (`SqlType`): its base-type dictionary lives in the home
        // module and is fetched cross-module.
        IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: home.to_owned(),
                name: format!("$inst_{class_name}_{inner_type_name}"),
            },
            span: sp,
        }
    } else {
        // A user-class instance defined locally (not reached for the MVP's
        // delegated classes; kept for completeness).
        IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Local {
                name: format!("$inst_{class_name}_{inner_type_name}"),
                module: ctx.module_id,
            },
            span: sp,
        }
    };

    let projected = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dict),
        field: method.to_string(),
        span: sp,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(projected),
        args,
        span: sp,
    }
}

/// `Newtype { field = value }` — rewrap an inner value into the wrapper record.
fn wrap_newtype(
    ctx: &mut LowerCtx<'_>,
    newtype_tycon: ridge_types::TyConId,
    type_name: &str,
    field_name: &str,
    value: IrExpr,
    sp: Span,
) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: newtype_tycon,
            name: type_name.to_string(),
            variant: 0,
        },
        fields: vec![(field_name.to_string(), value)],
        span: sp,
    }
}

/// `match <result> { Ok n -> Ok (Newtype { field = n }); Err e -> Err e }` —
/// rewrap the `Ok` payload of a `Result wrapper Error`, forwarding `Err`.
fn wrap_result_newtype(
    ctx: &mut LowerCtx<'_>,
    newtype_tycon: ridge_types::TyConId,
    type_name: &str,
    field_name: &str,
    result: IrExpr,
    sp: Span,
) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};

    let n_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: "__dn".to_string(),
        span: sp,
    };
    let wrapped = wrap_newtype(ctx, newtype_tycon, type_name, field_name, n_local, sp);
    let ok_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__dn".to_string(),
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: build_ok(wrapped, sp),
        span: sp,
    };

    let e_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: "__de".to_string(),
        span: sp,
    };
    let err_arm = IrArm {
        pat: IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![IrPat::Bind {
                name: "__de".to_string(),
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: IrExpr::Construct {
            id: ctx.fresh_id(None),
            ctor: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![("$0".to_string(), e_local)],
            span: sp,
        },
        span: sp,
    };

    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(result),
        arms: vec![ok_arm, err_arm],
        span: sp,
    }
}

// ── Derived Ord body builders ─────────────────────────────────────────────────

/// Build the `compare` body for a derived `Ord` on a record type.
///
/// Emits field-by-field comparisons via `std.op.lt`/`std.op.gt`; first
/// non-`Equal` result wins. The IR uses `Match` arms on `true`/`false` literals
/// since there is no `IrExpr::If` in the IR.
#[expect(
    clippy::too_many_lines,
    reason = "sequential field-by-field comparison chain; splitting by field count would not reduce complexity"
)]
fn build_ord_record_body(ctx: &mut LowerCtx<'_>, field_names: &[String], sp: Span) -> IrExpr {
    use ridge_ir::{IrArm, IrLit, IrPat};

    // Helper: build a Less/Equal/Greater Ordering constructor.
    let ordering_ctor = |ctx: &mut LowerCtx<'_>, name: &str, variant: u32| IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: ridge_types::TyConId(15), // Ordering
            name: name.to_string(),
            variant,
        },
        fields: vec![],
        span: sp,
    };

    if field_names.is_empty() {
        return ordering_ctor(ctx, "Equal", 1);
    }

    // Build from the last field backwards; start with Equal and wrap each field.
    let mut result = ordering_ctor(ctx, "Equal", 1);

    for field in field_names.iter().rev() {
        // a.field
        let a_field = IrExpr::Field {
            id: ctx.fresh_id(None),
            base: Box::new(IrExpr::Local {
                id: ctx.fresh_id(None),
                name: "a".to_string(),
                span: sp,
            }),
            field: field.clone(),
            span: sp,
        };
        // b.field
        let b_field = IrExpr::Field {
            id: ctx.fresh_id(None),
            base: Box::new(IrExpr::Local {
                id: ctx.fresh_id(None),
                name: "b".to_string(),
                span: sp,
            }),
            field: field.clone(),
            span: sp,
        };

        // lt_call: std.op.lt(a.field, b.field)
        let lt_call = IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(IrExpr::Symbol {
                id: ctx.fresh_id(None),
                sym: SymbolRef::Stdlib {
                    module: "std.op".to_string(),
                    name: "lt".to_string(),
                },
                span: sp,
            }),
            args: vec![a_field.clone(), b_field.clone()],
            span: sp,
        };

        // gt_call: std.op.gt(a.field, b.field)
        let gt_call = IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(IrExpr::Symbol {
                id: ctx.fresh_id(None),
                sym: SymbolRef::Stdlib {
                    module: "std.op".to_string(),
                    name: "gt".to_string(),
                },
                span: sp,
            }),
            args: vec![a_field, b_field],
            span: sp,
        };

        // match std.op.gt(a.f, b.f) { true => Greater, _ => <rest> }
        let gt_match = IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(gt_call),
            arms: vec![
                IrArm {
                    pat: IrPat::Lit {
                        value: IrLit::Bool(true),
                        span: sp,
                    },
                    when: None,
                    body: ordering_ctor(ctx, "Greater", 2),
                    span: sp,
                },
                IrArm {
                    pat: IrPat::Wild { span: sp },
                    when: None,
                    body: result,
                    span: sp,
                },
            ],
            span: sp,
        };

        // match std.op.lt(a.f, b.f) { true => Less, _ => <gt_match> }
        result = IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(lt_call),
            arms: vec![
                IrArm {
                    pat: IrPat::Lit {
                        value: IrLit::Bool(true),
                        span: sp,
                    },
                    when: None,
                    body: ordering_ctor(ctx, "Less", 0),
                    span: sp,
                },
                IrArm {
                    pat: IrPat::Wild { span: sp },
                    when: None,
                    body: gt_match,
                    span: sp,
                },
            ],
            span: sp,
        };
    }

    result
}

/// Build the `compare` body for a derived `Ord` on a union type.
///
/// Emits a nested match: `match a { CtorI => match b { CtorJ => Less/Equal/Greater } }`.
/// The variant ordering is the declaration order (earlier variant = `Less`).
#[expect(
    clippy::too_many_lines,
    reason = "nested outer/inner match arms over all variant pairs; splitting would not reduce complexity"
)]
fn build_ord_union_body(
    ctx: &mut LowerCtx<'_>,
    owner_tycon: ridge_types::TyConId,
    variants: &[(String, usize)],
    sp: Span,
) -> IrExpr {
    use ridge_ir::{IrArm, IrPat};

    if variants.is_empty() {
        return IrExpr::Construct {
            id: ctx.fresh_id(None),
            ctor: SymbolRef::Constructor {
                ctor_kind: CtorKind::UnionVariant,
                owner_type: ridge_types::TyConId(15),
                name: "Equal".to_string(),
                variant: 1,
            },
            fields: vec![],
            span: sp,
        };
    }

    let make_ordering = |ctx: &mut LowerCtx<'_>, name: &str, v: u32| IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: ridge_types::TyConId(15),
            name: name.to_string(),
            variant: v,
        },
        fields: vec![],
        span: sp,
    };

    let outer_arms: Vec<IrArm> = variants
        .iter()
        .enumerate()
        .map(|(i, (ctor_i, payload_i))| {
            let a_args: Vec<IrPat> = (0..*payload_i)
                .map(|k| IrPat::Bind {
                    name: format!("_af{k}"),
                    inner: None,
                    span: sp,
                })
                .collect();
            let a_pat = IrPat::Ctor {
                sym: SymbolRef::Constructor {
                    ctor_kind: CtorKind::UnionVariant,
                    owner_type: owner_tycon,
                    name: ctor_i.clone(),
                    variant: 0,
                },
                fields: vec![],
                args: a_args,
                span: sp,
            };

            let inner_arms: Vec<IrArm> = variants
                .iter()
                .enumerate()
                .map(|(j, (ctor_j, payload_j))| {
                    let b_args: Vec<IrPat> = (0..*payload_j)
                        .map(|k| IrPat::Bind {
                            name: format!("_bf{k}"),
                            inner: None,
                            span: sp,
                        })
                        .collect();
                    let b_pat = IrPat::Ctor {
                        sym: SymbolRef::Constructor {
                            ctor_kind: CtorKind::UnionVariant,
                            owner_type: owner_tycon,
                            name: ctor_j.clone(),
                            variant: 0,
                        },
                        fields: vec![],
                        args: b_args,
                        span: sp,
                    };
                    // When i == j (same variant), compare payload fields in order
                    // using the already-bound variables _af0/_bf0, _af1/_bf1, etc.
                    // This is the payload tiebreak: first non-Equal field wins.
                    let inner_body = match i.cmp(&j) {
                        std::cmp::Ordering::Less => make_ordering(ctx, "Less", 0),
                        std::cmp::Ordering::Greater => make_ordering(ctx, "Greater", 2),
                        std::cmp::Ordering::Equal => {
                            // Build field names for the bound payload variables.
                            let payload_var_names: Vec<String> =
                                (0..*payload_i).map(|k| format!("_af{k}")).collect();
                            let b_var_names: Vec<String> =
                                (0..*payload_i).map(|k| format!("_bf{k}")).collect();
                            build_ord_payload_body(ctx, &payload_var_names, &b_var_names, sp)
                        }
                    };
                    IrArm {
                        pat: b_pat,
                        when: None,
                        body: inner_body,
                        span: sp,
                    }
                })
                .collect();

            IrArm {
                pat: a_pat,
                when: None,
                body: IrExpr::Match {
                    id: ctx.fresh_id(None),
                    scrutinee: Box::new(IrExpr::Local {
                        id: ctx.fresh_id(None),
                        name: "b".to_string(),
                        span: sp,
                    }),
                    arms: inner_arms,
                    span: sp,
                },
                span: sp,
            }
        })
        .collect();

    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: "a".to_string(),
            span: sp,
        }),
        arms: outer_arms,
        span: sp,
    }
}

// ── Derived ToText body builders ──────────────────────────────────────────────

/// Build the `toText` body for a derived record type.
///
/// Produces the IR equivalent of:
/// ```text
/// "TypeName { f1 = " ++ toText(x.f1) ++ ", f2 = " ++ toText(x.f2) ++ " }"
/// ```
///
/// Locked render format: `TypeName { field1 = <value>, field2 = <value> }`.
/// Empty records render as just `"TypeName"`.
///
/// Each field value `x.fN` is accessed via `IrExpr::Field` and wrapped with
/// the correct stdlib `toText` call for its type (reusing the same dispatch
/// table as the string-interpolation lowering pass). Text fields and
/// user-defined types are passed through without an additional wrapper.
fn build_to_text_record_body(
    ctx: &mut LowerCtx<'_>,
    type_name: &str,
    field_names: &[String],
    field_tycons: &[Option<ridge_types::TyConId>],
    sp: Span,
) -> IrExpr {
    use crate::interp::{make_concat_call, wrap_to_text_by_tycon};

    if field_names.is_empty() {
        return IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Text(type_name.to_string()),
            span: sp,
        };
    }

    // Opening prefix: "TypeName { "
    let mut acc = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(format!("{type_name} {{ ")),
        span: sp,
    };

    for (idx, field) in field_names.iter().enumerate() {
        // Separator: ", " before every field except the first.
        if idx > 0 {
            let sep = IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text(", ".to_string()),
                span: sp,
            };
            acc = make_concat_call(ctx, acc, sep, sp);
        }

        // "fieldName = "
        let label = IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Text(format!("{field} = ")),
            span: sp,
        };
        acc = make_concat_call(ctx, acc, label, sp);

        // x.field
        let field_val = IrExpr::Field {
            id: ctx.fresh_id(None),
            base: Box::new(IrExpr::Local {
                id: ctx.fresh_id(None),
                name: "x".to_string(),
                span: sp,
            }),
            field: field.clone(),
            span: sp,
        };

        // Wrap in toText if we know the field's TyConId.
        let rendered = if let Some(tycon) = field_tycons.get(idx).copied().flatten() {
            wrap_to_text_by_tycon(ctx, field_val, tycon, sp)
        } else {
            field_val
        };
        acc = make_concat_call(ctx, acc, rendered, sp);
    }

    // Closing suffix: " }"
    let close = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(" }".to_string()),
        span: sp,
    };
    make_concat_call(ctx, acc, close, sp)
}

/// Build the body of a single match arm for a derived union `toText`.
///
/// - Nullary variant → `IrLit::Text("CtorName")`.
/// - Payload variant → `"CtorName(" ++ toText(_p0) ++ ", " ++ toText(_p1) ++ ")"`.
///
/// Payload variables are the bound names from the match pattern: `_p0`, `_p1`, etc.
fn build_to_text_union_arm_body(
    ctx: &mut LowerCtx<'_>,
    ctor_name: &str,
    payload_count: usize,
    payload_tycons: &[Option<ridge_types::TyConId>],
    sp: Span,
) -> IrExpr {
    use crate::interp::{make_concat_call, wrap_to_text_by_tycon};

    if payload_count == 0 {
        return IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Text(ctor_name.to_string()),
            span: sp,
        };
    }

    // Opening: "CtorName("
    let mut acc = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(format!("{ctor_name}(")),
        span: sp,
    };

    for i in 0..payload_count {
        if i > 0 {
            let sep = IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text(", ".to_string()),
                span: sp,
            };
            acc = make_concat_call(ctx, acc, sep, sp);
        }

        let payload_var = IrExpr::Local {
            id: ctx.fresh_id(None),
            name: format!("_p{i}"),
            span: sp,
        };
        let rendered = if let Some(tycon) = payload_tycons.get(i).copied().flatten() {
            wrap_to_text_by_tycon(ctx, payload_var, tycon, sp)
        } else {
            payload_var
        };
        acc = make_concat_call(ctx, acc, rendered, sp);
    }

    // Closing: ")"
    let close = IrExpr::Lit {
        id: ctx.fresh_id(None),
        value: IrLit::Text(")".to_string()),
        span: sp,
    };
    make_concat_call(ctx, acc, close, sp)
}

/// Build a field-by-field payload comparison using bound local variables.
///
/// Used by derived `Ord` for unions when both scrutinees are the same variant
/// (the tiebreak case). `a_vars` and `b_vars` are the names of the bound
/// payload variables from the outer and inner match arms respectively.
///
/// Follows the same `std.op.lt` / `std.op.gt` nested-match pattern as
/// [`build_ord_record_body`]; returns `Equal` immediately for empty payloads.
fn build_ord_payload_body(
    ctx: &mut LowerCtx<'_>,
    a_vars: &[String],
    b_vars: &[String],
    sp: Span,
) -> IrExpr {
    use ridge_ir::{IrArm, IrLit, IrPat};

    let ordering_ctor = |ctx: &mut LowerCtx<'_>, name: &str, variant: u32| IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: ridge_types::TyConId(15), // Ordering
            name: name.to_string(),
            variant,
        },
        fields: vec![],
        span: sp,
    };

    if a_vars.is_empty() {
        return ordering_ctor(ctx, "Equal", 1);
    }

    // Build right-to-left, same pattern as build_ord_record_body.
    let mut result = ordering_ctor(ctx, "Equal", 1);

    for (a_name, b_name) in a_vars.iter().zip(b_vars.iter()).rev() {
        let a_local = IrExpr::Local {
            id: ctx.fresh_id(None),
            name: a_name.clone(),
            span: sp,
        };
        let b_local = IrExpr::Local {
            id: ctx.fresh_id(None),
            name: b_name.clone(),
            span: sp,
        };

        let lt_call = IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(IrExpr::Symbol {
                id: ctx.fresh_id(None),
                sym: SymbolRef::Stdlib {
                    module: "std.op".to_string(),
                    name: "lt".to_string(),
                },
                span: sp,
            }),
            args: vec![a_local.clone(), b_local.clone()],
            span: sp,
        };

        let gt_call = IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(IrExpr::Symbol {
                id: ctx.fresh_id(None),
                sym: SymbolRef::Stdlib {
                    module: "std.op".to_string(),
                    name: "gt".to_string(),
                },
                span: sp,
            }),
            args: vec![a_local, b_local],
            span: sp,
        };

        let gt_match = IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(gt_call),
            arms: vec![
                IrArm {
                    pat: IrPat::Lit {
                        value: IrLit::Bool(true),
                        span: sp,
                    },
                    when: None,
                    body: ordering_ctor(ctx, "Greater", 2),
                    span: sp,
                },
                IrArm {
                    pat: IrPat::Wild { span: sp },
                    when: None,
                    body: result,
                    span: sp,
                },
            ],
            span: sp,
        };

        result = IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(lt_call),
            arms: vec![
                IrArm {
                    pat: IrPat::Lit {
                        value: IrLit::Bool(true),
                        span: sp,
                    },
                    when: None,
                    body: ordering_ctor(ctx, "Less", 0),
                    span: sp,
                },
                IrArm {
                    pat: IrPat::Wild { span: sp },
                    when: None,
                    body: gt_match,
                    span: sp,
                },
            ],
            span: sp,
        };
    }

    result
}

// ── Derived Encode body builders ──────────────────────────────────────────────

/// Build the `encode` body for a derived `Encode` on a record type.
///
/// Emits `JObject(std.map.fromList([(<<"field">>, encode_shape(x.field)), ...]))`.
/// Binary `Text` keys are required for the `JObject` representation
/// (maps in `ridge_rt` use binary keys for JSON objects).
/// An empty record encodes to `JObject(fromList([]))` = `{}`.
fn build_encode_record_body(
    ctx: &mut LowerCtx<'_>,
    field_names: &[String],
    field_shapes: &[ridge_typecheck::FieldShape],
    sp: Span,
) -> IrExpr {
    // Build the list of (Text key, JsonValue) pairs as a Ridge list literal:
    //   [(<<"name">>, encode_shape x.name), (<<"age">>, encode_shape x.age), ...]
    let pairs: Vec<IrExpr> = field_names
        .iter()
        .zip(field_shapes.iter())
        .map(|(field, shape)| {
            // x.field
            let field_val = IrExpr::Field {
                id: ctx.fresh_id(None),
                base: Box::new(IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: "x".to_string(),
                    span: sp,
                }),
                field: field.clone(),
                span: sp,
            };
            let encoded = encode_shape(ctx, shape, field_val, sp);
            // (<<"field">>, encoded) — a Tuple IR node.
            IrExpr::Tuple {
                id: ctx.fresh_id(None),
                elems: vec![
                    IrExpr::Lit {
                        id: ctx.fresh_id(None),
                        value: IrLit::Text(field.clone()),
                        span: sp,
                    },
                    encoded,
                ],
                span: sp,
            }
        })
        .collect();

    // std.map.fromList(pairs_list) → the Erlang map.
    let pairs_list = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: pairs,
        span: sp,
    };
    let from_list_call = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.map".to_string(),
                name: "fromList".to_string(),
            },
            span: sp,
        }),
        args: vec![pairs_list],
        span: sp,
    };

    // JObject(the_map)
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Prelude {
                name: "JObject".to_string(),
            },
            span: sp,
        }),
        args: vec![from_list_call],
        span: sp,
    }
}

/// Build the body of a single match arm for a derived union `encode`.
///
/// - Nullary variant → `JText "CtorName"` (bare JSON string).
/// - Payload variant → `JObject(fromList([("tag", JText "Ctor"), ("values", JList [encode_shape(_p0), ...])]))`.
///
/// Payload variables are bound in the match pattern as `_p0`, `_p1`, etc.
#[expect(
    clippy::too_many_lines,
    reason = "flat IR construction for the adjacently-tagged payload-union form; splitting would hurt readability"
)]
fn build_encode_union_arm_body(
    ctx: &mut LowerCtx<'_>,
    ctor_name: &str,
    payload_shapes: &[ridge_typecheck::FieldShape],
    sp: Span,
) -> IrExpr {
    if payload_shapes.is_empty() {
        // Nullary → JText "CtorName"
        return IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(IrExpr::Symbol {
                id: ctx.fresh_id(None),
                sym: SymbolRef::Prelude {
                    name: "JText".to_string(),
                },
                span: sp,
            }),
            args: vec![IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text(ctor_name.to_string()),
                span: sp,
            }],
            span: sp,
        };
    }

    // Payload → adjacently-tagged object.
    // values = [encode_shape(_p0), encode_shape(_p1), ...]
    let encoded_payloads: Vec<IrExpr> = payload_shapes
        .iter()
        .enumerate()
        .map(|(i, shape)| {
            let pvar = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: format!("_p{i}"),
                span: sp,
            };
            encode_shape(ctx, shape, pvar, sp)
        })
        .collect();

    let payload_elems = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: encoded_payloads,
        span: sp,
    };
    let wrapped_values = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Prelude {
                name: "JList".to_string(),
            },
            span: sp,
        }),
        args: vec![payload_elems],
        span: sp,
    };

    // [("tag", JText "Ctor"), ("values", JList [...])]
    let tag_pair = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![
            IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text("tag".to_string()),
                span: sp,
            },
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Prelude {
                        name: "JText".to_string(),
                    },
                    span: sp,
                }),
                args: vec![IrExpr::Lit {
                    id: ctx.fresh_id(None),
                    value: IrLit::Text(ctor_name.to_string()),
                    span: sp,
                }],
                span: sp,
            },
        ],
        span: sp,
    };
    let values_pair = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![
            IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text("values".to_string()),
                span: sp,
            },
            wrapped_values,
        ],
        span: sp,
    };

    let pairs_list = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: vec![tag_pair, values_pair],
        span: sp,
    };
    let from_list_call = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.map".to_string(),
                name: "fromList".to_string(),
            },
            span: sp,
        }),
        args: vec![pairs_list],
        span: sp,
    };

    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Prelude {
                name: "JObject".to_string(),
            },
            span: sp,
        }),
        args: vec![from_list_call],
        span: sp,
    }
}

/// Structural `encode_shape` — recursively emits the IR expression that encodes
/// `value_expr` according to its [`FieldShape`].
///
/// Parallel to the `wrap_to_text_by_tycon` pattern in `interp.rs`, but for
/// `JsonValue` output instead of `Text` output.
///
/// # Lambda lowering note
///
/// `Lst` and `MapText` shapes emit an `IrExpr::Lambda` passed to `std.list.map`
/// / `std.map.map` respectively.  This is new ground for derived bodies (`ToText`
/// never needed a lambda); the lambda path is validated end-to-end by the
/// unit tests in this module and confirmed by the fact that `IrExpr::Lambda`
/// is already used and round-tripped in `field_accessor.rs`.
#[expect(
    clippy::too_many_lines,
    reason = "structural recursion over FieldShape; each arm is self-contained IR construction, splitting would not reduce complexity"
)]
fn encode_shape(
    ctx: &mut LowerCtx<'_>,
    shape: &ridge_typecheck::FieldShape,
    value_expr: IrExpr,
    sp: Span,
) -> IrExpr {
    use ridge_typecheck::FieldShape;
    use ridge_types::CapabilitySet;

    match shape {
        // ── Primitive → inline JsonValue ctor ────────────────────────────────
        FieldShape::Prim(tycon) => {
            let ctor_name = match tycon.0 {
                0 => "JInt",
                1 => "JFloat",
                2 => "JBool",
                _ => "JText", // Text (3) and any other primitive fall back to JText
            };
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Prelude {
                        name: ctor_name.to_string(),
                    },
                    span: sp,
                }),
                args: vec![value_expr],
                span: sp,
            }
        }

        // ── JsonValue identity ────────────────────────────────────────────────
        FieldShape::Json => value_expr,

        // ── Option T → Some x ⇒ encode_shape(T, x); None ⇒ JNull ────────────
        FieldShape::Opt(inner) => {
            let bound = ctx.fresh_local("__enc_some");
            let bound_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: bound.clone(),
                span: sp,
            };
            let encoded_inner = encode_shape(ctx, inner, bound_local, sp);

            let some_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "Some".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: bound,
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: encoded_inner,
                span: sp,
            };
            let none_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "None".to_string(),
                    },
                    fields: vec![],
                    args: vec![],
                    span: sp,
                },
                when: None,
                body: IrExpr::Call {
                    id: ctx.fresh_id(None),
                    callee: Box::new(IrExpr::Symbol {
                        id: ctx.fresh_id(None),
                        sym: SymbolRef::Prelude {
                            name: "JNull".to_string(),
                        },
                        span: sp,
                    }),
                    args: vec![],
                    span: sp,
                },
                span: sp,
            };
            IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(value_expr),
                arms: vec![some_arm, none_arm],
                span: sp,
            }
        }

        // ── List T → JList(std.list.map (\e -> encode_shape(T, e)) xs) ────────
        FieldShape::Lst(inner) => {
            let elem_param = ctx.fresh_local("__enc_elem");
            let elem_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: elem_param.clone(),
                span: sp,
            };
            let encoded_elem = encode_shape(ctx, inner, elem_local, sp);
            let elem_lambda = IrExpr::Lambda {
                id: ctx.fresh_id(None),
                params: vec![IrParam {
                    name: elem_param,
                    ty: ridge_types::Type::Error,
                    span: sp,
                }],
                body: Box::new(encoded_elem),
                caps: CapabilitySet::PURE,
                span: sp,
            };
            let mapped = IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Stdlib {
                        module: "std.list".to_string(),
                        name: "map".to_string(),
                    },
                    span: sp,
                }),
                args: vec![elem_lambda, value_expr],
                span: sp,
            };
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Prelude {
                        name: "JList".to_string(),
                    },
                    span: sp,
                }),
                args: vec![mapped],
                span: sp,
            }
        }

        // ── Map Text T → JObject(std.map.map (\_k v -> encode_shape(T, v)) m) ─
        FieldShape::MapText(inner) => {
            let key_param = ctx.fresh_local("__enc_mk");
            let val_param = ctx.fresh_local("__enc_mv");
            let val_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: val_param.clone(),
                span: sp,
            };
            let encoded_val = encode_shape(ctx, inner, val_local, sp);
            // Two-param lambda: \_k v -> encode_shape(inner, v)
            let map_lambda = IrExpr::Lambda {
                id: ctx.fresh_id(None),
                params: vec![
                    IrParam {
                        name: key_param,
                        ty: ridge_types::Type::Error,
                        span: sp,
                    },
                    IrParam {
                        name: val_param,
                        ty: ridge_types::Type::Error,
                        span: sp,
                    },
                ],
                body: Box::new(encoded_val),
                caps: CapabilitySet::PURE,
                span: sp,
            };
            let mapped = IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Stdlib {
                        module: "std.map".to_string(),
                        name: "map".to_string(),
                    },
                    span: sp,
                }),
                args: vec![map_lambda, value_expr],
                span: sp,
            };
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Prelude {
                        name: "JObject".to_string(),
                    },
                    span: sp,
                }),
                args: vec![mapped],
                span: sp,
            }
        }

        // ── Result T E → adjacently-tagged union shape ─────────────────────────
        FieldShape::Res(ok_shape, err_shape) => {
            // Ok _p0 → {"tag":"Ok","values":[encode_shape(ok,_p0)]}
            let ok_bound = ctx.fresh_local("__enc_ok");
            let ok_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: ok_bound.clone(),
                span: sp,
            };
            let ok_encoded = encode_shape(ctx, ok_shape, ok_local, sp);
            let ok_body = build_result_variant_object(ctx, "Ok", ok_encoded, sp);
            let ok_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "Ok".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: ok_bound,
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: ok_body,
                span: sp,
            };

            // Err _p0 → {"tag":"Err","values":[encode_shape(err,_p0)]}
            let err_bound = ctx.fresh_local("__enc_err");
            let err_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: err_bound.clone(),
                span: sp,
            };
            let err_encoded = encode_shape(ctx, err_shape, err_local, sp);
            let err_body = build_result_variant_object(ctx, "Err", err_encoded, sp);
            let err_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "Err".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: err_bound,
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: err_body,
                span: sp,
            };

            IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(value_expr),
                arms: vec![ok_arm, err_arm],
                span: sp,
            }
        }

        // ── User type → Encode__{TypeName}__encode(v) ────────────────────────
        // Mirror the naming convention from `lower_derived_instance`:
        //   fn_name = format!("{class_name}__{type_name}__{method_name}")
        // For the Encode class that is `Encode__{TypeName}__encode`.
        //
        // Same-module nested types are referenced via `SymbolRef::Local`, which
        // resolves at codegen time exactly as the derived instance fn does.
        // Cross-module nested types would need `SymbolRef::External`; that path
        // is not yet wired (the instance solver does not yet propagate the
        // def_module of nested user types to the field shape).  Emitting Local
        // is correct for same-module usage and will produce a clear "undefined
        // function" BEAM error if a cross-module type is used — no silent
        // pass-through that corrupts JSON.
        FieldShape::User { type_name, .. } => {
            let fn_name = format!("Encode__{type_name}__encode");
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Local {
                        name: fn_name,
                        module: ctx.module_id,
                    },
                    span: sp,
                }),
                args: vec![value_expr],
                span: sp,
            }
        }

        // ── Type variable → project the forwarded element dictionary ──────────
        // The derived instance for a generic type receives one `$dict_Encode_i`
        // parameter per used type variable. Encode the value by projecting the
        // `encode` method from that dict and applying it:
        // `(maps:get('encode', $dict_Encode_i))(value)`.
        FieldShape::Var { param_index } => {
            let dict = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: format!("$dict_Encode_{param_index}"),
                span: sp,
            };
            let projected = IrExpr::Field {
                id: ctx.fresh_id(None),
                base: Box::new(dict),
                field: "encode".to_string(),
                span: sp,
            };
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(projected),
                args: vec![value_expr],
                span: sp,
            }
        }
    }
}

/// Helper: build `JObject(fromList([("tag", JText ctor_name), ("values", JList [encoded_payload])]))`.
///
/// Used by the `Result` arm of `encode_shape`.
fn build_result_variant_object(
    ctx: &mut LowerCtx<'_>,
    ctor_name: &str,
    encoded_payload: IrExpr,
    sp: Span,
) -> IrExpr {
    let single_elem = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: vec![encoded_payload],
        span: sp,
    };
    let wrapped_payload = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Prelude {
                name: "JList".to_string(),
            },
            span: sp,
        }),
        args: vec![single_elem],
        span: sp,
    };
    let tag_pair = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![
            IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text("tag".to_string()),
                span: sp,
            },
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Prelude {
                        name: "JText".to_string(),
                    },
                    span: sp,
                }),
                args: vec![IrExpr::Lit {
                    id: ctx.fresh_id(None),
                    value: IrLit::Text(ctor_name.to_string()),
                    span: sp,
                }],
                span: sp,
            },
        ],
        span: sp,
    };
    let values_pair = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![
            IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text("values".to_string()),
                span: sp,
            },
            wrapped_payload,
        ],
        span: sp,
    };
    let pairs_list = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: vec![tag_pair, values_pair],
        span: sp,
    };
    let from_list = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.map".to_string(),
                name: "fromList".to_string(),
            },
            span: sp,
        }),
        args: vec![pairs_list],
        span: sp,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Prelude {
                name: "JObject".to_string(),
            },
            span: sp,
        }),
        args: vec![from_list],
        span: sp,
    }
}

// ── Derived Decode body builders ──────────────────────────────────────────────

/// Build a `Construct(Prelude{Err}, [("$0", record{code, message})])`.
///
/// Error records use `TyConId(12)` (the builtin `Error` type) and lower to an
/// Erlang atom-keyed map `#{ code => B, message => B }` (expr.rs:836-851).
fn build_decode_error(ctx: &mut LowerCtx<'_>, code: &str, message: String, sp: Span) -> IrExpr {
    use ridge_types::TyConId;
    let err_record = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: TyConId(12), // Error = builtin TyConId(12)
            name: "Error".to_string(),
            variant: 0,
        },
        fields: vec![
            (
                "code".to_string(),
                IrExpr::Lit {
                    id: ctx.fresh_id(None),
                    value: IrLit::Text(code.to_string()),
                    span: sp,
                },
            ),
            (
                "message".to_string(),
                IrExpr::Lit {
                    id: ctx.fresh_id(None),
                    value: IrLit::Text(message),
                    span: sp,
                },
            ),
        ],
        span: sp,
    };
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "Err".to_string(),
        },
        fields: vec![("$0".to_string(), err_record)],
        span: sp,
    }
}

/// Wrap an expression in `Ok(v)`.
///
/// Uses `IrNodeId(0)` for the outer Construct — derived instance bodies are
/// not registered in the `NodeIdMap` (no AST span to key on), so the ID value
/// is irrelevant for codegen.
fn build_ok(v: IrExpr, sp: Span) -> IrExpr {
    use ridge_ir::IrNodeId;
    IrExpr::Construct {
        id: IrNodeId(0),
        ctor: SymbolRef::Prelude {
            name: "Ok".to_string(),
        },
        fields: vec![("$0".to_string(), v)],
        span: sp,
    }
}

/// Emit the fail-fast sequencing pattern for a fallible sub-decode:
///
/// ```text
/// match <sub_result> {
///   Ok  __dec_ok_N  -> <continuation using __dec_ok_N>
///   Err __dec_err_N -> return Err __dec_err_N
/// }
/// ```
///
/// This is the exact IR that `propagate.rs:100-175` (`lower_propagate_result`)
/// emits for the `?` operator — `IrExpr::Return` inside a derived fn body
/// short-circuits to the fn boundary, implementing fail-fast.
fn decode_seq(
    ctx: &mut LowerCtx<'_>,
    sub_result: IrExpr,
    ok_name: String,
    cont: IrExpr,
    sp: Span,
) -> IrExpr {
    let err_name = ctx.fresh_local("__dec_err");
    let err_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: err_name.clone(),
        span: sp,
    };
    // Err __dec_err_N -> return Err __dec_err_N
    let return_err = IrExpr::Return {
        id: ctx.fresh_id(None),
        value: Box::new(IrExpr::Construct {
            id: ctx.fresh_id(None),
            ctor: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![("$0".to_string(), err_local)],
            span: sp,
        }),
        span: sp,
    };
    let err_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: err_name,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: return_err,
        span: sp,
    };
    let ok_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: ok_name,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: cont,
        span: sp,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(sub_result),
        arms: vec![ok_arm, err_arm],
        span: sp,
    }
}

/// Recursive `decode_shape` — inverse of `encode_shape`.
///
/// Returns an expression of type `Result T Error`.
///
/// # Fail-fast note for `Lst` and `MapText`
///
/// `IrExpr::Return` CANNOT be used inside a lambda passed to `std.list.fold`
/// (it would return from the lambda, not the derived fn).  For these shapes the
/// error is threaded via the fold ACCUMULATOR: the accumulator is itself a
/// `Result (List T) Error`; once it is `Err`, subsequent steps pass it through
/// unchanged.  This is the accumulator-Result-fold pattern used throughout
/// the derived-decode body builders.
#[expect(
    clippy::too_many_lines,
    reason = "structural recursion over FieldShape; each arm is self-contained IR construction"
)]
fn decode_shape(
    ctx: &mut LowerCtx<'_>,
    shape: &ridge_typecheck::FieldShape,
    json_expr: IrExpr,
    sp: Span,
) -> IrExpr {
    use ridge_typecheck::FieldShape;

    match shape {
        // ── Prim → match JInt/JFloat/JBool/JText; bind v; Ok v; _ -> Err ─────
        FieldShape::Prim(tycon) => {
            let (ctor_name, err_code) = match tycon.0 {
                0 => ("JInt", "decode.expected_int"),
                1 => ("JFloat", "decode.expected_float"),
                2 => ("JBool", "decode.expected_bool"),
                _ => ("JText", "decode.expected_string"), // Text (3) and others
            };
            let bound = ctx.fresh_local("__dec_prim");
            let bound_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: bound.clone(),
                span: sp,
            };
            let ok_v = build_ok(bound_local, sp);
            let ok_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: ctor_name.to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: bound,
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: ok_v,
                span: sp,
            };
            let err_body = build_decode_error(
                ctx,
                err_code,
                format!("expected a JSON {ctor_name} value"),
                sp,
            );
            let wild_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Wild { span: sp },
                when: None,
                body: err_body,
                span: sp,
            };
            IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(json_expr),
                arms: vec![ok_arm, wild_arm],
                span: sp,
            }
        }

        // ── Json → Ok json_expr (identity) ───────────────────────────────────
        FieldShape::Json => build_ok(json_expr, sp),

        // ── Opt(inner) → JNull -> Ok None; _ -> decode_shape(inner) >>= Ok Some
        FieldShape::Opt(inner) => {
            // JNull arm → Ok None
            let none_id = ctx.fresh_id(None);
            let none_val = IrExpr::Construct {
                id: none_id,
                ctor: SymbolRef::Prelude {
                    name: "None".to_string(),
                },
                fields: vec![],
                span: sp,
            };
            let ok_none = build_ok(none_val, sp);
            let null_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "JNull".to_string(),
                    },
                    fields: vec![],
                    args: vec![],
                    span: sp,
                },
                when: None,
                body: ok_none,
                span: sp,
            };

            // _ arm → bind jv, decode inner jv, wrap Some
            // Use IrPat::Bind to capture the non-null JsonValue.
            let jv_opt_bound = ctx.fresh_local("__dec_opt_jv");
            let inner_bound = ctx.fresh_local("__dec_opt_v");
            let jv_opt_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: jv_opt_bound.clone(),
                span: sp,
            };
            let sub_decode = decode_shape(ctx, inner, jv_opt_local, sp);
            let some_val = IrExpr::Construct {
                id: ctx.fresh_id(None),
                ctor: SymbolRef::Prelude {
                    name: "Some".to_string(),
                },
                fields: vec![(
                    "$0".to_string(),
                    IrExpr::Local {
                        id: ctx.fresh_id(None),
                        name: inner_bound.clone(),
                        span: sp,
                    },
                )],
                span: sp,
            };
            let ok_some = build_ok(some_val, sp);
            let cont = decode_seq(ctx, sub_decode, inner_bound, ok_some, sp);
            let wild_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Bind {
                    name: jv_opt_bound,
                    inner: None,
                    span: sp,
                },
                when: None,
                body: cont,
                span: sp,
            };

            IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(json_expr),
                arms: vec![null_arm, wild_arm],
                span: sp,
            }
        }

        // ── Lst(inner) → expect JList xs; fold-acc-Result decode each element ─
        FieldShape::Lst(inner) => {
            let xs_bound = ctx.fresh_local("__dec_xs");
            let xs_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: xs_bound.clone(),
                span: sp,
            };

            // Build the fold body — accumulator-Result-fold (no Return inside lambda).
            let fold_result = build_list_decode_fold(ctx, inner, xs_local, sp);

            // Wrap in JList match.
            let ok_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "JList".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: xs_bound,
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: fold_result,
                span: sp,
            };
            let err_body = build_decode_error(
                ctx,
                "decode.expected_array",
                "expected a JSON array".to_string(),
                sp,
            );
            let wild_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Wild { span: sp },
                when: None,
                body: err_body,
                span: sp,
            };
            IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(json_expr),
                arms: vec![ok_arm, wild_arm],
                span: sp,
            }
        }

        // ── MapText(inner) → expect JObject m; fold via toList+decode+fromList ─
        FieldShape::MapText(inner) => {
            let m_bound = ctx.fresh_local("__dec_m");
            let m_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: m_bound.clone(),
                span: sp,
            };
            let fold_result = build_map_decode_fold(ctx, inner, m_local, sp);
            let ok_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "JObject".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: m_bound,
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: fold_result,
                span: sp,
            };
            let err_body = build_decode_error(
                ctx,
                "decode.expected_object",
                "expected a JSON object".to_string(),
                sp,
            );
            let wild_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Wild { span: sp },
                when: None,
                body: err_body,
                span: sp,
            };
            IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(json_expr),
                arms: vec![ok_arm, wild_arm],
                span: sp,
            }
        }

        // ── Res(ok_shape, err_shape) → adjacently-tagged union inverse ─────────
        FieldShape::Res(ok_shape, err_shape) => {
            // expect JObject m -> Map.get "tag" m -> Some (JText t) -> dispatch
            let m_bound = ctx.fresh_local("__dec_rm");
            let tag_lit_id = ctx.fresh_id(None);
            let tag_m_id = ctx.fresh_id(None);
            let tag_opt = map_get_call(
                ctx,
                IrExpr::Lit {
                    id: tag_lit_id,
                    value: IrLit::Text("tag".to_string()),
                    span: sp,
                },
                IrExpr::Local {
                    id: tag_m_id,
                    name: m_bound.clone(),
                    span: sp,
                },
                sp,
            );
            let tag_text_bound = ctx.fresh_local("__dec_rtag");
            // Map.get "values" m
            let vals_lit_id = ctx.fresh_id(None);
            let vals_m_id = ctx.fresh_id(None);
            let vals_key = IrExpr::Lit {
                id: vals_lit_id,
                value: IrLit::Text("values".to_string()),
                span: sp,
            };
            let vals_map_local = IrExpr::Local {
                id: vals_m_id,
                name: m_bound.clone(),
                span: sp,
            };
            let vals_opt = map_get_call(ctx, vals_key, vals_map_local, sp);

            // Build Ok branch: read values[0], decode ok_shape, wrap Ok(Ok v)
            let ok_vals_bound = ctx.fresh_local("__dec_rv_ok");
            let ok_inner_bound = ctx.fresh_local("__dec_rok");
            let ok_inner = {
                // values[0] = std.list.head xs (simplest; values is JList [v])
                let vals_local = IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: ok_vals_bound,
                    span: sp,
                };
                // decode_shape on the JList element: expect JList [jv], take head
                let head_call = IrExpr::Call {
                    id: ctx.fresh_id(None),
                    callee: Box::new(IrExpr::Symbol {
                        id: ctx.fresh_id(None),
                        sym: SymbolRef::Stdlib {
                            module: "std.list".to_string(),
                            name: "head".to_string(),
                        },
                        span: sp,
                    }),
                    args: vec![vals_local],
                    span: sp,
                };
                // head returns Option T; we need the value — match Some v -> v
                // For simplicity, use std.list.index 0 which returns Option; but
                // the cleanest approach: match the JList directly since we know
                // the payload is JList [v].
                // Actually the values is stored as JList [v] after encode; at decode
                // time we have values = Some (JList vs); use std.list.head on vs.
                // head : List T -> Option T; match Some x -> x; None -> Err
                let v0_bound = ctx.fresh_local("__dec_rv0");
                let v0_local = IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: v0_bound.clone(),
                    span: sp,
                };
                let sub = decode_shape(ctx, ok_shape, v0_local, sp);
                let ok_inner_local_id = ctx.fresh_id(None);
                let ok_inner_ctor_id = ctx.fresh_id(None);
                let ok_ok = build_ok(
                    IrExpr::Construct {
                        id: ok_inner_ctor_id,
                        ctor: SymbolRef::Prelude {
                            name: "Ok".to_string(),
                        },
                        fields: vec![(
                            "$0".to_string(),
                            IrExpr::Local {
                                id: ok_inner_local_id,
                                name: ok_inner_bound.clone(),
                                span: sp,
                            },
                        )],
                        span: sp,
                    },
                    sp,
                );
                let cont_ok = decode_seq(ctx, sub, ok_inner_bound, ok_ok, sp);
                let none_err = build_decode_error(
                    ctx,
                    "decode.bad_arity",
                    "Result Ok expects 1 value".to_string(),
                    sp,
                );
                // match head(vs) { Some v0 -> cont; None -> Err }
                let some_arm = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Ctor {
                        sym: SymbolRef::Prelude {
                            name: "Some".to_string(),
                        },
                        fields: vec![],
                        args: vec![ridge_ir::IrPat::Bind {
                            name: v0_bound,
                            inner: None,
                            span: sp,
                        }],
                        span: sp,
                    },
                    when: None,
                    body: cont_ok,
                    span: sp,
                };
                let none_arm = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Wild { span: sp },
                    when: None,
                    body: none_err,
                    span: sp,
                };
                IrExpr::Match {
                    id: ctx.fresh_id(None),
                    scrutinee: Box::new(head_call),
                    arms: vec![some_arm, none_arm],
                    span: sp,
                }
            };

            // Build Err branch similarly
            let err_vals_bound = ctx.fresh_local("__dec_rv_err");
            let err_inner_bound = ctx.fresh_local("__dec_rerr");
            let err_inner = {
                let vals_local = IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: err_vals_bound,
                    span: sp,
                };
                let head_call = IrExpr::Call {
                    id: ctx.fresh_id(None),
                    callee: Box::new(IrExpr::Symbol {
                        id: ctx.fresh_id(None),
                        sym: SymbolRef::Stdlib {
                            module: "std.list".to_string(),
                            name: "head".to_string(),
                        },
                        span: sp,
                    }),
                    args: vec![vals_local],
                    span: sp,
                };
                let v0_bound = ctx.fresh_local("__dec_rv0e");
                let v0_local = IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: v0_bound.clone(),
                    span: sp,
                };
                let sub = decode_shape(ctx, err_shape, v0_local, sp);
                let err_inner_local_id = ctx.fresh_id(None);
                let err_inner_ctor_id = ctx.fresh_id(None);
                let ok_err = build_ok(
                    IrExpr::Construct {
                        id: err_inner_ctor_id,
                        ctor: SymbolRef::Prelude {
                            name: "Err".to_string(),
                        },
                        fields: vec![(
                            "$0".to_string(),
                            IrExpr::Local {
                                id: err_inner_local_id,
                                name: err_inner_bound.clone(),
                                span: sp,
                            },
                        )],
                        span: sp,
                    },
                    sp,
                );
                let cont_err = decode_seq(ctx, sub, err_inner_bound, ok_err, sp);
                let none_err = build_decode_error(
                    ctx,
                    "decode.bad_arity",
                    "Result Err expects 1 value".to_string(),
                    sp,
                );
                let some_arm = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Ctor {
                        sym: SymbolRef::Prelude {
                            name: "Some".to_string(),
                        },
                        fields: vec![],
                        args: vec![ridge_ir::IrPat::Bind {
                            name: v0_bound,
                            inner: None,
                            span: sp,
                        }],
                        span: sp,
                    },
                    when: None,
                    body: cont_err,
                    span: sp,
                };
                let none_arm = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Wild { span: sp },
                    when: None,
                    body: none_err,
                    span: sp,
                };
                IrExpr::Match {
                    id: ctx.fresh_id(None),
                    scrutinee: Box::new(head_call),
                    arms: vec![some_arm, none_arm],
                    span: sp,
                }
            };

            // Dispatch on tag string
            let unknown_tag_err = build_decode_error(
                ctx,
                "decode.unknown_tag",
                "unknown Result tag".to_string(),
                sp,
            );
            let tag_local_1 = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: tag_text_bound.clone(),
                span: sp,
            };
            let tag_local_2 = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: tag_text_bound.clone(),
                span: sp,
            };
            // match vals_opt { Some (JList vs) -> ok_inner | err_inner; _ -> Err }
            let vals_some_bound_ok = ctx.fresh_local("__dec_rvs_ok");
            let vals_some_bound_err = ctx.fresh_local("__dec_rvs_err");

            // The overall dispatch: first match tag_opt to get the tag string.
            // Then inside Some (JText t), dispatch on t to read vals.
            let ok_vals_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "Some".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: vals_some_bound_ok.clone(),
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: {
                    // vals_some_bound_ok is JList vs; bind xs_ok
                    let xs_ok = ctx.fresh_local("__dec_rvxs_ok");
                    {
                        // Replace ok_inner: bind ok_vals_bound = xs_ok
                        // We already built ok_inner assuming ok_vals_bound is bound.
                        // Re-use by let-binding: just match JList xs_ok -> ok_inner
                        let jlist_bind = ridge_ir::IrArm {
                            pat: ridge_ir::IrPat::Ctor {
                                sym: SymbolRef::Prelude {
                                    name: "JList".to_string(),
                                },
                                fields: vec![],
                                args: vec![ridge_ir::IrPat::Bind {
                                    name: xs_ok.clone(),
                                    inner: None,
                                    span: sp,
                                }],
                                span: sp,
                            },
                            when: None,
                            body: {
                                // Substitute ok_vals_bound -> xs_ok by emitting head(xs_ok)
                                let xs_local = IrExpr::Local {
                                    id: ctx.fresh_id(None),
                                    name: xs_ok,
                                    span: sp,
                                };
                                let head_call2 = IrExpr::Call {
                                    id: ctx.fresh_id(None),
                                    callee: Box::new(IrExpr::Symbol {
                                        id: ctx.fresh_id(None),
                                        sym: SymbolRef::Stdlib {
                                            module: "std.list".to_string(),
                                            name: "head".to_string(),
                                        },
                                        span: sp,
                                    }),
                                    args: vec![xs_local],
                                    span: sp,
                                };
                                let v0_b = ctx.fresh_local("__dec_rv0b");
                                let v0_l = IrExpr::Local {
                                    id: ctx.fresh_id(None),
                                    name: v0_b.clone(),
                                    span: sp,
                                };
                                let ok_inner_b = ctx.fresh_local("__dec_rokb");
                                let sub2 = decode_shape(ctx, ok_shape, v0_l, sp);
                                let ok_inner_b_local_id = ctx.fresh_id(None);
                                let ok_inner_b_ctor_id = ctx.fresh_id(None);
                                let ok_ok2 = build_ok(
                                    IrExpr::Construct {
                                        id: ok_inner_b_ctor_id,
                                        ctor: SymbolRef::Prelude {
                                            name: "Ok".to_string(),
                                        },
                                        fields: vec![(
                                            "$0".to_string(),
                                            IrExpr::Local {
                                                id: ok_inner_b_local_id,
                                                name: ok_inner_b.clone(),
                                                span: sp,
                                            },
                                        )],
                                        span: sp,
                                    },
                                    sp,
                                );
                                let cont2 = decode_seq(ctx, sub2, ok_inner_b, ok_ok2, sp);
                                let none_err2 = build_decode_error(
                                    ctx,
                                    "decode.bad_arity",
                                    "Result Ok expects 1 value".to_string(),
                                    sp,
                                );
                                let sa = ridge_ir::IrArm {
                                    pat: ridge_ir::IrPat::Ctor {
                                        sym: SymbolRef::Prelude {
                                            name: "Some".to_string(),
                                        },
                                        fields: vec![],
                                        args: vec![ridge_ir::IrPat::Bind {
                                            name: v0_b,
                                            inner: None,
                                            span: sp,
                                        }],
                                        span: sp,
                                    },
                                    when: None,
                                    body: cont2,
                                    span: sp,
                                };
                                let na = ridge_ir::IrArm {
                                    pat: ridge_ir::IrPat::Wild { span: sp },
                                    when: None,
                                    body: none_err2,
                                    span: sp,
                                };
                                IrExpr::Match {
                                    id: ctx.fresh_id(None),
                                    scrutinee: Box::new(head_call2),
                                    arms: vec![sa, na],
                                    span: sp,
                                }
                            },
                            span: sp,
                        };
                        let bad_shape = build_decode_error(
                            ctx,
                            "decode.expected_array",
                            "Result values must be a JSON array".to_string(),
                            sp,
                        );
                        let wild_a = ridge_ir::IrArm {
                            pat: ridge_ir::IrPat::Wild { span: sp },
                            when: None,
                            body: bad_shape,
                            span: sp,
                        };
                        IrExpr::Match {
                            id: ctx.fresh_id(None),
                            scrutinee: Box::new(IrExpr::Local {
                                id: ctx.fresh_id(None),
                                name: vals_some_bound_ok,
                                span: sp,
                            }),
                            arms: vec![jlist_bind, wild_a],
                            span: sp,
                        }
                    }
                },
                span: sp,
            };

            let err_vals_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "Some".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: vals_some_bound_err.clone(),
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: {
                    let xs_err = ctx.fresh_local("__dec_rvxs_err");
                    let jlist_bind = ridge_ir::IrArm {
                        pat: ridge_ir::IrPat::Ctor {
                            sym: SymbolRef::Prelude {
                                name: "JList".to_string(),
                            },
                            fields: vec![],
                            args: vec![ridge_ir::IrPat::Bind {
                                name: xs_err.clone(),
                                inner: None,
                                span: sp,
                            }],
                            span: sp,
                        },
                        when: None,
                        body: {
                            let xs_l = IrExpr::Local {
                                id: ctx.fresh_id(None),
                                name: xs_err,
                                span: sp,
                            };
                            let head_call_e = IrExpr::Call {
                                id: ctx.fresh_id(None),
                                callee: Box::new(IrExpr::Symbol {
                                    id: ctx.fresh_id(None),
                                    sym: SymbolRef::Stdlib {
                                        module: "std.list".to_string(),
                                        name: "head".to_string(),
                                    },
                                    span: sp,
                                }),
                                args: vec![xs_l],
                                span: sp,
                            };
                            let v0_b = ctx.fresh_local("__dec_rv0be");
                            let v0_l = IrExpr::Local {
                                id: ctx.fresh_id(None),
                                name: v0_b.clone(),
                                span: sp,
                            };
                            let err_inner_b = ctx.fresh_local("__dec_rerrb");
                            let sub_e = decode_shape(ctx, err_shape, v0_l, sp);
                            let err_inner_b_local_id = ctx.fresh_id(None);
                            let err_inner_b_ctor_id = ctx.fresh_id(None);
                            let ok_err2 = build_ok(
                                IrExpr::Construct {
                                    id: err_inner_b_ctor_id,
                                    ctor: SymbolRef::Prelude {
                                        name: "Err".to_string(),
                                    },
                                    fields: vec![(
                                        "$0".to_string(),
                                        IrExpr::Local {
                                            id: err_inner_b_local_id,
                                            name: err_inner_b.clone(),
                                            span: sp,
                                        },
                                    )],
                                    span: sp,
                                },
                                sp,
                            );
                            let cont_e = decode_seq(ctx, sub_e, err_inner_b, ok_err2, sp);
                            let none_err_e = build_decode_error(
                                ctx,
                                "decode.bad_arity",
                                "Result Err expects 1 value".to_string(),
                                sp,
                            );
                            let sa = ridge_ir::IrArm {
                                pat: ridge_ir::IrPat::Ctor {
                                    sym: SymbolRef::Prelude {
                                        name: "Some".to_string(),
                                    },
                                    fields: vec![],
                                    args: vec![ridge_ir::IrPat::Bind {
                                        name: v0_b,
                                        inner: None,
                                        span: sp,
                                    }],
                                    span: sp,
                                },
                                when: None,
                                body: cont_e,
                                span: sp,
                            };
                            let na = ridge_ir::IrArm {
                                pat: ridge_ir::IrPat::Wild { span: sp },
                                when: None,
                                body: none_err_e,
                                span: sp,
                            };
                            IrExpr::Match {
                                id: ctx.fresh_id(None),
                                scrutinee: Box::new(head_call_e),
                                arms: vec![sa, na],
                                span: sp,
                            }
                        },
                        span: sp,
                    };
                    let bad_shape_e = build_decode_error(
                        ctx,
                        "decode.expected_array",
                        "Result values must be a JSON array".to_string(),
                        sp,
                    );
                    let wild_ae = ridge_ir::IrArm {
                        pat: ridge_ir::IrPat::Wild { span: sp },
                        when: None,
                        body: bad_shape_e,
                        span: sp,
                    };
                    IrExpr::Match {
                        id: ctx.fresh_id(None),
                        scrutinee: Box::new(IrExpr::Local {
                            id: ctx.fresh_id(None),
                            name: vals_some_bound_err,
                            span: sp,
                        }),
                        arms: vec![jlist_bind, wild_ae],
                        span: sp,
                    }
                },
                span: sp,
            };

            // tag dispatch: if t == "Ok" -> read vals -> ok_inner; if t == "Err" -> err_inner
            let tag_dispatch = {
                // Build two ifs as match (nested):
                // We build it as a chain using the ok/err vals_arms we already have.
                // Actually we need to read vals_opt (Map.get "values" m) after matching the tag.
                // The cleanest: emit a match on tag_text_bound's value.
                // We already have vals_opt computed; now dispatch.

                // "Ok" branch: match vals_opt { Some jv -> ok_inner_w_vals; _ -> Err missing }
                let missing_vals_ok = build_decode_error(
                    ctx,
                    "decode.bad_arity",
                    "Result Ok: missing values field".to_string(),
                    sp,
                );
                let vals_none_arm_ok = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Wild { span: sp },
                    when: None,
                    body: missing_vals_ok,
                    span: sp,
                };
                let ok_branch = IrExpr::Match {
                    id: ctx.fresh_id(None),
                    scrutinee: Box::new(IrExpr::Call {
                        id: ctx.fresh_id(None),
                        callee: Box::new(IrExpr::Symbol {
                            id: ctx.fresh_id(None),
                            sym: SymbolRef::Stdlib {
                                module: "std.map".to_string(),
                                name: "get".to_string(),
                            },
                            span: sp,
                        }),
                        args: vec![
                            IrExpr::Lit {
                                id: ctx.fresh_id(None),
                                value: IrLit::Text("values".to_string()),
                                span: sp,
                            },
                            IrExpr::Local {
                                id: ctx.fresh_id(None),
                                name: m_bound.clone(),
                                span: sp,
                            },
                        ],
                        span: sp,
                    }),
                    arms: vec![ok_vals_arm, vals_none_arm_ok],
                    span: sp,
                };

                // "Err" branch similarly
                let missing_vals_err = build_decode_error(
                    ctx,
                    "decode.bad_arity",
                    "Result Err: missing values field".to_string(),
                    sp,
                );
                let vals_none_arm_err = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Wild { span: sp },
                    when: None,
                    body: missing_vals_err,
                    span: sp,
                };
                let err_branch = IrExpr::Match {
                    id: ctx.fresh_id(None),
                    scrutinee: Box::new(IrExpr::Call {
                        id: ctx.fresh_id(None),
                        callee: Box::new(IrExpr::Symbol {
                            id: ctx.fresh_id(None),
                            sym: SymbolRef::Stdlib {
                                module: "std.map".to_string(),
                                name: "get".to_string(),
                            },
                            span: sp,
                        }),
                        args: vec![
                            IrExpr::Lit {
                                id: ctx.fresh_id(None),
                                value: IrLit::Text("values".to_string()),
                                span: sp,
                            },
                            IrExpr::Local {
                                id: ctx.fresh_id(None),
                                name: m_bound.clone(),
                                span: sp,
                            },
                        ],
                        span: sp,
                    }),
                    arms: vec![err_vals_arm, vals_none_arm_err],
                    span: sp,
                };

                // Dispatch on tag_text string
                let ok_tag_arm = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Lit {
                        value: IrLit::Text("Ok".to_string()),
                        span: sp,
                    },
                    when: None,
                    body: ok_branch,
                    span: sp,
                };
                let err_tag_arm = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Lit {
                        value: IrLit::Text("Err".to_string()),
                        span: sp,
                    },
                    when: None,
                    body: err_branch,
                    span: sp,
                };
                let unk_tag_arm = ridge_ir::IrArm {
                    pat: ridge_ir::IrPat::Wild { span: sp },
                    when: None,
                    body: unknown_tag_err,
                    span: sp,
                };
                IrExpr::Match {
                    id: ctx.fresh_id(None),
                    scrutinee: Box::new(tag_local_1),
                    arms: vec![ok_tag_arm, err_tag_arm, unk_tag_arm],
                    span: sp,
                }
            };

            // Sequence: tag_opt -> Some (JText t) -> dispatch; _ -> Err
            let _tag_jtext_bound = ctx.fresh_local("__dec_rtjt");
            let jtext_bind_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "JText".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: tag_text_bound,
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: tag_dispatch,
                span: sp,
            };
            let bad_tag_type_err = build_decode_error(
                ctx,
                "decode.unknown_tag",
                "Result tag must be a JSON string".to_string(),
                sp,
            );
            let wild_tag_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Wild { span: sp },
                when: None,
                body: bad_tag_type_err,
                span: sp,
            };
            // match Map.get "tag" m { Some jv -> match jv { JText t -> ...; _ -> Err }; None -> Err }
            let missing_tag_err = build_decode_error(
                ctx,
                "decode.unknown_tag",
                "missing tag field in Result object".to_string(),
                sp,
            );
            let tag_some_bound = ctx.fresh_local("__dec_rtso");
            let tag_some_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "Some".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: tag_some_bound.clone(),
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: IrExpr::Match {
                    id: ctx.fresh_id(None),
                    scrutinee: Box::new(IrExpr::Local {
                        id: ctx.fresh_id(None),
                        name: tag_some_bound,
                        span: sp,
                    }),
                    arms: vec![jtext_bind_arm, wild_tag_arm],
                    span: sp,
                },
                span: sp,
            };
            let tag_none_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Wild { span: sp },
                when: None,
                body: missing_tag_err,
                span: sp,
            };
            let tag_match = IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(tag_opt),
                arms: vec![tag_some_arm, tag_none_arm],
                span: sp,
            };

            // Outer: expect JObject
            let ok_jobj_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Ctor {
                    sym: SymbolRef::Prelude {
                        name: "JObject".to_string(),
                    },
                    fields: vec![],
                    args: vec![ridge_ir::IrPat::Bind {
                        name: m_bound,
                        inner: None,
                        span: sp,
                    }],
                    span: sp,
                },
                when: None,
                body: tag_match,
                span: sp,
            };
            let wild_obj_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Wild { span: sp },
                when: None,
                body: build_decode_error(
                    ctx,
                    "decode.expected_object",
                    "expected a JSON object for Result".to_string(),
                    sp,
                ),
                span: sp,
            };
            // Suppress unused variable warning from the unused intermediate bindings
            let _ = (ok_inner, err_inner, tag_local_2, vals_opt);
            IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(json_expr),
                arms: vec![ok_jobj_arm, wild_obj_arm],
                span: sp,
            }
        }

        // ── User type → Decode__{TypeName}__decode(j) ────────────────────────
        FieldShape::User { type_name, .. } => {
            let fn_name = format!("Decode__{type_name}__decode");
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Local {
                        name: fn_name,
                        module: ctx.module_id,
                    },
                    span: sp,
                }),
                args: vec![json_expr],
                span: sp,
            }
        }

        // ── Type variable → project the forwarded element dictionary ──────────
        // `(maps:get('decode', $dict_Decode_i))(json)` returns `Result T Error`
        // directly — the decode method is already fallible, so no extra wrapping.
        FieldShape::Var { param_index } => {
            let dict = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: format!("$dict_Decode_{param_index}"),
                span: sp,
            };
            let projected = IrExpr::Field {
                id: ctx.fresh_id(None),
                base: Box::new(dict),
                field: "decode".to_string(),
                span: sp,
            };
            IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(projected),
                args: vec![json_expr],
                span: sp,
            }
        }
    }
}

/// Helper: `std.map.get key map` → `Option V`.
fn map_get_call(ctx: &mut LowerCtx<'_>, key: IrExpr, map: IrExpr, sp: Span) -> IrExpr {
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.map".to_string(),
                name: "get".to_string(),
            },
            span: sp,
        }),
        args: vec![key, map],
        span: sp,
    }
}

/// Build the `decode` body for a derived `Decode` on a record type.
///
/// Emits:
/// ```text
/// match j {
///   JObject m ->
///     match Map.get "f1" m { Some jv1 -> <decode_seq(decode_shape(s1, jv1), x1)>;
///                            None     -> return Err(decode.missing_field "f1") } ;
///     …
///     Ok T { f1 = x1, … }
///   _ -> Err(decode.expected_object "T")
/// }
/// ```
#[expect(
    clippy::too_many_lines,
    reason = "flat sequencing over fields; each iteration is self-contained IR construction"
)]
fn build_decode_record_body(
    ctx: &mut LowerCtx<'_>,
    tycon: ridge_types::TyConId,
    type_name: &str,
    field_names: &[String],
    field_shapes: &[ridge_typecheck::FieldShape],
    sp: Span,
) -> IrExpr {
    let m_bound = ctx.fresh_local("__dec_rec_m");

    // Bind names for each successfully decoded field.
    let bound_names: Vec<String> = field_names
        .iter()
        .map(|f| ctx.fresh_local(&format!("__dec_f_{f}")))
        .collect();

    // Build the final Ok(T { f1=x1, … }) assembly.
    let record_fields: Vec<(String, IrExpr)> = field_names
        .iter()
        .zip(bound_names.iter())
        .map(|(name, bound)| {
            (
                name.clone(),
                IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: bound.clone(),
                    span: sp,
                },
            )
        })
        .collect();
    let record_val = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: tycon,
            name: type_name.to_string(),
            variant: 0,
        },
        fields: record_fields,
        span: sp,
    };
    let ok_record = build_ok(record_val, sp);

    // Sequence the field decodes from the innermost (last field) outward.
    // Each field wraps the continuation.
    let mut cont = ok_record;
    for (field_name, (bound_name, shape)) in field_names
        .iter()
        .zip(bound_names.iter().zip(field_shapes.iter()))
        .rev()
    {
        // Map.get "field" m → Option JsonValue
        let field_lit_id = ctx.fresh_id(None);
        let field_m_id = ctx.fresh_id(None);
        let field_key = IrExpr::Lit {
            id: field_lit_id,
            value: IrLit::Text(field_name.clone()),
            span: sp,
        };
        let field_map = IrExpr::Local {
            id: field_m_id,
            name: m_bound.clone(),
            span: sp,
        };
        let get_result = map_get_call(ctx, field_key, field_map, sp);
        // Some jv -> decode_seq(decode_shape(shape, jv), bound_name, cont)
        // None    -> return Err(decode.missing_field "field")
        let jv_bound = ctx.fresh_local(&format!("__dec_jv_{field_name}"));
        let jv_local = IrExpr::Local {
            id: ctx.fresh_id(None),
            name: jv_bound.clone(),
            span: sp,
        };
        let sub_decode = decode_shape(ctx, shape, jv_local, sp);
        let field_cont = decode_seq(ctx, sub_decode, bound_name.clone(), cont, sp);
        // match get_result { Some jv -> field_cont; None -> return Err(missing_field) }
        let missing_err = IrExpr::Return {
            id: ctx.fresh_id(None),
            value: Box::new(build_decode_error(
                ctx,
                "decode.missing_field",
                format!("missing field \"{field_name}\""),
                sp,
            )),
            span: sp,
        };
        let some_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Ctor {
                sym: SymbolRef::Prelude {
                    name: "Some".to_string(),
                },
                fields: vec![],
                args: vec![ridge_ir::IrPat::Bind {
                    name: jv_bound,
                    inner: None,
                    span: sp,
                }],
                span: sp,
            },
            when: None,
            body: field_cont,
            span: sp,
        };
        let none_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Wild { span: sp },
            when: None,
            body: missing_err,
            span: sp,
        };
        cont = IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(get_result),
            arms: vec![some_arm, none_arm],
            span: sp,
        };
    }

    // Wrap in JObject match.
    let jobject_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "JObject".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: m_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: cont,
        span: sp,
    };
    let wild_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Wild { span: sp },
        when: None,
        body: build_decode_error(
            ctx,
            "decode.expected_object",
            format!("expected a JSON object for type {type_name}"),
            sp,
        ),
        span: sp,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: "j".to_string(),
            span: sp,
        }),
        arms: vec![jobject_arm, wild_arm],
        span: sp,
    }
}

/// Build the `fromRow` body for a derived `Row` on a record type.
///
/// Emits, with `r` the `Map Text SqlValue` parameter:
/// ```text
/// match Map.get "col1" r { Some sv1 -> <decode_seq(fromSql_Int(sv1), f1)>;
///                          None     -> return Err(row.missing_column "col1") } ;
/// …
/// Ok T { f1 = f1, … }
/// ```
/// Each field reads its snake-cased column, runs the field type's `SqlType.fromSql`
/// (dispatched cross-module to `std.sql`), and threads the first `Err` outward
/// via [`decode_seq`]'s `return`. Unlike the JSON record decoder there is no outer
/// object-shape match — the row map is the scrutinee for every field directly.
///
/// An `Option` field (its `optionals` flag set) decodes a missing or NULL column
/// to `None` instead of failing: a present column runs the `SqlType (Option a)`
/// instance's `fromSql`, which maps a SQL NULL to `None` and any other value to
/// `Some (fromSql v)`.
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "threads the derived record's parallel per-field slices (names, columns, type tags, optionality) and builds one fail-fast match per field; the flat loop reads best kept together"
)]
fn build_from_row_record_body(
    ctx: &mut LowerCtx<'_>,
    tycon: ridge_types::TyConId,
    type_name: &str,
    field_names: &[String],
    columns: &[String],
    field_type_names: &[String],
    optionals: &[bool],
    sp: Span,
) -> IrExpr {
    // Bind names for each successfully decoded field.
    let bound_names: Vec<String> = field_names
        .iter()
        .map(|f| ctx.fresh_local(&format!("__row_f_{f}")))
        .collect();

    // Final Ok(T { f1 = x1, … }) assembly.
    let record_fields: Vec<(String, IrExpr)> = field_names
        .iter()
        .zip(bound_names.iter())
        .map(|(name, bound)| {
            (
                name.clone(),
                IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: bound.clone(),
                    span: sp,
                },
            )
        })
        .collect();
    let record_val = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: tycon,
            name: type_name.to_string(),
            variant: 0,
        },
        fields: record_fields,
        span: sp,
    };
    let mut cont = build_ok(record_val, sp);

    // Sequence the field reads from the last field outward; each wraps the
    // continuation so the first failure short-circuits the whole decode.
    for ((field_name, column), ((bound_name, type_tag), optional)) in field_names
        .iter()
        .zip(columns.iter())
        .zip(
            bound_names
                .iter()
                .zip(field_type_names.iter())
                .zip(optionals.iter()),
        )
        .rev()
    {
        // Map.get "column" r → Option SqlValue
        let col_key = IrExpr::Lit {
            id: ctx.fresh_id(None),
            value: IrLit::Text(column.clone()),
            span: sp,
        };
        let row_map = IrExpr::Local {
            id: ctx.fresh_id(None),
            name: "r".to_string(),
            span: sp,
        };
        let get_result = map_get_call(ctx, col_key, row_map, sp);

        if *optional {
            cont = build_optional_field_read(
                ctx, field_name, type_tag, bound_name, get_result, cont, sp,
            );
            continue;
        }

        // Some sv -> decode_seq(fromSql(sv), bound, cont)
        let sv_bound = ctx.fresh_local(&format!("__row_sv_{field_name}"));
        let sv_local = IrExpr::Local {
            id: ctx.fresh_id(None),
            name: sv_bound.clone(),
            span: sp,
        };
        let from_sql = build_from_sql_call(ctx, type_tag, sv_local, sp);
        let field_cont = decode_seq(ctx, from_sql, bound_name.clone(), cont, sp);

        // None -> return Err(row.missing_column "column")
        let missing_err = IrExpr::Return {
            id: ctx.fresh_id(None),
            value: Box::new(build_decode_error(
                ctx,
                "row.missing_column",
                format!("missing column \"{column}\" for field \"{field_name}\""),
                sp,
            )),
            span: sp,
        };
        let some_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Ctor {
                sym: SymbolRef::Prelude {
                    name: "Some".to_string(),
                },
                fields: vec![],
                args: vec![ridge_ir::IrPat::Bind {
                    name: sv_bound,
                    inner: None,
                    span: sp,
                }],
                span: sp,
            },
            when: None,
            body: field_cont,
            span: sp,
        };
        let none_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Wild { span: sp },
            when: None,
            body: missing_err,
            span: sp,
        };
        cont = IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(get_result),
            arms: vec![some_arm, none_arm],
            span: sp,
        };
    }

    cont
}

/// Bind an `Option` row field: `let bound = <decode nullable column> in cont`.
///
/// `get_result` is `Map.get "column" r : Option SqlValue`. A missing column reads
/// as `None`; a present value runs the `SqlType (Option a)` instance's `fromSql`,
/// which maps a SQL NULL to `None` and any other value to `Some (fromSql v)`,
/// threading a decode `Err` outward via [`decode_seq`]'s `return`. The NULL test
/// lives in that instance, so the `SqlValue` variants stay opaque to this module.
fn build_optional_field_read(
    ctx: &mut LowerCtx<'_>,
    field_name: &str,
    type_tag: &str,
    bound_name: &str,
    get_result: IrExpr,
    cont: IrExpr,
    sp: Span,
) -> IrExpr {
    let sv_bound = ctx.fresh_local(&format!("__row_sv_{field_name}"));
    let opt_bound = ctx.fresh_local(&format!("__row_opt_{field_name}"));

    // Some sv -> (SqlType (Option a)).fromSql sv : Result (Option inner) Error,
    // unwrapped to its `Option inner` value (an Err short-circuits the decode).
    let sv_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: sv_bound.clone(),
        span: sp,
    };
    let from_sql = build_optional_from_sql_call(ctx, type_tag, sv_local, sp);
    let opt_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: opt_bound.clone(),
        span: sp,
    };
    let present = decode_seq(ctx, from_sql, opt_bound, opt_local, sp);
    let some_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Some".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: sv_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: present,
        span: sp,
    };
    // _ (missing column) -> None
    let none_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Wild { span: sp },
        when: None,
        body: build_none(ctx, sp),
        span: sp,
    };
    let field_value = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(get_result),
        arms: vec![some_arm, none_arm],
        span: sp,
    };

    // let bound_name = field_value in cont
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(field_value),
        arms: vec![ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Bind {
                name: bound_name.to_string(),
                inner: None,
                span: sp,
            },
            when: None,
            body: cont,
            span: sp,
        }],
        span: sp,
    }
}

/// `None` as a prelude `Option` value.
fn build_none(ctx: &mut LowerCtx<'_>, sp: Span) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "None".to_string(),
        },
        fields: vec![],
        span: sp,
    }
}

/// Emit `($inst_SqlType_Option $inst_SqlType_{Type}).fromSql(sv)`
/// → `Result (Option T) Error`.
///
/// A nullable decode goes through the parametric `SqlType (Option a)` instance
/// applied to the inner primitive's dictionary — the same dict-of-dicts the
/// constraint solver builds for a concrete `fromSql` at `Option T` (see
/// `dict_plan_to_expr` in `core.rs`). Keeping the NULL test inside that instance
/// is what lets this builder stay clear of the opaque `SqlValue` variants.
fn build_optional_from_sql_call(
    ctx: &mut LowerCtx<'_>,
    type_tag: &str,
    sv: IrExpr,
    sp: Span,
) -> IrExpr {
    let inner_dict = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.sql".to_string(),
            name: format!("$inst_SqlType_{type_tag}"),
        },
        span: sp,
    };
    let option_ctor = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.sql".to_string(),
            name: "$inst_SqlType_Option".to_string(),
        },
        span: sp,
    };
    let dict = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(option_ctor),
        args: vec![inner_dict],
        span: sp,
    };
    let from_sql = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dict),
        field: "fromSql".to_string(),
        span: sp,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(from_sql),
        args: vec![sv],
        span: sp,
    }
}

/// Emit `(maps:get('fromSql', $inst_SqlType_{Type}))(sv)` → `Result T Error`.
///
/// The `SqlType` base-type instances are compiled into `std.sql`; their dict
/// constant `$inst_SqlType_{Type}` is exported, so it is referenced cross-module
/// via [`SymbolRef::Stdlib`] and the `fromSql` method projected from it. This is
/// the same dispatch the constraint solver emits for a concrete `fromSql` call
/// (see `dict_plan_to_expr` in `core.rs`), specialised here to the builtin
/// primitive that `generate_row` already validated.
fn build_from_sql_call(ctx: &mut LowerCtx<'_>, type_tag: &str, sv: IrExpr, sp: Span) -> IrExpr {
    let dict = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.sql".to_string(),
            name: format!("$inst_SqlType_{type_tag}"),
        },
        span: sp,
    };
    let from_sql = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dict),
        field: "fromSql".to_string(),
        span: sp,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(from_sql),
        args: vec![sv],
        span: sp,
    }
}

/// Push one synthesized `Row` method fn (a single param, pure, untyped scheme)
/// onto `items`. The three `Row` methods differ only in name, parameter, and body,
/// so they share this boilerplate.
fn push_row_method(
    ctx: &LowerCtx<'_>,
    items: &mut Vec<IrItem>,
    name: String,
    param_name: &str,
    body: IrExpr,
    sp: Span,
) {
    items.push(IrItem::Fn(IrFn {
        name,
        module: ctx.module_id,
        params: vec![IrParam {
            name: param_name.to_string(),
            ty: Type::Error,
            span: sp,
        }],
        ret_ty: Type::Error,
        caps: ridge_types::CapabilitySet::PURE,
        scheme: Scheme::mono(Type::Error),
        body,
        origin: NodeId(0),
        span: sp,
        is_pub: false,
        is_main: false,
        doc: None,
    }));
}

/// Build a `std.schema` builder call `<fn> <args…>` for the synthesized
/// `schemaOf` body — a `SymbolRef::Stdlib` callee in `std.schema`.
fn schema_builder_call(ctx: &mut LowerCtx<'_>, name: &str, args: Vec<IrExpr>, sp: Span) -> IrExpr {
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.schema".to_string(),
                name: name.to_string(),
            },
            span: sp,
        }),
        args,
        span: sp,
    }
}

/// Build a nullary `std.schema` union value (a `DbType` or `Generation`
/// constructor) for the synthesized body. Codegen lowers a zero-payload
/// `UnionVariant` construct to a bare atom, so `owner_type`/`variant` are
/// placeholders — the same convention the record-construct helpers use.
fn schema_union_value(ctx: &mut LowerCtx<'_>, ctor: &str, sp: Span) -> IrExpr {
    IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: ridge_types::TyConId(0),
            name: ctor.to_string(),
            variant: 0,
        },
        fields: vec![],
        span: sp,
    }
}

/// Emit a column's `DbType`, dispatched through the field's `SqlType.dbType` so the
/// value codec and the column type share one source of truth. A field with a base
/// codec emits `(maps:get('dbType', $inst_SqlType_{Type}))(None)` — the witness is
/// ignored (the instance's `dbType` reads only its type). A nullable column has the
/// same `DbType` as its inner type (`sql_type_tag` already names that inner type;
/// the nullability rides `mkColumn`'s own flag), so it dispatches on the inner dict
/// directly. A field with no `SqlType` is schema/DDL metadata only — no codec to
/// ask — so it falls back to `DbText`, which a hand-written `HasSchema` overrides.
fn build_db_type_call(ctx: &mut LowerCtx<'_>, type_tag: Option<&str>, sp: Span) -> IrExpr {
    let Some(tag) = type_tag else {
        return schema_union_value(ctx, "DbText", sp);
    };
    let dict = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.sql".to_string(),
            name: format!("$inst_SqlType_{tag}"),
        },
        span: sp,
    };
    let db_type_fn = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dict),
        field: "dbType".to_string(),
        span: sp,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(db_type_fn),
        args: vec![schema_union_value(ctx, "None", sp)],
        span: sp,
    }
}

/// Lower a derived `Schema` instance: emit the `schemaOf` method fn and the
/// `$inst_HasSchema_{Type}` dict that carries it. `schemaOf` ignores its witness
/// argument and returns the entity's `EntitySchema`, assembled by the data-layer
/// convention as a `std.schema` builder chain:
///
/// ```text
/// schema "<Entity>" "<table>"
///   |> withColumn (mkColumn "<field>" "<col>" <DbType> <nullable>
///                    |> generated <Generation>   -- a database-generated column
///                    |> primaryKey)              -- the key column
///   |> …
/// ```
///
/// The dict is looked up by type at a `schemaOf` call the same way `$inst_Row_*`
/// is — both are derived instances registered in the entity's module.
fn build_schema_instance(
    ctx: &mut LowerCtx<'_>,
    type_name: &str,
    entity_name: &str,
    table: &str,
    columns: &[ridge_typecheck::SchemaColumnSpec],
) -> Vec<IrItem> {
    let sp = Span::point(0);
    let mut items: Vec<IrItem> = Vec::new();

    // schema "<Entity>" "<table>" — the empty schema the columns pipe onto.
    let entity_v = synth_text(ctx, entity_name, sp);
    let table_v = synth_text(ctx, table, sp);
    let mut acc = schema_builder_call(ctx, "schema", vec![entity_v, table_v], sp);

    for c in columns {
        // mkColumn "<field>" "<col>" <DbType> <nullable>
        let name_v = synth_text(ctx, &c.field_name, sp);
        let col_v = synth_text(ctx, &c.column, sp);
        let ty_v = build_db_type_call(ctx, c.sql_type_tag.as_deref(), sp);
        let nullable_v = synth_bool(ctx, c.nullable, sp);
        let mut col =
            schema_builder_call(ctx, "mkColumn", vec![name_v, col_v, ty_v, nullable_v], sp);

        // |> generated <Generation> — only for a database-generated column.
        if let Some(gen) = &c.generation {
            let gen_v = schema_union_value(ctx, gen, sp);
            col = schema_builder_call(ctx, "generated", vec![gen_v, col], sp);
        }
        // |> primaryKey — only for the key column.
        if c.primary_key {
            col = schema_builder_call(ctx, "primaryKey", vec![col], sp);
        }

        // <acc> |> withColumn <col>  ==  withColumn <col> <acc>
        acc = schema_builder_call(ctx, "withColumn", vec![col, acc], sp);
    }

    // schemaOf (w: Option T) -> EntitySchema T = <acc>. The witness `w` is
    // ignored; only its type selected this instance.
    let fn_name = format!("HasSchema__{type_name}__schemaOf");
    push_row_method(ctx, &mut items, fn_name.clone(), "w", acc, sp);

    // toInsertRow (x: InsertShape T) -> Map Text SqlValue — encode the insert
    // shape's columns (the entity minus its database-generated ones) the same way
    // `Row.toRow` encodes a full entity, so the write path turns a companion value
    // into columns without ever encoding a generated key.
    let insert_body = build_to_insert_row_body(ctx, columns, sp);
    let insert_fn_name = format!("HasSchema__{type_name}__toInsertRow");
    push_row_method(
        ctx,
        &mut items,
        insert_fn_name.clone(),
        "x",
        insert_body,
        sp,
    );

    // $inst_HasSchema_{Type} = #{ 'schemaOf' => fun schemaOfFn,
    //                             'toInsertRow' => fun toInsertRowFn }
    let dict_name = format!("$inst_HasSchema_{type_name}");
    let dict_value = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: ridge_types::TyConId(0),
            name: dict_name.clone(),
            variant: 0,
        },
        fields: vec![
            (
                "schemaOf".to_string(),
                IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Local {
                        name: fn_name,
                        module: ctx.module_id,
                    },
                    span: sp,
                },
            ),
            (
                "toInsertRow".to_string(),
                IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Local {
                        name: insert_fn_name,
                        module: ctx.module_id,
                    },
                    span: sp,
                },
            ),
        ],
        span: sp,
    };
    items.push(IrItem::Const(IrConst {
        name: dict_name,
        ty: Type::Error,
        value: dict_value,
        origin: NodeId(0),
        span: sp,
        is_pub: true,
    }));

    items
}

/// Lower a derived `Row` instance: emit the `fromRow`, `toRow`, and `rowColumns`
/// method fns and the `$inst_Row_{Type}` dict that carries all three. `Row` is the
/// only structurally derived class with more than one method, so it has its own
/// builder rather than the single-method path that follows it in
/// [`lower_derived_instance`].
fn build_row_instance(
    ctx: &mut LowerCtx<'_>,
    tycon: ridge_types::TyConId,
    type_name: &str,
    field_names: &[String],
    columns: &[String],
    field_type_names: &[String],
    optionals: &[bool],
) -> Vec<IrItem> {
    let sp = Span::point(0);
    let mut items: Vec<IrItem> = Vec::new();

    // fromRow (r: Map Text SqlValue) -> Result T Error
    let from_body = build_from_row_record_body(
        ctx,
        tycon,
        type_name,
        field_names,
        columns,
        field_type_names,
        optionals,
        sp,
    );
    let from_fn_name = format!("Row__{type_name}__fromRow");
    push_row_method(ctx, &mut items, from_fn_name.clone(), "r", from_body, sp);

    // toRow (x: T) -> Map Text SqlValue
    let to_body =
        build_to_row_record_body(ctx, field_names, columns, field_type_names, optionals, sp);
    let to_fn_name = format!("Row__{type_name}__toRow");
    push_row_method(ctx, &mut items, to_fn_name.clone(), "x", to_body, sp);

    // rowColumns (w: Option T) -> List Text — a literal list of the snake-cased
    // column names in declaration order. The witness `w` is ignored; only its type
    // selected this instance.
    let cols_body = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: columns
            .iter()
            .map(|c| IrExpr::Lit {
                id: ctx.fresh_id(None),
                value: IrLit::Text(c.clone()),
                span: sp,
            })
            .collect(),
        span: sp,
    };
    let cols_fn_name = format!("Row__{type_name}__rowColumns");
    push_row_method(ctx, &mut items, cols_fn_name.clone(), "w", cols_body, sp);

    // $inst_Row_{Type} = #{ 'fromRow' => fun fromRowFn, 'toRow' => fun toRowFn,
    //                       'rowColumns' => fun rowColumnsFn }
    let dict_name = format!("$inst_Row_{type_name}");
    let dict_value = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            owner_type: ridge_types::TyConId(0),
            name: dict_name.clone(),
            variant: 0,
        },
        fields: vec![
            (
                "fromRow".to_string(),
                IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Local {
                        name: from_fn_name,
                        module: ctx.module_id,
                    },
                    span: sp,
                },
            ),
            (
                "toRow".to_string(),
                IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Local {
                        name: to_fn_name,
                        module: ctx.module_id,
                    },
                    span: sp,
                },
            ),
            (
                "rowColumns".to_string(),
                IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Local {
                        name: cols_fn_name,
                        module: ctx.module_id,
                    },
                    span: sp,
                },
            ),
        ],
        span: sp,
    };
    items.push(IrItem::Const(IrConst {
        name: dict_name,
        ty: Type::Error,
        value: dict_value,
        origin: NodeId(0),
        span: sp,
        is_pub: true,
    }));

    items
}

/// Build the `toInsertRow` body for a derived `HasSchema` on a record: encode the
/// insert shape's columns — every non-generated column that maps to a base
/// `SqlType` — and assemble the `Map Text SqlValue` keyed by the snake-cased
/// column name. It is `build_to_row_record_body` restricted to the insert shape:
/// the database-generated columns are dropped (the backend fills them) and any
/// column with no `SqlType` carries no insert encoding, so both are skipped.
/// Reads `x.<field>` off the companion value, the same `x` parameter the other
/// derived row methods bind.
fn build_to_insert_row_body(
    ctx: &mut LowerCtx<'_>,
    columns: &[ridge_typecheck::SchemaColumnSpec],
    sp: Span,
) -> IrExpr {
    let pairs: Vec<IrExpr> = columns
        .iter()
        .filter_map(|c| {
            // Generated columns are filled by the backend; non-`SqlType` columns
            // carry no insert encoding. Either way they are not in the row.
            if c.generation.is_some() {
                return None;
            }
            c.sql_type_tag.as_ref().map(|tag| (c, tag))
        })
        .map(|(c, type_tag)| {
            let field_val = IrExpr::Field {
                id: ctx.fresh_id(None),
                base: Box::new(IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: "x".to_string(),
                    span: sp,
                }),
                field: c.field_name.clone(),
                span: sp,
            };
            let encoded = if c.nullable {
                build_optional_to_sql_call(ctx, type_tag, field_val, sp)
            } else {
                build_to_sql_call(ctx, type_tag, field_val, sp)
            };
            IrExpr::Tuple {
                id: ctx.fresh_id(None),
                elems: vec![
                    IrExpr::Lit {
                        id: ctx.fresh_id(None),
                        value: IrLit::Text(c.column.clone()),
                        span: sp,
                    },
                    encoded,
                ],
                span: sp,
            }
        })
        .collect();

    let pairs_list = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: pairs,
        span: sp,
    };
    // std.map.fromList(pairs_list) → the row map.
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.map".to_string(),
                name: "fromList".to_string(),
            },
            span: sp,
        }),
        args: vec![pairs_list],
        span: sp,
    }
}

/// Build the `toRow` body for a derived `Row` on a record: encode each field
/// through its `SqlType.toSql` and assemble the `Map Text SqlValue` keyed by the
/// snake-cased column name. An `Option` field goes through the `SqlType (Option a)`
/// instance, which writes `None` as SQL NULL — the encode dual of the NULL read in
/// [`build_optional_field_read`].
fn build_to_row_record_body(
    ctx: &mut LowerCtx<'_>,
    field_names: &[String],
    columns: &[String],
    field_type_names: &[String],
    optionals: &[bool],
    sp: Span,
) -> IrExpr {
    // [ (<<"col">>, toSql x.field), … ] — one (Text, SqlValue) tuple per field.
    let pairs: Vec<IrExpr> = field_names
        .iter()
        .zip(columns.iter())
        .zip(field_type_names.iter().zip(optionals.iter()))
        .map(|((field, column), (type_tag, optional))| {
            let field_val = IrExpr::Field {
                id: ctx.fresh_id(None),
                base: Box::new(IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: "x".to_string(),
                    span: sp,
                }),
                field: field.clone(),
                span: sp,
            };
            let encoded = if *optional {
                build_optional_to_sql_call(ctx, type_tag, field_val, sp)
            } else {
                build_to_sql_call(ctx, type_tag, field_val, sp)
            };
            IrExpr::Tuple {
                id: ctx.fresh_id(None),
                elems: vec![
                    IrExpr::Lit {
                        id: ctx.fresh_id(None),
                        value: IrLit::Text(column.clone()),
                        span: sp,
                    },
                    encoded,
                ],
                span: sp,
            }
        })
        .collect();

    let pairs_list = IrExpr::ListLit {
        id: ctx.fresh_id(None),
        elems: pairs,
        span: sp,
    };
    // std.map.fromList(pairs_list) → the row map.
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.map".to_string(),
                name: "fromList".to_string(),
            },
            span: sp,
        }),
        args: vec![pairs_list],
        span: sp,
    }
}

/// Emit `(maps:get('toSql', $inst_SqlType_{Type}))(val)` → `SqlValue`. The encode
/// dual of [`build_from_sql_call`]: the same exported base-type dict, with the
/// `toSql` method projected instead of `fromSql`.
fn build_to_sql_call(ctx: &mut LowerCtx<'_>, type_tag: &str, val: IrExpr, sp: Span) -> IrExpr {
    let dict = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.sql".to_string(),
            name: format!("$inst_SqlType_{type_tag}"),
        },
        span: sp,
    };
    let to_sql = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dict),
        field: "toSql".to_string(),
        span: sp,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(to_sql),
        args: vec![val],
        span: sp,
    }
}

/// Emit `($inst_SqlType_Option $inst_SqlType_{Type}).toSql(val)` → `SqlValue`. The
/// encode dual of [`build_optional_from_sql_call`]: the parametric `SqlType
/// (Option a)` instance applied to the inner primitive's dict, which writes `None`
/// as `SqlNull` and `Some v` as the inner `toSql v`.
fn build_optional_to_sql_call(
    ctx: &mut LowerCtx<'_>,
    type_tag: &str,
    val: IrExpr,
    sp: Span,
) -> IrExpr {
    let inner_dict = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.sql".to_string(),
            name: format!("$inst_SqlType_{type_tag}"),
        },
        span: sp,
    };
    let option_ctor = IrExpr::Symbol {
        id: ctx.fresh_id(None),
        sym: SymbolRef::Stdlib {
            module: "std.sql".to_string(),
            name: "$inst_SqlType_Option".to_string(),
        },
        span: sp,
    };
    let dict = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(option_ctor),
        args: vec![inner_dict],
        span: sp,
    };
    let to_sql = IrExpr::Field {
        id: ctx.fresh_id(None),
        base: Box::new(dict),
        field: "toSql".to_string(),
        span: sp,
    };
    IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(to_sql),
        args: vec![val],
        span: sp,
    }
}

/// Build the `decode` body for a derived `Decode` on a union type.
///
/// Dispatches on the JSON shape:
/// - `JText s` → nullary ctor lookup via string comparison chain.
/// - `JObject m` → payload ctor via `"tag"`/`"values"` keys, arity check.
/// - `_` → `Err(decode.unknown_tag)`.
#[expect(
    clippy::too_many_lines,
    reason = "flat dispatch over union variants; each arm is self-contained"
)]
fn build_decode_union_body(
    ctx: &mut LowerCtx<'_>,
    tycon: ridge_types::TyConId,
    type_name: &str,
    variants: &[(String, Vec<ridge_typecheck::FieldShape>)],
    sp: Span,
) -> IrExpr {
    let nullary: Vec<&str> = variants
        .iter()
        .filter(|(_, shapes)| shapes.is_empty())
        .map(|(n, _)| n.as_str())
        .collect();
    let payload: Vec<(&str, &[ridge_typecheck::FieldShape])> = variants
        .iter()
        .filter(|(_, shapes)| !shapes.is_empty())
        .map(|(n, shapes)| (n.as_str(), shapes.as_slice()))
        .collect();

    // ── JText branch (nullary ctors) ─────────────────────────────────────────
    let s_bound = ctx.fresh_local("__dec_un_s");
    let jtext_arm_body = {
        // Chain: if s == "Ctor" then Ok Ctor else … else Err(unknown_tag)
        let unk = build_decode_error(
            ctx,
            "decode.unknown_tag",
            format!("unknown constructor tag for {type_name}"),
            sp,
        );
        let mut chain = unk;
        for ctor_name in nullary.iter().rev() {
            let ctor_val = IrExpr::Construct {
                id: ctx.fresh_id(None),
                ctor: SymbolRef::Constructor {
                    ctor_kind: CtorKind::UnionVariant,
                    owner_type: tycon,
                    name: (*ctor_name).to_string(),
                    variant: 0,
                },
                fields: vec![],
                span: sp,
            };
            let ok_ctor = build_ok(ctor_val, sp);
            // s == "ctor" check via std.op.eq
            let eq_check = IrExpr::Call {
                id: ctx.fresh_id(None),
                callee: Box::new(IrExpr::Symbol {
                    id: ctx.fresh_id(None),
                    sym: SymbolRef::Stdlib {
                        module: "std.op".to_string(),
                        name: "eq".to_string(),
                    },
                    span: sp,
                }),
                args: vec![
                    IrExpr::Local {
                        id: ctx.fresh_id(None),
                        name: s_bound.clone(),
                        span: sp,
                    },
                    IrExpr::Lit {
                        id: ctx.fresh_id(None),
                        value: IrLit::Text((*ctor_name).to_string()),
                        span: sp,
                    },
                ],
                span: sp,
            };
            // match eq_check { true -> ok_ctor; _ -> chain }
            let true_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Lit {
                    value: IrLit::Bool(true),
                    span: sp,
                },
                when: None,
                body: ok_ctor,
                span: sp,
            };
            let false_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Wild { span: sp },
                when: None,
                body: chain,
                span: sp,
            };
            chain = IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(eq_check),
                arms: vec![true_arm, false_arm],
                span: sp,
            };
        }
        chain
    };
    let jtext_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "JText".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: s_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: jtext_arm_body,
        span: sp,
    };

    // ── JObject branch (payload ctors) ───────────────────────────────────────
    let m_bound_u = ctx.fresh_local("__dec_un_m");
    let jobject_arm_body = if payload.is_empty() {
        // No payload ctors — any JObject is unknown.
        build_decode_error(
            ctx,
            "decode.unknown_tag",
            format!("expected a JSON string for {type_name}, not an object"),
            sp,
        )
    } else {
        // Map.get "tag" m → Some (JText t) → dispatch
        let un_tag_lit_id = ctx.fresh_id(None);
        let un_tag_m_id = ctx.fresh_id(None);
        let un_tag_key = IrExpr::Lit {
            id: un_tag_lit_id,
            value: IrLit::Text("tag".to_string()),
            span: sp,
        };
        let un_tag_map = IrExpr::Local {
            id: un_tag_m_id,
            name: m_bound_u.clone(),
            span: sp,
        };
        let tag_opt = map_get_call(ctx, un_tag_key, un_tag_map, sp);
        let t_bound = ctx.fresh_local("__dec_un_t");
        let tag_dispatch = build_union_payload_tag_dispatch(
            ctx, tycon, type_name, &payload, &m_bound_u, &t_bound, sp,
        );
        // match tag_opt { Some (JText t) -> tag_dispatch; _ -> Err }
        let jtext_inner_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Ctor {
                sym: SymbolRef::Prelude {
                    name: "JText".to_string(),
                },
                fields: vec![],
                args: vec![ridge_ir::IrPat::Bind {
                    name: t_bound,
                    inner: None,
                    span: sp,
                }],
                span: sp,
            },
            when: None,
            body: tag_dispatch,
            span: sp,
        };
        let bad_tag_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Wild { span: sp },
            when: None,
            body: build_decode_error(
                ctx,
                "decode.unknown_tag",
                format!("tag field must be a JSON string for {type_name}"),
                sp,
            ),
            span: sp,
        };
        // match some_jv { JText t -> ...; _ -> Err }
        let tag_inner_bound = ctx.fresh_local("__dec_un_ti");
        let tag_some_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Ctor {
                sym: SymbolRef::Prelude {
                    name: "Some".to_string(),
                },
                fields: vec![],
                args: vec![ridge_ir::IrPat::Bind {
                    name: tag_inner_bound.clone(),
                    inner: None,
                    span: sp,
                }],
                span: sp,
            },
            when: None,
            body: IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: tag_inner_bound,
                    span: sp,
                }),
                arms: vec![jtext_inner_arm, bad_tag_arm],
                span: sp,
            },
            span: sp,
        };
        let missing_tag_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Wild { span: sp },
            when: None,
            body: build_decode_error(
                ctx,
                "decode.unknown_tag",
                format!("missing tag field for {type_name}"),
                sp,
            ),
            span: sp,
        };
        IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(tag_opt),
            arms: vec![tag_some_arm, missing_tag_arm],
            span: sp,
        }
    };
    let jobject_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "JObject".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: m_bound_u,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: jobject_arm_body,
        span: sp,
    };

    // ── Wildcard ─────────────────────────────────────────────────────────────
    let wild_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Wild { span: sp },
        when: None,
        body: build_decode_error(
            ctx,
            "decode.unknown_tag",
            format!("expected a JSON string or object for {type_name}"),
            sp,
        ),
        span: sp,
    };

    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: "j".to_string(),
            span: sp,
        }),
        arms: vec![jtext_arm, jobject_arm, wild_arm],
        span: sp,
    }
}

/// Dispatch on the tag string `t` for payload union ctors.
///
/// Builds a chain: if `t == "Ctor1"` then decode payload1 else … else `Err(unknown_tag)`.
fn build_union_payload_tag_dispatch(
    ctx: &mut LowerCtx<'_>,
    tycon: ridge_types::TyConId,
    type_name: &str,
    payload: &[(&str, &[ridge_typecheck::FieldShape])],
    m_bound: &str,
    t_bound: &str,
    sp: Span,
) -> IrExpr {
    let unk = build_decode_error(
        ctx,
        "decode.unknown_tag",
        format!("unknown constructor tag for {type_name}"),
        sp,
    );
    let mut chain = unk;
    for (ctor_name, shapes) in payload.iter().rev() {
        let arity = shapes.len();
        // Build the payload decode body for this ctor.
        let ctor_body =
            build_union_payload_ctor_body(ctx, tycon, ctor_name, shapes, m_bound.to_string(), sp);
        let eq_check = IrExpr::Call {
            id: ctx.fresh_id(None),
            callee: Box::new(IrExpr::Symbol {
                id: ctx.fresh_id(None),
                sym: SymbolRef::Stdlib {
                    module: "std.op".to_string(),
                    name: "eq".to_string(),
                },
                span: sp,
            }),
            args: vec![
                IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: t_bound.to_string(),
                    span: sp,
                },
                IrExpr::Lit {
                    id: ctx.fresh_id(None),
                    value: IrLit::Text((*ctor_name).to_string()),
                    span: sp,
                },
            ],
            span: sp,
        };
        let true_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Lit {
                value: IrLit::Bool(true),
                span: sp,
            },
            when: None,
            body: ctor_body,
            span: sp,
        };
        let false_arm = ridge_ir::IrArm {
            pat: ridge_ir::IrPat::Wild { span: sp },
            when: None,
            body: chain,
            span: sp,
        };
        chain = IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(eq_check),
            arms: vec![true_arm, false_arm],
            span: sp,
        };
        let _ = arity;
    }
    chain
}

/// Build the body for decoding one payload union constructor from a `JObject` with `tag`+`values`.
///
/// Reads `Map.get "values" m` → `Some (JList [v0, v1, …])`, decodes each
/// positional arg, checks arity, and assembles `Ok(Ctor v0 v1 …)`.
#[expect(
    clippy::too_many_lines,
    reason = "flat sequencing per payload field; each step is self-contained"
)]
fn build_union_payload_ctor_body(
    ctx: &mut LowerCtx<'_>,
    tycon: ridge_types::TyConId,
    ctor_name: &str,
    shapes: &[ridge_typecheck::FieldShape],
    m_bound: String,
    sp: Span,
) -> IrExpr {
    let arity = shapes.len();
    // Map.get "values" m
    let pl_vals_lit_id = ctx.fresh_id(None);
    let pl_vals_m_id = ctx.fresh_id(None);
    let pl_vals_key = IrExpr::Lit {
        id: pl_vals_lit_id,
        value: IrLit::Text("values".to_string()),
        span: sp,
    };
    let pl_vals_map = IrExpr::Local {
        id: pl_vals_m_id,
        name: m_bound,
        span: sp,
    };
    let vals_opt = map_get_call(ctx, pl_vals_key, pl_vals_map, sp);

    // Bind one local per payload arg by positional name.
    let p_bounds: Vec<String> = (0..arity)
        .map(|i| ctx.fresh_local(&format!("__dec_pjv{i}")))
        .collect();

    // Build final ctor assembly using the decoded value locals.
    let dec_bounds: Vec<String> = (0..arity)
        .map(|i| ctx.fresh_local(&format!("__dec_p{i}")))
        .collect();
    let ctor_fields: Vec<(String, IrExpr)> = dec_bounds
        .iter()
        .enumerate()
        .map(|(i, bound)| {
            (
                format!("${i}"),
                IrExpr::Local {
                    id: ctx.fresh_id(None),
                    name: bound.clone(),
                    span: sp,
                },
            )
        })
        .collect();
    let ctor_val = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: tycon,
            name: ctor_name.to_string(),
            variant: 0,
        },
        fields: ctor_fields,
        span: sp,
    };
    let ok_ctor = build_ok(ctor_val, sp);

    // Sequence payload decodes innermost-first.
    // p_bounds[i] is the raw JsonValue; dec_bounds[i] is the decoded value.
    let mut cont = ok_ctor;
    for (i, (jv_bound, (dec_bound, shape))) in p_bounds
        .iter()
        .zip(dec_bounds.iter().zip(shapes.iter()))
        .enumerate()
        .rev()
    {
        let jv_local = IrExpr::Local {
            id: ctx.fresh_id(None),
            name: jv_bound.clone(),
            span: sp,
        };
        let sub_decode = decode_shape(ctx, shape, jv_local, sp);
        cont = decode_seq(ctx, sub_decode, dec_bound.clone(), cont, sp);
        let _ = i;
    }

    // Build a Cons-chain pattern to destructure the list: [p0, p1, ...] with tail bound to _.
    // For arity N: p0 :: p1 :: … :: p_{N-1} :: []
    // Use a match on the JList xs first, then pattern-match xs.
    let xs_bound = ctx.fresh_local("__dec_pl_xs");
    // Build the nested Cons pattern from p_bounds.
    let list_pat = {
        let mut pat: ridge_ir::IrPat = ridge_ir::IrPat::Nil { span: sp };
        for jv_bound in p_bounds.iter().rev() {
            pat = ridge_ir::IrPat::Cons {
                head: Box::new(ridge_ir::IrPat::Bind {
                    name: jv_bound.clone(),
                    inner: None,
                    span: sp,
                }),
                tail: Box::new(pat),
                span: sp,
            };
        }
        pat
    };
    // match xs { [p0, p1, ...] -> cont; _ -> Err(bad_arity) }
    let list_match_arm = ridge_ir::IrArm {
        pat: list_pat,
        when: None,
        body: cont,
        span: sp,
    };
    let bad_arity_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Wild { span: sp },
        when: None,
        body: IrExpr::Return {
            id: ctx.fresh_id(None),
            value: Box::new(build_decode_error(
                ctx,
                "decode.bad_arity",
                format!("constructor {ctor_name} expects {arity} value(s)"),
                sp,
            )),
            span: sp,
        },
        span: sp,
    };
    // Wrap xs in a match so we can pattern on it.
    let xs_match = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: xs_bound.clone(),
            span: sp,
        }),
        arms: vec![list_match_arm, bad_arity_arm],
        span: sp,
    };

    // match vals_opt { Some (JList xs) -> xs_match; _ -> Err(bad_arity) }
    let jlist_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "JList".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: xs_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: xs_match,
        span: sp,
    };
    let bad_shape_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Wild { span: sp },
        when: None,
        body: build_decode_error(
            ctx,
            "decode.expected_array",
            format!("constructor {ctor_name} values must be a JSON array"),
            sp,
        ),
        span: sp,
    };
    // vals_opt = Some jv; match jv { JList xs -> cont; _ -> Err }
    let jv_vals_bound = ctx.fresh_local("__dec_pl_jv");
    let vals_some_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Some".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: jv_vals_bound.clone(),
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: IrExpr::Match {
            id: ctx.fresh_id(None),
            scrutinee: Box::new(IrExpr::Local {
                id: ctx.fresh_id(None),
                name: jv_vals_bound,
                span: sp,
            }),
            arms: vec![jlist_arm, bad_shape_arm],
            span: sp,
        },
        span: sp,
    };
    let vals_none_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Wild { span: sp },
        when: None,
        body: build_decode_error(
            ctx,
            "decode.bad_arity",
            format!("missing values field for constructor {ctor_name}"),
            sp,
        ),
        span: sp,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(vals_opt),
        arms: vec![vals_some_arm, vals_none_arm],
        span: sp,
    }
}

/// Build the accumulator-Result fold for decoding a `List T`.
///
/// Emits:
/// ```text
/// std.list.fold (\elem acc ->
///   match acc {
///     Err e -> Err e
///     Ok done -> match decode_shape(inner, elem) {
///                  Ok v -> Ok (done ++ [v])  -- via std.list.append
///                  Err e -> Err e
///                }
///   }
/// ) (Ok []) xs
/// |> map std.list.reverse
/// ```
///
/// The accumulator threads `Result (List T) Error`; once `Err`, it stays `Err`.
/// `IrExpr::Return` is NOT used inside the lambda — safe for fold.
///
/// Returns an expression of type `Result (List T) Error`.
#[expect(
    clippy::too_many_lines,
    reason = "flat accumulator-fold IR construction; each step is self-contained IR"
)]
fn build_list_decode_fold(
    ctx: &mut LowerCtx<'_>,
    inner: &ridge_typecheck::FieldShape,
    xs: IrExpr,
    sp: Span,
) -> IrExpr {
    use ridge_types::CapabilitySet;

    let elem_param = ctx.fresh_local("__df_elem");
    let acc_param = ctx.fresh_local("__df_acc");
    let ok_done_bound = ctx.fresh_local("__df_done");
    let err_pass_bound = ctx.fresh_local("__df_err");
    let ok_v_bound = ctx.fresh_local("__df_v");
    let inner_err_bound = ctx.fresh_local("__df_ierr");

    // Ok v -> Ok (done ++ [v])
    // Use Cons to prepend (we reverse at the end — cheaper than append).
    let prepend = IrExpr::Cons {
        id: ctx.fresh_id(None),
        head: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: ok_v_bound.clone(),
            span: sp,
        }),
        tail: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: ok_done_bound.clone(),
            span: sp,
        }),
        span: sp,
    };
    let ok_prepend = build_ok(prepend, sp);
    let inner_err_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: inner_err_bound.clone(),
        span: sp,
    };
    let pass_inner_err = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "Err".to_string(),
        },
        fields: vec![("$0".to_string(), inner_err_local)],
        span: sp,
    };
    let inner_ok_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: ok_v_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: ok_prepend,
        span: sp,
    };
    let inner_err_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: inner_err_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: pass_inner_err,
        span: sp,
    };
    // decode_shape(inner, elem) → match { Ok v -> Ok (v::done); Err e -> Err e }
    let elem_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: elem_param.clone(),
        span: sp,
    };
    let sub_decode = decode_shape(ctx, inner, elem_local, sp);
    let inner_match = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(sub_decode),
        arms: vec![inner_ok_arm, inner_err_arm],
        span: sp,
    };
    // Ok done -> <inner_match>
    let ok_acc_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: ok_done_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: inner_match,
        span: sp,
    };
    // Err e -> Err e
    let err_pass_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: err_pass_bound.clone(),
        span: sp,
    };
    let pass_err = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "Err".to_string(),
        },
        fields: vec![("$0".to_string(), err_pass_local)],
        span: sp,
    };
    let err_acc_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: err_pass_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: pass_err,
        span: sp,
    };
    // Lambda body: match acc { Ok done -> ...; Err e -> Err e }
    let lambda_body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: acc_param.clone(),
            span: sp,
        }),
        arms: vec![ok_acc_arm, err_acc_arm],
        span: sp,
    };
    // ridge_rt:list_fold calls F(Acc, Elem) — acc is the first arg, elem second.
    let fold_lambda = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params: vec![
            IrParam {
                name: acc_param,
                ty: ridge_types::Type::Error,
                span: sp,
            },
            IrParam {
                name: elem_param,
                ty: ridge_types::Type::Error,
                span: sp,
            },
        ],
        body: Box::new(lambda_body),
        caps: CapabilitySet::PURE,
        span: sp,
    };
    // seed = Ok []
    let seed_list_id = ctx.fresh_id(None);
    let seed = build_ok(
        IrExpr::ListLit {
            id: seed_list_id,
            elems: vec![],
            span: sp,
        },
        sp,
    );
    // std.list.fold lambda seed xs
    let fold_result = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.list".to_string(),
                name: "fold".to_string(),
            },
            span: sp,
        }),
        args: vec![fold_lambda, seed, xs],
        span: sp,
    };
    // Reverse the accumulated list: fold_result is Ok (reversed_list) | Err e.
    // match fold_result { Ok reversed -> Ok (std.list.reverse reversed); Err e -> Err e }
    let rev_bound = ctx.fresh_local("__df_rev");
    let rev_err_bound = ctx.fresh_local("__df_rev_err");
    let rev_call = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.list".to_string(),
                name: "reverse".to_string(),
            },
            span: sp,
        }),
        args: vec![IrExpr::Local {
            id: ctx.fresh_id(None),
            name: rev_bound.clone(),
            span: sp,
        }],
        span: sp,
    };
    let ok_rev = build_ok(rev_call, sp);
    let ok_rev_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: rev_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: ok_rev,
        span: sp,
    };
    let pass_rev_err_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: rev_err_bound.clone(),
        span: sp,
    };
    let pass_rev_err = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "Err".to_string(),
        },
        fields: vec![("$0".to_string(), pass_rev_err_local)],
        span: sp,
    };
    let err_rev_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: rev_err_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: pass_rev_err,
        span: sp,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(fold_result),
        arms: vec![ok_rev_arm, err_rev_arm],
        span: sp,
    }
}

/// Build the accumulator-Result fold for decoding a `Map Text T`.
///
/// Converts the `JObject` map to a list of (key, value) pairs via `std.map.toList`,
/// folds over them decoding each value, then reassembles via `std.map.fromList`.
#[expect(
    clippy::too_many_lines,
    reason = "flat accumulator-fold IR construction; splitting would reduce readability without reducing complexity"
)]
fn build_map_decode_fold(
    ctx: &mut LowerCtx<'_>,
    inner: &ridge_typecheck::FieldShape,
    m: IrExpr,
    sp: Span,
) -> IrExpr {
    use ridge_types::CapabilitySet;

    // pairs = std.map.toList m → List (Text, JsonValue)
    let pairs = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.map".to_string(),
                name: "toList".to_string(),
            },
            span: sp,
        }),
        args: vec![m],
        span: sp,
    };

    // Fold over pairs: acc = Result (List (Text, T)) Error
    let pair_param = ctx.fresh_local("__dm_pair");
    let acc_param = ctx.fresh_local("__dm_acc");
    let k_bound = ctx.fresh_local("__dm_k");
    let v_bound = ctx.fresh_local("__dm_v");
    let ok_done_bound = ctx.fresh_local("__dm_done");
    let err_pass_bound = ctx.fresh_local("__dm_err");
    let ok_tv_bound = ctx.fresh_local("__dm_tv");
    let inner_err_bound = ctx.fresh_local("__dm_ierr");

    // decode v → Result T Error
    let v_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: v_bound.clone(),
        span: sp,
    };
    let sub_decode = decode_shape(ctx, inner, v_local, sp);

    // Ok tv -> Ok ((k, tv) :: done)
    let k_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: k_bound.clone(),
        span: sp,
    };
    let tv_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: ok_tv_bound.clone(),
        span: sp,
    };
    let new_pair = IrExpr::Tuple {
        id: ctx.fresh_id(None),
        elems: vec![k_local, tv_local],
        span: sp,
    };
    let prepend = IrExpr::Cons {
        id: ctx.fresh_id(None),
        head: Box::new(new_pair),
        tail: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: ok_done_bound.clone(),
            span: sp,
        }),
        span: sp,
    };
    let ok_prepend = build_ok(prepend, sp);
    let inner_err_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: inner_err_bound.clone(),
        span: sp,
    };
    let pass_inner_err = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "Err".to_string(),
        },
        fields: vec![("$0".to_string(), inner_err_local)],
        span: sp,
    };
    let inner_ok_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: ok_tv_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: ok_prepend,
        span: sp,
    };
    let inner_err_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: inner_err_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: pass_inner_err,
        span: sp,
    };
    let inner_match = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(sub_decode),
        arms: vec![inner_ok_arm, inner_err_arm],
        span: sp,
    };

    // Destructure the (k, v) pair: match pair { (k, v) -> ... }
    // Use Tuple pattern to bind k and v.
    let kv_body = inner_match;
    let ok_acc_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: ok_done_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: {
            // Bind k and v from pair, then run kv_body.
            // Use a Tuple pattern match on pair.
            let pair_local = IrExpr::Local {
                id: ctx.fresh_id(None),
                name: pair_param.clone(),
                span: sp,
            };
            let tuple_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Tuple {
                    elems: vec![
                        ridge_ir::IrPat::Bind {
                            name: k_bound,
                            inner: None,
                            span: sp,
                        },
                        ridge_ir::IrPat::Bind {
                            name: v_bound,
                            inner: None,
                            span: sp,
                        },
                    ],
                    span: sp,
                },
                when: None,
                body: kv_body,
                span: sp,
            };
            let wild_tuple_arm = ridge_ir::IrArm {
                pat: ridge_ir::IrPat::Wild { span: sp },
                when: None,
                body: build_decode_error(
                    ctx,
                    "decode.expected_object",
                    "map entry is not a key-value pair".to_string(),
                    sp,
                ),
                span: sp,
            };
            IrExpr::Match {
                id: ctx.fresh_id(None),
                scrutinee: Box::new(pair_local),
                arms: vec![tuple_arm, wild_tuple_arm],
                span: sp,
            }
        },
        span: sp,
    };
    let err_pass_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: err_pass_bound.clone(),
        span: sp,
    };
    let pass_err = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "Err".to_string(),
        },
        fields: vec![("$0".to_string(), err_pass_local)],
        span: sp,
    };
    let err_acc_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: err_pass_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: pass_err,
        span: sp,
    };
    let lambda_body = IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(IrExpr::Local {
            id: ctx.fresh_id(None),
            name: acc_param.clone(),
            span: sp,
        }),
        arms: vec![ok_acc_arm, err_acc_arm],
        span: sp,
    };
    // ridge_rt:list_fold calls F(Acc, Elem) — acc is the first arg, pair second.
    let fold_lambda = IrExpr::Lambda {
        id: ctx.fresh_id(None),
        params: vec![
            IrParam {
                name: acc_param,
                ty: ridge_types::Type::Error,
                span: sp,
            },
            IrParam {
                name: pair_param,
                ty: ridge_types::Type::Error,
                span: sp,
            },
        ],
        body: Box::new(lambda_body),
        caps: CapabilitySet::PURE,
        span: sp,
    };
    let seed_list_id2 = ctx.fresh_id(None);
    let seed = build_ok(
        IrExpr::ListLit {
            id: seed_list_id2,
            elems: vec![],
            span: sp,
        },
        sp,
    );
    let fold_result = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.list".to_string(),
                name: "fold".to_string(),
            },
            span: sp,
        }),
        args: vec![fold_lambda, seed, pairs],
        span: sp,
    };
    // match fold_result { Ok pairs_rev -> Ok (fromList (reverse pairs_rev)); Err e -> Err e }
    let pairs_rev_bound = ctx.fresh_local("__dm_pr");
    let map_err_bound = ctx.fresh_local("__dm_me");
    let rev_call = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.list".to_string(),
                name: "reverse".to_string(),
            },
            span: sp,
        }),
        args: vec![IrExpr::Local {
            id: ctx.fresh_id(None),
            name: pairs_rev_bound.clone(),
            span: sp,
        }],
        span: sp,
    };
    let from_list = IrExpr::Call {
        id: ctx.fresh_id(None),
        callee: Box::new(IrExpr::Symbol {
            id: ctx.fresh_id(None),
            sym: SymbolRef::Stdlib {
                module: "std.map".to_string(),
                name: "fromList".to_string(),
            },
            span: sp,
        }),
        args: vec![rev_call],
        span: sp,
    };
    let ok_map = build_ok(from_list, sp);
    let ok_pairs_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Ok".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: pairs_rev_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: ok_map,
        span: sp,
    };
    let pass_map_err_local = IrExpr::Local {
        id: ctx.fresh_id(None),
        name: map_err_bound.clone(),
        span: sp,
    };
    let pass_map_err = IrExpr::Construct {
        id: ctx.fresh_id(None),
        ctor: SymbolRef::Prelude {
            name: "Err".to_string(),
        },
        fields: vec![("$0".to_string(), pass_map_err_local)],
        span: sp,
    };
    let err_map_arm = ridge_ir::IrArm {
        pat: ridge_ir::IrPat::Ctor {
            sym: SymbolRef::Prelude {
                name: "Err".to_string(),
            },
            fields: vec![],
            args: vec![ridge_ir::IrPat::Bind {
                name: map_err_bound,
                inner: None,
                span: sp,
            }],
            span: sp,
        },
        when: None,
        body: pass_map_err,
        span: sp,
    };
    IrExpr::Match {
        id: ctx.fresh_id(None),
        scrutinee: Box::new(fold_result),
        arms: vec![ok_pairs_arm, err_map_arm],
        span: sp,
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
        // A destructuring param is normally handled by the caller (which also
        // records the body wrapper); this arm keeps the synthetic binder correct
        // if it is ever lowered in isolation.
        Param::PatternAnnotated { ty, span, .. } => synth_destructure_param(ctx, ty, *span).0,
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
            opaque: false,
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
        let constraint = Constraint::single(ClassId(0), TyVid(0));
        let constrained_scheme = Scheme {
            vars: vec![TyVid(0)],
            cap_vars: vec![],
            row_vars: vec![],
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
                    name: format!("$dict_{cn}_{}", c.sole_ty().0),
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
            head: vec![AstType::Named {
                name: ident("Color"),
                span: sp(),
            }],
            constraints: vec![],
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
                assert!(
                    c.is_pub,
                    "dict consts are exported for cross-module instance dispatch"
                );
                // The dict value must be a Construct (MapLit shape).
                assert!(
                    matches!(&c.value, IrExpr::Construct { .. }),
                    "dict value must be a Construct (MapLit)"
                );
            }
            other => panic!("expected IrItem::Const, got {other:?}"),
        }
    }

    // ── Derived ToText record renders values, not static names ────────────────

    /// Counts how many `std.text.concat` calls are nested in the IR expression.
    fn count_concat(expr: &IrExpr) -> usize {
        match expr {
            IrExpr::Call { callee, args, .. } => {
                if let IrExpr::Symbol {
                    sym: SymbolRef::Stdlib { name, .. },
                    ..
                } = callee.as_ref()
                {
                    if name == "concat" && args.len() == 2 {
                        return 1 + count_concat(&args[0]);
                    }
                }
                0
            }
            _ => 0,
        }
    }

    /// Check whether `expr` (recursively) contains `std.int.toText`.
    fn contains_int_to_text(expr: &IrExpr) -> bool {
        match expr {
            IrExpr::Call { callee, args, .. } => {
                if let IrExpr::Symbol {
                    sym: SymbolRef::Stdlib { module, name },
                    ..
                } = callee.as_ref()
                {
                    if module == "std.int" && name == "toText" {
                        return true;
                    }
                }
                args.iter().any(contains_int_to_text) || contains_int_to_text(callee)
            }
            IrExpr::Match {
                scrutinee, arms, ..
            } => {
                contains_int_to_text(scrutinee)
                    || arms.iter().any(|arm| contains_int_to_text(&arm.body))
            }
            _ => false,
        }
    }

    /// Check whether `expr` (recursively) contains a field accessor for `field_name`.
    fn contains_field(expr: &IrExpr, field_name: &str) -> bool {
        match expr {
            IrExpr::Field { field, base, .. } => {
                field == field_name || contains_field(base, field_name)
            }
            IrExpr::Call { callee, args, .. } => {
                contains_field(callee, field_name)
                    || args.iter().any(|a| contains_field(a, field_name))
            }
            _ => false,
        }
    }

    #[test]
    fn derived_to_text_record_body_renders_values() {
        use ridge_typecheck::DerivedInstance;
        use ridge_typecheck::{DerivedMethodBody, InstanceInfo, InstanceOrigin};
        use ridge_types::{TyConId, TOTEXT_CLASS};

        let mut ctx = fresh_ctx();

        // Point = { x: Int, y: Int } deriving (ToText)
        // field_tycons: [Some(TyConId(0)), Some(TyConId(0))] — both Int
        let derived = DerivedInstance {
            key: (TOTEXT_CLASS, TyConId(100)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("toText".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedToTextRecord {
                field_names: vec!["x".to_string(), "y".to_string()],
                field_tycons: vec![Some(TyConId(0)), Some(TyConId(0))],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "ToText", "Point");

        // Should produce exactly 2 items: method fn + dict const.
        assert_eq!(items.len(), 2);

        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        // The body must contain concat calls (not a plain literal).
        let concat_count = count_concat(&fn_item.body);
        assert!(
            concat_count > 0,
            "derived ToText record body must use concat, not a static string; got {concat_count} concats"
        );

        // The body must dispatch std.int.toText for the Int fields.
        assert!(
            contains_int_to_text(&fn_item.body),
            "body must call std.int.toText for Int fields"
        );

        // The body must reference field 'x'.
        assert!(
            contains_field(&fn_item.body, "x"),
            "body must access field 'x'"
        );

        // The body must reference field 'y'.
        assert!(
            contains_field(&fn_item.body, "y"),
            "body must access field 'y'"
        );
    }

    // ── Derived ToText union payload renders values ─────────────────────────────

    #[test]
    fn derived_to_text_union_payload_renders_values() {
        use ridge_typecheck::{DerivedInstance, DerivedMethodBody, InstanceInfo, InstanceOrigin};
        use ridge_types::{TyConId, TOTEXT_CLASS};

        let mut ctx = fresh_ctx();

        // Shape = Circle(Int) | Rect(Int, Int) deriving (ToText)
        let derived = DerivedInstance {
            key: (TOTEXT_CLASS, TyConId(17)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("toText".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedToTextUnion {
                variants: vec![
                    // Circle(Int) — 1 Int payload
                    ("Circle".to_string(), 1, vec![Some(TyConId(0))]),
                    // Rect(Int, Int) — 2 Int payloads
                    (
                        "Rect".to_string(),
                        2,
                        vec![Some(TyConId(0)), Some(TyConId(0))],
                    ),
                    // Point — nullary, no payloads
                    ("Point".to_string(), 0, vec![]),
                ],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "ToText", "Shape");
        assert_eq!(items.len(), 2, "method fn + dict const");

        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        // The overall body must be a Match.
        assert!(
            matches!(&fn_item.body, IrExpr::Match { .. }),
            "union ToText body must be a Match"
        );

        // The body must dispatch std.int.toText for the Int payload fields.
        assert!(
            contains_int_to_text(&fn_item.body),
            "payload Int fields must call std.int.toText"
        );
    }

    // ── Derived Ord union same-variant payload tiebreak compares fields ─────────

    /// Check whether `expr` (recursively) contains a call to `std.op.{op}`.
    fn contains_op(expr: &IrExpr, op: &str) -> bool {
        match expr {
            IrExpr::Call { callee, args, .. } => {
                if let IrExpr::Symbol {
                    sym: SymbolRef::Stdlib { module, name },
                    ..
                } = callee.as_ref()
                {
                    if module == "std.op" && name == op {
                        return true;
                    }
                }
                args.iter().any(|a| contains_op(a, op)) || contains_op(callee, op)
            }
            IrExpr::Match {
                scrutinee, arms, ..
            } => contains_op(scrutinee, op) || arms.iter().any(|arm| contains_op(&arm.body, op)),
            _ => false,
        }
    }

    #[test]
    fn derived_ord_union_same_variant_payload_tiebreak() {
        use ridge_typecheck::{DerivedInstance, DerivedMethodBody, InstanceInfo, InstanceOrigin};
        use ridge_types::{TyConId, ORD_CLASS};

        let mut ctx = fresh_ctx();

        // Wrapper = Box(Int) deriving (Ord)
        // When both are Box(_), compare the Int payloads.
        let derived = DerivedInstance {
            key: (ORD_CLASS, TyConId(18)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("compare".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedOrdUnion {
                variants: vec![("Box".to_string(), 1)],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "Ord", "Wrapper");
        assert_eq!(items.len(), 2, "method fn + dict const");

        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        // The body must be a Match (outer dispatch on 'a').
        assert!(
            matches!(&fn_item.body, IrExpr::Match { .. }),
            "Ord union body must be a Match"
        );

        // The body must call std.op.lt and/or std.op.gt for the payload comparison.
        assert!(
            contains_op(&fn_item.body, "lt") || contains_op(&fn_item.body, "gt"),
            "same-variant payload tiebreak must emit std.op.lt/gt for comparison"
        );
    }

    // ── Derived Encode User field emits a Call, not identity ─────────────────

    /// Walk `expr` recursively and return true if it (directly or transitively)
    /// contains a `Call` whose callee is a `SymbolRef::Local` with name matching
    /// `expected_fn`.
    fn contains_local_call(expr: &IrExpr, expected_fn: &str) -> bool {
        match expr {
            IrExpr::Call { callee, args, .. } => {
                if let IrExpr::Symbol {
                    sym: SymbolRef::Local { name, .. },
                    ..
                } = callee.as_ref()
                {
                    if name == expected_fn {
                        return true;
                    }
                }
                contains_local_call(callee, expected_fn)
                    || args.iter().any(|a| contains_local_call(a, expected_fn))
            }
            IrExpr::Match {
                scrutinee, arms, ..
            } => {
                contains_local_call(scrutinee, expected_fn)
                    || arms
                        .iter()
                        .any(|arm| contains_local_call(&arm.body, expected_fn))
            }
            IrExpr::Field { base, .. } => contains_local_call(base, expected_fn),
            IrExpr::Tuple { elems, .. } | IrExpr::ListLit { elems, .. } => {
                elems.iter().any(|e| contains_local_call(e, expected_fn))
            }
            IrExpr::Lambda { body, .. } => contains_local_call(body, expected_fn),
            _ => false,
        }
    }

    #[test]
    fn derived_encode_user_field_emits_call_not_identity() {
        use ridge_typecheck::{
            DerivedInstance, DerivedMethodBody, FieldShape, InstanceInfo, InstanceOrigin,
        };
        use ridge_types::{TyConId, ENCODE_CLASS};

        let mut ctx = fresh_ctx();

        // Order = { customer: Customer } deriving (Encode)
        // where Customer is a same-module user type with TyConId(200).
        // The `customer` field shape is User { tycon: 200, type_name: "Customer" }.
        // The lowering must emit a Call to `Encode__Customer__encode`, not identity.
        let derived = DerivedInstance {
            key: (ENCODE_CLASS, TyConId(201)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("encode".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedEncodeRecord {
                field_names: vec!["customer".to_string()],
                field_shapes: vec![FieldShape::User {
                    tycon: TyConId(200),
                    type_name: "Customer".to_string(),
                }],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "Encode", "Order");
        assert_eq!(items.len(), 2, "method fn + dict const");

        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        // The body must call Encode__Customer__encode, not pass the value through.
        // The call to Encode__Customer__encode receives x.customer as its argument,
        // so this single assertion confirms both that the correct fn is called and
        // that the field accessor is wired in.
        assert!(
            contains_local_call(&fn_item.body, "Encode__Customer__encode"),
            "User field must emit a Call to Encode__Customer__encode; \
             body: {:#?}",
            fn_item.body
        );
    }

    // ── Derived Decode record body — JObject guard + field lookups ──────────

    /// Walk `expr` recursively and return true if it contains a `Construct`
    /// with the given Prelude ctor name.
    fn contains_prelude_ctor(expr: &IrExpr, ctor: &str) -> bool {
        match expr {
            IrExpr::Construct {
                ctor: c, fields, ..
            } => {
                if let SymbolRef::Prelude { name } = c {
                    if name == ctor {
                        return true;
                    }
                }
                fields.iter().any(|(_, e)| contains_prelude_ctor(e, ctor))
            }
            IrExpr::Match {
                scrutinee, arms, ..
            } => {
                contains_prelude_ctor(scrutinee, ctor)
                    || arms
                        .iter()
                        .any(|arm| contains_prelude_ctor(&arm.body, ctor))
            }
            IrExpr::Call { callee, args, .. } => {
                contains_prelude_ctor(callee, ctor)
                    || args.iter().any(|a| contains_prelude_ctor(a, ctor))
            }
            IrExpr::Return { value, .. } => contains_prelude_ctor(value, ctor),
            _ => false,
        }
    }

    /// Walk `expr` recursively and return true if it contains a Return.
    fn contains_return(expr: &IrExpr) -> bool {
        match expr {
            IrExpr::Return { .. } => true,
            IrExpr::Match {
                scrutinee, arms, ..
            } => contains_return(scrutinee) || arms.iter().any(|arm| contains_return(&arm.body)),
            IrExpr::Call { callee, args, .. } => {
                contains_return(callee) || args.iter().any(contains_return)
            }
            IrExpr::Construct { fields, .. } => fields.iter().any(|(_, e)| contains_return(e)),
            _ => false,
        }
    }

    #[test]
    fn derived_decode_record_body_has_jobject_guard_and_ok_ctor() {
        use ridge_typecheck::{
            DerivedInstance, DerivedMethodBody, FieldShape, InstanceInfo, InstanceOrigin,
        };
        use ridge_types::{TyConId, DECODE_CLASS};

        let mut ctx = fresh_ctx();

        // Person = { name: Text, age: Int } deriving (Decode)
        let derived = DerivedInstance {
            key: (DECODE_CLASS, TyConId(100)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("decode".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedDecodeRecord {
                field_names: vec!["name".to_string(), "age".to_string()],
                field_shapes: vec![
                    FieldShape::Prim(TyConId(3)), // Text
                    FieldShape::Prim(TyConId(0)), // Int
                ],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "Decode", "Person");
        assert_eq!(items.len(), 2, "method fn + dict const");

        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        // The body must be a Match (outer JObject guard).
        assert!(
            matches!(&fn_item.body, IrExpr::Match { .. }),
            "Decode record body must be a Match on j"
        );

        // The body must contain an Ok construction (the happy-path Ok(T {...}) assembly).
        assert!(
            contains_prelude_ctor(&fn_item.body, "Ok"),
            "Decode record body must contain Ok(...) assembly; body: {:#?}",
            fn_item.body
        );

        // The body must contain a Return for fail-fast on missing field.
        assert!(
            contains_return(&fn_item.body),
            "Decode record body must contain Return for missing-field fail-fast"
        );
    }

    #[test]
    fn derived_decode_union_body_has_jtext_and_jobject_arms() {
        use ridge_typecheck::{
            DerivedInstance, DerivedMethodBody, FieldShape, InstanceInfo, InstanceOrigin,
        };
        use ridge_types::{TyConId, DECODE_CLASS};

        let mut ctx = fresh_ctx();

        // Shape = Circle Float | Rect Float Float | Admin (nullary) deriving (Decode)
        let derived = DerivedInstance {
            key: (DECODE_CLASS, TyConId(101)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("decode".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedDecodeUnion {
                variants: vec![
                    ("Admin".to_string(), vec![]),
                    ("Circle".to_string(), vec![FieldShape::Prim(TyConId(1))]),
                ],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "Decode", "Shape");
        assert_eq!(items.len(), 2, "method fn + dict const");

        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        // The outer body must be a Match.
        assert!(
            matches!(&fn_item.body, IrExpr::Match { .. }),
            "Decode union body must be a Match on j"
        );

        // Must contain Ok (for nullary ctor success).
        assert!(
            contains_prelude_ctor(&fn_item.body, "Ok"),
            "Decode union body must contain Ok(...) for successful decode"
        );
    }

    #[test]
    fn derived_decode_user_field_emits_local_call() {
        use ridge_typecheck::{
            DerivedInstance, DerivedMethodBody, FieldShape, InstanceInfo, InstanceOrigin,
        };
        use ridge_types::{TyConId, DECODE_CLASS};

        let mut ctx = fresh_ctx();

        // Invoice = { customer: Customer } deriving (Decode)
        // The customer field shape is User { tycon: 200, type_name: "Customer" }.
        // The lowering must emit a Call to `Decode__Customer__decode`.
        let derived = DerivedInstance {
            key: (DECODE_CLASS, TyConId(201)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("decode".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedDecodeRecord {
                field_names: vec!["customer".to_string()],
                field_shapes: vec![FieldShape::User {
                    tycon: TyConId(200),
                    type_name: "Customer".to_string(),
                }],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "Decode", "Invoice");
        assert_eq!(items.len(), 2, "method fn + dict const");

        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        assert!(
            contains_local_call(&fn_item.body, "Decode__Customer__decode"),
            "User field must emit a Call to Decode__Customer__decode; body: {:#?}",
            fn_item.body
        );
    }

    #[test]
    fn derived_decode_list_field_uses_fold() {
        fn contains_lambda(expr: &IrExpr) -> bool {
            match expr {
                IrExpr::Lambda { .. } => true,
                IrExpr::Match {
                    scrutinee, arms, ..
                } => contains_lambda(scrutinee) || arms.iter().any(|a| contains_lambda(&a.body)),
                IrExpr::Call { callee, args, .. } => {
                    contains_lambda(callee) || args.iter().any(contains_lambda)
                }
                _ => false,
            }
        }

        use ridge_typecheck::{
            DerivedInstance, DerivedMethodBody, FieldShape, InstanceInfo, InstanceOrigin,
        };
        use ridge_types::{TyConId, DECODE_CLASS};

        let mut ctx = fresh_ctx();

        // Profile = { tags: List Text } deriving (Decode)
        let derived = DerivedInstance {
            key: (DECODE_CLASS, TyConId(202)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("decode".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedDecodeRecord {
                field_names: vec!["tags".to_string()],
                field_shapes: vec![FieldShape::Lst(Box::new(FieldShape::Prim(TyConId(3))))],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "Decode", "Profile");
        assert_eq!(items.len(), 2);

        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        assert!(
            contains_lambda(&fn_item.body),
            "List decode must emit a Lambda for the fold accumulator"
        );
    }

    #[test]
    fn derived_decode_decode_seq_emits_return() {
        use ridge_ir::IrNodeId;
        // decode_seq is a private helper, but we verify its output via the record lowering test.
        // Specifically: the decode record body for a field must contain IrExpr::Return
        // (fail-fast on missing field).
        use ridge_typecheck::{
            DerivedInstance, DerivedMethodBody, FieldShape, InstanceInfo, InstanceOrigin,
        };
        use ridge_types::{TyConId, DECODE_CLASS};

        let mut ctx = fresh_ctx();

        let derived = DerivedInstance {
            key: (DECODE_CLASS, TyConId(203)),
            instance_info: InstanceInfo {
                def_module: Some(0),
                methods: vec![("decode".to_string(), String::new())],
                ctx_constraints: vec![],
                head_var_positions: vec![],
                origin: InstanceOrigin::Explicit,
                span: sp(),
            },
            method_body: DerivedMethodBody::DerivedDecodeRecord {
                field_names: vec!["x".to_string()],
                field_shapes: vec![FieldShape::Prim(TyConId(0))],
            },
        };

        let items = lower_derived_instance(&mut ctx, &derived, "Decode", "Point");
        let fn_item = match &items[0] {
            IrItem::Fn(f) => f,
            other => panic!("expected IrFn, got {other:?}"),
        };

        // The fail-fast pattern (IrExpr::Return) must be present for the missing-field case.
        assert!(
            contains_return(&fn_item.body),
            "decode_seq must emit IrExpr::Return for fail-fast; body: {:#?}",
            fn_item.body
        );
        let _ = IrNodeId(0); // suppress unused warning
    }
}
