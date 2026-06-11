//! Ridge type inference, capability checking, and exhaustiveness analysis.
//!
//! # Entry points
//!
//! - [`typecheck_workspace`] — type-check an entire [`ResolvedWorkspace`].
//! - [`typecheck_module_incremental`] — re-check a single edited module against
//!   an already-typed workspace (LSP incremental hot-path).
//!
//! Both entry points never short-circuit on the first error; they accumulate all
//! diagnostics and return a result containing both the (potentially partial)
//! typed output and the full error vector (spec §10.4 result-aggregation policy).

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

pub mod actor;
pub mod caps_check;
pub mod caps_infer;
pub mod class_env;
pub mod collect;
pub mod cross_module;
pub mod ctx;
pub mod derive;
pub mod error;
pub mod exhaustiveness;
pub mod infer;
pub mod instantiate;
pub mod interp;
pub mod pipe_propagate;
pub mod prelude;
pub mod quote;
pub mod records;
pub mod render;
pub mod scc;
pub mod solve;
pub mod stdlib_env;
pub mod stdlib_signatures;
pub mod stdlib_types;
pub mod tycon_collect;
pub mod unify;
pub mod unions;

pub use class_env::{
    register_prelude_classes, register_prelude_instances, ClassTable, InstanceEnv, InstanceInfo,
    InstanceOrigin,
};
pub use collect::{collect_workspace, CollectResult};
pub use derive::{
    derive_instances, DelegArg, DelegResult, DelegatedMethod, DerivedInstance, DerivedMethodBody,
    FieldShape,
};
pub use error::TypeError;
pub use render::{emit_internal, emit_internal_strict, render_type_with};
pub use ridge_resolve::Severity;
pub use ridge_types::BuiltinTyCons;
pub use solve::{DictPlan, DictResolution};

// Re-export witness types from ridge_types — the canonical definitions live there.
pub use ridge_types::{MatchWitness, WitnessKind, WitnessPat};

use ridge_ast::Item;
use ridge_resolve::{ModuleId, NodeId, ResolvedWorkspace};
use ridge_types::{AnonRecordTable, CapabilitySet, Scheme, TyConArena, TyConDecl, TyConId, Type};
use rustc_hash::FxHashMap;
use std::sync::Arc;

// ── Result types ──────────────────────────────────────────────────────────────

/// Result of type-checking an entire workspace.
#[derive(Debug)]
pub struct TypecheckResult {
    /// Always present; may be partial if errors were found.
    pub typed: TypedWorkspace,
    /// All `T###` diagnostics accumulated during the pass, paired with the
    /// [`ModuleId`] of the module that produced each error.
    pub errors: Vec<(ModuleId, TypeError)>,
}

/// Result of type-checking a single module.
#[derive(Debug)]
pub struct ModuleTypecheckResult {
    /// The typed representation of this module.
    pub typed: TypedModule,
    /// `T###` diagnostics for this module only.
    pub errors: Vec<TypeError>,
    /// Anonymous record table built by the pre-scan for this module.
    ///
    /// Merged into [`TypedWorkspace::anon_records`] by the workspace
    /// driver after all modules are checked.
    pub anon_records: AnonRecordTable,
    /// Generalised top-level `fn`/`const` schemes for this module, keyed by name.
    ///
    /// The workspace driver stores these so importing modules (checked later in
    /// dependency order) can seed them into their environment.
    pub name_schemes: FxHashMap<String, ridge_types::Scheme>,
}

/// Result of incrementally type-checking a single edited module.
///
/// Carries the freshly typed module plus the full `TyCon` list its `node_types`
/// index into. Because an incremental check appends the edited module's `TyCons`
/// to the arena, this list supersedes the caller's cached
/// [`TypedWorkspace::tycons`] when rendering or storing the edited module.
#[derive(Debug)]
pub struct ModuleTypecheckIncremental {
    /// The freshly type-checked module, its errors, and its anon-record table.
    pub result: ModuleTypecheckResult,
    /// Builtins plus every module's `TyCons`, with the edited module's freshly
    /// interned at the tail. `result.typed.node_types` index into this list.
    pub tycons: Vec<TyConDecl>,
}

/// The fully type-checked workspace.
///
/// Produced by [`typecheck_workspace`]; consumed by Phase 5 (lowering),
/// Phase 6 (codegen), and Phase 8 (LSP).
#[derive(Debug)]
#[non_exhaustive]
pub struct TypedWorkspace {
    /// One [`TypedModule`] per module, indexed by [`ModuleId::0`].
    pub modules: Vec<TypedModule>,
    /// All type-constructor declarations: built-ins plus every user `TyCon`.
    pub tycons: Vec<TyConDecl>,
    /// Shortcut handles into `tycons` for the 12 built-in `TyCons`.
    pub builtins: BuiltinTyCons,
    /// Shape → [`ridge_types::TyConId`] map for anonymous record types.
    ///
    /// Populated by [`crate::tycon_collect::prescan_inline_records`] during
    /// type-checking and frozen here for Phase 5 (lowering) to resolve
    /// `Type::Record` AST nodes without re-interning.  Read-only after
    /// `typecheck_workspace` returns.
    pub anon_records: AnonRecordTable,
    /// Workspace-level class registry (name → `ClassId` + metadata).
    ///
    /// Populated by the collect pass when class/instance declarations are
    /// present. Empty for pre-typeclass workspaces; consumers must treat an
    /// empty table as equivalent to "no typeclasses defined".
    pub class_table: ClassTable,
    /// Workspace-level instance registry (`(ClassId, TyConId)` → instance metadata).
    ///
    /// Populated by the collect pass. Used by the lowering pass to determine
    /// which dictionary value to thread at each constrained call site.
    pub instance_env: InstanceEnv,
    /// All instances synthesised from `deriving (…)` clauses.
    ///
    /// The lowering pass emits method fns and dict values for each entry. Empty
    /// for workspaces without any `deriving` clauses.
    pub derived_instances: Vec<crate::derive::DerivedInstance>,
    /// Reconciled stdlib type names → their reserved-block `TyConId`.
    ///
    /// Populated from [`crate::stdlib_types::intern_stdlib_types`]; empty during
    /// the standard library's own build. The lowering pass reads this to resolve
    /// a reconciled type's constructor to its `(owner, variant)` from the arena.
    pub stdlib_tycons: FxHashMap<String, TyConId>,
}

/// A single module after type-checking.
#[derive(Debug)]
#[non_exhaustive]
pub struct TypedModule {
    /// The module's stable index within the workspace.
    pub id: ModuleId,
    /// Parsed AST, borrowed from the [`ResolvedWorkspace`].
    pub ast: Arc<ridge_ast::Module>,
    /// Type stamped on every `NodeId` that names an expression.
    ///
    /// Indexed by `NodeId.0`; `None` if no type was assigned (e.g. a
    /// non-expression position).
    pub node_types: Vec<Option<Type>>,
    /// Generalised schemes for top-level decls and `let`-bound locals.
    pub schemes: FxHashMap<NodeId, Scheme>,
    /// Inferred capability set for each `fn` / `on` / `init` / lambda decl.
    pub inferred_caps: FxHashMap<NodeId, CapabilitySet>,
    /// Per-`match` exhaustiveness witnesses, keyed by the `match` expression's
    /// `NodeId`.
    pub match_witnesses: FxHashMap<NodeId, Vec<MatchWitness>>,
    /// Per-constraint dictionary resolution plan produced by the constraint
    /// solver.
    ///
    /// Keyed by `(ClassId, TyVid)` — uniquely identifies one deferred
    /// constraint at one instantiation site. The lowering pass reads this map
    /// to emit dictionary arguments at constrained call sites.
    ///
    /// Empty for modules that contain no constrained functions (the common
    /// pre-typeclass case).
    pub dict_resolution: DictResolution,
    /// Quoted lambdas captured during inference, keyed by the lambda's span.
    ///
    /// The lowering pass reads this to reify a quoted body into a `QExpr` tree
    /// instead of a closure. Empty for any module that uses no quotation.
    pub quoted_lambdas: FxHashMap<ridge_ast::Span, crate::quote::QuoteInfo>,
}

// ── Entry points ──────────────────────────────────────────────────────────────

