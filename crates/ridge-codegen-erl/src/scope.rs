//! `LocalScope`: SSA-suffix table for `var`-bound mutable locals.
//!
//! Ridge `var`-bound locals use SSA-suffix renaming to simulate mutation in
//! Core Erlang (which has no mutable variables).  Each `var n = 0` introduces
//! `V_N` (index 0); each subsequent `n <-` assignment increments the index and
//! introduces `V_N1`, `V_N2`, etc.
//!
//! The `LocalScope` struct tracks `name → current_ssa_index` for the body of a
//! single function.  It is cloned before lowering each `Match` arm so that
//! pattern-variable bindings in one arm do not bleed into another.

// Exercised from expr.rs (and indirectly item-level emission); until the top-level
// pipeline is wired, these are test-only.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness
#![allow(clippy::redundant_pub_crate)]

use crate::core_ast::CErlVar;
use ridge_resolve::ModuleId;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// SSA-suffix table for `var`-bound mutable locals within a function body.
///
/// Each `VarIn` binding introduces the variable at index 0 (emitted as the
/// bare mangled name, e.g. `V_N`).  Each `Assign { target: Local }` increments
/// the index and emits a fresh binding `V_N1`, `V_N2`, etc.
///
/// Scoping convention:
/// - Index 0 → bare mangled name, e.g. `V_Count` (no numeric suffix).
/// - Index ≥ 1 → mangled name + decimal suffix, e.g. `V_Count1`, `V_Count2`.
///
/// Conceptual versions such as `N0 / N1 / N2` start with the initial
/// `VarIn` binding at index 0; subsequent assigns increment from 1 onward.
///
/// # SSA naming rationale
/// The conceptual sequence `N0/N1/N2` identifies successive versions, but
/// the emitted name for index 0 is the *bare* name rather than `V_N0`.
/// Omitting the `0` suffix keeps Core Erlang output readable and matches
/// the un-suffixed naming of let-bound (non-var) locals.
///
/// The `fn_arity` field carries the module's function-arity table so that
/// `lower_symbol` can resolve `SymbolRef::Local` used as a value.
/// It is cheap to clone because it is reference-counted.
///
/// The `actor_parent` field, when `Some`, identifies the parent Ridge module and
/// its BEAM name so that `lower_static_call` can emit qualified inter-module calls
/// for `SymbolRef::Local { module: parent_id }` references from within actor
/// handler and init bodies.
///
/// The `letrec_locals` field tracks names that were registered into `fn_arity`
/// for a handler-local recursive lambda (emitted as `letrec`).  These must NOT
/// be routed through the parent-module qualified call path in B-6 — they are
/// resolved by the Core Erlang letrec scope, not by a cross-module call.
/// What a module-level name is, for the two questions emission asks of it.
///
/// The arity answers "how many arguments does a call pass". `is_const` answers
/// the one arity used to answer on its own: whether writing the name *is* its
/// value. A `const` is evaluated at every use-site; a `fn` is a reference until
/// something calls it. That was the same question while only a `const` could
/// have arity 0 — a zero-parameter function makes the two different, and the
/// number alone can no longer tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalShape {
    /// Parameters the name takes; `0` for a `const`.
    pub(crate) arity: u32,
    /// `true` for `const`, `false` for `fn` and `@ffi`.
    pub(crate) is_const: bool,
}

impl LocalShape {
    /// A function or `@ffi` stub of the given arity.
    pub(crate) const fn func(arity: u32) -> Self {
        Self {
            arity,
            is_const: false,
        }
    }

    /// A module-level `const`, which is always arity 0 and always evaluated.
    pub(crate) const fn constant() -> Self {
        Self {
            arity: 0,
            is_const: true,
        }
    }
}

/// One mutable binding's counter and creation order within its function.
#[derive(Debug, Clone, Copy)]
struct MutableVersion {
    next: u32,
    born: u64,
}

/// The value before a write, keyed by lexical mutable identity rather than name.
#[derive(Debug, Clone)]
struct ValueWrite {
    born: u64,
    before: CErlVar,
}

