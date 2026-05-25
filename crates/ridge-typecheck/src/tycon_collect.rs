//! User-defined `TyCon` collection from a module AST (§4.2, T17 wiring).
//!
//! Walks every `TypeDecl` and `ActorDecl` in a module, converts them to
//! [`TyConDecl`]s, and interns them in the shared [`TyConArena`].  Also seeds
//! the inference context environment with constructor schemes for union variants
//! and the `TyCon` name→id map for named-type resolution.
//!
//! # Two-pass invariant
//!
//! This runs BEFORE `typecheck_module_decls` (T7 / T6 Algorithm W).  After
//! this pass every top-level type name is resolvable via
//! `ctx.tycon_names[name]` (or a side-table passed to `ast_type_to_type`).
//!
//! # Alias resolution
//!
//! Aliases are interned as `TyConKind::Alias(body)` where `body` is the
//! eagerly-resolved RHS.  Because we may see aliases before the types they
//! reference (source order), we do a second pass over aliases to fill in any
//! `Type::Var` placeholders that were created for forward-referenced named types.
//! For 0.1.0 (no cross-module type aliases), a single pass suffices because
//! mutual alias cycles are prohibited by the grammar.

use ridge_ast::{ActorDecl, ActorMember, Constructor, Item, Module, TypeBody, TypeDecl};
use ridge_types::{
    ActorSchema, BuiltinTyCons, CapabilitySet, HandlerSchema, RecordField, RecordSchema, Scheme,
    TyConArena, TyConDecl, TyConId, TyConKind, TyVid, Type, UnionSchema, UnionVariant,
    VariantPayload,
};
use rustc_hash::FxHashMap;

use crate::caps_check::caps_from_ast_slice;
use crate::ctx::InferCtx;

// ── Public API ────────────────────────────────────────────────────────────────

/// Result of collecting user `TyCons` from a module.
pub struct TyConCollectResult {
    /// Names of user-defined `TyCons`, mapping to their arena IDs.
    /// Used by `ast_type_to_type` to resolve named types.
    pub user_tycon_names: FxHashMap<String, TyConId>,
}

/// Walk `module`, register every `TypeDecl` and `ActorDecl` in `arena`, and
/// bind constructor schemes and `TyCon` names in `ctx`.
///
/// After this call:
/// - Every user-defined record/union/alias/actor type has a `TyConId` in `arena`.
/// - Union constructors are bound in `ctx.env` as Schemes (for T6 inference).
/// - `result.user_tycon_names` maps each type name to its `TyConId`.
///
/// # Pass order
///
/// 1. First pass: register all `TyCon` names with `TyConKind::Primitive` as a
///    placeholder so that forward references in field types resolve correctly.
/// 2. Second pass: build the real schema for each `TyCon` and replace the
///    placeholder.
pub fn collect_user_tycons(
    module: &Module,
    arena: &mut TyConArena,
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
) -> TyConCollectResult {
    // ── Pass 1: intern placeholders for every user type name ─────────────────
    //
    // Pass 1 reserves a stable `TyConId` for every `TypeDecl` and `ActorDecl`
    // BEFORE pass 2 starts resolving field types.  Without this, a field
    // (or state-field, or handler arg) that mentions a type declared later in
    // the same source file falls through to `Type::Var(fresh)` in
    // `ast_type_to_ridge_type`, leaving the typechecker with a free type
    // variable where a concrete `Type::Con(actor_id, _)` should be — that's
    // what previously surfaced as `T020 send (\`!\`) on non-actor / found
    // type Con(TyConId(11), [Var(TyVid(0))])` for a perfectly idiomatic
    // forward-referencing actor handle.
    let mut name_to_id: FxHashMap<String, TyConId> = FxHashMap::default();
    for item in &module.items {
        match item {
            Item::Type(td) => {
                #[expect(clippy::cast_possible_truncation, reason = "type param count fits u32")]
                let id = arena.intern(TyConDecl {
                    id: TyConId(0), // overwritten by intern
                    name: td.name.text.clone(),
                    arity: td.params.len() as u32,
                    kind: TyConKind::Primitive, // placeholder; replaced in pass 2
                    def_span: Some(td.span),
                });
                name_to_id.insert(td.name.text.clone(), id);
            }
            Item::Actor(ad) => {
                let id = arena.intern(TyConDecl {
                    id: TyConId(0),
                    name: ad.name.text.clone(),
                    arity: 0,
                    kind: TyConKind::Primitive, // placeholder; replaced in pass 2
                    def_span: Some(ad.span),
                });
                name_to_id.insert(ad.name.text.clone(), id);
            }
            _ => {}
        }
    }

    // ── Pass 2: build real schemas and write them back via `replace_kind`. ───
    //
    // Every name is already in `name_to_id`, so forward references resolve to
    // the right `TyConId` (the placeholder kind is fine — the *id* is what
    // `ast_type_to_ridge_type` needs).  Union constructors are bound only on
    // this pass so they observe the final schemas.
    for item in &module.items {
        match item {
            Item::Type(td) => {
                let id = name_to_id[&td.name.text];
                let kind = build_type_kind_fresh(td, b, ctx, &name_to_id, arena);
                arena.replace_kind(id, kind);
                bind_constructor_schemes(td, id, b, ctx, &name_to_id, arena);
            }
            Item::Actor(ad) => {
                let id = name_to_id[&ad.name.text];
                let kind = build_actor_kind_fresh(ad, b, ctx, &name_to_id, arena);
                arena.replace_kind(id, kind);
            }
            _ => {}
        }
    }

    TyConCollectResult {
        user_tycon_names: name_to_id,
    }
}

