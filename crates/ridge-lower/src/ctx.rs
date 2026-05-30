//! Per-module lowering context (`LowerCtx`).
//!
//! [`LowerCtx`] is the mutable accumulator threaded through every lowering rule
//! in Phase 5.  It owns:
//! - the fresh-ID counter for IR nodes,
//! - the AST→IR provenance map (`source_map`),
//! - the propagation-scope stack for `?`/`try` desugaring (§4.2–§4.3),
//! - the `in_actor_body` flag for `Assign` target classification (§4.14),
//! - the fresh-local counter for synthetic name generation (R6),
//! - the accumulated [`LowerError`] vector.
//!
//! Lowering rules invoke `LowerCtx::fresh_id` and the scope-stack helpers
//! as each rule module is implemented.

use crate::error::LowerError;
use ridge_ast::{Item, Span};
use ridge_ir::{IrNodeId, LoweredModule};
use ridge_resolve::{BindingMap, ModuleId, NodeId, NodeIdMap, SymbolId, SymbolTable};
use ridge_typecheck::TypedWorkspace;
use ridge_types::{CapabilitySet, ShapeKey, TyConId, Type};
use rustc_hash::{FxHashMap, FxHashSet};

/// Name-to-`TyConId` cache, built lazily from the workspace on first lookup.
type TyConNameCache = FxHashMap<String, TyConId>;

/// Actor-name–to–`ModuleId` cache, built lazily on first call to
/// [`LowerCtx::lookup_actor_module`]. Keyed by the actor's source-level name;
/// on same-name-in-two-modules collisions the lowest `ModuleId.0` wins
/// (deterministic by module-walk order). // OQ-PHASE45-006
type ActorModuleCache = FxHashMap<String, ModuleId>;

/// Per-module state threaded through all Phase 5 lowering rules.
///
/// One `LowerCtx` is created per module, lives for the duration of
/// `lower_module`, and is consumed (or dropped) when the [`LoweredModule`] is
/// returned.
pub struct LowerCtx<'tw> {
    /// The stable index of the module being lowered.
    pub module_id: ModuleId,
    /// Monotone counter for allocating dense `IrNodeId`s (starts at 0).
    pub ir_node_id_counter: u32,
    /// Borrowed slice of the upstream `TypedModule.node_types` table.
    ///
    /// Indexed by `NodeId.0`; `None` for positions that carry no type
    /// (e.g. non-expression AST positions).
    pub node_types: &'tw [Option<Type>],
    /// AST-`NodeId` → IR-`IrNodeId` provenance map.
    ///
    /// Accumulated as IR nodes are emitted via [`fresh_id`][Self::fresh_id].
    /// Sparse — synthetic nodes (e.g. interpolation `ToText` calls) have no
    /// upstream `NodeId` and are absent (§3.7).
    pub source_map: FxHashMap<IrNodeId, NodeId>,
    /// Stack of expected return types for `?` / `try` propagation desugaring.
    ///
    /// Pushed when entering an `Option`/`Result`-returning scope; popped on
    /// exit.  An empty stack when a `?` operator is encountered triggers a
    /// defensive [`LowerError::PropagateOutsideScope`] (L003) (§4.2/§4.3).
    pub propagation_scope_stack: Vec<Type>,
    /// `true` when the lowerer is inside an actor handler or `init` body.
    ///
    /// Flips the `Assign` target classification to `StateField` vs. `Local`
    /// (R8 / §4.14).
    pub in_actor_body: bool,
    /// Names of the enclosing actor's state fields, when `in_actor_body == true`.
    ///
    /// `None` outside an actor handler/init body; `Some(set)` while lowering the
    /// body of `init` or an `on` handler.  Used by `lower_assign` to classify
    /// `<-` targets as `AssignTarget::StateField` when the name appears in this
    /// set (R8 / §4.14 / T10).
    ///
    /// A save/restore pattern in `actor_lower` ensures nested actors (disallowed
    /// by Phase 4, but defensively handled) do not corrupt the enclosing state.
    pub current_state_fields: Option<FxHashSet<String>>,
    /// Monotone counter for generating unique synthetic local names (R6).
    ///
    /// Shared across all prefixes within a module so that `__prop_ok_0`,
    /// `__with_base_1`, `__prop_ok_2` are all globally unique within a module.
    pub fresh_local_counter: u32,
    /// Defensive errors accumulated during lowering (§5.1).
    ///
    /// Non-empty only when the upstream `TypedWorkspace` was partial or
    /// contained unsolved type variables.  All variants have `Severity::Error`.
    pub errors: Vec<LowerError>,
    /// Span-keyed `NodeId` map reconstructed from the module AST.  Used by
    /// [`crate::core`] to look up bindings for `Ident` and `QualifiedName`
    /// nodes via `(span, kind) → NodeId`.
    ///
    /// `None` for `LowerCtx`s constructed without an AST (e.g. unit tests
    /// that pass no `ResolvedModule`).
    pub node_id_map: Option<NodeIdMap>,
    /// Binding side-table from the upstream `ResolvedModule`, indexed by
    /// `NodeId.0`.
    ///
    /// `None` for `LowerCtx`s constructed without a `ResolvedModule`.
    pub binding_map: Option<&'tw BindingMap>,
    /// Workspace-level context from `TypedWorkspace`.
    ///
    /// Carries `tycons` (the `TyConDecl` arena) and `builtins` (built-in
    /// `TyConId` shortcuts).  Required for `with` schema lookup (§4.5) and
    /// interp `ToText` dispatch (§4.6).  `None` for unit tests that do not wire
    /// the full pipeline.
    pub workspace: Option<&'tw TypedWorkspace>,
    /// The current module's `inferred_caps` side-table from Phase 4.
    ///
    /// Keyed by the proxy `NodeId(span.start)` that `ridge-typecheck` uses for
    /// each top-level `fn` declaration.  See [`LowerCtx::lookup_inferred_caps`]
    /// for the proxy-key contract.  `None` for unit tests that do not run the
    /// full pipeline.
    pub inferred_caps: Option<&'tw FxHashMap<NodeId, CapabilitySet>>,
    /// Lazy name→`TyConId` cache populated on first call to
    /// [`LowerCtx::lookup_tycon_by_name`].
    tycon_name_cache: Option<TyConNameCache>,
    /// Lazy actor-name→`ModuleId` cache populated on first call to
    /// [`LowerCtx::lookup_actor_module`]. Built from
    /// `TypedWorkspace.modules[*].ast.items` (one linear scan). // OQ-PHASE45-006
    actor_module_cache: Option<ActorModuleCache>,
    /// Per-module symbol table from the upstream `ResolvedModule`, borrowed
    /// for the duration of `lower_module`.
    ///
    /// Used by [`LowerCtx::lookup_constructor_tycon`] to translate a
    /// `SymbolId` (resolve-layer) to the owning type's source name, which is
    /// then resolved to a `TyConId` via [`LowerCtx::lookup_tycon_by_name`].
    ///
    /// `None` for `LowerCtx`s constructed without a `ResolvedModule` (e.g.
    /// unit tests that pass no `ResolvedModule`). // OQ-PHASE45-007
    pub symbol_table: Option<&'tw SymbolTable>,
}

