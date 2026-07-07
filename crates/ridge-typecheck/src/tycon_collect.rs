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
//! Aliases are interned as `TyConKind::Alias { params, body }` where
//! `params` are fresh `TyVid`s standing in for the alias's declared
//! parameters and `body` is the eagerly-resolved RHS, with each
//! `Type::Var(p)` referring back to a `p` in `params`.  At use sites,
//! `ast_type_to_ridge_type` substitutes the params with the supplied
//! argument types before wrapping in `Type::Alias { name, body }`.  A
//! dedicated chain pass (`resolve_alias_chains`) walks every alias body
//! after pass 2 and expands any embedded reference to another alias so
//! `type IntStack = Stack Int` lands directly on `List Int`.

use ridge_ast::visit::{walk_module, Visit};
use ridge_ast::{ActorDecl, ActorMember, Constructor, Item, Module, TypeBody, TypeDecl};
use ridge_types::{
    shape_key, ActorSchema, AnonRecordTable, BuiltinTyCons, CapabilitySet, HandlerSchema,
    RecordField, RecordSchema, Scheme, TyConArena, TyConDecl, TyConId, TyConKind, TyVid, Type,
    UnionSchema, UnionVariant, VariantPayload,
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
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashMap is the canonical hasher for this crate; matches synth_table_mirrors and the rest of the typecheck API"
)]
pub fn collect_user_tycons(
    module: &Module,
    module_id: ridge_resolve::ModuleId,
    arena: &mut TyConArena,
    b: &BuiltinTyCons,
    imported_tycons: &FxHashMap<String, TyConId>,
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
                    def_module_raw: Some(module_id.0),
                    opaque: td.opaque,
                    is_anon: false,
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
                    def_module_raw: Some(module_id.0),
                    opaque: false, // actors cannot be opaque
                    is_anon: false,
                });
                name_to_id.insert(ad.name.text.clone(), id);
            }
            _ => {}
        }
    }

    // Resolution view for pass 2: the local names plus this module's imported
    // type names. A union variant or record field that references an imported
    // type — `DefaultLit SqlValue`, say — must resolve to the producer's
    // `TyConId` while constructor schemes are built here, not fall through to a
    // fresh variable that only later inference can pin. A constructor that is
    // declared but never applied in its own module (so no call site unifies that
    // variable) would otherwise generalise with an unsolved variable. A local
    // declaration of the same name still wins. The returned `user_tycon_names`
    // stays local-only; imports are merged into `ctx.user_tycon_names` by the
    // caller after this pass.
    let resolve_names = if imported_tycons.is_empty() {
        name_to_id.clone()
    } else {
        let mut merged = name_to_id.clone();
        for (name, &tid) in imported_tycons {
            merged.entry(name.clone()).or_insert(tid);
        }
        merged
    };

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
                let kind = build_type_kind_fresh(td, b, ctx, &resolve_names, arena);
                arena.replace_kind(id, kind);
                bind_constructor_schemes(td, id, b, ctx, &resolve_names, arena);
            }
            Item::Actor(ad) => {
                let id = name_to_id[&ad.name.text];
                let kind = build_actor_kind_fresh(ad, b, ctx, &resolve_names, arena);
                arena.replace_kind(id, kind);
            }
            _ => {}
        }
    }

    // ── Pass 3: resolve multi-step alias chains. ─────────────────────────────
    //
    // Pass 2 reads `ctx.tycon_decls` to decide whether a `Named("Foo")`
    // reference inside an alias body should wrap as `Type::Alias`.  But
    // `ctx.tycon_decls` does not get synced from the arena until after the
    // outer driver in `lib.rs` runs `arena.all().to_vec()`, so within pass 2
    // every later alias sees its earlier siblings as their *placeholder*
    // kind, not their real kind.  The result is that
    // `type IntList = List Int; type Numbers = IntList` leaves Numbers'
    // body as `Type::Con(IntList, [])` — a dead end that never unifies with
    // `List Int` because `shallow_resolve` peels `Type::Alias` but not
    // `Type::Con(alias_id, _)`.
    //
    // Pass 3 walks every alias body in arena order and substitutes any
    // embedded `Type::Con(alias_id, _)` with the alias's resolved body,
    // following the chain to the terminal non-alias type.  A `visited` set
    // breaks any cycle defensively (the grammar already forbids them, but a
    // typo or future relaxation should not melt the typechecker).
    resolve_alias_chains(arena);

    TyConCollectResult {
        user_tycon_names: name_to_id,
    }
}

// ── Column codegen: `deriving (Table)` mirrors ────────────────────────────────

