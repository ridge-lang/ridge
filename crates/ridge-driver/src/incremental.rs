//! Incremental recompilation engine.
//!
//! Keeps a resolved + typed workspace in sync across single-file edits without
//! re-running the whole pipeline. An edit re-resolves and re-type-checks the
//! edited module; when the edit changes the module's exported surface, the
//! transitive set of modules that import it (its reverse-dependency closure) is
//! recomputed too. Everything else is served from the cache.
//!
//! The result is identical to a full rebuild — same diagnostics, same per-node
//! types — because every recomputed module goes through the same
//! `resolve_module_incremental` / `typecheck_module_incremental` primitives a
//! full build uses, and modules outside the closure cannot depend on the edit.
//!
//! This engine is LSP-only. The `ridge build` / `ridge check` CLI keeps the
//! full deterministic pipeline, which is also the oracle the incremental path
//! is tested against.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rustc_hash::{FxHashSet, FxHasher};

use ridge_resolve::{
    resolve_module_incremental, ModuleId, ResolveError, ResolvedVisibility, ResolvedWorkspace,
    SymbolEntry, SymbolKind, SymbolTable,
};
use ridge_typecheck::{typecheck_module_incremental, TypeError, TypecheckResult, TypedWorkspace};

/// A resolved + typed workspace plus the bookkeeping needed to recompute it
/// incrementally.
#[derive(Debug)]
pub struct IncrementalState {
    /// The resolved workspace (symbols, imports, bindings, dependency edges).
    pub resolved: ResolvedWorkspace,
    /// The typed workspace (per-module `node_types`, schemes, the `TyCon` arena).
    pub typed: TypedWorkspace,
    /// Type errors per module — [`TypedWorkspace`] does not retain them.
    pub type_errors: Vec<(ModuleId, TypeError)>,
    /// Discovery-phase resolve errors (e.g. `R023`), which an in-file edit never
    /// changes. Retained so the full diagnostic set can be reproduced.
    pub disc_resolve_errors: Vec<ResolveError>,
    /// Exported-surface hash per module, indexed by `ModuleId.0`.
    surface_hashes: Vec<u64>,
}

impl IncrementalState {
    /// Seed the cache from a full resolve + type-check.
    #[must_use]
    pub fn new(
        resolved: ResolvedWorkspace,
        typecheck: TypecheckResult,
        disc_resolve_errors: Vec<ResolveError>,
    ) -> Self {
        let surface_hashes = resolved
            .modules
            .iter()
            .map(|m| surface_hash(&m.symbols))
            .collect();
        Self {
            resolved,
            typed: typecheck.typed,
            type_errors: typecheck.errors,
            disc_resolve_errors,
            surface_hashes,
        }
    }

    /// Apply an edit to one module's source and recompute everything it affects.
    ///
    /// Re-parses and re-resolves the edited module, then — only if its exported
    /// surface changed — re-resolves the transitive set of modules that import
    /// it. Every module in that set is then re-type-checked. The caches are
    /// updated in place; the returned vector is the set of modules that were
    /// recomputed (the edited module first), so a caller can republish exactly
    /// those modules' diagnostics.
    pub fn recompile(&mut self, edited_id: ModuleId, new_source: &str) -> Vec<ModuleId> {
        let n = self.resolved.modules.len();
        if edited_id.0 as usize >= n {
            return Vec::new();
        }

        // Parse the edited source; refresh its lex/parse diagnostics.
        let parsed = ridge_parser::parse_source(new_source);
        let ast = Arc::new(parsed.module);
        self.resolved.parse_errors.retain(|(m, _)| *m != edited_id);
        self.resolved.lex_errors.retain(|(m, _)| *m != edited_id);
        for e in parsed.errors {
            self.resolved.parse_errors.push((edited_id, e));
        }
        for e in parsed.lex_errors {
            self.resolved.lex_errors.push((edited_id, e));
        }

        // Re-resolve the edited module against the full workspace context.
        let old_hash = self.surface_hashes[edited_id.0 as usize];
        let mut fresh_resolve_errors: Vec<(ModuleId, ResolveError)> =
            resolve_module_incremental(&mut self.resolved, edited_id, &ast, true)
                .into_iter()
                .map(|e| (edited_id, e))
                .collect();
        let new_hash = surface_hash(&self.resolved.modules[edited_id.0 as usize].symbols);
        self.surface_hashes[edited_id.0 as usize] = new_hash;

        // Decide what to recompute: just the edited module for a surface-
        // preserving edit, otherwise the edited module plus everything that
        // transitively imports it.
        let recompute: Vec<ModuleId> = if new_hash == old_hash {
            vec![edited_id]
        } else {
            reverse_dep_closure(&self.resolved.graph.deps, edited_id)
        };

        // Re-resolve every dependent (the edited module is already done).
        for &dep in recompute.iter().skip(1) {
            let dep_ast = Arc::clone(&self.resolved.module_asts[dep.0 as usize]);
            let errs = resolve_module_incremental(&mut self.resolved, dep, &dep_ast, true);
            fresh_resolve_errors.extend(errs.into_iter().map(|e| (dep, e)));
            self.surface_hashes[dep.0 as usize] =
                surface_hash(&self.resolved.modules[dep.0 as usize].symbols);
        }

        // Swap the recomputed modules' resolve diagnostics into the cache.
        let in_set: FxHashSet<ModuleId> = recompute.iter().copied().collect();
        self.resolved.errors.retain(|(m, _)| !in_set.contains(m));
        self.resolved.errors.extend(fresh_resolve_errors);

        // Re-type-check the recomputed set, threading the growing TyCon list
        // through so each module sees the previous ones' interned TyCons.
        let mut fresh_type_errors: Vec<(ModuleId, TypeError)> = Vec::new();
        for &m in &recompute {
            let inc = typecheck_module_incremental(m, &self.resolved, &self.typed);
            self.typed.tycons = inc.tycons;
            self.typed.anon_records.extend(inc.result.anon_records);
            fresh_type_errors.extend(inc.result.errors.into_iter().map(|e| (m, e)));
            self.typed.modules[m.0 as usize] = inc.result.typed;
        }
        self.type_errors.retain(|(m, _)| !in_set.contains(m));
        self.type_errors.extend(fresh_type_errors);

        recompute
    }
}