impl<'tw> LowerCtx<'tw> {
    /// Construct a fresh `LowerCtx` for `module_id`.
    ///
    /// `node_types` is borrowed from `TypedModule.node_types` for the lifetime
    /// `'tw`; all counters start at zero and all collections are empty.
    ///
    /// This constructor does NOT wire the `BindingMap` / `NodeIdMap` needed for
    /// `Ident`/`Qualified` lowering.  Use `attach_bindings` when
    /// a `ResolvedModule` is available.
    #[must_use]
    pub fn new(module_id: ModuleId, node_types: &'tw [Option<Type>]) -> Self {
        Self {
            module_id,
            ir_node_id_counter: 0,
            node_types,
            source_map: FxHashMap::default(),
            propagation_scope_stack: Vec::new(),
            in_actor_body: false,
            current_state_fields: None,
            fresh_local_counter: 0,
            errors: Vec::new(),
            node_id_map: None,
            binding_map: None,
            workspace: None,
            inferred_caps: None,
            tycon_name_cache: None,
            actor_module_cache: None,
            symbol_table: None,
        }
    }

    /// Attach the binding side-tables produced by the resolve pass.
    ///
    /// `node_id_map` is the `(Span, NodeKind) → NodeId` index reconstructed
    /// from the module AST; `binding_map` is the `BindingMap` from the
    /// corresponding `ResolvedModule`.  Both are required to lower `Ident` and
    /// `QualifiedName` atoms.
    pub fn attach_bindings(&mut self, node_id_map: NodeIdMap, binding_map: &'tw BindingMap) {
        self.node_id_map = Some(node_id_map);
        self.binding_map = Some(binding_map);
    }