/// Copy-on-write journals let branch scopes share the active value boundary.
#[derive(Debug, Clone)]
struct ValueFrame {
    parent: Option<Arc<Self>>,
    born_before: u64,
    writes: FxHashMap<String, ValueWrite>,
    created: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalScope {
    /// `base_mangled_name → current_ssa_index`.
    table: FxHashMap<String, MutableVersion>,
    /// Creation order distinguishes enclosing bindings from value-local ones.
    next_binding: u64,
    /// Writes are recorded during emission, without rescanning initializer IR.
    value_frame: Option<Arc<ValueFrame>>,
    /// Source names rebound by a nested mutable declaration.
    mutable_aliases: FxHashMap<String, String>,
    /// Mutable locals transported by the nearest Result scope.
    pub(crate) result_scope_vars: Vec<crate::result_scope::ResultState>,
    /// Values evaluated by an actor callback may carry its implicit state.
    /// Fresh function/lambda scopes deliberately do not inherit this permission.
    pub(crate) actor_value_context: bool,
    /// Module-level fn/const table: `name → what it is`.
    /// Shared (Arc) so that clone (e.g. for match arms, lambda scopes) is cheap.
    pub(crate) fn_arity: Arc<FxHashMap<String, LocalShape>>,
    /// Workspace-wide codegen tables (beam names, record metadata, module
    /// FQNs, version history).
    ///
    /// Shared via `Arc`s inside so cloning a scope stays cheap. Empty in
    /// unit tests — call sites fall back to legacy behavior then.
    pub(crate) tables: CodegenTables,
    /// When lowering actor bodies: `(parent_module_id, parent_beam_name)`.
    ///
    /// Any `SymbolRef::Local { module: M }` where `M == parent_module_id` must be
    /// emitted as a qualified `call 'parent_beam_name':'name' (args…)` because actor
    /// modules are separate BEAM compilation units and cannot make unqualified calls
    /// into the parent module.  `None` in non-actor contexts.
    pub(crate) actor_parent: Option<(ModuleId, Arc<str>)>,
    /// Handler-local letrec function names.
    ///
    /// When a recursive lambda is detected inside an actor handler body, its name is
    /// temporarily registered in `fn_arity` so that self-references inside the lambda
    /// body emit `LocalFnRef` rather than `Var`.  That same name must NOT be treated
    /// as a parent-module function by the B-6 qualified-call routing — it is resolved
    /// by the surrounding `letrec` binding, not by a cross-module call.
    ///
    /// Entries are inserted immediately before lowering the letrec lambda/body and
    /// removed immediately after.  Shared (Arc) to keep clone cheap.
    pub(crate) letrec_locals: Arc<FxHashSet<String>>,
    /// The BEAM module name of the current compilation unit.
    ///
    /// Set when lowering module-level items (e.g. `IrFn`, `IrConst`).  Used by
    /// `lower_spawn` to derive the actor's BEAM module name as
    /// `"<own_module>_<actor_name_lc>"` — matching the convention used by `actor.rs`
    /// (`lower_actor` appends the actor name to the parent module's beam name).
    ///
    /// `None` in unit tests or contexts where the beam name is unknown.
    pub(crate) own_module_beam_name: Option<Arc<str>>,
    /// Current SSA index of the synthetic `__state` map inside an actor handler
    /// or init body.
    ///
    /// State-field assigns (`field <- expr`) lower to a `let V_State<n+1> = ...`
    /// chain. Without tracking the latest index in the scope, reads of the
    /// `__state` base local (lowered from any state-field ident inside the same
    /// handler) would always resolve to bare `V_State` and see the pre-mutation
    /// value. The actor body lowering bumps this in lockstep with the
    /// `state_idx` parameter; the `IrExpr::Local { name: "__state" }` lookup in
    /// `lower_expr` consults this field to emit `V_State<actor_state_idx>`.
    pub(crate) actor_state_idx: u32,
}

/// Workspace-wide tables shared by every scope in a compilation.
///
/// - `beam_names`: `ModuleId.0 → stable FQN-derived beam name`. A
///   `SymbolRef::External` call knows only its callee's `ModuleId`; this
///   table keeps cross-module calls resolving when modules are added,
///   removed, or reordered. Empty in unit tests — the external-call site
///   falls back to the legacy `module_<id>` segment.
/// - `record_meta`: `TyConId → RecordMeta` used to tag record values with
///   their type identity (`__ridge_v`). Empty in unit tests — no tags.
/// - `module_fqns`: `ModuleId.0 → dotted FQN`, needed to render field types
///   for the shared shape hash.
/// - `version_history`: previous-build record/actor shapes; codegen reads it
///   to derive structural migration edges. Empty on fresh builds.
#[derive(Debug, Clone, Default)]
pub(crate) struct CodegenTables {
    /// Workspace declaration shapes: functions are referenced; constants are loaded.
    pub(crate) external_shapes: Arc<FxHashMap<ModuleId, FxHashMap<String, LocalShape>>>,
    /// Stable beam module names, indexed by `ModuleId.0`.
    pub(crate) beam_names: std::sync::Arc<Vec<String>>,
    /// Record version metadata, keyed by type-constructor id.
    pub(crate) record_meta:
        std::sync::Arc<rustc_hash::FxHashMap<ridge_types::TyConId, crate::record_meta::RecordMeta>>,
    /// Fully-qualified module names, indexed by `ModuleId.0`.
    pub(crate) module_fqns: std::sync::Arc<Vec<String>>,
    /// Previous-build version history (empty on fresh builds).
    pub(crate) version_history: std::sync::Arc<ridge_types::history::VersionHistory>,
}

impl CodegenTables {
    /// Build the shared tables from a lowered workspace.
    ///
    /// `beam_names` comes straight from the workspace (seeded by the driver;
    /// empty in hand-built workspaces). `record_meta` is derived from the
    /// workspace's type-constructor declarations; it is empty when the
    /// workspace carries no tycons (unit tests), which disables `__ridge_v`
    /// tagging. `module_fqns` and `version_history` ride the same workspace
    /// fields the driver seeds.
    pub(crate) fn from_workspace(ws: &ridge_ir::LoweredWorkspace) -> Self {
        Self {
            external_shapes: Arc::new(crate::module::build_external_shapes(ws)),
            beam_names: Arc::new(ws.target_names.clone()),
            record_meta: Arc::new(crate::record_meta::build_record_meta(
                &ws.tycons,
                &ws.target_names,
                &ws.module_fqns,
            )),
            module_fqns: Arc::new(ws.module_fqns.clone()),
            version_history: Arc::new(ws.version_history.clone()),
        }
    }
}

impl LocalScope {
    /// Create a fresh empty scope with no var-bound names and an empty arity table.
    pub(crate) fn new() -> Self {
        Self {
            table: FxHashMap::default(),
            next_binding: 0,
            value_frame: None,
            mutable_aliases: FxHashMap::default(),
            result_scope_vars: Vec::new(),
            actor_value_context: false,
            fn_arity: Arc::new(FxHashMap::default()),
            actor_parent: None,
            letrec_locals: Arc::new(FxHashSet::default()),
            own_module_beam_name: None,
            actor_state_idx: 0,
            tables: CodegenTables::default(),
        }
    }