/// Type-check the entire workspace.
///
/// Always returns a [`TypecheckResult`] with every error encountered; never
/// panics, never short-circuits on the first failure (spec §10.4
/// result-aggregation policy).
///
/// # Pipeline
///
/// 1. Allocate a shared [`TyConArena`] and register the 12 built-in `TyCons`.
/// 2. Reuse the ASTs the resolver already parsed (`ResolvedWorkspace::module_asts`).
/// 3. For each module (in topological order from `ws.graph.deps`):
///    a. Collect user `TyCons` from `TypeDecl` / `ActorDecl` nodes.
///    b. Seed the env with prelude + stdlib qualified bindings.
///    c. Run SCC-based Algorithm W over top-level `fn` decls.
///    d. Run capability checking over each `fn` decl.
///    e. Run `check_actor_encapsulation` for each `actor` decl.
/// 4. Accumulate all diagnostics; return them alongside the typed workspace.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "linear workspace typecheck driver; splitting would obscure the pass order"
)]
pub fn typecheck_workspace(ws: &ResolvedWorkspace) -> TypecheckResult {
    let mut all_errors: Vec<(ModuleId, TypeError)> = Vec::new();

    // Step 1: Shared TyCon arena + built-in registration.
    let mut arena = TyConArena::new();
    let b = BuiltinTyCons::allocate(&mut arena);

    // Reconciled stdlib types occupy a reserved block immediately after the
    // built-ins and before any user type, so their ids are stable workspace-wide
    // and `builtins_len` below shifts the user-type prediction base past them.
    // The standard library's own build declares these types from source, so the
    // reservation is skipped there to avoid a second, conflicting declaration.
    let stdlib_tycon_names = if ws.graph.is_stdlib {
        FxHashMap::default()
    } else {
        crate::stdlib_types::intern_stdlib_types(&mut arena, &b)
    };

    // Type-check producers before consumers so a module's imported types and
    // schemes are already available when it is checked.
    let check_order = crate::cross_module::topo_order(&ws.graph.deps);

    // Predict each module's own type/actor `TyConId`s in the SAME order the
    // collect pass interns them (the shared arena makes a producer's id valid in
    // any consumer). Used to seed imported type names into each consumer's
    // `user_tycon_names`, and flattened for the instance-collection pass which
    // only needs a name to resolve to some declaring id.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "builtin TyCon count is a small constant"
    )]
    let builtins_len = arena.all().len() as u32;
    let per_module_tycon_names = crate::cross_module::predict_module_tycon_names(
        &ws.module_asts,
        &check_order,
        builtins_len,
    );
    let workspace_tycon_names =
        crate::cross_module::flatten_tycon_names(&per_module_tycon_names, &check_order);
    let symbol_tables: Vec<&ridge_resolve::SymbolTable> =
        ws.modules.iter().map(|m| &m.symbols).collect();

    // Step 2: Reuse the ASTs the resolver already parsed — no second parse pass.
    // Filled by `ModuleId.0` slot (checking runs in dependency order, but the
    // typed workspace stays `ModuleId`-indexed for downstream consumers).
    let mut typed_slots: Vec<Option<TypedModule>> = (0..ws.modules.len()).map(|_| None).collect();
    // Merged anonymous record table across all modules.
    let mut workspace_anon_records: AnonRecordTable = AnonRecordTable::default();
    // Each module's exported fn/const schemes (by `ModuleId.0`), populated as the
    // module is checked so later (dependent) modules can seed them.
    let mut exported_schemes: Vec<FxHashMap<String, ridge_types::Scheme>> = (0..ws.modules.len())
        .map(|_| FxHashMap::default())
        .collect();

    // Run the workspace collect pass to build the class/instance registries.
    // This runs over all module ASTs before any module is type-checked so the
    // solver sees every instance.
    let module_ast_pairs: Vec<(u32, &ridge_ast::Module)> = ws
        .modules
        .iter()
        .zip(&ws.module_asts)
        .map(|(rm, ast)| (rm.id.0, ast.as_ref()))
        .collect();
    // Enrich the tycon-name map the collect pass sees with the reconciled stdlib
    // types (e.g. `MemAdapter`) so `register_stdlib_instances` can key the
    // in-memory `Adapter` instance by its reconciled id. Empty during a stdlib
    // build, where the source instance in data.ridge is collected directly.
    let mut collect_tycon_names = workspace_tycon_names.clone();
    for (name, &id) in &stdlib_tycon_names {
        collect_tycon_names.entry(name.clone()).or_insert(id);
    }
    let collect_result = collect_workspace(&module_ast_pairs, &collect_tycon_names);
    // Coherence errors are workspace-level; accumulate them tagged with the
    // module they originated in (use ModuleId(0) as a fallback — coherence
    // errors carry their own span, so the module tag is informational only).
    for err in collect_result.errors {
        all_errors.push((ModuleId(0), err));
    }
    let class_table = collect_result.class_table;
    let instance_env = collect_result.instance_env;

    // Step 3: Type-check each module in dependency order (producers first).
    for &mid in &check_order {
        let Some(rm) = ws.modules.iter().find(|m| m.id == mid) else {
            continue;
        };
        // Reuse the resolver's AST for this module (indexed by ModuleId).
        let ast_opt = ws.module_asts.get(rm.id.0 as usize);
        // If the AST is somehow absent (e.g. an earlier I/O error), produce
        // an empty typed module and continue.
        let ast = if let Some(ast) = ast_opt {
            Arc::clone(ast)
        } else {
            typed_slots[rm.id.0 as usize] = Some(TypedModule {
                id: rm.id,
                ast: Arc::new(ridge_ast::Module {
                    items: Vec::new(),
                    doc: Vec::new(),
                    span: ridge_ast::Span::point(0),
                }),
                node_types: Vec::new(),
                schemes: FxHashMap::default(),
                inferred_caps: FxHashMap::default(),
                match_witnesses: FxHashMap::default(),
                dict_resolution: FxHashMap::default(),
                quoted_lambdas: FxHashMap::default(),
            });
            continue;
        };

        let imported_tycons = crate::cross_module::imported_tycon_names(
            &rm.imports,
            &symbol_tables,
            &per_module_tycon_names,
            &stdlib_tycon_names,
            &b,
        );
        let imported_schemes = crate::cross_module::imported_value_schemes(
            &rm.imports,
            &symbol_tables,
            &exported_schemes,
        );
        let result = typecheck_module_inner(
            rm.id,
            &ast,
            rm.node_ids.clone(),
            &rm.imports,
            &imported_tycons,
            &imported_schemes,
            &workspace_tycon_names,
            &stdlib_tycon_names,
            &mut arena,
            &b,
            Some((&class_table, &instance_env)),
        );
        // `node_types` is indexed by `NodeId.0` and grown on demand, so it can be
        // shorter than the full map but must never exceed it. A violation means
        // resolve and typecheck disagree on the NodeId space.
        debug_assert!(
            result.typed.node_types.len() <= rm.node_ids.len(),
            "node_types ({}) exceeds NodeIdMap size ({}) for module {:?}",
            result.typed.node_types.len(),
            rm.node_ids.len(),
            rm.id
        );
        all_errors.extend(result.errors.into_iter().map(|e| (rm.id, e)));
        // Merge this module's anon_records (last-write wins; same shapes share
        // the same TyConId workspace-wide because the arena is shared).
        workspace_anon_records.extend(result.anon_records);
        // Expose this module's schemes to modules that import it (checked later).
        exported_schemes[rm.id.0 as usize] = result.name_schemes;
        typed_slots[rm.id.0 as usize] = Some(result.typed);
    }

    // Re-assemble typed modules in `ModuleId` order for downstream consumers.
    let typed_modules: Vec<TypedModule> = typed_slots
        .into_iter()
        .enumerate()
        .map(|(i, slot)| {
            slot.unwrap_or_else(|| {
                empty_module_result(ModuleId(u32::try_from(i).unwrap_or(u32::MAX))).typed
            })
        })
        .collect();

    // Collect all TyConDecls from arena for the typed workspace.
    let tycons: Vec<TyConDecl> = arena.all().to_vec();

    TypecheckResult {
        typed: TypedWorkspace {
            modules: typed_modules,
            tycons,
            builtins: b,
            anon_records: workspace_anon_records,
            class_table,
            instance_env,
            derived_instances: collect_result.derived_instances,
            stdlib_tycons: stdlib_tycon_names,
        },
        errors: all_errors,
    }
}

/// Build an empty [`ModuleTypecheckResult`] for a module that could not be
/// located in the workspace (an out-of-range id or a missing AST).
fn empty_module_result(module_id: ModuleId) -> ModuleTypecheckResult {
    ModuleTypecheckResult {
        typed: TypedModule {
            id: module_id,
            ast: Arc::new(ridge_ast::Module {
                items: Vec::new(),
                doc: Vec::new(),
                span: ridge_ast::Span::point(0),
            }),
            node_types: Vec::new(),
            schemes: FxHashMap::default(),
            inferred_caps: FxHashMap::default(),
            match_witnesses: FxHashMap::default(),
            dict_resolution: FxHashMap::default(),
            quoted_lambdas: FxHashMap::default(),
        },
        errors: Vec::new(),
        anon_records: AnonRecordTable::default(),
        name_schemes: FxHashMap::default(),
    }
}