// ── Schema builders ───────────────────────────────────────────────────────────

/// Build `TyConKind` from a `TypeDecl` (uses the seeded `name_to_id`).
fn build_type_kind_fresh(
    td: &TypeDecl,
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
    names: &FxHashMap<String, TyConId>,
    _arena: &TyConArena,
) -> TyConKind {
    // Build a param→TyVid map for the type's own parameters.
    let param_vids: Vec<TyVid> = td.params.iter().map(|_| ctx.fresh_tyvid()).collect();
    let param_name_map: FxHashMap<&str, TyVid> = td
        .params
        .iter()
        .zip(param_vids.iter())
        .map(|(p, &v)| (p.text.as_str(), v))
        .collect();

    match &td.body {
        TypeBody::Record(rec_body) => {
            let fields: Vec<RecordField> = rec_body
                .fields
                .iter()
                .map(|f| RecordField {
                    name: f.name.text.clone(),
                    ty: ast_type_to_ridge_type(b, ctx, &f.ty, names, &param_name_map),
                })
                .collect();
            TyConKind::Record(RecordSchema::new(param_vids, fields))
        }

        TypeBody::Union(union_body) => {
            let variants: Vec<UnionVariant> = union_body
                .alternatives
                .iter()
                .map(|c| build_variant(c, b, ctx, names, &param_name_map))
                .collect();
            TyConKind::Union(UnionSchema {
                params: param_vids,
                variants,
            })
        }

        TypeBody::Alias(alias_ty) => {
            // Eager alias resolution.
            let body = ast_type_to_ridge_type(b, ctx, alias_ty, names, &param_name_map);
            TyConKind::Alias(body)
        }
    }
}

/// Build `TyConKind::Actor` from an `ActorDecl`.
fn build_actor_kind_fresh(
    ad: &ActorDecl,
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
    names: &FxHashMap<String, TyConId>,
    _arena: &TyConArena,
) -> TyConKind {
    let empty_params: FxHashMap<&str, TyVid> = FxHashMap::default();

    let mut state_fields: Vec<RecordField> = Vec::new();
    let mut init_params: Option<Vec<Type>> = None;
    let mut init_caps = CapabilitySet::PURE;
    let mut handlers: Vec<HandlerSchema> = Vec::new();

    for member in &ad.members {
        match member {
            ActorMember::State(s) => {
                state_fields.push(RecordField {
                    name: s.name.text.clone(),
                    ty: ast_type_to_ridge_type(b, ctx, &s.ty, names, &empty_params),
                });
            }
            ActorMember::Init(init) => {
                let params: Vec<Type> = init
                    .params
                    .iter()
                    .map(|p| match p {
                        ridge_ast::Param::Bare(_) => Type::Var(ctx.fresh_tyvid()),
                        ridge_ast::Param::Annotated { ty, .. } => {
                            ast_type_to_ridge_type(b, ctx, ty, names, &empty_params)
                        }
                    })
                    .collect();
                init_params = Some(params);
                init_caps = caps_from_ast_slice(&init.caps);
            }
            ActorMember::On(handler) => {
                let h_params: Vec<Type> = handler
                    .params
                    .iter()
                    .map(|p| match p {
                        ridge_ast::Param::Bare(_) => Type::Var(ctx.fresh_tyvid()),
                        ridge_ast::Param::Annotated { ty, .. } => {
                            ast_type_to_ridge_type(b, ctx, ty, names, &empty_params)
                        }
                    })
                    .collect();
                let ret_ty = handler.ret.as_ref().map_or_else(
                    || Type::Con(b.unit, vec![]),
                    |t| ast_type_to_ridge_type(b, ctx, t, names, &empty_params),
                );
                let handler_caps = caps_from_ast_slice(&handler.caps);
                handlers.push(HandlerSchema {
                    name: handler.name.text.clone(),
                    params: h_params,
                    ret: ret_ty,
                    caps: handler_caps,
                });
            }
        }
    }

    TyConKind::Actor(ActorSchema {
        state_fields,
        init_params,
        init_caps,
        handlers,
    })
}