    /// Create a scope seeded with the given fn/const arity table.
    ///
    /// Used by item-level lowering so that `SymbolRef::Local` can resolve
    /// arity when a local fn or const is used as a value expression.
    pub(crate) fn with_arity(fn_arity: FxHashMap<String, LocalShape>) -> Self {
        Self {
            table: FxHashMap::default(),
            next_binding: 0,
            value_frame: None,
            mutable_aliases: FxHashMap::default(),
            result_scope_vars: Vec::new(),
            actor_value_context: false,
            fn_arity: Arc::new(fn_arity),
            actor_parent: None,
            letrec_locals: Arc::new(FxHashSet::default()),
            own_module_beam_name: None,
            actor_state_idx: 0,
            tables: CodegenTables::default(),
        }
    }

    /// Create a scope seeded with the given fn/const arity table and the module's BEAM name.
    ///
    /// Like `with_arity`, but also carries `own_module_beam_name` so that
    /// `lower_spawn` can derive actor BEAM module names via the same convention
    /// as `actor.rs` (appending `"_<actor_name_lc>"` to the parent beam name).
    pub(crate) fn with_arity_and_module(
        fn_arity: FxHashMap<String, LocalShape>,
        module_beam_name: &str,
    ) -> Self {
        Self {
            table: FxHashMap::default(),
            next_binding: 0,
            value_frame: None,
            mutable_aliases: FxHashMap::default(),
            result_scope_vars: Vec::new(),
            actor_value_context: false,
            fn_arity: Arc::new(fn_arity),
            actor_parent: None,
            letrec_locals: Arc::new(FxHashSet::default()),
            own_module_beam_name: Some(Arc::from(module_beam_name)),
            actor_state_idx: 0,
            tables: CodegenTables::default(),
        }
    }