    /// Allocate the next [`IrNodeId`] and record the AST→IR provenance link.
    ///
    /// `origin` is the upstream AST [`NodeId`] that produced this IR node.
    /// Pass `None` for purely synthetic nodes (e.g. the `ToText` call inserted
    /// by interpolation lowering) — synthetic nodes are **not** entered into
    /// `source_map` (§3.7 sparse-map contract).
    ///
    /// IDs are dense and start at 0 so that `IrNodeId.0` can be used directly
    /// as a `Vec` index into `node_types`.
    pub fn fresh_id(&mut self, origin: Option<NodeId>) -> IrNodeId {
        let id = IrNodeId(self.ir_node_id_counter);
        self.ir_node_id_counter += 1;
        if let Some(nid) = origin {
            self.source_map.insert(id, nid);
        }
        id
    }

    /// Generate a fresh `__prefix_N` synthetic local name (R6 mitigation).
    ///
    /// The counter `N` is shared across all prefixes within this module so
    /// that every generated name is globally unique within the lowered module.
    /// For example, calling `fresh_local("__prop_ok")` twice followed by
    /// `fresh_local("__with_base")` produces `"__prop_ok_0"`, `"__prop_ok_1"`,
    /// `"__with_base_2"`.
    pub fn fresh_local(&mut self, prefix: &str) -> String {
        let n = self.fresh_local_counter;
        self.fresh_local_counter += 1;
        format!("{prefix}_{n}")
    }

    /// Attach the workspace reference for `with` schema and `ToText` dispatch.
    ///
    /// Called from [`crate::lower_module`] once the `TypedWorkspace` is
    /// available.  `None` is accepted as a defensive no-op for tests that do
    /// not run the full pipeline.
    pub const fn attach_workspace(&mut self, ws: &'tw TypedWorkspace) {
        self.workspace = Some(ws);
    }

    /// Attach the current module's `inferred_caps` side-table.
    ///
    /// Called from [`crate::lower_module`] immediately after the `TypedModule`
    /// is available.  Used by [`Self::lookup_inferred_caps`] to read Phase 4's
    /// capability inference results for top-level `fn` declarations.
    pub const fn attach_inferred_caps(&mut self, caps: &'tw FxHashMap<NodeId, CapabilitySet>) {
        self.inferred_caps = Some(caps);
    }

    /// Looks up Phase 4's `inferred_caps` side-table by the proxy `NodeId`
    /// derivation that `ridge-typecheck` uses (`NodeId(span.start)`). This
    /// proxy contract is fragile and shared with upstream — if the upstream
    /// keying changes, this helper must change in lockstep.
    ///
    /// Falls back to `CapabilitySet::PURE` when:
    /// - The `inferred_caps` table was not attached (test scaffolding), or
    /// - The proxy key has no entry (upstream keyed only top-level `fn` decls).
    ///
    /// # Proxy contract
    ///
    /// `ridge-typecheck` inserts `NodeId(f.span.start)` for each top-level
    /// `FnDecl` (see `crates/ridge-typecheck/src/lib.rs`, step D).  Handler and
    /// init caps are stored in the `ActorSchema` inside the `TyConArena`, not in
    /// `inferred_caps`.  Lambda caps have no upstream entry at all.
    ///
    /// Call sites pass `decl.span` (the whole declaration span); both real
    /// `NodeId` and proxy `NodeId(span.start)` keys are dual-inserted by the
    /// resolve pass, so the proxy key is the correct primary lookup.
    #[must_use]
    pub fn lookup_inferred_caps(&self, decl_span: Span) -> CapabilitySet {
        let Some(caps_map) = self.inferred_caps else {
            return CapabilitySet::PURE;
        };
        let proxy_nid = NodeId(decl_span.start);
        caps_map
            .get(&proxy_nid)
            .copied()
            .unwrap_or(CapabilitySet::PURE)
    }