/// Build a `UnionVariant` from a `Constructor`.
fn build_variant(
    ctor: &Constructor,
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
    names: &FxHashMap<String, TyConId>,
    param_name_map: &FxHashMap<&str, TyVid>,
) -> UnionVariant {
    match ctor {
        Constructor::Positional { name, args, .. } => {
            let payload_types: Vec<Type> = args
                .iter()
                .map(|a| ast_type_to_ridge_type(b, ctx, a, names, param_name_map))
                .collect();
            let kind = if payload_types.is_empty() {
                VariantPayload::Nullary
            } else {
                VariantPayload::Positional(payload_types)
            };
            UnionVariant {
                name: name.text.clone(),
                kind,
            }
        }
        Constructor::Record { name, body, .. } => {
            let fields: Vec<RecordField> = body
                .fields
                .iter()
                .map(|f| RecordField {
                    name: f.name.text.clone(),
                    ty: ast_type_to_ridge_type(b, ctx, &f.ty, names, param_name_map),
                })
                .collect();
            let rec_schema = RecordSchema::new(vec![], fields);
            UnionVariant {
                name: name.text.clone(),
                kind: VariantPayload::Record(rec_schema),
            }
        }
    }
}

/// Bind constructor schemes in `ctx.env` for a union type declaration.
///
/// For each variant `Ctor args` of the union, binds `Ctor` as:
/// - Nullary: `∀ params. () -> TyCon params`
/// - Positional(types): `∀ params. (t1, t2, …) -> TyCon params`
fn bind_constructor_schemes(
    td: &TypeDecl,
    tycon_id: TyConId,
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
    names: &FxHashMap<String, TyConId>,
    _arena: &TyConArena,
) {
    let TypeBody::Union(union_body) = &td.body else {
        return;
    };

    // The union's result type is `Type::Con(tycon_id, param_vars)`.
    let param_vids: Vec<TyVid> = td.params.iter().map(|_| ctx.fresh_tyvid()).collect();
    let param_name_map: FxHashMap<&str, TyVid> = td
        .params
        .iter()
        .zip(param_vids.iter())
        .map(|(p, &v)| (p.text.as_str(), v))
        .collect();

    let result_ty = Type::Con(tycon_id, param_vids.iter().map(|v| Type::Var(*v)).collect());

    for ctor in &union_body.alternatives {
        let (name, payload_types) = match ctor {
            Constructor::Positional { name, args, .. } => {
                let tys: Vec<Type> = args
                    .iter()
                    .map(|a| ast_type_to_ridge_type(b, ctx, a, names, &param_name_map))
                    .collect();
                (name.text.clone(), tys)
            }
            Constructor::Record { name, .. } => {
                // Record-constructor scheme: no positional payload.
                (name.text.clone(), vec![])
            }
        };

        // Build scheme: ∀ params. (payload...) -> TyCon params
        let fn_ty = ridge_types::Type::Fn {
            params: payload_types,
            ret: Box::new(result_ty.clone()),
            caps: ridge_types::CapRow::Concrete(CapabilitySet::PURE),
        };
        let scheme = Scheme {
            vars: param_vids.clone(),
            cap_vars: vec![],
            ty: fn_ty,
        };
        ctx.env.bind(name, scheme);
    }
}

// ── AST type → ridge_types::Type conversion ───────────────────────────────────