/// Incrementally type-check a single edited module against an already-typed
/// workspace.
///
/// The caller supplies the [`ResolvedWorkspace`] — already updated for the edit
/// by [`ridge_resolve::resolve_module_incremental`] — and the [`TypedWorkspace`]
/// from the prior full check. This re-checks one module without touching the
/// rest of the workspace.
///
/// The arena is rebuilt from `typed_ws.tycons`, preserving every existing
/// `TyConId`; the edited module's own `TyCons` are then interned at the tail
/// with fresh ids. The edited module's `node_types` therefore index into the
/// returned [`ModuleTypecheckIncremental::tycons`], not the stale
/// `typed_ws.tycons` — the raw ids may differ from a full build, but the types
/// they denote are identical. The class/instance registries from the prior
/// check are reused unchanged, so this path is correct only while the edit
/// leaves the workspace's class/instance/deriving surface intact.
#[must_use]
pub fn typecheck_module_incremental(
    module_id: ModuleId,
    ws: &ResolvedWorkspace,
    typed_ws: &TypedWorkspace,
) -> ModuleTypecheckIncremental {
    // Find the resolved module entry.
    let Some(rm) = ws.modules.iter().find(|m| m.id == module_id) else {
        return ModuleTypecheckIncremental {
            result: empty_module_result(module_id),
            tycons: typed_ws.tycons.clone(),
        };
    };

    // Reuse the AST the resolver retained for this module — no re-parse.
    let Some(ast) = ws.module_asts.get(module_id.0 as usize).map(Arc::clone) else {
        return ModuleTypecheckIncremental {
            result: empty_module_result(module_id),
            tycons: typed_ws.tycons.clone(),
        };
    };

    // Rebuild the arena from the prior check, preserving every existing TyConId,
    // so this module's TyCons append at the tail with fresh ids.
    let mut arena = TyConArena::new();
    for decl in &typed_ws.tycons {
        arena.intern(decl.clone());
    }
    let b = &typed_ws.builtins;

    // The incremental path stays module-local for cross-module seeding (no
    // imported-type or imported-scheme maps); a full rebuild covers cross-module
    // changes.
    let no_imported_tycons = FxHashMap::default();
    let no_imported_schemes = FxHashMap::default();
    // The prior arena holds every workspace type, so a name→id map over it lets
    // class-method signatures resolve cross-module type references (e.g. a
    // stdlib class whose methods mention a type from its own module).
    let global_tycon_names: FxHashMap<String, TyConId> = typed_ws
        .tycons
        .iter()
        .map(|d| (d.name.clone(), d.id))
        .collect();
    let result = typecheck_module_inner(
        module_id,
        &ast,
        rm.node_ids.clone(),
        &rm.imports,
        &no_imported_tycons,
        &no_imported_schemes,
        &global_tycon_names,
        &typed_ws.stdlib_tycons,
        &mut arena,
        b,
        Some((&typed_ws.class_table, &typed_ws.instance_env)),
    );

    ModuleTypecheckIncremental {
        tycons: arena.all().to_vec(),
        result,
    }
}

// ── Internal pipeline ─────────────────────────────────────────────────────────

/// Run capability checking for every fn decl and return the per-`NodeId`
/// inferred capability map.
///
/// Extracted from `typecheck_module_inner` Step D to keep that function under
/// the line-count lint threshold.
fn infer_caps_for_decls(
    ctx: &mut crate::ctx::InferCtx,
    b: &BuiltinTyCons,
    fn_decls: &[&ridge_ast::FnDecl],
) -> FxHashMap<NodeId, CapabilitySet> {
    use crate::caps_check::{caps_from_ast_slice, check_caps_decl};
    use ridge_ast::{Body, Expr as AstExpr};
    use ridge_resolve::NodeKind;

    let mut inferred_caps: FxHashMap<NodeId, CapabilitySet> = FxHashMap::default();
    for f in fn_decls {
        let declared = if caps_check::is_file_private(&f.name.text) {
            None
        } else if f.caps.is_empty() {
            Some(CapabilitySet::PURE)
        } else {
            Some(caps_from_ast_slice(&f.caps))
        };
        // Body::Ffi has no expression to check caps against — T3 validates it.
        // We skip it here and leave no inferred-caps entry for the decl.
        let expr = match &f.body {
            Body::Expr(e) => e,
            Body::Ffi { .. } => continue,
        };
        let effective = check_caps_decl(ctx, b, &f.name.text, declared, expr, f.span);
        let (body_span, body_kind) = match expr {
            AstExpr::Block(blk) => (blk.span, NodeKind::Block),
            AstExpr::Try { span, .. } => (*span, NodeKind::Try),
            other => (other.span(), NodeKind::Expr),
        };
        let real_nid = ctx
            .node_id_map
            .as_ref()
            .and_then(|m| m.get(body_span, body_kind))
            .unwrap_or(NodeId(f.span.start));
        inferred_caps.insert(real_nid, effective);
        let proxy_nid = NodeId(f.span.start);
        if proxy_nid != real_nid {
            inferred_caps.insert(proxy_nid, effective);
        }
    }
    inferred_caps
}

/// Run `infer_expr` over every actor handler body in the module so that the
/// `node_types` side-table is populated for those expressions.
///
/// Handlers and init blocks are not part of the SCC walk over top-level `fn`
/// decls (which is what populates `node_types` for ordinary functions), so
/// without this pass any expression inside a handler body — including
/// arithmetic operands — has no associated type when lowering runs.  That
/// silently downgrades arithmetic dispatch to the Int family for code that is
/// actually Float, producing runtime `badarith` crashes on Float division.
///
/// State fields and handler parameters are bound into a fresh env frame
/// before inferring the body.  The body's type is intentionally not unified
/// against the declared return type here: this pass is only for side-effect
/// population of `node_types`, and surfacing additional T-errors at this
/// point would be a behaviour change beyond the scope of the dispatch fix.
fn typecheck_actor_bodies(
    ctx: &mut crate::ctx::InferCtx,
    b: &BuiltinTyCons,
    ast: &Arc<ridge_ast::Module>,
    arena: &TyConArena,
) {
    use crate::infer::infer_expr;
    use ridge_ast::ActorMember;
    use ridge_types::TyConKind;

    let monoscheme = |ty: ridge_types::Type| Scheme {
        vars: vec![],
        cap_vars: vec![],
        row_vars: vec![],
        ty,
        constraints: vec![],
    };

    for item in &ast.items {
        let Item::Actor(ad) = item else { continue };
        let Some(&actor_id) = ctx.user_tycon_names.get(&ad.name.text) else {
            continue;
        };
        let TyConKind::Actor(schema) = &arena.get(actor_id).kind else {
            continue;
        };

        let mut handler_idx = 0usize;
        for member in &ad.members {
            let ActorMember::On(handler) = member else {
                continue;
            };
            let Some(handler_schema) = schema.handlers.get(handler_idx) else {
                handler_idx += 1;
                continue;
            };
            handler_idx += 1;

            ctx.env.push_frame();

            // Bind state fields.
            for field in &schema.state_fields {
                ctx.env
                    .bind(field.name.clone(), monoscheme(field.ty.clone()));
            }

            // Bind handler parameters.
            for (param, ty) in handler.params.iter().zip(handler_schema.params.iter()) {
                match param {
                    ridge_ast::Param::Bare(id) => {
                        ctx.env.bind(id.text.clone(), monoscheme(ty.clone()));
                    }
                    ridge_ast::Param::Annotated { name, .. } => {
                        ctx.env.bind(name.text.clone(), monoscheme(ty.clone()));
                    }
                    ridge_ast::Param::PatternAnnotated { pat, span, .. } => {
                        crate::infer::infer_pattern(ctx, b, pat, ty);
                        crate::exhaustiveness::check_param_irrefutable(ctx, b, pat, ty, *span);
                    }
                }
            }

            // Walk the body purely for side-effect: populates ctx.node_types_accum.
            let _ = infer_expr(ctx, b, &handler.body);

            ctx.env.pop_frame();
        }
    }
}

