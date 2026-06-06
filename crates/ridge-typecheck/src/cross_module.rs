//! Cross-module type seeding.
//!
//! The type checker is otherwise module-local for user symbols: `collect_user_tycons`
//! only knows the current module's `type`/`actor` declarations, so an imported type
//! used in an annotation (`import m (User)` then `(u: User)`) would fall through to a
//! fresh type variable. This module bridges that gap by mapping each consumer module's
//! imported type names to the producer's (workspace-global) `TyConId`.
//!
//! The `TyConArena` is shared across the whole workspace, so a producer's `TyConId` is
//! valid in any consumer. We only need to discover, for each imported type name, which
//! `TyConId` the producer declared it as.

use rustc_hash::FxHashMap;
use std::sync::Arc;

use ridge_ast::{Item, Module};
use ridge_resolve::{Binding, ImportResolution, ImportTarget, ModuleId, SymbolKind, SymbolTable};
use ridge_types::{BuiltinTyCons, Scheme, TyConId};

/// Map a stdlib opaque type name to its pre-registered builtin `TyConId`.
///
/// Stdlib taint wrappers are interned as builtins (see `BuiltinTyCons`) rather
/// than collected from source, so an importing module resolves the bare name to
/// these ids. Returns `None` for any name that is not a known stdlib opaque type.
fn stdlib_opaque_tycon(b: &BuiltinTyCons, name: &str) -> Option<TyConId> {
    match name {
        "Sql" => Some(b.sql),
        "Html" => Some(b.html),
        "SecureCookie" => Some(b.secure_cookie),
        "SqlValue" => Some(b.sql_value),
        _ => None,
    }
}

/// Order modules so every producer is type-checked before its consumers.
///
/// `deps[m.0]` lists the modules that module `m` imports. A post-order DFS over
/// those edges yields dependencies before dependents (leaves first), which is
/// exactly the order needed to seed a consumer with its producers' schemes.
/// Import cycles (already reported as `R003`) are broken by the visited set;
/// their members get an arbitrary relative order.
#[must_use]
pub(crate) fn topo_order(deps: &[Vec<ModuleId>]) -> Vec<ModuleId> {
    let n = deps.len();
    let mut state = vec![0u8; n]; // 0 = unvisited, 1 = on-stack, 2 = done
    let mut order = Vec::with_capacity(n);
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some(&mut (node, ref mut idx)) = stack.last_mut() {
            state[node] = 1;
            if *idx < deps[node].len() {
                let child = deps[node][*idx].0 as usize;
                *idx += 1;
                if child < n && state[child] == 0 {
                    stack.push((child, 0));
                }
            } else {
                state[node] = 2;
                order.push(ModuleId(u32::try_from(node).unwrap_or(u32::MAX)));
                stack.pop();
            }
        }
    }
    order
}

/// Predict, per module, the `type/actor name -> TyConId` arena ids that the
/// user-tycon collect pass assigns.
///
/// Every named `TypeDecl`/`ActorDecl` interns exactly one arena entry, in the
/// order modules are type-checked (`check_order`) then source order, starting at
/// `builtins_len` (the number of built-in `TyCons`). This mirrors
/// `collect_user_tycons` pass-1 interning as driven by the same order, so the
/// predicted id equals the arena id the producer module holds after its collect
/// pass runs. The result is indexed by `ModuleId.0`.
#[must_use]
pub(crate) fn predict_module_tycon_names(
    module_asts: &[Arc<Module>],
    check_order: &[ModuleId],
    builtins_len: u32,
) -> Vec<FxHashMap<String, TyConId>> {
    let mut next = builtins_len;
    let mut per_module: Vec<FxHashMap<String, TyConId>> = (0..module_asts.len())
        .map(|_| FxHashMap::default())
        .collect();
    for &mid in check_order {
        let Some(ast) = module_asts.get(mid.0 as usize) else {
            continue;
        };
        let map = &mut per_module[mid.0 as usize];
        for item in &ast.items {
            let name = match item {
                Item::Type(td) => Some(td.name.text.clone()),
                Item::Actor(ad) => Some(ad.name.text.clone()),
                _ => None,
            };
            if let Some(n) = name {
                map.insert(n, TyConId(next));
                next += 1;
            }
        }
    }
    per_module
}