/// Hash a module's exported surface — the declarations another module could
/// import. A body-only edit leaves this unchanged (it touches no declaration
/// name, kind, arity, capability set, or visibility); adding, removing, or
/// changing an importable declaration changes it.
///
/// File-private (`_`-prefixed) symbols are excluded: no other module can see
/// them, so they can never affect a dependent. The hash is span-free, so an edit
/// that merely shifts a later declaration's position does not perturb it.
fn surface_hash(symbols: &SymbolTable) -> u64 {
    let mut parts: Vec<String> = symbols
        .entries
        .iter()
        .filter(|e| !matches!(e.visibility, ResolvedVisibility::FilePrivate))
        .map(entry_surface)
        .collect();
    parts.sort();
    let mut hasher = FxHasher::default();
    for part in &parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

/// A span-free string capturing everything about a symbol that a dependent can
/// observe: its name, kind (with arity / capabilities / handler shape),
/// visibility, and external-export flag.
fn entry_surface(e: &SymbolEntry) -> String {
    // Most `SymbolKind` variants are span-free, so their `Debug` form is a safe
    // fingerprint. `Actor` is the exception — its `StateField` / `HandlerSig`
    // carry declaration spans — so it is rendered field by field instead.
    let kind = match &e.kind {
        SymbolKind::Actor { state, handlers } => {
            let st: Vec<&str> = state.iter().map(|s| s.name.as_str()).collect();
            let hd: Vec<String> = handlers
                .iter()
                .map(|h| format!("{}{:?}", h.name, h.caps))
                .collect();
            format!("Actor{{state:{st:?},handlers:{hd:?}}}")
        }
        other => format!("{other:?}"),
    };
    format!(
        "{}\u{1}{}\u{1}{:?}\u{1}{}",
        e.name, kind, e.visibility, e.exported_externally
    )
}

/// The edited module followed by every module that transitively imports it, in
/// breadth-first order from the edit. `deps[a]` lists the modules `a` imports,
/// so the reverse edges give the dependents. A visited set keeps the walk
/// terminating even when imports form a cycle.
fn reverse_dep_closure(deps: &[Vec<ModuleId>], edited: ModuleId) -> Vec<ModuleId> {
    let n = deps.len();
    let mut reverse: Vec<Vec<ModuleId>> = vec![Vec::new(); n];
    for (a, row) in deps.iter().enumerate() {
        for &b in row {
            let bi = b.0 as usize;
            if bi < n {
                reverse[bi].push(ModuleId(u32::try_from(a).unwrap_or(u32::MAX)));
            }
        }
    }

    let mut visited = vec![false; n];
    let mut order: Vec<ModuleId> = Vec::new();
    let mut queue: VecDeque<ModuleId> = VecDeque::new();
    visited[edited.0 as usize] = true;
    queue.push_back(edited);
    while let Some(m) = queue.pop_front() {
        order.push(m);
        for &dependent in &reverse[m.0 as usize] {
            let di = dependent.0 as usize;
            if !visited[di] {
                visited[di] = true;
                queue.push_back(dependent);
            }
        }
    }
    order
}