/// Synthesize the column mirror for every record in `module` that derives
/// `Table`.
///
/// For an entity `User`, this interns a `UserCols` record type — one
/// `Column User T` field per entity field `f: T` — and registers two value
/// schemes so user code referencing them type-checks:
///
/// - `userCols  : UserCols`     — the column mirror
/// - `userTable : Table User`   — the table metadata
///
/// Name resolution has already reserved the three names (see
/// [`ridge_ast::column_mirror`]); this fills in their types. Lowering emits the
/// values. `Column`/`Table` are compiler builtins ([`BuiltinTyCons`]), so user
/// code never imports or names them.
///
/// Idempotent: on an incremental re-check the arena already holds the mirror
/// (rebuilt from the prior pass), so its schema is replaced in place rather than
/// duplicated.
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashMap is the canonical hasher for this crate; matches collect_workspace and the rest of the typecheck API"
)]
pub fn synth_table_mirrors(
    module: &Module,
    module_id: ridge_resolve::ModuleId,
    arena: &mut TyConArena,
    b: &BuiltinTyCons,
    global_tycon_names: &FxHashMap<String, TyConId>,
    ctx: &mut InferCtx,
) {
    use ridge_ast::column_mirror as cm;

    let column_id = b.column;
    let table_id = b.table;

    let mono = |ty: Type| Scheme {
        vars: vec![],
        cap_vars: vec![],
        row_vars: vec![],
        ty,
        constraints: vec![],
    };

    for item in &module.items {
        let Item::Type(td) = item else { continue };
        if !cm::has_table_derive(&td.deriving) {
            continue;
        }
        let TypeBody::Record(_) = &td.body else {
            continue;
        };
        let entity = td.name.text.as_str();

        let Some(&entity_id) = ctx.user_tycon_names.get(entity) else {
            continue;
        };
        let entity_ty = Type::Con(entity_id, vec![]);

        // Mirror each entity field `f: T` as `f: Column Entity T`. The schema was
        // built by pass 2 of `collect_user_tycons`, so it is available here.
        let mirror_fields: Vec<RecordField> = {
            let TyConKind::Record(schema) = &arena.get(entity_id).kind else {
                continue;
            };
            schema
                .record_fields()
                .iter()
                .map(|f| RecordField {
                    name: f.name.clone(),
                    ty: Type::Con(column_id, vec![entity_ty.clone(), f.ty.clone()]),
                })
                .collect()
        };

        let mirror_name = cm::mirror_type_name(entity);
        let mirror_kind = TyConKind::Record(RecordSchema::new(vec![], mirror_fields));
        let mirror_id = if let Some(&existing) = global_tycon_names.get(&mirror_name) {
            // Incremental re-check: the mirror is already in the rebuilt arena.
            arena.replace_kind(existing, mirror_kind);
            existing
        } else {
            arena.intern(TyConDecl {
                id: TyConId(0), // overwritten by intern
                name: mirror_name.clone(),
                arity: 0,
                kind: mirror_kind,
                def_span: Some(td.span),
                def_module_raw: Some(module_id.0),
                opaque: false,
                is_anon: false,
            })
        };
        ctx.user_tycon_names.insert(mirror_name, mirror_id);

        let cols_value = cm::mirror_value_name(entity);
        let cols_scheme = mono(Type::Con(mirror_id, vec![]));
        ctx.name_schemes_accum
            .insert(cols_value.clone(), cols_scheme.clone());
        ctx.env.bind(cols_value, cols_scheme);

        let table_value = cm::table_value_name(entity);
        let table_scheme = mono(Type::Con(table_id, vec![entity_ty]));
        ctx.name_schemes_accum
            .insert(table_value.clone(), table_scheme.clone());
        ctx.env.bind(table_value, table_scheme);
    }
}

// ── Insert companions: `deriving (Schema)` insert shapes ───────────────────────