    /// Create a scope that shares an already-Arc'd arity table.
    ///
    /// Used by lambda lowering to inherit the parent scope's arity table
    /// without cloning the underlying map.
    pub(crate) fn with_arity_arc(fn_arity: Arc<FxHashMap<String, LocalShape>>) -> Self {
        Self {
            table: FxHashMap::default(),
            next_binding: 0,
            value_frame: None,
            mutable_aliases: FxHashMap::default(),
            result_scope_vars: Vec::new(),
            actor_value_context: false,
            fn_arity,
            actor_parent: None,
            letrec_locals: Arc::new(FxHashSet::default()),
            own_module_beam_name: None,
            actor_state_idx: 0,
            tables: CodegenTables::default(),
        }
    }

    /// Create a scope for actor body lowering.
    ///
    /// Carries the parent module's `ModuleId` and BEAM name so that
    /// `lower_static_call` can detect cross-module `SymbolRef::Local` references
    /// and emit qualified `call 'parent':'fn' (args…)` instead of the unqualified
    /// `apply 'fn'/arity (args…)` form which would fail at BEAM load time.
    pub(crate) fn with_actor_parent(
        fn_arity: FxHashMap<String, LocalShape>,
        parent_module_id: ModuleId,
        parent_beam_name: &str,
    ) -> Self {
        Self {
            table: FxHashMap::default(),
            next_binding: 0,
            value_frame: None,
            mutable_aliases: FxHashMap::default(),
            result_scope_vars: Vec::new(),
            actor_value_context: false,
            fn_arity: Arc::new(fn_arity),
            actor_parent: Some((parent_module_id, Arc::from(parent_beam_name))),
            letrec_locals: Arc::new(FxHashSet::default()),
            // Carry the parent module's BEAM name so that `IrExpr::Spawn`
            // lowered inside an actor handler derives its target via the
            // canonical `"<parent>_<actor_lc>"` shape (e.g.
            // `ridge_module_0_worker`) instead of the test-only fallback
            // `ridge_actor_<id>_<name>` placeholder which produces a
            // runtime `undefined function ridge_actor_*:init/1` crash.
            own_module_beam_name: Some(Arc::from(parent_beam_name)),
            actor_state_idx: 0,
            tables: CodegenTables::default(),
        }
    }

    /// Allocate a binding distinct from every version in an inner scope.
    pub(crate) fn join_index(&mut self, name: &str, inner: &Self) -> u32 {
        let next = inner.table.get(name).map_or(0, |v| v.next);
        let entry = self.version(name.to_owned());
        entry.next = entry.next.max(next);
        let index = entry.next;
        entry.next += 1;
        index
    }

    /// Resolve a source name to its lexical mutable binding.
    pub(crate) fn mutable_key<'a>(&'a self, name: &'a str) -> &'a str {
        self.mutable_aliases.get(name).map_or(name, String::as_str)
    }

