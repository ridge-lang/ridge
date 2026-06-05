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
use ridge_resolve::{Binding, ImportResolution, SymbolKind, SymbolTable};
use ridge_types::TyConId;

/// Predict, per module, the `type/actor name -> TyConId` arena ids that the
/// user-tycon collect pass assigns.
///
/// Every named `TypeDecl`/`ActorDecl` interns exactly one arena entry, in module
/// order then source order, starting at `builtins_len` (the number of built-in
/// `TyCons` already in the arena). This mirrors `collect_user_tycons` pass-1
/// interning, so the predicted id equals the arena id the producer module holds
/// after its own collect pass runs.
///
/// The result is indexed by `ModuleId.0` (the same order as `module_asts`).
#[must_use]
pub(crate) fn predict_module_tycon_names(
    module_asts: &[Arc<Module>],
    builtins_len: u32,
) -> Vec<FxHashMap<String, TyConId>> {
    let mut next = builtins_len;
    let mut per_module = Vec::with_capacity(module_asts.len());
    for ast in module_asts {
        let mut map: FxHashMap<String, TyConId> = FxHashMap::default();
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
        per_module.push(map);
    }
    per_module
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
) -> FxHashMap<String, TyConId> {
    let mut out: FxHashMap<String, TyConId> = FxHashMap::default();
    for ir in imports {
        for eb in &ir.effective_bindings {
            let Binding::ImportedSymbol { module, symbol, .. } = &eb.binding else {
                continue;
            };
            let Some(table) = symbol_tables.get(module.0 as usize) else {
                continue;
            };
            let Some(entry) = table.entries.get(symbol.0 as usize) else {
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
    }
    out
}
