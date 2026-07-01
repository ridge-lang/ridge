//! §3.12 — `LocalScope`: SSA-suffix table for `var`-bound mutable locals.
//!
//! Ridge `var`-bound locals use SSA-suffix renaming to simulate mutation in
//! Core Erlang (which has no mutable variables).  Each `var n = 0` introduces
//! `V_N` (index 0); each subsequent `n <-` assignment increments the index and
//! introduces `V_N1`, `V_N2`, etc.
//!
//! The `LocalScope` struct tracks `name → current_ssa_index` for the body of a
//! single function.  It is cloned before lowering each `Match` arm so that
//! pattern-variable bindings in one arm do not bleed into another.

// Exercised from expr.rs (and indirectly T8); until T8 wires the top-level
// pipeline these are test-only.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
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
/// Scoping convention (§3.12 + §4.7):
/// - Index 0 → bare mangled name, e.g. `V_Count` (no numeric suffix).
/// - Index ≥ 1 → mangled name + decimal suffix, e.g. `V_Count1`, `V_Count2`.
///
/// This matches the §3.12 illustrative example (`N0 / N1 / N2`) in spirit while
/// matching §4.7's "the `0` suffix is the SSA index" semantics: index 0 is the
/// initial `VarIn` binding, subsequent assigns increment from 1 onward.
///
/// # OQ annotation
/// The choice to emit the *bare* name for index 0 (rather than `V_N0`) resolves
/// the tension between §3.12 (`N0/N1/N2` illustrative) and §4.7 ("0 suffix is
/// the SSA index").  We choose bare-for-zero because it produces the most
/// readable Core Erlang output and is consistent with §4.2's un-suffixed naming
/// for let-bound (non-var) locals.
///
/// The `fn_arity` field carries the module's function-arity table so that
/// `lower_symbol` can resolve `SymbolRef::Local` used as a value (T8 wiring).
/// It is cheap to clone because it is reference-counted.
///
/// The `actor_parent` field, when `Some`, identifies the parent Ridge module and
/// its BEAM name so that `lower_static_call` can emit qualified inter-module calls
/// for `SymbolRef::Local { module: parent_id }` references from within actor
/// handler and init bodies (B-6 fix, Phase 6 pass 3).
///
/// The `letrec_locals` field tracks names that were registered into `fn_arity`
/// for a handler-local recursive lambda (emitted as `letrec`).  These must NOT
/// be routed through the parent-module qualified call path in B-6 — they are
/// resolved by the Core Erlang letrec scope, not by a cross-module call.
#[derive(Debug, Clone)]
pub(crate) struct LocalScope {
    /// `base_mangled_name → current_ssa_index`.
    table: FxHashMap<String, u32>,
    /// Module-level fn/const arity table: `name → arity`.
    /// Shared (Arc) so that clone (e.g. for match arms, lambda scopes) is cheap.
    pub(crate) fn_arity: Arc<FxHashMap<String, u32>>,
    /// Workspace-wide arity table for symbols in *other* modules:
    /// `module_id → (name → arity)`.
    ///
    /// A `SymbolRef::External` call knows only its callee's module id and name,
    /// not its arity, so it cannot tell whether a trailing `()` is a real
    /// `Unit` argument or the punctuation of a zero-parameter call. This table
    /// supplies the callee's arity across the module boundary, letting the
    /// cross-module call apply the same unit-paren shim the local path uses.
    /// Shared (Arc) so cloning a scope stays cheap.
    pub(crate) external_arity: Arc<FxHashMap<ModuleId, FxHashMap<String, u32>>>,
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

impl LocalScope {
    /// Create a fresh empty scope with no var-bound names and an empty arity table.
    pub(crate) fn new() -> Self {
        Self {
            table: FxHashMap::default(),
            fn_arity: Arc::new(FxHashMap::default()),
            actor_parent: None,
            letrec_locals: Arc::new(FxHashSet::default()),
            own_module_beam_name: None,
            actor_state_idx: 0,
            external_arity: Arc::new(FxHashMap::default()),
        }
    }

    /// Create a scope seeded with the given fn/const arity table.
    ///
    /// Used by item-level lowering (T8) so that `SymbolRef::Local` can resolve
    /// arity when a local fn or const is used as a value expression.
    pub(crate) fn with_arity(fn_arity: FxHashMap<String, u32>) -> Self {
        Self {
            table: FxHashMap::default(),
            fn_arity: Arc::new(fn_arity),
            actor_parent: None,
            letrec_locals: Arc::new(FxHashSet::default()),
            own_module_beam_name: None,
            actor_state_idx: 0,
            external_arity: Arc::new(FxHashMap::default()),
        }
    }

