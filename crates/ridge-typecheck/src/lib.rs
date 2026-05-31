//! Ridge type inference, capability checking, and exhaustiveness analysis.
//!
//! # Entry points
//!
//! - [`typecheck_workspace`] — type-check an entire [`ResolvedWorkspace`].
//! - [`typecheck_module`] — type-check a single module against an already-
//!   typed workspace (LSP hot-path).
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
pub mod ctx;
pub mod derive;
pub mod error;
pub mod exhaustiveness;
pub mod infer;
pub mod instantiate;
pub mod interp;
pub mod pipe_propagate;
pub mod prelude;
pub mod records;
pub mod render;
pub mod scc;
pub mod solve;
pub mod stdlib_env;
pub mod stdlib_signatures;
pub mod tycon_collect;
pub mod unify;
pub mod unions;

pub use class_env::{
    register_prelude_classes, register_prelude_instances, ClassTable, InstanceEnv, InstanceInfo,
    InstanceOrigin,
};
pub use collect::{collect_workspace, CollectResult};
pub use derive::{derive_instances, DerivedInstance, DerivedMethodBody};
pub use error::TypeError;
pub use render::{emit_internal, emit_internal_strict};
pub use ridge_resolve::Severity;
pub use ridge_types::BuiltinTyCons;
pub use solve::{DictPlan, DictResolution};

// Re-export witness types from ridge_types — the canonical definitions live there.
pub use ridge_types::{MatchWitness, WitnessKind, WitnessPat};

use ridge_ast::Item;
use ridge_resolve::{build_module_graph, ModuleId, NodeId, ResolvedWorkspace};
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
/// 2. Re-parse each module's source via `build_module_graph` to obtain ASTs.
/// 3. For each module (in topological order from `ws.graph.deps`):
///    a. Collect user `TyCons` from `TypeDecl` / `ActorDecl` nodes.
///    b. Seed the env with prelude + stdlib qualified bindings.
///    c. Run SCC-based Algorithm W over top-level `fn` decls.
///    d. Run capability checking over each `fn` decl.
///    e. Run `check_actor_encapsulation` for each `actor` decl.
/// 4. Accumulate all diagnostics; return them alongside the typed workspace.
#[must_use]
pub fn typecheck_workspace(ws: &ResolvedWorkspace) -> TypecheckResult {
    let mut all_errors: Vec<(ModuleId, TypeError)> = Vec::new();

    // Step 1: Shared TyCon arena + built-in registration.
    let mut arena = TyConArena::new();
    let b = BuiltinTyCons::allocate(&mut arena);

    // Step 2: Re-parse all modules to obtain their ASTs.
    let module_graph = build_module_graph(&ws.graph);

    let mut typed_modules: Vec<TypedModule> = Vec::with_capacity(ws.modules.len());
    // Merged anonymous record table across all modules.
    let mut workspace_anon_records: AnonRecordTable = AnonRecordTable::default();

    // Step 2b: Pre-collect user TyCon names from ALL modules to build a
    // name → TyConId map for the collect pass. This lets the collect pass
    // resolve user-defined instance head types (e.g. `instance ToText Color`
    // → `TyConId` for `Color`) without needing the full TyConArena.
    //
    // We predict the TyConIds by scanning the AST names in source order.
    // Each TypeDecl and ActorDecl allocates exactly one ID in the arena
    // (in the order they appear across modules). The arena currently holds
    // only the built-in TyCons, so the next ID is `arena.all().len()`.
    // We replicate the collect_user_tycons pass-1 ID assignment here.
    let mut workspace_tycon_names: FxHashMap<String, TyConId> = FxHashMap::default();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "arena size is bounded by program size; exceeding 2^32 TyCons is not realistic"
    )]
    let mut next_id = arena.all().len() as u32;
    for pm in &module_graph.modules {
        for item in &pm.ast.items {
            let name = match item {
                Item::Type(td) => Some(td.name.text.clone()),
                Item::Actor(ad) => Some(ad.name.text.clone()),
                _ => None,
            };
            if let Some(n) = name {
                // Only record if not already present (same name declared in
                // multiple modules — take the first occurrence).
                workspace_tycon_names.entry(n).or_insert_with(|| {
                    let id = TyConId(next_id);
                    next_id += 1;
                    id
                });
            }
        }
    }

    // Run the workspace collect pass to build the class/instance registries.
    // This runs over all module ASTs before any module is type-checked so the
    // solver sees every instance.
    let module_ast_pairs: Vec<(u32, &ridge_ast::Module)> = module_graph
        .modules
        .iter()
        .map(|pm| (pm.id.0, pm.ast.as_ref()))
        .collect();
    let collect_result = collect_workspace(&module_ast_pairs, &workspace_tycon_names);
    // Coherence errors are workspace-level; accumulate them tagged with the
    // module they originated in (use ModuleId(0) as a fallback — coherence
    // errors carry their own span, so the module tag is informational only).
    for err in collect_result.errors {
        all_errors.push((ModuleId(0), err));
    }
    let class_table = collect_result.class_table;
    let instance_env = collect_result.instance_env;

    // Step 3: Type-check each module.
    for rm in &ws.modules {
        // Find the corresponding parsed module (by ModuleId).
        let pm_opt = module_graph.modules.iter().find(|pm| pm.id == rm.id);
        // If we couldn't find the parsed module (e.g. I/O error), produce
        // an empty typed module and continue.
        let ast = if let Some(pm) = pm_opt {
            Arc::clone(&pm.ast)
        } else {
            typed_modules.push(TypedModule {
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
            });
            continue;
        };

        let result = typecheck_module_inner(
            rm.id,
            &ast,
            &rm.imports,
            &mut arena,
            &b,
            Some(&class_table),
            Some(&instance_env),
        );
        all_errors.extend(result.errors.into_iter().map(|e| (rm.id, e)));
        // Merge this module's anon_records (last-write wins; same shapes share
        // the same TyConId workspace-wide because the arena is shared).
        workspace_anon_records.extend(result.anon_records);
        typed_modules.push(result.typed);
    }

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
        },
        errors: all_errors,
    }
}

