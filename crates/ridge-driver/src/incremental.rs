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

use ridge_ast::{Item, MethodSig, Module, Param, Type};
use ridge_resolve::{
    resolve_module_incremental, ModuleId, ResolveError, ResolvedVisibility, ResolvedWorkspace,
    SymbolEntry, SymbolKind, SymbolTable,
};
use ridge_typecheck::{
    typecheck_module_incremental, typecheck_workspace, TypeError, TypecheckResult, TypedWorkspace,
};

use crate::sources::WorkspaceSourceCache;

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
    /// Typeclass-surface hash per module — its `class` / `instance` / `deriving`
    /// declarations — indexed by `ModuleId.0`.
    registry_hashes: Vec<u64>,
    /// Current source text per module, indexed by `ModuleId.0`. Empty unless the
    /// caller seeds it (the LSP path) via [`IncrementalState::with_module_sources`];
    /// an edit updates the edited module's entry so [`IncrementalState::source_cache`]
    /// always reflects what was actually compiled.
    module_sources: Vec<Arc<String>>,
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
        let registry_hashes = resolved
            .module_asts
            .iter()
            .map(|ast| registry_hash(ast))
            .collect();
        Self {
            resolved,
            typed: typecheck.typed,
            type_errors: typecheck.errors,
            disc_resolve_errors,
            surface_hashes,
            registry_hashes,
            module_sources: Vec::new(),
        }
    }

    /// Seed the per-module source text, indexed by `ModuleId.0`.
    ///
    /// Enables [`source_cache`](Self::source_cache); without it the engine still
    /// recompiles correctly but cannot reproduce a source cache.
    #[must_use]
    pub fn with_module_sources(mut self, sources: Vec<Arc<String>>) -> Self {
        self.module_sources = sources;
        self
    }

    /// A source cache reflecting each module's current text — the on-disk text at
    /// seed time, plus whatever later edits replaced it with. Built without
    /// touching disk, so it always matches what the engine actually compiled.
    #[must_use]
    pub fn source_cache(&self) -> WorkspaceSourceCache {
        WorkspaceSourceCache::from_module_texts(&self.resolved.graph, &self.module_sources)
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

        // Track the edited module's new source so `source_cache` keeps matching
        // what was compiled. No-op when sources were not seeded (non-LSP callers).
        if let Some(slot) = self.module_sources.get_mut(edited_id.0 as usize) {
            *slot = Arc::new(new_source.to_owned());
        }

        // A class / instance / deriving change needs the workspace registries
        // rebuilt with their global coherence checks; detect it straight from the
        // AST and take the deep-recompute path.
        let old_registry = self.registry_hashes[edited_id.0 as usize];
        let new_registry = registry_hash(&ast);
        self.registry_hashes[edited_id.0 as usize] = new_registry;
        if new_registry != old_registry {
            let _ = resolve_module_incremental(&mut self.resolved, edited_id, &ast, true);
            self.surface_hashes[edited_id.0 as usize] =
                surface_hash(&self.resolved.modules[edited_id.0 as usize].symbols);
            return self.deep_recompute();
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

    /// Rebuild the whole workspace's resolution and type-check from the cached
    /// ASTs (no re-parse). Used when an edit changes the typeclass surface: the
    /// class/instance registries and the global class-method index are rebuilt
    /// with their cross-module coherence checks, which a single-module recompute
    /// cannot do. Every module is recomputed, so the result is exactly a full
    /// check of the edited sources.
    fn deep_recompute(&mut self) -> Vec<ModuleId> {
        let n = self.resolved.modules.len();
        let mut all_resolve_errors: Vec<(ModuleId, ResolveError)> = Vec::new();
        for i in 0..n {
            let mid = ModuleId(u32::try_from(i).unwrap_or(u32::MAX));
            let ast = Arc::clone(&self.resolved.module_asts[i]);
            let errs = resolve_module_incremental(&mut self.resolved, mid, &ast, true);
            all_resolve_errors.extend(errs.into_iter().map(|e| (mid, e)));
            self.surface_hashes[i] = surface_hash(&self.resolved.modules[i].symbols);
            self.registry_hashes[i] = registry_hash(&ast);
        }
        self.resolved.errors = all_resolve_errors;

        let tc = typecheck_workspace(&self.resolved);
        self.typed = tc.typed;
        self.type_errors = tc.errors;

        (0..n)
            .map(|i| ModuleId(u32::try_from(i).unwrap_or(u32::MAX)))
            .collect()
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

/// Hash a module's typeclass surface — its `class` / `instance` declarations and
/// any `deriving` clauses. A change here means the workspace class/instance
/// registries must be rebuilt with their global coherence checks, so it is
/// tracked separately from the ordinary symbol surface. The hash is span-free,
/// so a body edit elsewhere in the file leaves it untouched.
fn registry_hash(module: &Module) -> u64 {
    let mut parts: Vec<String> = Vec::new();
    for item in &module.items {
        match item {
            Item::ClassDecl(c) => {
                let mut supers: Vec<&str> = c
                    .superclasses
                    .iter()
                    .map(|s| s.class.text.as_str())
                    .collect();
                supers.sort_unstable();
                let mut methods: Vec<String> =
                    c.methods.iter().map(method_sig_fingerprint).collect();
                methods.sort();
                parts.push(format!(
                    "C|{}|{}|{supers:?}|{methods:?}",
                    c.name.text, c.ty_var.text
                ));
            }
            Item::InstanceDecl(i) => {
                let mut methods: Vec<&str> =
                    i.methods.iter().map(|m| m.name.text.as_str()).collect();
                methods.sort_unstable();
                parts.push(format!(
                    "I|{}|{}|{methods:?}",
                    i.class.text,
                    render_ast_type(&i.ty)
                ));
            }
            Item::Type(t) if !t.deriving.is_empty() => {
                let mut der: Vec<&str> = t.deriving.iter().map(|d| d.text.as_str()).collect();
                der.sort_unstable();
                parts.push(format!("D|{}|{der:?}", t.name.text));
            }
            _ => {}
        }
    }
    parts.sort();
    let mut hasher = FxHasher::default();
    for part in &parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

/// A span-free fingerprint of a class method signature: its name, parameter
/// types, and return type.
fn method_sig_fingerprint(m: &MethodSig) -> String {
    let params: Vec<String> = m.params.iter().map(render_param).collect();
    format!(
        "{}({})->{}",
        m.name.text,
        params.join(","),
        render_ast_type(&m.ret)
    )
}

/// A span-free rendering of a parameter's declared type (`_` when unannotated).
fn render_param(p: &Param) -> String {
    match p {
        Param::Bare(_) => "_".to_string(),
        Param::Annotated { ty, .. } => render_ast_type(ty),
    }
}

/// A span-free, structural rendering of an AST type: two types render to the
/// same string exactly when they have the same shape, regardless of where they
/// appear in source.
fn render_ast_type(ty: &Type) -> String {
    match ty {
        Type::Primitive { name, .. } => format!("{name:?}"),
        Type::Named { name, .. } | Type::Var { name, .. } => name.text.clone(),
        Type::App { head, args, .. } => {
            let rendered: Vec<String> = args.iter().map(render_ast_type).collect();
            format!("{}<{}>", head.text, rendered.join(","))
        }
        Type::Tuple { elems, .. } => {
            let rendered: Vec<String> = elems.iter().map(render_ast_type).collect();
            format!("({})", rendered.join(","))
        }
        Type::List { elem, .. } => format!("[{}]", render_ast_type(elem)),
        Type::Paren { inner, .. } => render_ast_type(inner),
        Type::Fn { fn_ty, .. } => {
            let rendered: Vec<String> = fn_ty.params.iter().map(render_ast_type).collect();
            format!(
                "fn{:?}({})->{}",
                fn_ty.caps,
                rendered.join(","),
                render_ast_type(&fn_ty.ret)
            )
        }
        Type::Record { fields, .. } => {
            let mut rendered: Vec<String> = fields
                .iter()
                .map(|f| format!("{}:{}", f.name.text, render_ast_type(&f.ty)))
                .collect();
            rendered.sort();
            format!("{{{}}}", rendered.join(","))
        }
    }
}