/// Synthesize the insert companion for every `deriving (Schema)` record whose
/// schema marks database-generated columns.
///
/// Records the per-entity targets the `InsertShape e` projection reduces against.
/// For an entity `User { id: Int, email: Text }` whose `id` is a conventional
/// serial key, this interns a `UserInsert { email: Text }` record — the entity
/// minus its generated columns — so a typed `insert` accepts a value that simply
/// has no field for the database-filled `id`, and writing one by hand is a type
/// error. The entity then maps to that companion in [`InferCtx::insert_shapes`]
/// (and the companion back to the entity in [`InferCtx::insert_shape_entities`]),
/// which is what the `InsertShape` reduction reads. An entity with no generated
/// column maps to itself, so the projection is the identity and an insert of that
/// entity is unchanged.
///
/// Name resolution has already reserved the companion's type and auto-constructor
/// names (see [`ridge_ast::column_mirror`] and the resolver's `deriving (Schema)`
/// branch); this fills in the record schema. The companion's `Row` instance — the
/// dual encode the insert body runs — is synthesized separately, alongside the
/// verbs that consume it.
///
/// Idempotent: on an incremental re-check the companion already lives in the
/// rebuilt arena, so its schema is replaced in place rather than duplicated.
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashMap is the canonical hasher for this crate; matches synth_table_mirrors and the rest of the typecheck API"
)]
pub fn synth_insert_shapes(
    module: &Module,
    module_id: ridge_resolve::ModuleId,
    arena: &mut TyConArena,
    global_tycon_names: &FxHashMap<String, TyConId>,
    ctx: &mut InferCtx,
) {
    use ridge_ast::column_mirror as cm;

    for item in &module.items {
        let Item::Type(td) = item else { continue };
        if !cm::has_schema_derive(&td.deriving) {
            continue;
        }
        let TypeBody::Record(rec) = &td.body else {
            continue;
        };
        let entity = td.name.text.as_str();
        let Some(&entity_id) = ctx.user_tycon_names.get(entity) else {
            continue;
        };

        // The generated columns the convention drops from the companion. Decided
        // from the AST field (name + declared type), the single source of truth
        // the schema derive also reads.
        let generated: Vec<&str> = rec
            .fields
            .iter()
            .filter(|f| cm::is_generated_field(&f.name.text, &f.ty))
            .map(|f| f.name.text.as_str())
            .collect();

        if generated.is_empty() {
            // No generated column → no companion. The reduction treats any entity
            // without a table entry as its own insert shape, so nothing to record.
            continue;
        }

        // Companion fields = the entity's record fields minus the generated ones,
        // carried with the RESOLVED field types from the entity's arena schema
        // (built in pass 2 of `collect_user_tycons`).
        let companion_fields: Vec<RecordField> = {
            let TyConKind::Record(schema) = &arena.get(entity_id).kind else {
                continue;
            };
            schema
                .record_fields()
                .iter()
                .filter(|f| !generated.iter().any(|g| *g == f.name))
                .cloned()
                .collect()
        };

        let companion_name = cm::insert_companion_type_name(entity);
        let companion_kind = TyConKind::Record(RecordSchema::new(vec![], companion_fields));
        let companion_id = if let Some(&existing) = global_tycon_names.get(&companion_name) {
            // Incremental re-check: the companion is already in the rebuilt arena.
            arena.replace_kind(existing, companion_kind);
            existing
        } else {
            arena.intern(TyConDecl {
                id: TyConId(0), // overwritten by intern
                name: companion_name.clone(),
                arity: 0,
                kind: companion_kind,
                def_span: Some(td.span),
                def_module_raw: Some(module_id.0),
                opaque: false,
                is_anon: false,
            })
        };
        ctx.user_tycon_names.insert(companion_name, companion_id);
        ctx.insert_shapes.insert(entity_id, companion_id);
        ctx.insert_shape_entities.insert(companion_id, entity_id);
    }
}

// ── Pre-scan: anonymous record interning ──────────────────────────────────────

/// Walk all AST `Type::Record` nodes in every module, intern a unique
/// anonymous `TyCon` per structural shape, and return the complete
/// `AnonRecordTable` (shape → `TyConId`).
///
/// Must be called AFTER pass-1 (all named `TyCon` ids are stable) and after
/// `resolve_alias_chains` (alias bodies are terminal).  Must be called BEFORE
/// `ast_type_to_ridge_type` is invoked on any `Type::Record` node.
///
/// Uses the same `b` / `names` / `ctx` context as `ast_type_to_ridge_type` so
/// that primitive and user-defined types resolve consistently.
pub fn prescan_inline_records(
    modules: &[&Module],
    arena: &mut TyConArena,
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
) -> AnonRecordTable {
    let mut table: AnonRecordTable = AnonRecordTable::default();
    let mut counter: usize = 0;
    // Collect name→id from tycon_decls (already populated in ctx).
    let names: FxHashMap<String, TyConId> = ctx
        .tycon_decls
        .iter()
        .map(|d| (d.name.clone(), d.id))
        .collect();

    for module in modules {
        let mut collector = InlineRecordCollector {
            arena,
            b,
            ctx,
            names: &names,
            table: &mut table,
            counter: &mut counter,
        };
        walk_module(&mut collector, module);
    }
    table
}

/// Visitor that walks every `Type::Record` node bottom-up and interns it.
struct InlineRecordCollector<'a> {
    arena: &'a mut TyConArena,
    b: &'a BuiltinTyCons,
    ctx: &'a mut InferCtx,
    names: &'a FxHashMap<String, TyConId>,
    table: &'a mut AnonRecordTable,
    counter: &'a mut usize,
}