    /// Looks up a `TyConId` by name from the workspace's tycon list.
    ///
    /// On the first call the lookup builds a name→`TyConId` cache from
    /// `workspace.tycons` (single linear scan), stored in `self.tycon_name_cache`
    /// for subsequent O(1) queries.  If no workspace is attached, or no matching
    /// tycon is found, returns `None`.
    ///
    /// The fallback at each call site is `TyConId(0)` (which is `Int` for the
    /// built-in arena), documented at each use so that snapshot output makes the
    /// miss visible.
    #[must_use]
    pub fn lookup_tycon_by_name(&mut self, name: &str) -> Option<TyConId> {
        let ws = self.workspace?;
        // Build the cache on first use.
        if self.tycon_name_cache.is_none() {
            let cache: TyConNameCache = ws
                .tycons
                .iter()
                .enumerate()
                .map(|(i, decl)| {
                    // Safety: workspace tycon count is bounded by program size; u32 is sufficient.
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "tycon count bounded by program size; exceeding 2^32 is not realistic"
                    )]
                    (decl.name.clone(), TyConId(i as u32))
                })
                .collect();
            self.tycon_name_cache = Some(cache);
        }
        self.tycon_name_cache
            .as_ref()
            .and_then(|c| c.get(name).copied())
    }

    /// Look up the [`TyConId`] for an anonymous record type by its structural shape.
    ///
    /// Reads the frozen `TypedWorkspace::anon_records` table populated by the
    /// typecheck pre-scan.  Returns `None` when no workspace is attached (unit
    /// tests) or when the shape has no entry (agreement-failure sentinel: the
    /// typecheck and lower canonicalizers disagree — should not happen in
    /// correct programs).
    #[must_use]
    pub fn lookup_anon_by_shape(&self, key: &ShapeKey) -> Option<TyConId> {
        self.workspace?.anon_records.get(key).copied()
    }

    /// Look up the `ModuleId` of an actor by its source-level name.
    ///
    /// On the first call this builds an `actor_name → ModuleId` cache by
    /// scanning every module's `ast.items` in `ModuleId.0` order (a single
    /// linear workspace-wide pass). Subsequent calls are O(1).
    ///
    /// **Collision policy** (OQ-PHASE45-006): when two modules declare an
    /// actor with the same name the *lower* `ModuleId.0` wins (first
    /// encountered in the scan). The `BindingMap` lookup at the call site
    /// (which already carries the disambiguated module) is always preferred
    /// over this bare-name fallback.
    ///
    /// Returns `None` if the workspace is not attached, or if no actor with
    /// the given name exists in any module.
    #[must_use]
    pub fn lookup_actor_module(&mut self, actor_name: &str) -> Option<ModuleId> {
        let ws = self.workspace?;
        // Build the cache on first use.
        if self.actor_module_cache.is_none() {
            let mut cache: ActorModuleCache = FxHashMap::default();
            for tmod in &ws.modules {
                let mod_id = tmod.id;
                for item in &tmod.ast.items {
                    if let Item::Actor(decl) = item {
                        // First-encountered wins (lowest ModuleId.0 — modules
                        // are walked in ModuleId.0 order by construction).
                        cache.entry(decl.name.text.clone()).or_insert(mod_id);
                    }
                }
            }
            self.actor_module_cache = Some(cache);
        }
        self.actor_module_cache
            .as_ref()
            .and_then(|c| c.get(actor_name).copied())
    }

    /// Attach the per-module symbol table from the upstream `ResolvedModule`.
    ///
    /// Called from [`crate::lower_module`] when a `ResolvedModule` is
    /// available.  Used by [`Self::lookup_constructor_tycon`] to translate
    /// a resolve-layer `SymbolId` to its owner type's source name.
    pub const fn attach_symbol_table(&mut self, table: &'tw SymbolTable) {
        self.symbol_table = Some(table);
    }

    /// Translate a resolve-layer `SymbolId` (constructor owner) to its IR-layer
    /// `TyConId` via the owner type's source name.
    ///
    /// **Path**: `symbol_table.entries[owner_type.0].name` → owner type's
    /// source name → [`LowerCtx::lookup_tycon_by_name`] → `TyConId`.
    ///
    /// Returns `None` on any failure (missing symbol table, out-of-bounds
    /// `SymbolId`, or no matching tycon name). Callers fall back to
    /// `TyConId(0)` exactly as today — no behavioural regression. // OQ-PHASE45-007
    #[must_use]
    pub fn lookup_constructor_tycon(&mut self, owner_type: SymbolId) -> Option<TyConId> {
        let table = self.symbol_table?;
        let entry = table.entries.get(owner_type.0 as usize)?;
        let owner_name = entry.name.clone();
        self.lookup_tycon_by_name(&owner_name)
    }

    /// Look up the type assigned to a `NodeId` in the upstream `node_types`
    /// side-table.
    ///
    /// Returns `None` if the table is shorter than `id.0` (which is the case
    /// during T17-deferred lowering where the table is always empty).
    #[must_use]
    pub fn node_type(&self, id: NodeId) -> Option<&Type> {
        self.node_types.get(id.0 as usize).and_then(Option::as_ref)
    }

    /// Push `ty` onto the propagation-scope stack.
    ///
    /// Called when entering any `Option`- or `Result`-returning scope where
    /// `?` desugaring is valid (§4.2).
    pub fn push_propagation_scope(&mut self, ty: Type) {
        self.propagation_scope_stack.push(ty);
    }

    /// Pop the top propagation scope.
    ///
    /// Returns `Some(ty)` if a scope was active, or `None` if the stack was
    /// already empty.  The caller is responsible for emitting
    /// [`LowerError::PropagateOutsideScope`] (L003) when `None` is returned.
    pub fn pop_propagation_scope(&mut self) -> Option<Type> {
        self.propagation_scope_stack.pop()
    }

    /// Peek at the current expected return type without popping.
    ///
    /// Returns `None` if the stack is empty (no enclosing `Option`/`Result`
    /// scope).
    #[must_use]
    pub fn current_propagation_scope(&self) -> Option<&Type> {
        self.propagation_scope_stack.last()
    }

    /// Consume the context and return the accumulated `LoweredModule`.
    ///
    /// Returns an empty shell.  Callers should populate `items` before
    /// calling this method or use `finish_with_items`.
    #[must_use]
    pub fn finish(self) -> LoweredModule {
        LoweredModule::empty(self.module_id, self.node_types.len())
    }

    /// Consume the context and return a populated [`LoweredModule`].
    ///
    /// Used by `lower_module` once the item-walking driver has accumulated
    /// the full `items` vector.  The `node_types` vector is grown to at
    /// least `node_types.len()` entries (all `None`) — this preserves
    /// index-parity with the upstream `TypedModule.node_types` table.
    #[must_use]
    pub fn finish_with_items(self, items: Vec<ridge_ir::IrItem>) -> LoweredModule {
        let node_type_capacity = self.node_types.len();
        let source_map = self.source_map;
        // Allocate an all-None type side-table sized to match the upstream
        // TypedModule.node_types length (index-parity invariant).
        let ir_node_types: Vec<Option<ridge_types::Type>> = vec![None; node_type_capacity];
        LoweredModule::new(self.module_id, items, ir_node_types, source_map)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── §3.1 actor_module_cache unit tests ───────────────────────────────────

    // B-ctx-1: Cache is empty (None) on a freshly constructed LowerCtx.
    //
    // Verifies that `actor_module_cache` is not built eagerly on construction.
    // Before any call to `lookup_actor_module`, the cache must be `None`.
    #[test]
    fn actor_module_cache_empty_initially() {
        let ctx = LowerCtx::new(ModuleId(0), &[]);
        assert!(
            ctx.actor_module_cache.is_none(),
            "actor_module_cache must be None before first lookup"
        );
    }

    // B-ctx-2: lookup_actor_module returns None when no workspace is attached.
    //
    // Without a workspace the cache cannot be built; the method must return
    // None immediately (never panic). After the call the cache stays None
    // because there is nothing to scan.
    #[test]
    fn actor_module_lookup_none_without_workspace() {
        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        let result = ctx.lookup_actor_module("Counter");
        assert!(
            result.is_none(),
            "lookup must return None when no workspace is attached"
        );
        // The cache remains None (nothing was built — no workspace to scan).
        assert!(
            ctx.actor_module_cache.is_none(),
            "cache stays None when workspace absent"
        );
    }

    // B-ctx-3: lookup_constructor_tycon returns None when symbol_table is absent.
    //
    // Defensive fallback: without the symbol table the method returns None so
    // callers fall back to TyConId(0).
    #[test]
    fn lookup_constructor_tycon_none_without_symbol_table() {
        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        let result = ctx.lookup_constructor_tycon(SymbolId(0));
        assert!(
            result.is_none(),
            "lookup_constructor_tycon must return None when symbol_table is absent"
        );
    }

    // B-ctx-4: lookup_constructor_tycon returns None for out-of-range SymbolId.
    //
    // Even when the symbol table is attached, an out-of-range SymbolId must
    // return None rather than panic.
    #[test]
    fn lookup_constructor_tycon_none_for_out_of_range_symbol_id() {
        // Build a minimal SymbolTable with zero entries.
        let table = SymbolTable::empty(ModuleId(0));
        let table_ref = Box::leak(Box::new(table));

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_symbol_table(table_ref);

        // SymbolId(99) is out of range for an empty table.
        let result = ctx.lookup_constructor_tycon(SymbolId(99));
        assert!(
            result.is_none(),
            "out-of-range SymbolId must return None, not panic"
        );
    }
}