    /// Create a scope seeded with the given fn/const arity table and the module's BEAM name.
    ///
    /// Like `with_arity`, but also carries `own_module_beam_name` so that
    /// `lower_spawn` can derive actor BEAM module names via the same convention
    /// as `actor.rs` (appending `"_<actor_name_lc>"` to the parent beam name).
    pub(crate) fn with_arity_and_module(
        fn_arity: FxHashMap<String, u32>,
        module_beam_name: &str,
    ) -> Self {
        Self {
            table: FxHashMap::default(),
            fn_arity: Arc::new(fn_arity),
            actor_parent: None,
            letrec_locals: Arc::new(FxHashSet::default()),
            own_module_beam_name: Some(Arc::from(module_beam_name)),
            actor_state_idx: 0,
            external_arity: Arc::new(FxHashMap::default()),
        }
    }

    /// Create a scope that shares an already-Arc'd arity table.
    ///
    /// Used by lambda lowering to inherit the parent scope's arity table
    /// without cloning the underlying map.
    pub(crate) fn with_arity_arc(fn_arity: Arc<FxHashMap<String, u32>>) -> Self {
        Self {
            table: FxHashMap::default(),
            fn_arity,
            actor_parent: None,
            letrec_locals: Arc::new(FxHashSet::default()),
            own_module_beam_name: None,
            actor_state_idx: 0,
            external_arity: Arc::new(FxHashMap::default()),
        }
    }

    /// Create a scope for actor body lowering (B-6 fix, Phase 6 pass 3).
    ///
    /// Carries the parent module's `ModuleId` and BEAM name so that
    /// `lower_static_call` can detect cross-module `SymbolRef::Local` references
    /// and emit qualified `call 'parent':'fn' (args…)` instead of the unqualified
    /// `apply 'fn'/arity (args…)` form which would fail at BEAM load time.
    pub(crate) fn with_actor_parent(
        fn_arity: FxHashMap<String, u32>,
        parent_module_id: ModuleId,
        parent_beam_name: &str,
    ) -> Self {
        Self {
            table: FxHashMap::default(),
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
            external_arity: Arc::new(FxHashMap::default()),
        }
    }

    /// Introduce or increment the SSA index for `name`.
    ///
    /// - First call: inserts at index `0` and returns `0`.
    /// - Subsequent calls: increments and returns the new index.
    pub(crate) fn bump(&mut self, name: &str) -> u32 {
        let entry = self.table.entry(name.to_owned()).or_insert(0);
        // For the first bump (introducing the VarIn binding), index is 0.
        // For re-assignments, the stored value is already the previous index;
        // increment it before returning.
        //
        // Wait — we need to think carefully:
        //   1st call (VarIn introduces the name): returns 0.
        //   2nd call (1st Assign): returns 1.
        //   3rd call (2nd Assign): returns 2.
        //
        // `or_insert(0)` sets to 0 if absent.  On first call we want to return 0
        // without incrementing.  On subsequent calls we increment first.
        //
        // We encode this by using a sentinel: store u32::MAX as "not yet bumped".
        // That adds complexity.  Simpler: store "next index to return".
        //
        // Actually the simplest correct approach:
        //   - `entry` starts at 0 on first insertion.
        //   - Return current value, then increment the stored value for next call.
        //     → first call returns 0, second returns 1, etc.
        let idx = *entry;
        *entry = idx + 1;
        idx
    }

    /// Return the current SSA index for `name`, or `None` if the name is not
    /// tracked (i.e. it is a let-bound or param local, not a `var`).
    pub(crate) fn current_index(&self, name: &str) -> Option<u32> {
        self.table.get(name).map(|next| {
            // `next` is the *next* index to be returned by `bump`.
            // `current` = next - 1 (the last index that was returned).
            next.saturating_sub(1)
        })
    }
}

/// Construct the Core Erlang variable name for a `var`-bound local.
///
/// - `idx == 0` → bare mangled name (e.g. `V_Count`).
/// - `idx >= 1` → mangled name + decimal suffix (e.g. `V_Count1`, `V_Count2`).
///
/// `base_mangled` is the result of [`crate::expr::name_to_erl_var`].
///
/// # SSA suffix scheme (§3.12 + §4.7)
/// Index 0 emits the bare name (no `0` literal suffix) to keep the initial
/// binding readable.  Index 1, 2, … append the decimal digit directly.  This
/// reconciles §3.12's illustrative `N0/N1/N2` with §4.7's "the `0` suffix is
/// the SSA index" statement: both refer to the same scheme with index 0 being
/// the initial binding.
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