    /// BEAM variable base for one lexical binding, distinct for shadowed locals.
    pub(crate) fn mutable_base(key: &str) -> String {
        key.strip_prefix('#').map_or_else(
            || crate::expr::name_to_erl_var(key),
            |id| format!("MutableShadow{id}"),
        )
    }

    /// Read a lexical binding without resolving a later shadow of its name.
    pub(crate) fn mutable_value(&self, key: &str) -> CErlVar {
        ssa_var(
            &Self::mutable_base(key),
            self.table.get(key).map_or(0, |v| v.next.saturating_sub(1)),
        )
    }

    /// Introduce a mutable binding, giving shadows their own identity.
    pub(crate) fn enter_mutable(&mut self, name: &str, id: u32) -> Option<String> {
        let previous = self.mutable_aliases.get(name).cloned();
        if self.current_index(name).is_some() {
            self.mutable_aliases
                .insert(name.to_owned(), format!("#{id}"));
        }
        self.bump(name);
        previous
    }

    /// Restore name lookup after the declaration's continuation has been lowered.
    pub(crate) fn leave_mutable(&mut self, name: &str, previous: Option<String>) {
        if let Some(key) = previous {
            self.mutable_aliases.insert(name.to_owned(), key);
        } else {
            self.mutable_aliases.remove(name);
        }
    }

    /// Capture the current versions by value; parameters start new bindings.
    pub(crate) fn inherit_mutable_captures(&mut self, outer: &Self, params: &[ridge_ir::IrParam]) {
        self.table.clone_from(&outer.table);
        self.next_binding = outer.next_binding;
        self.mutable_aliases.clone_from(&outer.mutable_aliases);
        for param in params {
            self.mutable_aliases.remove(&param.name);
            self.table.remove(&param.name);
        }
    }

    /// Introduce or increment the SSA index for `name`.
    ///
    /// - First call: inserts at index `0` and returns `0`.
    /// - Subsequent calls: increments and returns the new index.
    pub(crate) fn bump(&mut self, name: &str) -> u32 {
        let key = self.mutable_key(name).to_owned();
        self.bump_lexical(&key)
    }

    /// Allocate a version for an identity already resolved before shadowing.
    pub(crate) fn bump_lexical(&mut self, key: &str) -> u32 {
        let entry = self.version(key.to_owned());
        // Store the next index to emit: declaration 0, first assignment 1, ...
        let idx = entry.next;
        entry.next = idx + 1;
        idx
    }

    /// Return the current SSA index for `name`, or `None` if the name is not
    /// tracked (i.e. it is a let-bound or param local, not a `var`).
    pub(crate) fn current_index(&self, name: &str) -> Option<u32> {
        self.table.get(self.mutable_key(name)).map(|version| {
            // `next` is the *next* index to be returned by `bump`.
            // `current` = next - 1 (the last index that was returned).
            version.next.saturating_sub(1)
        })
    }

    fn version(&mut self, key: String) -> &mut MutableVersion {
        self.table.entry(key).or_insert_with_key(|key| {
            if let Some(frame) = &mut self.value_frame {
                Arc::make_mut(frame).created.push(key.clone());
            }
            let born = self.next_binding;
            self.next_binding += 1;
            MutableVersion { next: 0, born }
        })
    }

    pub(crate) fn has_tracked_locals(&self) -> bool {
        !self.table.is_empty()
    }

    pub(crate) fn begin_value(&mut self) {
        self.value_frame = Some(Arc::new(ValueFrame {
            parent: self.value_frame.take(),
            born_before: self.next_binding,
            writes: FxHashMap::default(),
            created: Vec::new(),
        }));
    }

    /// Record source assignment sites, never compiler temporary counters.
    pub(crate) fn record_assignment(&mut self, name: &str) {
        if self.value_frame.is_none() {
            return;
        }
        let key = self.mutable_key(name).to_owned();
        self.record_assignment_key(&key);
    }

    pub(crate) fn record_assignment_key(&mut self, key: &str) {
        let Some(frame) = &self.value_frame else {
            return;
        };
        if let Some(version) = self.table.get(key) {
            if version.born >= frame.born_before || frame.writes.contains_key(key) {
                return;
            }
            self.import_write(
                key,
                ValueWrite {
                    born: version.born,
                    before: self.mutable_value(key),
                },
            );
        }
    }