/// Type-check a single module given its parsed AST, resolved imports, and a
/// shared (mutable) `TyCon` arena.
///
/// This is the single-module body used by both [`typecheck_workspace`] and
/// [`typecheck_module_incremental`].
///
/// `registries` supplies the workspace-level class/instance tables produced by
/// [`crate::collect::collect_workspace`]. When `None`, empty registries are used
/// and the constraint solver is a no-op (the pre-typeclass behavior for the LSP
/// hot-path and unit tests). The two tables always travel together, so they are
/// passed as one optional pair.
#[expect(
    clippy::too_many_arguments,
    reason = "per-module typecheck threads its resolver inputs explicitly"
)]
#[expect(
    clippy::too_many_lines,
    reason = "linear per-module typecheck pipeline; splitting would obscure pass order"
)]
fn typecheck_module_inner(
    id: ModuleId,
    ast: &Arc<ridge_ast::Module>,
    node_id_map: ridge_resolve::NodeIdMap,
    imports: &[ridge_resolve::ImportResolution],
    imported_tycons: &FxHashMap<String, TyConId>,
    imported_schemes: &FxHashMap<String, ridge_types::Scheme>,
    global_tycon_names: &FxHashMap<String, TyConId>,
    stdlib_tycon_names: &FxHashMap<String, TyConId>,
    arena: &mut TyConArena,
    b: &BuiltinTyCons,
    registries: Option<(
        &crate::class_env::ClassTable,
        &crate::class_env::InstanceEnv,
    )>,
) -> ModuleTypecheckResult {
    use crate::actor::{check_actor_encapsulation, check_actor_mailbox_config};
    use crate::ctx::InferCtx;
    use crate::scc::typecheck_module_decls;
    use crate::stdlib_env::seed_stdlib_env;
    use crate::tycon_collect::{collect_user_tycons, prescan_inline_records};

    let mut ctx = InferCtx::new();
    // Attach the resolver's NodeIdMap so infer_expr can write back per-expression
    // types. The map is stamped once during resolve and threaded in here rather
    // than rebuilt, so resolve and typecheck stay keyed by the same NodeIds.
    ctx.node_id_map = Some(node_id_map);

    // Push the module-level env frame.
    ctx.env.push_frame();

    // Record which module is being inferred so the opaque-type field boundary
    // (records.rs) can compare against each type's defining module.
    ctx.current_module_raw = Some(id.0);

    // Step A: Collect user TyCons and seed env with constructor schemes.
    let tycon_result = collect_user_tycons(ast, id, arena, b, &mut ctx);
    // Populate the user_tycon_names map for ast_type_to_type resolution.
    ctx.user_tycon_names = tycon_result.user_tycon_names;
    // Seed imported type names (cross-module): a local declaration of the same
    // name always wins, so only insert imports that don't shadow a local type.
    for (name, &tid) in imported_tycons {
        ctx.user_tycon_names.entry(name.clone()).or_insert(tid);
    }
    // Step A1: Column codegen — synthesize the `deriving (Table)` mirrors (the
    // `<Entity>Cols` type plus the `<entity>Cols` / `<entity>Table` values)
    // before the snapshot, so field access on a mirror resolves and fn/const
    // bodies that reference the values type-check. Runs after the import merge so
    // `Column`/`Table` from std.sql are resolvable.
    crate::tycon_collect::synth_table_mirrors(ast, id, arena, b, global_tycon_names, &mut ctx);
    // Schema codegen: bind `<entity>Schema : Schema` for every `deriving (Schema)`
    // record. The descriptor type is the `Schema` builtin (no per-entity type), so
    // this only registers value schemes; lowering emits the values.
    crate::tycon_collect::synth_schema_descriptors(ast, b, &mut ctx);
    // Snapshot all TyConDecls (builtins + user) for record/union inference.
    ctx.tycon_decls = arena.all().to_vec();

    // Step A2: Pre-scan inline record types and intern anonymous TyCons.
    // Must run AFTER pass-1 (names stable) and alias-chain resolution, BEFORE
    // inference begins so that ast_type_to_ridge_type can look up shapes.
    let anon_table = prescan_inline_records(&[ast], arena, b, &mut ctx);
    ctx.anon_records = anon_table;
    // Re-snapshot after anon TyCon interning so ctx.tycon_decls includes them.
    ctx.tycon_decls = arena.all().to_vec();

    // Step B-pre: Seed one scheme per class method at lowest precedence.
    // These bindings are entered first so that any same-named local fn or stdlib
    // entry bound afterwards (by seed_stdlib_env or by the SCC fn-type step)
    // overwrites them — implementing "class methods shadow at lowest precedence".
    // The constraint deferral in `instantiate` then handles the rest: each call
    // site that instantiates the scheme pushes a `Constraint` into
    // `deferred_constraints`, which the solver later resolves as Static or Forward.
    if let Some((ct, _)) = registries {
        seed_class_method_schemes(&mut ctx, b, ct, global_tycon_names);
        seed_prelude_codec_schemes(&mut ctx, b);
        // `SqlValue` is a builtin (#20) for user builds, but the standard
        // library's own build also interns sql.ridge's source `pub type
        // SqlValue` as a distinct tycon. There, the codec/seam method schemes
        // must name that source type so a stdlib module importing `SqlValue`
        // (e.g. std.repo threading rows to the `Adapter` verbs) agrees with the
        // seeded schemes rather than tripping the source-vs-builtin mismatch.
        // The reconciled block is empty exactly during the stdlib's own build,
        // so its emptiness selects the source id; user builds keep the builtin.
        let sql_value = if stdlib_tycon_names.is_empty() {
            global_tycon_names
                .get("SqlValue")
                .copied()
                .unwrap_or(b.sql_value)
        } else {
            b.sql_value
        };
        seed_sql_codec_schemes(&mut ctx, b, ct, sql_value);
    }

    // Step B: Seed env with prelude constructors + stdlib qualified bindings.
    // The class table (when present) lets reconciled stdlib functions be seeded
    // with their class constraints — e.g. std.repo's verbs over `Adapter`/`Row`.
    seed_stdlib_env(
        &mut ctx,
        b,
        imports,
        stdlib_tycon_names,
        registries.map(|(ct, _)| ct),
    );

    // Step B1: Seed schemes for fns/consts imported from other workspace modules
    // (cross-module value seeding). Bound after stdlib but before local consts and
    // fns, so a same-named local declaration always wins.
    for (name, scheme) in imported_schemes {
        ctx.env.bind(name.clone(), scheme.clone());
    }

    // Step B2: Bind top-level const declarations in the env so fn bodies that
    // reference them resolve correctly (e.g. `defaultGenerations`, `alphabet`).
    // Consts are typed by inferring their value expression under the current env.
    for item in &ast.items {
        if let Item::Const(c) = item {
            use crate::infer::infer_expr;
            let ty = infer_expr(&mut ctx, b, &c.value);
            let scheme = ridge_types::Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty,
                constraints: vec![],
            };
            ctx.name_schemes_accum
                .insert(c.name.text.clone(), scheme.clone());
            ctx.env.bind(c.name.text.clone(), scheme);
        }
    }

    // Step C: Collect top-level fn decls and run SCC-based Algorithm W.
    // typecheck_module_decls also populates ctx.schemes_accum (T4).
    let fn_decls: Vec<&ridge_ast::FnDecl> = ast
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Fn(f) = item {
                Some(f)
            } else {
                None
            }
        })
        .collect();

    // Use the caller-supplied registries when available; fall back to empty
    // registries so the constraint solver is a no-op for unconstrained modules.
    let scratch_class_table = crate::class_env::ClassTable::new();
    let scratch_instance_env = crate::class_env::InstanceEnv::new();
    let (ct, ie) = registries.unwrap_or((&scratch_class_table, &scratch_instance_env));
    typecheck_module_decls(&mut ctx, b, &fn_decls, ct, ie);

    // Step D: Capability checking for each fn decl.
    // OQ-PHASE45-005: span-keyed lookup via fn body's span + NodeKind.
    // D040: file-private / unannotated / annotated cap handling is inside
    // infer_caps_for_decls; backward-compat dual-insert is also there.
    let inferred_caps = infer_caps_for_decls(&mut ctx, b, &fn_decls);

    // Step D2: Type-check actor handler bodies so that node_types is populated
    // for every expression inside a handler.  Without this, dispatchers in
    // ridge-lower that consult node_types (notably the Float-vs-Int dispatch
    // for `BinOp::Div`) can't tell which family to pick and fall back to the
    // Int default, which emits `erlang:div/2` and crashes on Float operands.
    typecheck_actor_bodies(&mut ctx, b, ast, arena);

    // Step E: Actor encapsulation checks + mailbox config validation.
    for item in &ast.items {
        if let Item::Actor(ad) = item {
            // Mailbox config validation (T027 — `drop oldest` rejection). This
            // does not depend on the type-constructor arena and runs even when
            // the actor has no TyCon entry yet.
            ctx.errors.extend(check_actor_mailbox_config(ad));

            // Retrieve the actor's TyConId from the names map.
            if let Some(&actor_id) = ctx.user_tycon_names.get(&ad.name.text) {
                let decl = arena.get(actor_id);
                if let ridge_types::TyConKind::Actor(schema) = &decl.kind {
                    // Actor-level declared caps: actors in 0.1.0 have no explicit
                    // cap annotation in the AST. The effective cap set is the
                    // union of all handler caps (Model B, D018). Handler caps are
                    // always ⊆ this union by construction, so T019 can only fire
                    // via the init block (init_caps ⊄ union(handler_caps)).
                    let actor_caps = schema
                        .handlers
                        .iter()
                        .fold(CapabilitySet::PURE, |acc, h| acc.union(&h.caps));
                    // Per-handler spans — not yet wired; use actor span as fallback.
                    let handler_spans: Vec<Option<ridge_ast::Span>> =
                        schema.handlers.iter().map(|_| None).collect();
                    let encap_errors = check_actor_encapsulation(
                        &ad.name.text,
                        actor_caps,
                        schema,
                        &handler_spans,
                        ad.span,
                    );
                    ctx.errors.extend(encap_errors);
                }
            }
        }
    }

    // Note: detect_unsolved_type_vars (T023) is already called by
    // typecheck_module_decls internally. No need to repeat here.

    // Phase 4.5 T4: capture generalised schemes for top-level decls BEFORE
    // popping the env frame. ctx.schemes_accum was populated by
    // typecheck_module_decls (scc.rs) via write_scheme_if_top_level.
    // OQ-PHASE45-003: top-level decl schemes only; let-bound locals excluded.
    let schemes = std::mem::take(&mut ctx.schemes_accum);

    // Capture the dictionary resolution plan accumulated during SCC solving.
    // Non-empty only when typeclass constraints were present in this module.
    let dict_resolution = std::mem::take(&mut ctx.dict_resolution_accum);

    ctx.env.pop_frame();

    // Phase 4.5 T3: move the node_types accumulator into TypedModule.
    // Every expression that was reached by infer_expr has its type recorded here.
    //
    // Resolve each entry deeply now that the union-find is complete. The
    // write-back during inference only shallow-resolves (the top constructor),
    // so a recorded `Box ?e` may still mention a variable that later unified with
    // a concrete type. The lowering pass reads these types to pick instance
    // dictionaries for parametric instances, where the element type — not just
    // the head constructor — selects the dictionary, so it must be fully ground.
    let mut node_types = std::mem::take(&mut ctx.node_types_accum);
    for slot in &mut node_types {
        if let Some(ty) = slot {
            *slot = Some(ctx.deep_resolve(ty));
        }
    }

    // Phase 4.5 T5: inferred_caps is now keyed by real NodeIds (or proxy fallback).
    // The T17 proxy comment is removed; the sweep will update LowerCtx::lookup_inferred_caps.
    // Move the quoted-lambda side-table out for the lowering pass.
    let quoted_lambdas = std::mem::take(&mut ctx.quoted_lambdas_accum);

    let typed = TypedModule {
        id,
        ast: Arc::clone(ast),
        node_types, // Phase 4.5 T3: populated via infer_expr write-back
        schemes,    // Phase 4.5 T4: populated by SCC generalise write-back
        inferred_caps,
        match_witnesses: FxHashMap::default(), // T17: populated by infer_expr
        dict_resolution, // populated by the constraint solver when classes are used
        quoted_lambdas,  // populated by the quotation checker when quotes are used
    };

    // Move the anon_records table out so the workspace driver can merge it.
    let anon_records = std::mem::take(&mut ctx.anon_records);

    ModuleTypecheckResult {
        typed,
        errors: ctx.errors,
        anon_records,
        name_schemes: ctx.name_schemes_accum,
    }
}