/// Flatten per-module type-name maps into a single workspace map (first
/// occurrence in check order wins), for the instance-collection pass which only
/// needs a name to resolve to some declaring `TyConId`.
#[must_use]
pub(crate) fn flatten_tycon_names(
    per_module: &[FxHashMap<String, TyConId>],
    check_order: &[ModuleId],
) -> FxHashMap<String, TyConId> {
    let mut flat: FxHashMap<String, TyConId> = FxHashMap::default();
    for &mid in check_order {
        if let Some(map) = per_module.get(mid.0 as usize) {
            for (name, &tid) in map {
                flat.entry(name.clone()).or_insert(tid);
            }
        }
    }
    flat
}

/// Build a consumer module's `local-name -> producer TyConId` map for the types
/// it imports.
///
/// Only **item imports** of types/actors are included (`import m (User)`), since
/// those introduce a bare name usable in annotations. Qualified type paths
/// (`m.User` in a type position) are not representable in the AST and are out of
/// scope here.
#[must_use]
pub(crate) fn imported_tycon_names(
    imports: &[ImportResolution],
    symbol_tables: &[&SymbolTable],
    per_module_tycon_names: &[FxHashMap<String, TyConId>],
    b: &BuiltinTyCons,
) -> FxHashMap<String, TyConId> {
    let mut out: FxHashMap<String, TyConId> = FxHashMap::default();
    for ir in imports {
        for eb in &ir.effective_bindings {
            match &eb.binding {
                // A type imported from another workspace module.
                Binding::ImportedSymbol { module, symbol, .. } => {
                    let Some(entry) = symbol_tables
                        .get(module.0 as usize)
                        .and_then(|t| t.entries.get(symbol.0 as usize))
                    else {
                        continue;
                    };
                    if !matches!(
                        entry.kind,
                        SymbolKind::Type { .. } | SymbolKind::Actor { .. }
                    ) {
                        continue;
                    }
                    if let Some(&tid) = per_module_tycon_names
                        .get(module.0 as usize)
                        .and_then(|m| m.get(&entry.name))
                    {
                        out.insert(eb.local_name.clone(), tid);
                    }
                }
                // An opaque taint wrapper imported from a builtin stdlib module
                // (`Sql`, `Html`) — resolve the bare name to its builtin TyConId so
                // annotations type-check and field access is gated (T036).
                Binding::StdlibSymbol { module, name } => {
                    let is_opaque = ridge_resolve::BUILTINS
                        .get(module.0 as usize)
                        .is_some_and(|m| m.opaque_types.contains(&name.as_str()));
                    if is_opaque {
                        if let Some(tid) = stdlib_opaque_tycon(b, name) {
                            out.insert(eb.local_name.clone(), tid);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Build the value-scheme bindings a consumer module gets from its imports.
///
/// Two shapes are seeded, both reusing the producer's already-computed schemes
/// from `exported_schemes` (indexed by `ModuleId.0`, available because producers
/// are type-checked first):
///
/// - **Item imports** (`import m (needsText)`): the bare local name is bound to
///   the producer's `fn`/`const` scheme.
/// - **Module aliases** (`import m as M`): every exported `fn`/`const` is bound
///   under the qualified key `M.<name>`, matching how `Expr::Qualified` looks up
///   `M.needsText` in the environment.
///
/// Generalised schemes are context-independent (they quantify their own vars and
/// reference workspace-global `TyConId`s), so a producer scheme is sound to
/// instantiate in any consumer.
#[must_use]
pub(crate) fn imported_value_schemes(
    imports: &[ImportResolution],
    symbol_tables: &[&SymbolTable],
    exported_schemes: &[FxHashMap<String, Scheme>],
) -> FxHashMap<String, Scheme> {
    let mut out: FxHashMap<String, Scheme> = FxHashMap::default();
    for ir in imports {
        for eb in &ir.effective_bindings {
            match &eb.binding {
                Binding::ImportedSymbol { module, symbol, .. } => {
                    let Some(table) = symbol_tables.get(module.0 as usize) else {
                        continue;
                    };
                    let Some(entry) = table.entries.get(symbol.0 as usize) else {
                        continue;
                    };
                    if !matches!(entry.kind, SymbolKind::Fn { .. } | SymbolKind::Const) {
                        continue;
                    }
                    if let Some(scheme) = exported_schemes
                        .get(module.0 as usize)
                        .and_then(|m| m.get(&entry.name))
                    {
                        out.insert(eb.local_name.clone(), scheme.clone());
                    }
                }
                Binding::ModuleAlias {
                    target: ImportTarget::WorkspaceModule(mid),
                    ..
                } => {
                    if let Some(map) = exported_schemes.get(mid.0 as usize) {
                        for (name, scheme) in map {
                            out.insert(format!("{}.{name}", eb.local_name), scheme.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}