impl<'ast> Visit<'ast> for InlineRecordCollector<'_> {
    fn visit_type(&mut self, t: &'ast ridge_ast::Type) {
        // Recurse FIRST (bottom-up: inner fields before outer).
        ridge_ast::visit::walk_type(self, t);

        if let ridge_ast::Type::Record { fields, span, .. } = t {
            intern_inline_record(
                self.arena,
                self.b,
                self.ctx,
                self.names,
                self.table,
                self.counter,
                fields,
                *span,
            );
        }
    }
}

/// Resolve the AST fields of a `Type::Record` to `ridge_types::Type` values,
/// compute the `ShapeKey`, and intern a new anonymous `TyCon` if not already
/// present.  Idempotent: the same shape always produces the same `TyConId`.
#[expect(
    clippy::too_many_arguments,
    reason = "flat helper called from visitor; threading all context is unavoidable without a struct"
)]
fn intern_inline_record(
    arena: &mut TyConArena,
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
    names: &FxHashMap<String, TyConId>,
    table: &mut AnonRecordTable,
    counter: &mut usize,
    fields: &[ridge_ast::RecordTypeField],
    span: ridge_ast::Span,
) {
    // Resolve each field's AST type using the same machinery as
    // ast_type_to_ridge_type.  Nested inline records are already interned by
    // the bottom-up visit, so they resolve as Type::Con(inner_anon_id, []).
    let resolved_fields: Vec<(String, Type)> = fields
        .iter()
        .map(|f| {
            let ty = resolve_field_type_for_prescan(b, ctx, &f.ty, names, table);
            (f.name.text.clone(), ty)
        })
        .collect();

    // Build the canonical shape key.
    let key = shape_key(&resolved_fields);

    // Intern on MISS.
    table.entry(key).or_insert_with(|| {
        // Build canonical (sorted-by-name) field list for the schema.
        let mut canonical_fields: Vec<RecordField> = resolved_fields
            .into_iter()
            .map(|(name, ty)| RecordField { name, ty })
            .collect();
        canonical_fields.sort_by(|a, b| a.name.cmp(&b.name));

        let anon_name = format!("{{anon record #{}}}", *counter);
        *counter += 1;

        let decl = TyConDecl {
            id: TyConId(0), // overwritten by arena.intern
            name: anon_name,
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(vec![], canonical_fields)),
            def_span: Some(span),
            def_module_raw: None, // no single owning module for workspace-wide anons
            opaque: false,
            is_anon: true,
        };
        arena.intern(decl)
    });
}

/// Resolve a single field's AST type to a `ridge_types::Type` during the
/// pre-scan.  This mirrors `ast_type_to_ridge_type` but also handles
/// `Type::Record` via the in-progress `table` (since nested inline records
/// were already interned in the bottom-up walk).
fn resolve_field_type_for_prescan(
    b: &BuiltinTyCons,
    ctx: &mut InferCtx,
    ast_ty: &ridge_ast::Type,
    names: &FxHashMap<String, TyConId>,
    table: &AnonRecordTable,
) -> Type {
    match ast_ty {
        ridge_ast::Type::Primitive { name, .. } => {
            use ridge_ast::PrimitiveType;
            let tycon = match name {
                PrimitiveType::Int => b.int,
                PrimitiveType::Float => b.float,
                PrimitiveType::Bool => b.bool,
                PrimitiveType::Text => b.text,
                PrimitiveType::Unit => b.unit,
                PrimitiveType::Timestamp => b.timestamp,
                PrimitiveType::Decimal => b.decimal,
            };
            Type::Con(tycon, vec![])
        }
        ridge_ast::Type::Named { name, .. } => {
            let n = name.text.as_str();
            if let Some(id) = crate::prelude::lookup_prelude_tycon(b, n) {
                return Type::Con(id, vec![]);
            }
            if let Some(&id) = names.get(n) {
                return Type::Con(id, vec![]);
            }
            Type::Var(ctx.fresh_tyvid())
        }
        ridge_ast::Type::App { head, args, .. } => {
            let n = head.text.as_str();
            let arg_tys: Vec<Type> = args
                .iter()
                .map(|a| resolve_field_type_for_prescan(b, ctx, a, names, table))
                .collect();
            if let Some(id) = crate::prelude::lookup_prelude_tycon(b, n) {
                return Type::Con(id, arg_tys);
            }
            if let Some(&id) = names.get(n) {
                return Type::Con(id, arg_tys);
            }
            Type::Var(ctx.fresh_tyvid())
        }
        ridge_ast::Type::Tuple { elems, .. } => {
            let ts: Vec<Type> = elems
                .iter()
                .map(|e| resolve_field_type_for_prescan(b, ctx, e, names, table))
                .collect();
            Type::Tuple(ts)
        }
        ridge_ast::Type::List { elem, .. } => {
            let elem_ty = resolve_field_type_for_prescan(b, ctx, elem, names, table);
            Type::Con(b.list, vec![elem_ty])
        }
        ridge_ast::Type::Fn { fn_ty, .. } => {
            let param_tys: Vec<Type> = fn_ty
                .params
                .iter()
                .map(|p| resolve_field_type_for_prescan(b, ctx, p, names, table))
                .collect();
            let ret_ty = resolve_field_type_for_prescan(b, ctx, &fn_ty.ret, names, table);
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
            resolve_field_type_for_prescan(b, ctx, inner, names, table)
        }
        ridge_ast::Type::Var { .. } => {
            // Type variables inside inline record fields are a tyvar-in-field
            // rejection case (P022 / T5).  Return a fresh var as a placeholder;
            // T5 will emit the diagnostic.
            Type::Var(ctx.fresh_tyvid())
        }
        ridge_ast::Type::Record { fields, .. } => {
            // Nested inline record: look up in the table (already interned by
            // the bottom-up visitor before we reach this field).
            let resolved: Vec<(String, Type)> = fields
                .iter()
                .map(|f| {
                    let ty = resolve_field_type_for_prescan(b, ctx, &f.ty, names, table);
                    (f.name.text.clone(), ty)
                })
                .collect();
            let key = shape_key(&resolved);
            if let Some(&id) = table.get(&key) {
                Type::Con(id, vec![])
            } else {
                // Should not happen in a correct bottom-up walk, but handle
                // defensively.
                log_prescan_miss();
                Type::Error
            }
        }
    }
}