/// Type-check a single module against an already-typechecked workspace.
///
/// The caller supplies a [`TypedWorkspace`] from a prior
/// [`typecheck_workspace`] call.  This is the LSP hot-path: re-check one
/// module without re-processing the entire workspace.
#[must_use]
pub fn typecheck_module(
    module_id: ModuleId,
    ws: &ResolvedWorkspace,
    typed_ws: &TypedWorkspace,
) -> ModuleTypecheckResult {
    // Find the resolved module entry.
    let Some(rm) = ws.modules.iter().find(|m| m.id == module_id) else {
        return ModuleTypecheckResult {
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
            },
            errors: Vec::new(),
            anon_records: AnonRecordTable::default(),
        };
    };

    // Re-parse this module's source to obtain an AST.
    let module_graph = build_module_graph(&ws.graph);
    let Some(pm) = module_graph.modules.iter().find(|pm| pm.id == module_id) else {
        return ModuleTypecheckResult {
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
            },
            errors: Vec::new(),
            anon_records: AnonRecordTable::default(),
        };
    };

    // Clone the arena from the typed workspace so we can add this module's
    // user TyCons without invalidating other modules' TyConIds.
    // (For the LSP path, we share builtins + other-module TyCons.)
    let mut arena = TyConArena::new();
    // Re-register all TyCons from the typed workspace (builtins + other modules).
    for decl in &typed_ws.tycons {
        arena.intern(decl.clone());
    }
    let b = &typed_ws.builtins;

    let ast = Arc::clone(&pm.ast);

    typecheck_module_inner(module_id, &ast, &rm.imports, &mut arena, b, None, None)
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
                let name = match param {
                    ridge_ast::Param::Bare(id) => id.text.clone(),
                    ridge_ast::Param::Annotated { name, .. } => name.text.clone(),
                };
                ctx.env.bind(name, monoscheme(ty.clone()));
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
/// [`typecheck_module`].
///
/// `class_table` and `instance_env` supply the workspace-level class/instance
/// registries produced by [`crate::collect::collect_workspace`]. When `None`,
/// empty registries are used and the constraint solver is a no-op (the pre-
/// typeclass behavior for the LSP hot-path and unit tests).
fn typecheck_module_inner(
    id: ModuleId,
    ast: &Arc<ridge_ast::Module>,
    imports: &[ridge_resolve::ImportResolution],
    arena: &mut TyConArena,
    b: &BuiltinTyCons,
    class_table: Option<&crate::class_env::ClassTable>,
    instance_env: Option<&crate::class_env::InstanceEnv>,
) -> ModuleTypecheckResult {
    use crate::actor::{check_actor_encapsulation, check_actor_mailbox_config};
    use crate::ctx::InferCtx;
    use crate::scc::typecheck_module_decls;
    use crate::stdlib_env::seed_stdlib_env;
    use crate::tycon_collect::{collect_user_tycons, prescan_inline_records};
    use ridge_resolve::assign_node_ids;

    // Phase 4.5 T2/T3: build the NodeIdMap for this module so that infer_expr
    // can write back per-expression types. R999 collisions from node stamping
    // are non-fatal — we accumulate them but continue inference.
    // OQ-PHASE45-005: span-keyed lookup; no NodeId fields added to FnDecl/ConstDecl.
    let (node_id_map, _nid_errors) = assign_node_ids(ast);

    let mut ctx = InferCtx::new();
    // Attach the NodeIdMap to the context so infer_expr can write back types.
    ctx.node_id_map = Some(node_id_map);

    // Push the module-level env frame.
    ctx.env.push_frame();

    // Step A: Collect user TyCons and seed env with constructor schemes.
    let tycon_result = collect_user_tycons(ast, id, arena, b, &mut ctx);
    // Populate the user_tycon_names map for ast_type_to_type resolution.
    ctx.user_tycon_names = tycon_result.user_tycon_names;
    // Snapshot all TyConDecls (builtins + user) for record/union inference.
    ctx.tycon_decls = arena.all().to_vec();

    // Step A2: Pre-scan inline record types and intern anonymous TyCons.
    // Must run AFTER pass-1 (names stable) and alias-chain resolution, BEFORE
    // inference begins so that ast_type_to_ridge_type can look up shapes.
    let anon_table = prescan_inline_records(&[ast], arena, b, &mut ctx);
    ctx.anon_records = anon_table;
    // Re-snapshot after anon TyCon interning so ctx.tycon_decls includes them.
    ctx.tycon_decls = arena.all().to_vec();

    // Step B: Seed env with prelude constructors + stdlib qualified bindings.
    seed_stdlib_env(&mut ctx, b, imports);

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
                ty,
                constraints: vec![],
            };
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
    let ct = class_table.unwrap_or(&scratch_class_table);
    let ie = instance_env.unwrap_or(&scratch_instance_env);
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
    let node_types = std::mem::take(&mut ctx.node_types_accum);

    // Phase 4.5 T5: inferred_caps is now keyed by real NodeIds (or proxy fallback).
    // The T17 proxy comment is removed; the sweep will update LowerCtx::lookup_inferred_caps.
    let typed = TypedModule {
        id,
        ast: Arc::clone(ast),
        node_types, // Phase 4.5 T3: populated via infer_expr write-back
        schemes,    // Phase 4.5 T4: populated by SCC generalise write-back
        inferred_caps,
        match_witnesses: FxHashMap::default(), // T17: populated by infer_expr
        dict_resolution, // populated by the constraint solver when classes are used
    };

    // Move the anon_records table out so the workspace driver can merge it.
    let anon_records = std::mem::take(&mut ctx.anon_records);

    ModuleTypecheckResult {
        typed,
        errors: ctx.errors,
        anon_records,
    }
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
}