// ── Class method scheme seeding ────────────────────────────────────────────────

/// Seeds one polymorphic scheme per class method into `ctx.env`.
///
/// The scheme has the shape `∀a. Fn{params, ret} with constraints=[Constraint{class, a}]`
/// where `a` is a fresh `TyVid` and the param/ret types are derived from the
/// class body's AST method signatures.
///
/// These bindings are entered at the LOWEST precedence layer: because `env.bind`
/// inserts into the innermost (and only) frame, any subsequent binding for the
/// same name — from stdlib seeding, user `fn` declarations, or local params —
/// overwrites the method scheme, implementing correct shadowing.
///
/// When a method scheme is instantiated at a call site, `instantiate` in
/// `instantiate.rs` defers the constraint into `ctx.deferred_constraints`.
/// The SCC solver then resolves it as `Static` (concrete receiver) or retains
/// it as `Forward` (polymorphic receiver), enabling the implicit-acquisition
/// semantic described in the design (no explicit `where` clause required).
fn seed_class_method_schemes(
    ctx: &mut crate::ctx::InferCtx,
    b: &ridge_types::BuiltinTyCons,
    class_table: &crate::class_env::ClassTable,
    global_tycon_names: &FxHashMap<String, TyConId>,
) {
    use crate::tycon_collect::ast_type_to_ridge_type;
    use ridge_types::{CapRow, CapabilitySet, Constraint, Scheme, Type};
    use rustc_hash::FxHashMap;

    // Class-method signatures may reference types declared in the class's own
    // module while being seeded into a *different* module's env — every global
    // class method is seeded into every module so bare-name calls resolve. A
    // method like `toSql (x: a) -> SqlValue` declared in `std.sql` is therefore
    // seeded into, say, `std.crypto`, where `SqlValue` is not in the local
    // `user_tycon_names`. Resolving against only the current module's names
    // would leave it as a fresh var, which then trips T023 (unsolved type
    // variable) when the scheme is generalised. Merge the workspace-global type
    // map under the local one (local wins on a name clash, preserving shadowing)
    // so cross-module type names in a signature resolve to their shared arena id.
    let mut sig_tycon_names = global_tycon_names.clone();
    for (name, &id) in &ctx.user_tycon_names {
        sig_tycon_names.insert(name.clone(), id);
    }

    for (class_id, class_info) in class_table.iter() {
        for sig in &class_info.method_sigs {
            // Skip methods whose AST types were not recorded (prelude methods
            // registered without source; their ToText/Eq/Ord dispatch is handled
            // by the existing interpolation path).
            let Some(ast_ret) = &sig.ast_ret_type else {
                continue;
            };
            if sig.ast_param_types.is_empty() {
                continue;
            }

            // Allocate a fresh TyVid per class type variable — one for an
            // ordinary class (`a`), several for a multi-parameter class (`a b`).
            // Map each name so that occurrences in param/ret types resolve to the
            // matching fresh TyVid.
            let mut tyvar_map: FxHashMap<&str, ridge_types::TyVid> = FxHashMap::default();
            let mut class_tyvids: Vec<ridge_types::TyVid> =
                Vec::with_capacity(sig.class_ty_vars.len());
            for name in &sig.class_ty_vars {
                let tv = ctx.fresh_tyvid();
                class_tyvids.push(tv);
                if !name.is_empty() {
                    tyvar_map.insert(name.as_str(), tv);
                }
            }
            // A method whose AST types were recorded always has at least one
            // class variable; keep one as a defensive fallback so the scheme is
            // never empty-quantified.
            if class_tyvids.is_empty() {
                class_tyvids.push(ctx.fresh_tyvid());
            }

            // Convert AST param types to Ridge types, substituting class_ty_var.
            let param_types: Vec<Type> = sig
                .ast_param_types
                .iter()
                .map(|ast_ty| ast_type_to_ridge_type(b, ctx, ast_ty, &sig_tycon_names, &tyvar_map))
                .collect();

            // Convert AST return type.
            let ret_type = ast_type_to_ridge_type(b, ctx, ast_ret, &sig_tycon_names, &tyvar_map);

            let fn_ty = Type::Fn {
                params: param_types,
                ret: Box::new(ret_type),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };

            // Build a polymorphic scheme ∀[class_tyvids]. fn_ty with one
            // constraint over all the class variables.
            let constraint_tys: smallvec::SmallVec<[ridge_types::TyVid; 1]> =
                class_tyvids.iter().copied().collect();
            let scheme = Scheme {
                vars: class_tyvids,
                cap_vars: vec![],
                row_vars: vec![],
                ty: fn_ty,
                constraints: vec![Constraint::new(class_id, constraint_tys)],
            };

            // Seed at lowest precedence: bind under the method name.
            // Subsequent bindings for the same name (user fns, stdlib) will
            // overwrite this entry, keeping existing programs green.
            ctx.env.bind(sig.name.clone(), scheme);
        }
    }
}