/// Emit a diagnostic-level note when a nested inline record was not found in
/// the pre-scan table (indicates a walk-order bug).
const fn log_prescan_miss() {
    // In production, this path should never be hit.  We avoid panicking so
    // inference can continue and produce more useful diagnostics.
    // A future observability pass can wire a tracing::warn! here.
}

/// Walk every `TyConKind::Alias` body in `arena` and expand any
/// `Type::Con(alias_id, args)` embedded inside it to the alias's terminal
/// body — substituting the inner alias's parameters with `args` when the
/// arities line up.  The expanded body keeps the wrapping
/// `TyConKind::Alias`, so use-sites still get a `Type::Alias { name, body }`
/// view at the outer wrap done by `ast_type_to_ridge_type`.
fn resolve_alias_chains(arena: &mut TyConArena) {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "arena len fits u32 in practice"
    )]
    let alias_ids: Vec<TyConId> = (0..arena.len())
        .map(|i| TyConId(i as u32))
        .filter(|&id| matches!(arena.get(id).kind, TyConKind::Alias { .. }))
        .collect();

    for id in alias_ids {
        let (original_params, original_body) = match &arena.get(id).kind {
            TyConKind::Alias { params, body } => (params.clone(), body.clone()),
            _ => continue,
        };
        let mut visited: Vec<TyConId> = vec![id];
        let resolved = chase_alias_chain(arena, &original_body, &mut visited);
        arena.replace_kind(
            id,
            TyConKind::Alias {
                params: original_params,
                body: resolved,
            },
        );
    }
}

/// Recursively expand any `Type::Con(alias_id, args)` reference inside
/// `ty` to the alias's resolved body, chasing through chained aliases.
/// For parametric aliases the inner alias's parameters are substituted
/// with the call-site `args` before recursing.
///
/// `visited` is a stack of alias ids currently being expanded; if an
/// alias references itself transitively the chain is left as `Type::Con`
/// rather than recursing forever.
fn chase_alias_chain(arena: &TyConArena, ty: &Type, visited: &mut Vec<TyConId>) -> Type {
    match ty {
        Type::Con(id, args) => {
            // Recurse into args first so they are themselves chained.
            let new_args: Vec<Type> = args
                .iter()
                .map(|a| chase_alias_chain(arena, a, visited))
                .collect();
            if !visited.contains(id) {
                if let TyConKind::Alias {
                    params: inner_params,
                    body: inner_body,
                } = &arena.get(*id).kind
                {
                    if new_args.len() == inner_params.len() {
                        let subst: FxHashMap<TyVid, Type> = inner_params
                            .iter()
                            .zip(new_args.iter())
                            .map(|(&p, a)| (p, a.clone()))
                            .collect();
                        let substituted = substitute_tyvars(inner_body, &subst);
                        visited.push(*id);
                        let resolved = chase_alias_chain(arena, &substituted, visited);
                        visited.pop();
                        return resolved;
                    }
                }
            }
            Type::Con(*id, new_args)
        }
        Type::Alias { name, body } => Type::Alias {
            name: *name,
            body: Box::new(chase_alias_chain(arena, body, visited)),
        },
        Type::Fn { params, ret, caps } => {
            let new_params: Vec<Type> = params
                .iter()
                .map(|p| chase_alias_chain(arena, p, visited))
                .collect();
            let new_ret = Box::new(chase_alias_chain(arena, ret, visited));
            Type::Fn {
                params: new_params,
                ret: new_ret,
                caps: caps.clone(),
            }
        }
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| chase_alias_chain(arena, e, visited))
                .collect(),
        ),
        _ => ty.clone(),
    }
}