    fn import_write(&mut self, key: &str, write: ValueWrite) {
        let Some(frame) = &mut self.value_frame else {
            return;
        };
        if write.born < frame.born_before && !frame.writes.contains_key(key) {
            Arc::make_mut(frame).writes.insert(key.to_owned(), write);
        }
    }

    /// Import possible branch writes; their values are packed inside each arm.
    pub(crate) fn merge_value_writes(&mut self, branch: &Self) {
        let Some(frame) = &branch.value_frame else {
            return;
        };
        for (key, write) in &frame.writes {
            if self.table.get(key).is_some_and(|v| v.born == write.born) {
                self.import_write(key, write.clone());
            }
        }
    }

    pub(crate) fn end_value(&mut self) -> Option<Vec<(String, CErlVar)>> {
        let frame = Arc::unwrap_or_clone(self.value_frame.take()?);
        self.value_frame = frame.parent;
        for key in frame.created {
            self.table.remove(&key);
        }
        for (key, write) in &frame.writes {
            self.import_write(key, write.clone());
        }
        let mut incoming: Vec<_> = frame
            .writes
            .iter()
            .map(|(key, write)| (key.clone(), write.before.clone()))
            .collect();
        incoming.sort_by(|a, b| a.0.cmp(&b.0));
        Some(incoming)
    }
}

/// Construct the Core Erlang variable name for a `var`-bound local.
///
/// - `idx == 0` → bare mangled name (e.g. `V_Count`).
/// - `idx >= 1` → mangled name + decimal suffix (e.g. `V_Count1`, `V_Count2`).
///
/// `base_mangled` is the result of [`crate::expr::name_to_erl_var`].
///
/// # SSA suffix scheme
/// Index 0 emits the bare name (no `0` literal suffix) to keep the initial
/// binding readable. Index 1, 2, … append the decimal digit directly.
/// The conceptual `N0/N1/N2` sequence starts at the initial binding;
/// the emitted name simply omits the zero suffix.
pub(crate) fn ssa_var(base_mangled: &str, idx: u32) -> CErlVar {
    if idx == 0 {
        CErlVar(base_mangled.to_owned())
    } else {
        CErlVar(format!("{base_mangled}{idx}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_bump_first_returns_zero() {
        let mut scope = LocalScope::new();
        assert_eq!(scope.bump("n"), 0, "first bump should return index 0");
        assert_eq!(scope.bump("n"), 1, "second bump should return index 1");
        assert_eq!(scope.bump("n"), 2, "third bump should return index 2");
    }

    #[test]
    fn scope_current_index_after_bump() {
        let mut scope = LocalScope::new();
        scope.bump("n"); // introduces at 0
        assert_eq!(scope.current_index("n"), Some(0));
        scope.bump("n"); // re-assigns to 1
        assert_eq!(scope.current_index("n"), Some(1));
    }

    #[test]
    fn scope_current_index_absent_name() {
        let scope = LocalScope::new();
        assert_eq!(scope.current_index("x"), None);
    }

    #[test]
    fn scope_isolated_clone() {
        let mut original = LocalScope::new();
        original.bump("n"); // n → index 0

        let mut cloned = original.clone();
        cloned.bump("n"); // cloned: n → index 1

        // original should still be at index 0
        assert_eq!(original.current_index("n"), Some(0));
        // cloned should be at index 1
        assert_eq!(cloned.current_index("n"), Some(1));
    }

    #[test]
    fn ssa_var_format() {
        // Index 0 → bare mangled name (no numeric suffix).
        assert_eq!(ssa_var("V_N", 0).0, "V_N");
        // Index 1 → suffix 1.
        assert_eq!(ssa_var("V_N", 1).0, "V_N1");
        // Index 5 → suffix 5.
        assert_eq!(ssa_var("V_Count", 5).0, "V_Count5");
    }
}