/// Convert an `ridge_ast::Type` annotation to a `ridge_types::Type`, using
/// `names` for user-defined type resolution and `param_name_map` for the
/// enclosing type's own parameters.
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashMap is the canonical hasher for this crate"
)]
#[allow(clippy::too_many_lines)]
pub fn ast_type_to_ridge_type(
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
    ast_ty: &ridge_ast::Type,
    names: &FxHashMap<String, TyConId>,
    param_name_map: &FxHashMap<&str, TyVid>,
) -> Type {
    /// If the user-defined `TyConId` resolves to a `TyConKind::Alias`,
    /// return a clone of its body for wrapping as `Type::Alias`.  Returns
    /// `None` for records, unions, actors, primitives, or builtins — those
    /// stay as opaque `Type::Con(id, args)`.
    fn alias_body_for(ctx: &InferCtx, id: TyConId) -> Option<Type> {
        let idx = id.0 as usize;
        let decl = ctx.tycon_decls.get(idx)?;
        match &decl.kind {
            TyConKind::Alias(body) => Some(body.clone()),
            _ => None,
        }
    }

    use ridge_ast::PrimitiveType;

    match ast_ty {
        ridge_ast::Type::Primitive { name, .. } => {
            let tycon = match name {
                PrimitiveType::Int => b.int,
                PrimitiveType::Float => b.float,
                PrimitiveType::Bool => b.bool,
                PrimitiveType::Text => b.text,
                PrimitiveType::Unit => b.unit,
                PrimitiveType::Timestamp => b.timestamp,
            };
            Type::Con(tycon, vec![])
        }

        ridge_ast::Type::Named { name, .. } => {
            let n = name.text.as_str();
            // Check type parameter (e.g. `a` in `Option a`).
            if let Some(&vid) = param_name_map.get(n) {
                return Type::Var(vid);
            }
            // Check prelude (Option, Result, Int, etc.).
            if let Some(id) = crate::prelude::lookup_prelude_tycon(b, n) {
                return Type::Con(id, vec![]);
            }
            // Check user-defined types.
            if let Some(&id) = names.get(n) {
                // Non-parametric alias (e.g. `type Bag = Map Text Text`):
                // wrap as `Type::Alias { name, body }` so `shallow_resolve`
                // peels through to the RHS and `Bag` unifies with the
                // alias body.  Otherwise the alias would intern as its own
                // opaque `Type::Con(alias_id, …)` and never unify with the
                // body, breaking the user-facing "alias means equal" model.
                if let Some(body) = alias_body_for(ctx, id) {
                    return Type::Alias {
                        name: id,
                        body: Box::new(body),
                    };
                }
                return Type::Con(id, vec![]);
            }
            // Unknown — allocate fresh var as fallback.
            Type::Var(ctx.fresh_tyvid())
        }

        ridge_ast::Type::App { head, args, .. } => {
            let n = head.text.as_str();
            let arg_tys: Vec<Type> = args
                .iter()
                .map(|a| ast_type_to_ridge_type(b, ctx, a, names, param_name_map))
                .collect();
            // Check prelude.
            if let Some(id) = crate::prelude::lookup_prelude_tycon(b, n) {
                return Type::Con(id, arg_tys);
            }
            // Check user-defined.
            if let Some(&id) = names.get(n) {
                // Non-parametric alias used in an application position with
                // arity 0 (`Bag`) — the parser still routes the bare form
                // through `Named`, but we mirror the wrap-as-Alias rule here
                // for symmetry.  Parametric aliases (`type Stack a = List
                // a`) are not yet supported: `TyConKind::Alias` does not
                // carry the alias's own type-parameter vids, so substitution
                // cannot run.  They still fall through to `Type::Con` and
                // continue to fail unification with their body, matching the
                // pre-fix behaviour until alias-params land.
                if arg_tys.is_empty() {
                    if let Some(body) = alias_body_for(ctx, id) {
                        return Type::Alias {
                            name: id,
                            body: Box::new(body),
                        };
                    }
                }
                return Type::Con(id, arg_tys);
            }
            Type::Var(ctx.fresh_tyvid())
        }

        ridge_ast::Type::Tuple { elems, .. } => {
            let ts: Vec<Type> = elems
                .iter()
                .map(|e| ast_type_to_ridge_type(b, ctx, e, names, param_name_map))
                .collect();
            Type::Tuple(ts)
        }

        ridge_ast::Type::List { elem, .. } => {
            let elem_ty = ast_type_to_ridge_type(b, ctx, elem, names, param_name_map);
            Type::Con(b.list, vec![elem_ty])
        }

        ridge_ast::Type::Fn { fn_ty, .. } => {
            let param_tys: Vec<Type> = fn_ty
                .params
                .iter()
                .map(|p| ast_type_to_ridge_type(b, ctx, p, names, param_name_map))
                .collect();
            let ret_ty = ast_type_to_ridge_type(b, ctx, &fn_ty.ret, names, param_name_map);
            let cap_row = if fn_ty.caps.is_empty() {
                ridge_types::CapRow::Concrete(CapabilitySet::PURE)
            } else {
                let mut cs = CapabilitySet::PURE;
                for cap in &fn_ty.caps {
                    cs = cs.union(&CapabilitySet::singleton(*cap));
                }
                ridge_types::CapRow::Concrete(cs)
            };
            Type::Fn {
                params: param_tys,
                ret: Box::new(ret_ty),
                caps: cap_row,
            }
        }

        ridge_ast::Type::Paren { inner, .. } => {
            ast_type_to_ridge_type(b, ctx, inner, names, param_name_map)
        }

        ridge_ast::Type::Var { name, .. } => {
            let n = name.text.as_str();
            if let Some(&vid) = param_name_map.get(n) {
                Type::Var(vid)
            } else {
                // Unknown type var in annotation — allocate fresh.
                Type::Var(ctx.fresh_tyvid())
            }
        }
    }
}