/// Substitute every `Type::Var(v)` for which `subst` has a mapping with
/// the corresponding type.  Free vars (not in `subst`) are preserved.
/// Used for parametric-alias expansion: the alias body holds its own
/// parameters as `Type::Var(p_i)` placeholders, and use-sites supply the
/// concrete argument types via `subst = { p_i -> arg_i }`.
fn substitute_tyvars(ty: &Type, subst: &FxHashMap<TyVid, Type>) -> Type {
    match ty {
        Type::Var(v) => subst.get(v).cloned().unwrap_or_else(|| ty.clone()),
        Type::Con(id, args) => Type::Con(
            *id,
            args.iter().map(|a| substitute_tyvars(a, subst)).collect(),
        ),
        Type::Fn { params, ret, caps } => Type::Fn {
            params: params.iter().map(|p| substitute_tyvars(p, subst)).collect(),
            ret: Box::new(substitute_tyvars(ret, subst)),
            caps: caps.clone(),
        },
        Type::Tuple(elems) => {
            Type::Tuple(elems.iter().map(|e| substitute_tyvars(e, subst)).collect())
        }
        Type::Alias { name, body } => Type::Alias {
            name: *name,
            body: Box::new(substitute_tyvars(body, subst)),
        },
        _ => ty.clone(),
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
            // Eager alias resolution.  `param_vids` are baked into the body
            // as `Type::Var(p)` placeholders; use sites substitute them with
            // the supplied argument types before wrapping in `Type::Alias`.
            let body = ast_type_to_ridge_type(b, ctx, alias_ty, names, &param_name_map);
            TyConKind::Alias {
                params: param_vids,
                body,
            }
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
                        ridge_ast::Param::Annotated { ty, .. }
                        | ridge_ast::Param::PatternAnnotated { ty, .. } => {
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
                        ridge_ast::Param::Annotated { ty, .. }
                        | ridge_ast::Param::PatternAnnotated { ty, .. } => {
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
            ActorMember::Mailbox(_) => {
                // Mailbox config contributes no type variables or schema info.
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
            row_vars: vec![],
            ty: fn_ty,
            constraints: vec![],
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
    /// return clones of its parameters and body for substitution + wrapping
    /// as `Type::Alias`.  Returns `None` for records, unions, actors,
    /// primitives, or builtins — those stay as opaque `Type::Con(id, args)`.
    fn alias_params_body(ctx: &InferCtx, id: TyConId) -> Option<(Vec<TyVid>, Type)> {
        let idx = id.0 as usize;
        let decl = ctx.tycon_decls.get(idx)?;
        match &decl.kind {
            TyConKind::Alias { params, body } => Some((params.clone(), body.clone())),
            _ => None,
        }
    }

    /// Look up a type-constructor `TyConId` by name from the full arena. A
    /// fallback for names that are not user-defined (absent from `names`) but are
    /// nonetheless registered constructors — notably reconciled stdlib types like
    /// `Repo`, `Query`, `MemAdapter`, and `Postgres`, which are imported into a
    /// user module's arena yet never enter its user-tycon-name map. Without this,
    /// an annotation such as `Repo User MemAdapter` resolves the head to a fresh
    /// variable and silently drops its type arguments — including a phantom entity
    /// the annotation was meant to pin, leaving it free to over-generalise.
    fn arena_tycon_by_name(ctx: &InferCtx, n: &str) -> Option<TyConId> {
        ctx.tycon_decls
            .iter()
            .position(|d| d.name == n)
            .map(|i| TyConId(u32::try_from(i).unwrap_or(u32::MAX)))
    }

    /// Wrap an alias use as `Type::Alias { name, body }`, substituting the
    /// alias's own parameters with `arg_tys` when supplied.  Caller is
    /// responsible for arity matching; this helper only runs the
    /// substitution path.
    fn wrap_alias(id: TyConId, params: &[TyVid], body: &Type, arg_tys: &[Type]) -> Type {
        if params.is_empty() {
            return Type::Alias {
                name: id,
                body: Box::new(body.clone()),
            };
        }
        let subst: FxHashMap<TyVid, Type> = params
            .iter()
            .zip(arg_tys.iter())
            .map(|(&p, a)| (p, a.clone()))
            .collect();
        let substituted = substitute_tyvars(body, &subst);
        Type::Alias {
            name: id,
            body: Box::new(substituted),
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
                PrimitiveType::Decimal => b.decimal,
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
                // alias body.  Parametric aliases referenced bare (no
                // arguments) are a partial application and fall through to
                // `Type::Con` — the kind error is caught elsewhere.
                if let Some((params, body)) = alias_params_body(ctx, id) {
                    if params.is_empty() {
                        return wrap_alias(id, &params, &body, &[]);
                    }
                }
                return Type::Con(id, vec![]);
            }
            // Fallback: a reconciled stdlib type (e.g. `MemAdapter`, `Postgres`)
            // in the arena but not the user-tycon-name map.
            if let Some(id) = arena_tycon_by_name(ctx, n) {
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
                // Alias used at an application site (`Bag`, `Stack Int`):
                // substitute the alias's own parameters with `arg_tys` and
                // wrap as `Type::Alias` so `shallow_resolve` peels through
                // to the body.  Arity mismatches fall through to a bare
                // `Type::Con` so the kind-error path keeps surfacing.
                if let Some((params, body)) = alias_params_body(ctx, id) {
                    if params.len() == arg_tys.len() {
                        return wrap_alias(id, &params, &body, &arg_tys);
                    }
                }
                return Type::Con(id, arg_tys);
            }
            // Fallback: a reconciled stdlib type applied to args (e.g. the
            // `Repo User MemAdapter` / `Query e a` annotations), present in the
            // arena but absent from the user-tycon-name map. Resolving the head
            // here keeps the arguments — without it the whole application would
            // collapse to a fresh variable and drop them.
            if let Some(id) = arena_tycon_by_name(ctx, n) {
                if let Some((params, body)) = alias_params_body(ctx, id) {
                    if params.len() == arg_tys.len() {
                        return wrap_alias(id, &params, &body, &arg_tys);
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

        // Inline record type → a structural `Type::Record`. A `| r` tail makes
        // the row open over a fresh row variable; a closed record has none.
        ridge_ast::Type::Record { fields, tail, .. } => {
            let resolved: Vec<(String, Type)> = fields
                .iter()
                .map(|f| {
                    let ty = ast_type_to_ridge_type(b, ctx, &f.ty, names, param_name_map);
                    (f.name.text.clone(), ty)
                })
                .collect();
            let row_tail = if tail.is_some() {
                ridge_types::RowTail::Open(ctx.fresh_rowvid())
            } else {
                ridge_types::RowTail::Closed
            };
            Type::record(resolved, row_tail)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{
        Body, FnDecl, Ident, Item, Param, RecordTypeField, Span, Type as AstType, Visibility,
    };
    use ridge_types::BuiltinTyCons;

    fn ds() -> Span {
        Span::point(0)
    }

    fn field(name: &str, ty: AstType) -> RecordTypeField {
        RecordTypeField {
            name: Ident::new(name, ds()),
            ty,
            span: ds(),
        }
    }

    fn int_ast() -> AstType {
        AstType::Primitive {
            name: ridge_ast::PrimitiveType::Int,
            span: ds(),
        }
    }

    fn text_ast() -> AstType {
        AstType::Primitive {
            name: ridge_ast::PrimitiveType::Text,
            span: ds(),
        }
    }

    fn make_ctx_with_builtins(arena: &mut TyConArena) -> (BuiltinTyCons, InferCtx) {
        let b = BuiltinTyCons::allocate(arena);
        let mut ctx = InferCtx::new();
        ctx.tycon_decls = arena.all().to_vec();
        (b, ctx)
    }

    // Build a one-item module containing a single fn with the given parameter type.
    fn module_with_fn_param(ty: AstType) -> ridge_ast::Module {
        let f = FnDecl {
            name: Ident::new("f", ds()),
            params: vec![Param::Annotated {
                name: Ident::new("r", ds()),
                ty,
                span: ds(),
            }],
            caps: vec![],
            ret: None,
            doc: None,
            vis: Visibility::Private,
            attrs: vec![],
            constraints: vec![],
            body: Body::Expr(ridge_ast::Expr::Unit(ds())),
            span: ds(),
        };
        ridge_ast::Module {
            items: vec![Item::Fn(f)],
            doc: vec![],
            span: ds(),
        }
    }

    // Single shape interns exactly once.
    #[test]
    fn prescan_single_shape_interns_once() {
        let mut arena = TyConArena::new();
        let (b, mut ctx) = make_ctx_with_builtins(&mut arena);

        let rec_ty = AstType::Record {
            fields: vec![field("x", int_ast()), field("y", int_ast())],
            tail: None,
            span: ds(),
        };
        let module = module_with_fn_param(rec_ty);

        let table = prescan_inline_records(&[&module], &mut arena, &b, &mut ctx);
        assert_eq!(table.len(), 1, "expected exactly one anonymous TyCon");
    }

    // Order-insensitive: two occurrences of the same shape share one entry.
    #[test]
    fn prescan_order_insensitive_sharing() {
        let mut arena = TyConArena::new();
        let (b, mut ctx) = make_ctx_with_builtins(&mut arena);

        // Module with two fn params: { x: Int, y: Int } and { y: Int, x: Int }
        let f1 = FnDecl {
            name: Ident::new("f1", ds()),
            params: vec![Param::Annotated {
                name: Ident::new("r", ds()),
                ty: AstType::Record {
                    fields: vec![field("x", int_ast()), field("y", int_ast())],
                    tail: None,
                    span: ds(),
                },
                span: ds(),
            }],
            caps: vec![],
            ret: None,
            doc: None,
            vis: Visibility::Private,
            attrs: vec![],
            constraints: vec![],
            body: Body::Expr(ridge_ast::Expr::Unit(ds())),
            span: ds(),
        };
        let f2 = FnDecl {
            name: Ident::new("f2", ds()),
            params: vec![Param::Annotated {
                name: Ident::new("r", ds()),
                ty: AstType::Record {
                    fields: vec![field("y", int_ast()), field("x", int_ast())],
                    tail: None,
                    span: ds(),
                },
                span: ds(),
            }],
            caps: vec![],
            ret: None,
            doc: None,
            vis: Visibility::Private,
            attrs: vec![],
            constraints: vec![],
            body: Body::Expr(ridge_ast::Expr::Unit(ds())),
            span: ds(),
        };
        let module = ridge_ast::Module {
            items: vec![Item::Fn(f1), Item::Fn(f2)],
            doc: vec![],
            span: ds(),
        };

        let table = prescan_inline_records(&[&module], &mut arena, &b, &mut ctx);
        assert_eq!(
            table.len(),
            1,
            "order-swapped shapes should share one entry"
        );
    }

    // Nested inline records produce two table entries.
    #[test]
    fn prescan_nested_produces_two_entries() {
        let mut arena = TyConArena::new();
        let (b, mut ctx) = make_ctx_with_builtins(&mut arena);

        // Outer: { inner: { id: Int } }
        let inner_ty = AstType::Record {
            fields: vec![field("id", int_ast())],
            tail: None,
            span: ds(),
        };
        let outer_ty = AstType::Record {
            fields: vec![field("inner", inner_ty)],
            tail: None,
            span: ds(),
        };
        let module = module_with_fn_param(outer_ty);

        let table = prescan_inline_records(&[&module], &mut arena, &b, &mut ctx);
        assert_eq!(
            table.len(),
            2,
            "nested inline records should produce two entries (inner + outer)"
        );
    }

    // Different field types → distinct entries.
    #[test]
    fn prescan_distinct_by_field_type() {
        let mut arena = TyConArena::new();
        let (b, mut ctx) = make_ctx_with_builtins(&mut arena);

        let f1 = FnDecl {
            name: Ident::new("g1", ds()),
            params: vec![Param::Annotated {
                name: Ident::new("r", ds()),
                ty: AstType::Record {
                    fields: vec![field("a", int_ast())],
                    tail: None,
                    span: ds(),
                },
                span: ds(),
            }],
            caps: vec![],
            ret: None,
            doc: None,
            vis: Visibility::Private,
            attrs: vec![],
            constraints: vec![],
            body: Body::Expr(ridge_ast::Expr::Unit(ds())),
            span: ds(),
        };
        let f2 = FnDecl {
            name: Ident::new("g2", ds()),
            params: vec![Param::Annotated {
                name: Ident::new("r", ds()),
                ty: AstType::Record {
                    fields: vec![field("a", text_ast())],
                    tail: None,
                    span: ds(),
                },
                span: ds(),
            }],
            caps: vec![],
            ret: None,
            doc: None,
            vis: Visibility::Private,
            attrs: vec![],
            constraints: vec![],
            body: Body::Expr(ridge_ast::Expr::Unit(ds())),
            span: ds(),
        };
        let module = ridge_ast::Module {
            items: vec![Item::Fn(f1), Item::Fn(f2)],
            doc: vec![],
            span: ds(),
        };

        let table = prescan_inline_records(&[&module], &mut arena, &b, &mut ctx);
        assert_eq!(
            table.len(),
            2,
            "different field types must produce distinct entries"
        );
    }
}