/// Seed type-environment schemes for the two prelude codec methods (`encode`,
/// `decode`) so that bare calls work without an inline `class` redeclaration.
///
/// `ToText`, `Eq`, and `Ord` are dispatched via language operators (`$"..."`,
/// `==`, comparison) and do not need an env scheme for bare calls.  `Encode`
/// and `Decode` have no operator and must be callable by bare name from user
/// code, so their schemes are seeded here rather than through the AST-driven
/// `seed_class_method_schemes` path (which requires `ast_param_types` to be
/// populated, which the prelude registry intentionally leaves empty).
///
/// Schemes:
///
/// - `encode :: ∀a. a → JsonValue where Encode a`
/// - `decode :: ∀a. JsonValue → Result a Error where Decode a`
fn seed_prelude_codec_schemes(ctx: &mut crate::ctx::InferCtx, b: &ridge_types::BuiltinTyCons) {
    use ridge_types::{
        CapRow, CapabilitySet, Constraint, Scheme, Type, DECODE_CLASS, ENCODE_CLASS,
    };

    // ── encode :: ∀a. a → JsonValue where Encode a ───────────────────────────
    {
        let a = ctx.fresh_tyvid();
        let fn_ty = Type::Fn {
            params: vec![Type::Var(a)],
            ret: Box::new(Type::Con(b.json_value, vec![])),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        ctx.env.bind(
            "encode".to_owned(),
            Scheme {
                vars: vec![a],
                cap_vars: vec![],
                row_vars: vec![],
                ty: fn_ty,
                constraints: vec![Constraint::single(ENCODE_CLASS, a)],
            },
        );
    }

    // ── decode :: ∀a. JsonValue → Result a Error where Decode a ─────────────
    {
        let a = ctx.fresh_tyvid();
        let fn_ty = Type::Fn {
            params: vec![Type::Con(b.json_value, vec![])],
            ret: Box::new(Type::Con(
                b.result,
                vec![Type::Var(a), Type::Con(b.error, vec![])],
            )),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        ctx.env.bind(
            "decode".to_owned(),
            Scheme {
                vars: vec![a],
                cap_vars: vec![],
                row_vars: vec![],
                ty: fn_ty,
                constraints: vec![Constraint::single(DECODE_CLASS, a)],
            },
        );
    }
}

/// Seed env schemes for std.sql's `toSql`/`fromSql` codec methods so bare calls
/// type-check once `std.sql` is imported (the resolver gates the names). Mirrors
/// `seed_prelude_codec_schemes` but for the dynamically-registered `SqlType`
/// class, whose id is looked up from the class table. Skipped when `SqlType` is
/// absent (empty registries / LSP hot path).
///
/// - `toSql   :: ∀a. a        -> SqlValue        where SqlType a`
/// - `fromSql :: ∀a. SqlValue -> Result a Error  where SqlType a`
#[expect(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "one flat block per stdlib codec/seam method (toSql/fromSql/fromRow/insert/all/join); \
              splitting per method would scatter the shared builtin-type setup, and the single-letter \
              locals mirror the type variables (a, e, c, p, r)"
)]
fn seed_sql_codec_schemes(
    ctx: &mut crate::ctx::InferCtx,
    b: &ridge_types::BuiltinTyCons,
    class_table: &crate::class_env::ClassTable,
    sql_value: ridge_types::TyConId,
) {
    use ridge_types::{CapRow, CapabilitySet, Constraint, Scheme, Type};
    let Some(sqltype) = class_table.id_by_name("SqlType") else {
        return;
    };
    // toSql :: ∀a. a -> SqlValue where SqlType a
    {
        let a = ctx.fresh_tyvid();
        let fn_ty = Type::Fn {
            params: vec![Type::Var(a)],
            ret: Box::new(Type::Con(sql_value, vec![])),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        ctx.env.bind(
            "toSql".to_owned(),
            Scheme {
                vars: vec![a],
                cap_vars: vec![],
                row_vars: vec![],
                ty: fn_ty,
                constraints: vec![Constraint::single(sqltype, a)],
            },
        );
    }
    // fromSql :: ∀a. SqlValue -> Result a Error where SqlType a
    {
        let a = ctx.fresh_tyvid();
        let fn_ty = Type::Fn {
            params: vec![Type::Con(sql_value, vec![])],
            ret: Box::new(Type::Con(
                b.result,
                vec![Type::Var(a), Type::Con(b.error, vec![])],
            )),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        ctx.env.bind(
            "fromSql".to_owned(),
            Scheme {
                vars: vec![a],
                cap_vars: vec![],
                row_vars: vec![],
                ty: fn_ty,
                constraints: vec![Constraint::single(sqltype, a)],
            },
        );
    }
    // fromRow :: ∀a. Map Text SqlValue -> Result a Error where Row a
    // The Row class is registered alongside SqlType (see register_stdlib_classes);
    // its instances come from `deriving (Row)`. Seeded here for the same reason as
    // the codec methods: bare `fromRow` calls type-check once std.sql is imported.
    if let Some(row) = class_table.id_by_name("Row") {
        let a = ctx.fresh_tyvid();
        let fn_ty = Type::Fn {
            params: vec![Type::Con(
                b.map,
                vec![Type::Con(b.text, vec![]), Type::Con(sql_value, vec![])],
            )],
            ret: Box::new(Type::Con(
                b.result,
                vec![Type::Var(a), Type::Con(b.error, vec![])],
            )),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        ctx.env.bind(
            "fromRow".to_owned(),
            Scheme {
                vars: vec![a],
                cap_vars: vec![],
                row_vars: vec![],
                ty: fn_ty,
                constraints: vec![Constraint::single(row, a)],
            },
        );
        // toRow :: ∀a. a -> Map Text SqlValue where Row a — the encode half of the
        // Row codec, dual to `fromRow`. Bare `toRow` calls type-check once std.sql
        // is imported; an `Option` field writes `None` as SQL NULL through its
        // `SqlType` instance.
        let a = ctx.fresh_tyvid();
        let to_row_ty = Type::Fn {
            params: vec![Type::Var(a)],
            ret: Box::new(Type::Con(
                b.map,
                vec![Type::Con(b.text, vec![]), Type::Con(sql_value, vec![])],
            )),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        };
        ctx.env.bind(
            "toRow".to_owned(),
            Scheme {
                vars: vec![a],
                cap_vars: vec![],
                row_vars: vec![],
                ty: to_row_ty,
                constraints: vec![Constraint::single(row, a)],
            },
        );
    }
    // The `Adapter` seam from std.data. Both methods are cap-free: opening an
    // adapter is the act gated by `db`, and the handle is the proof of access
    // thereafter (the actor handle-as-proof model, spec §6.4.1). Seeded here for
    // the same reason as the codec methods — bare `appendRow`/`all` type-check once
    // std.data is imported, dispatching on the connection-handle type.
    if let Some(adapter) = class_table.id_by_name("Adapter") {
        let row_ty = Type::Con(
            b.map,
            vec![Type::Con(b.text, vec![]), Type::Con(sql_value, vec![])],
        );
        // appendRow :: ∀a. a -> Text -> Map Text SqlValue -> Result Unit Error where Adapter a
        {
            let a = ctx.fresh_tyvid();
            let fn_ty = Type::Fn {
                params: vec![Type::Var(a), Type::Con(b.text, vec![]), row_ty.clone()],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![Type::Con(b.unit, vec![]), Type::Con(b.error, vec![])],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "appendRow".to_owned(),
                Scheme {
                    vars: vec![a],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // all :: ∀a. a -> Text -> Result (List (Map Text SqlValue)) Error where Adapter a
        {
            let a = ctx.fresh_tyvid();
            let fn_ty = Type::Fn {
                params: vec![Type::Var(a), Type::Con(b.text, vec![])],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![Type::Con(b.list, vec![row_ty]), Type::Con(b.error, vec![])],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "all".to_owned(),
                Scheme {
                    vars: vec![a],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // A `Map Text SqlValue` row, rebuilt fresh per scheme (the value above is
        // already moved into the insert/all schemes).
        let map_row = || {
            Type::Con(
                b.map,
                vec![Type::Con(b.text, vec![]), Type::Con(sql_value, vec![])],
            )
        };
        // A quoted predicate `Quote (e -> Bool)`. The entity `e` is the queried
        // record at the call site (`fn (u: User) -> ...`); it is its own scheme
        // variable, free of the `Adapter a` constraint, and is pinned from the
        // predicate's parameter annotation when the lambda is captured. The
        // function shape (one parameter, `Bool` result) is what the quotation
        // checker reads to accept a `where`-style predicate.
        let quote_pred = |e: ridge_types::TyVid| {
            Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e)],
                    ret: Box::new(Type::Con(b.bool, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                }],
            )
        };
        // select :: ∀a e. a -> Text -> Quote (e -> Bool)
        //                      -> Result (List (Map Text SqlValue)) Error where Adapter a
        {
            let a = ctx.fresh_tyvid();
            let e = ctx.fresh_tyvid();
            let fn_ty = Type::Fn {
                params: vec![Type::Var(a), Type::Con(b.text, vec![]), quote_pred(e)],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![
                        Type::Con(b.list, vec![map_row()]),
                        Type::Con(b.error, vec![]),
                    ],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "select".to_owned(),
                Scheme {
                    vars: vec![a, e],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // get :: ∀a. a -> Text -> Text -> SqlValue
        //                 -> Result (Option (Map Text SqlValue)) Error where Adapter a
        {
            let a = ctx.fresh_tyvid();
            let fn_ty = Type::Fn {
                params: vec![
                    Type::Var(a),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.text, vec![]),
                    Type::Con(sql_value, vec![]),
                ],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![
                        Type::Con(b.option, vec![map_row()]),
                        Type::Con(b.error, vec![]),
                    ],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "get".to_owned(),
                Scheme {
                    vars: vec![a],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // delete :: ∀a e. a -> Text -> Quote (e -> Bool) -> Result Int Error where Adapter a
        {
            let a = ctx.fresh_tyvid();
            let e = ctx.fresh_tyvid();
            let fn_ty = Type::Fn {
                params: vec![Type::Var(a), Type::Con(b.text, vec![]), quote_pred(e)],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![Type::Con(b.int, vec![]), Type::Con(b.error, vec![])],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "delete".to_owned(),
                Scheme {
                    vars: vec![a, e],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // updateRows :: ∀a e. a -> Text -> Map Text SqlValue -> Quote (e -> Bool)
        //                  -> Result Int Error where Adapter a. The changes map
        //   carries the columns to set; the predicate selects the rows.
        {
            let a = ctx.fresh_tyvid();
            let e = ctx.fresh_tyvid();
            let fn_ty = Type::Fn {
                params: vec![
                    Type::Var(a),
                    Type::Con(b.text, vec![]),
                    map_row(),
                    quote_pred(e),
                ],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![Type::Con(b.int, vec![]), Type::Con(b.error, vec![])],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "updateRows".to_owned(),
                Scheme {
                    vars: vec![a, e],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // fetch :: ∀a e. a -> Text -> Quote (e -> Bool) -> List (Bool, Text)
        //                  -> Int -> Int
        //                  -> Result (List (Map Text SqlValue)) Error where Adapter a.
        // The order keys are `(ascending?, column)` pairs; the two Ints are the
        // limit (negative for none) and offset (non-positive for none).
        {
            let a = ctx.fresh_tyvid();
            let e = ctx.fresh_tyvid();
            let orders = Type::Con(
                b.list,
                vec![Type::Tuple(vec![
                    Type::Con(b.bool, vec![]),
                    Type::Con(b.text, vec![]),
                ])],
            );
            let fn_ty = Type::Fn {
                params: vec![
                    Type::Var(a),
                    Type::Con(b.text, vec![]),
                    quote_pred(e),
                    orders,
                    Type::Con(b.int, vec![]),
                    Type::Con(b.int, vec![]),
                ],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![
                        Type::Con(b.list, vec![map_row()]),
                        Type::Con(b.error, vec![]),
                    ],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "fetch".to_owned(),
                Scheme {
                    vars: vec![a, e],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // countWhere :: ∀a e. a -> Text -> Quote (e -> Bool)
        //                  -> Result Int Error where Adapter a
        {
            let a = ctx.fresh_tyvid();
            let e = ctx.fresh_tyvid();
            let fn_ty = Type::Fn {
                params: vec![Type::Var(a), Type::Con(b.text, vec![]), quote_pred(e)],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![Type::Con(b.int, vec![]), Type::Con(b.error, vec![])],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "countWhere".to_owned(),
                Scheme {
                    vars: vec![a, e],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // project :: ∀a e. a -> Text -> Quote (e -> Bool) -> List (Bool, Text)
        //                  -> Int -> Int -> List (Text, Text)
        //                  -> Result (List (Map Text SqlValue)) Error where Adapter a.
        // Like `fetch`, plus a `(alias, column)` select-list: each returned row
        // holds only those columns, keyed by alias.
        {
            let a = ctx.fresh_tyvid();
            let e = ctx.fresh_tyvid();
            let orders = Type::Con(
                b.list,
                vec![Type::Tuple(vec![
                    Type::Con(b.bool, vec![]),
                    Type::Con(b.text, vec![]),
                ])],
            );
            let cols = Type::Con(
                b.list,
                vec![Type::Tuple(vec![
                    Type::Con(b.text, vec![]),
                    Type::Con(b.text, vec![]),
                ])],
            );
            let fn_ty = Type::Fn {
                params: vec![
                    Type::Var(a),
                    Type::Con(b.text, vec![]),
                    quote_pred(e),
                    orders,
                    Type::Con(b.int, vec![]),
                    Type::Con(b.int, vec![]),
                    cols,
                ],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![
                        Type::Con(b.list, vec![map_row()]),
                        Type::Con(b.error, vec![]),
                    ],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "project".to_owned(),
                Scheme {
                    vars: vec![a, e],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // join :: ∀a c p. a -> Text -> Text -> Quote c -> Quote p
        //              -> List (Bool, Text) -> Int -> Int
        //              -> Result (List (Map Text SqlValue, Map Text SqlValue)) Error
        //              where Adapter a.
        // The inner join of two tables: `cond` is the quoted condition over both
        // entities (its left columns range over the left table, its right over
        // the right), `pred` the left-side filter, then ordering and paging. Each
        // result row is the (left, right) pair of column maps the terminal decodes
        // into both entities. The two quotes are phantom here — the seam only
        // walks their captured trees.
        {
            let a = ctx.fresh_tyvid();
            let c = ctx.fresh_tyvid();
            let p = ctx.fresh_tyvid();
            let orders = Type::Con(
                b.list,
                vec![Type::Tuple(vec![
                    Type::Con(b.bool, vec![]),
                    Type::Con(b.text, vec![]),
                ])],
            );
            let pair = Type::Tuple(vec![map_row(), map_row()]);
            let fn_ty = Type::Fn {
                params: vec![
                    Type::Var(a),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.quote, vec![Type::Var(c)]),
                    Type::Con(b.quote, vec![Type::Var(p)]),
                    orders,
                    Type::Con(b.int, vec![]),
                    Type::Con(b.int, vec![]),
                ],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![Type::Con(b.list, vec![pair]), Type::Con(b.error, vec![])],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "join".to_owned(),
                Scheme {
                    vars: vec![a, c, p],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // joinSelect :: ∀a c p r. a -> Text -> Text -> Quote c -> Quote p
        //              -> List (Bool, Text) -> Int -> Int -> Quote r
        //              -> Result (List (Map Text SqlValue)) Error where Adapter a.
        // Like `join`, plus a quoted projection `r` over both entities: each
        // result row is a single map keyed by the projection's output aliases,
        // which the terminal decodes into the named result record.
        {
            let a = ctx.fresh_tyvid();
            let c = ctx.fresh_tyvid();
            let p = ctx.fresh_tyvid();
            let r = ctx.fresh_tyvid();
            let orders = Type::Con(
                b.list,
                vec![Type::Tuple(vec![
                    Type::Con(b.bool, vec![]),
                    Type::Con(b.text, vec![]),
                ])],
            );
            let fn_ty = Type::Fn {
                params: vec![
                    Type::Var(a),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.quote, vec![Type::Var(c)]),
                    Type::Con(b.quote, vec![Type::Var(p)]),
                    orders,
                    Type::Con(b.int, vec![]),
                    Type::Con(b.int, vec![]),
                    Type::Con(b.quote, vec![Type::Var(r)]),
                ],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![
                        Type::Con(b.list, vec![map_row()]),
                        Type::Con(b.error, vec![]),
                    ],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "joinSelect".to_owned(),
                Scheme {
                    vars: vec![a, c, p, r],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // leftJoin :: ∀a c p. a -> Text -> Text -> Quote c -> Quote p
        //              -> List (Bool, Text) -> Int -> Int
        //              -> Result (List (Map Text SqlValue, Option (Map Text SqlValue))) Error
        //              where Adapter a.
        // The left-outer form of `join`: same condition, predicate, ordering, and
        // paging, but each result row keeps the left map and reports the right as
        // `Some` of its column map when the join matched or `None` when the left
        // row had no match. The two quotes are phantom — the seam walks their
        // captured trees.
        {
            let a = ctx.fresh_tyvid();
            let c = ctx.fresh_tyvid();
            let p = ctx.fresh_tyvid();
            let orders = Type::Con(
                b.list,
                vec![Type::Tuple(vec![
                    Type::Con(b.bool, vec![]),
                    Type::Con(b.text, vec![]),
                ])],
            );
            let pair = Type::Tuple(vec![map_row(), Type::Con(b.option, vec![map_row()])]);
            let fn_ty = Type::Fn {
                params: vec![
                    Type::Var(a),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.quote, vec![Type::Var(c)]),
                    Type::Con(b.quote, vec![Type::Var(p)]),
                    orders,
                    Type::Con(b.int, vec![]),
                    Type::Con(b.int, vec![]),
                ],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![Type::Con(b.list, vec![pair]), Type::Con(b.error, vec![])],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "leftJoin".to_owned(),
                Scheme {
                    vars: vec![a, c, p],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
        // leftJoinSelect :: ∀a c p r. a -> Text -> Text -> Quote c -> Quote p
        //              -> List (Bool, Text) -> Int -> Int -> Quote r
        //              -> Result (List (Map Text SqlValue)) Error where Adapter a.
        // The left-outer form of `joinSelect`: same projection select-list, but a
        // `LEFT JOIN` keeps every left row and the right-side columns come back
        // NULL where the row had no match, decoding to `None` in the projected
        // shape's `Option` fields. The three quotes are phantom — the seam walks
        // their captured trees.
        {
            let a = ctx.fresh_tyvid();
            let c = ctx.fresh_tyvid();
            let p = ctx.fresh_tyvid();
            let r = ctx.fresh_tyvid();
            let orders = Type::Con(
                b.list,
                vec![Type::Tuple(vec![
                    Type::Con(b.bool, vec![]),
                    Type::Con(b.text, vec![]),
                ])],
            );
            let fn_ty = Type::Fn {
                params: vec![
                    Type::Var(a),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.text, vec![]),
                    Type::Con(b.quote, vec![Type::Var(c)]),
                    Type::Con(b.quote, vec![Type::Var(p)]),
                    orders,
                    Type::Con(b.int, vec![]),
                    Type::Con(b.int, vec![]),
                    Type::Con(b.quote, vec![Type::Var(r)]),
                ],
                ret: Box::new(Type::Con(
                    b.result,
                    vec![
                        Type::Con(b.list, vec![map_row()]),
                        Type::Con(b.error, vec![]),
                    ],
                )),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            };
            ctx.env.bind(
                "leftJoinSelect".to_owned(),
                Scheme {
                    vars: vec![a, c, p, r],
                    cap_vars: vec![],
                    row_vars: vec![],
                    ty: fn_ty,
                    constraints: vec![Constraint::single(adapter, a)],
                },
            );
        }
    }
}

/// Constraint signature of a reconciled stdlib function — its class constraints
/// plus the scheme's parameter types.
///
/// The lowering pass reads this to thread instance dictionaries: a call to a
/// constrained stdlib function (e.g. `Repo.all`, whose scheme carries `Adapter
/// a, Row e`) must prepend the resolved dicts, the same as a constrained local
/// fn. Returns `None` when `(module, name)` is not a reconciled stdlib function
/// or carries no function type.
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "callers always pass the workspace's FxHashMap; generalising over the hasher adds noise for no caller benefit"
)]
pub fn reconciled_fn_dict_sig(
    module: &str,
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
    classes: &crate::class_env::ClassTable,
) -> Option<(
    Vec<ridge_types::Constraint>,
    Vec<ridge_types::Type>,
    ridge_types::Type,
)> {
    let scheme =
        crate::stdlib_types::reconciled_fn_scheme(module, name, reconciled, b, Some(classes))?;
    let ridge_types::Type::Fn { params, ret, .. } = scheme.ty else {
        return None;
    };
    Some((scheme.constraints, params, *ret))
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_resolve::{discover_workspace, resolve_workspace};
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        fs::write(full, content).expect("write file");
    }

    fn typecheck_snippet(src: &str) -> TypecheckResult {
        let td = TempDir::new().expect("tempdir");
        write_file(
            td.path(),
            "ridge.toml",
            "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
        );
        write_file(
            td.path(),
            "apps/demo/ridge.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
        );
        write_file(td.path(), "apps/demo/src/main.ridge", src);

        let disc = discover_workspace(td.path());
        let ws = disc.graph.expect("graph");
        let resolved = resolve_workspace(ws);
        typecheck_workspace(&resolved)
    }

    /// T5-1: verify that after typechecking, `inferred_caps` uses real `NodeIds`
    /// that are reachable via the module's `node_id_map`.
    ///
    /// The test creates a fn with a capability-bearing call (Io.println) and
    /// checks that at least one key in `inferred_caps` matches the real `NodeId`
    /// returned by `node_id_map.get(body_span, body_kind)` where `body_kind` is
    /// Block/Try/Expr depending on the body's expression shape.
    // OQ-PHASE45-005: span-keyed lookup; verify real NodeId is in inferred_caps.
    #[test]
    fn t5_inferred_caps_uses_real_node_id() {
        use ridge_ast::{Body, Expr as AstExpr, Item};
        use ridge_resolve::NodeKind;

        // Syntax: capability annotation precedes fn name per Ridge grammar.
        // `fn io main` declares `main` with the `io` cap; import provides Io alias.
        // Type errors are acceptable (stdlib may not fully resolve in test env) —
        // we only need the fn to be parsed and to have its inferred_caps populated.
        let src = "import std.io as Io\nfn io main () =\n  Io.println \"hello\"\n";
        let result = typecheck_snippet(src);

        let mut found = false;
        for module in &result.typed.modules {
            if module.inferred_caps.is_empty() {
                continue;
            }
            // Re-run assign_node_ids on the AST to get the expected NodeIdMap.
            let (node_id_map, _) = ridge_resolve::assign_node_ids(&module.ast);

            // For each top-level fn, verify that its inferred_caps entry uses
            // the real NodeId keyed by the body's span + NodeKind.
            // This mirrors the keying logic in typecheck_module_inner Step D.
            for item in &module.ast.items {
                if let Item::Fn(f) = item {
                    // Body::Ffi has no expression span — skip.
                    let expr = match &f.body {
                        Body::Expr(e) => e,
                        Body::Ffi { .. } => continue,
                    };
                    let (body_span, body_kind) = match expr {
                        AstExpr::Block(b) => (b.span, NodeKind::Block),
                        AstExpr::Try { span, .. } => (*span, NodeKind::Try),
                        other => (other.span(), NodeKind::Expr),
                    };
                    if let Some(nid) = node_id_map.get(body_span, body_kind) {
                        if module.inferred_caps.contains_key(&nid) {
                            found = true; // real NodeId found in inferred_caps
                        }
                    }
                }
            }
        }
        assert!(
            found,
            "expected real NodeId (from node_id_map) in inferred_caps, but none matched"
        );
    }

    // ── Class method invocation typecheck tests ────────────────────────────────

    /// Calling a class method on a concrete receiver infers the correct return type
    /// with no T030 (ambiguous constraint) error.
    #[test]
    fn class_method_concrete_call_infers_ret_no_t030() {
        let src = r#"
class Describe a =
    describe (x: a) -> Text

type Color = Red | Green | Blue

fn colorDesc (c: Color) -> Text =
    match c
        Red   -> "red"
        Green -> "green"
        Blue  -> "blue"

instance Describe Color =
    describe (x: Color) -> Text = colorDesc x

pub fn test_call () -> Text =
    describe Red
"#;
        let result = typecheck_snippet(src);
        // No typecheck errors should fire (especially not T030).
        let t030_count = result
            .errors
            .iter()
            .filter(|e| e.1.code() == "T030")
            .count();
        assert_eq!(
            t030_count, 0,
            "T030 must not fire for a concrete method call; errors: {:?}",
            result.errors
        );
        let all_errors: Vec<_> = result
            .errors
            .iter()
            .filter(|e| e.1.code() != "T023")
            .collect();
        assert!(
            all_errors.is_empty(),
            "no typecheck errors expected for concrete method call; errors: {all_errors:?}"
        );
    }

    /// `fn announce (x: a) -> Text = describe x` (NO explicit `where` clause)
    /// must typecheck and the inferred scheme must carry the implicit constraint.
    #[test]
    fn class_method_implicit_constraint_acquisition() {
        let src = r#"
class Describe a =
    describe (x: a) -> Text

type Color = Red | Green | Blue

fn colorDesc (c: Color) -> Text =
    match c
        Red   -> "red"
        Green -> "green"
        Blue  -> "blue"

instance Describe Color =
    describe (x: Color) -> Text = colorDesc x

fn announce (x: a) -> Text =
    describe x

pub fn test_call () -> Text =
    announce Red
"#;
        let result = typecheck_snippet(src);
        // No T030 and no other fatal errors (implicit constraint should be retained).
        let t030_count = result
            .errors
            .iter()
            .filter(|e| e.1.code() == "T030")
            .count();
        assert_eq!(
            t030_count, 0,
            "T030 must not fire for implicit constraint acquisition; errors: {:?}",
            result.errors
        );
        // The announce fn's scheme should carry a Describe constraint.
        let has_constrained_announce = result
            .typed
            .modules
            .iter()
            .any(|m| m.schemes.values().any(|s| !s.constraints.is_empty()));
        assert!(
            has_constrained_announce,
            "expected `announce` to have a constraint in its scheme; modules: {:?}",
            result
                .typed
                .modules
                .iter()
                .map(|m| &m.schemes)
                .collect::<Vec<_>>()
        );
    }
}
