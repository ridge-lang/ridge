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
use ridge_types::{Scheme, TyConDecl, TyConId};

/// Index every compiler-provided type declaration by name.
///
/// "Compiler-provided" means everything the arena holds before a module's own
/// `type` declarations are collected: the built-ins (`Int`, `Error`, `Duration`,
/// `Output`, the taint wrappers) and the reconciled stdlib block. Both leave
/// `def_module_raw` unset, which is what distinguishes them from a user type;
/// anonymous record tycons share that trait and are excluded by name anyway.
///
/// Built by scanning the arena rather than from a hand-written table. The table
/// this replaced listed four names and silently dropped every other stdlib type
/// an importing module named, so `Duration { ms = "x" }` type-checked (#497).
pub(crate) fn compiler_tycon_names(decls: &[TyConDecl]) -> FxHashMap<String, TyConId> {
    decls
        .iter()
        .filter(|d| d.def_module_raw.is_none() && !d.is_anon)
        .map(|d| (d.name.clone(), d.id))
        .collect()
}

// ── TyConOrigins ──────────────────────────────────────────────────────────────

/// Where each type constructor was declared, and what to call it.
///
/// The coherence checks need to answer one question about an instance head —
/// *which module declares this type?* — and, when the answer makes the instance
/// illegal, to name the type and the module in the words the reader used.
///
/// Both halves are awkward to get at from the collect pass, which runs before
/// any module is type-checked: the arena holds only compiler-declared types at
/// that point and the user's are still predictions. Collecting both here means
/// the orphan rule reads a declaring module instead of guessing one from the
/// numeric range an id falls in. That guess is how the rule came to be a no-op
/// for every built-in past `JsonValue`: it compared against a hand-written
/// bound that the built-in table outgrew by forty-four entries, and nothing
/// ever compared the two.
///
/// A short or empty table is safe in the one direction that matters. Every
/// answer it does not have is `None`, and `None` matches no module, so a
/// missing entry turns into a reported orphan rather than a permitted one — a
/// caller that forgets to populate this gets a failing build, not a quiet hole
/// of the kind it exists to close.
#[derive(Debug, Default)]
pub struct TyConOrigins {
    /// Indexed by `TyConId.0`; ids past the end are unknown rather than absent,
    /// since the collect pass may see a head the prediction never covered.
    entries: Vec<TyConOrigin>,
    /// Fully-qualified module names, indexed by raw `ModuleId`.
    module_fqns: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct TyConOrigin {
    name: Option<String>,
    /// Raw `ModuleId` of the declaring module. `None` for the prelude and the
    /// reconciled stdlib block, which no user module can claim as its own.
    def_module: Option<u32>,
}

impl TyConOrigins {
    /// Builds the table from the two places a `TyConId` can come from.
    ///
    /// `compiler_decls` is the arena as it stands before user types are
    /// interned — built-ins first, then the reconciled stdlib block.
    /// `per_module` is the per-module prediction of the ids user `type` and
    /// `actor` declarations will receive, indexed by `ModuleId.0`; it is the
    /// same prediction the instance heads are resolved against, so the two
    /// cannot disagree about which id a declaration owns.
    #[must_use]
    pub fn new(
        compiler_decls: &[TyConDecl],
        per_module: &[FxHashMap<String, TyConId>],
        module_fqns: &[String],
    ) -> Self {
        let mut entries: Vec<TyConOrigin> = compiler_decls
            .iter()
            .map(|d| TyConOrigin {
                name: Some(d.name.clone()),
                // The opaque built-ins carry `u32::MAX` to mean "declared
                // somewhere no user module can be", which is the same answer as
                // `None` for every question asked here — and unlike `None` it
                // has a decimal rendering, so leaving it in means a message can
                // eventually offer `module#4294967295` as a place to move an
                // instance to. Normalised at the boundary so no reader of this
                // table has to know the sentinel exists.
                def_module: d.def_module_raw.filter(|m| *m != u32::MAX),
            })
            .collect();

        for (raw_module, names) in per_module.iter().enumerate() {
            let Ok(raw_module) = u32::try_from(raw_module) else {
                continue;
            };
            for (name, id) in names {
                let slot = id.0 as usize;
                if entries.len() <= slot {
                    entries.resize(slot + 1, TyConOrigin::default());
                }
                entries[slot] = TyConOrigin {
                    name: Some(name.clone()),
                    def_module: Some(raw_module),
                };
            }
        }

        Self {
            entries,
            module_fqns: module_fqns.to_vec(),
        }
    }

    /// The module that declares `id`, or `None` when it is a prelude type or an
    /// id this table never saw.
    #[must_use]
    pub fn declaring_module(&self, id: TyConId) -> Option<u32> {
        self.entries.get(id.0 as usize)?.def_module
    }

    /// The type's declared name, falling back to the raw id so a message is
    /// still readable if the table is short.
    #[must_use]
    pub fn type_name(&self, id: TyConId) -> String {
        self.entries
            .get(id.0 as usize)
            .and_then(|e| e.name.clone())
            .unwrap_or_else(|| format!("#{}", id.0))
    }

    /// The module's fully-qualified name, falling back to the raw id.
    #[must_use]
    pub fn module_name(&self, raw: u32) -> String {
        self.module_fqns
            .get(raw as usize)
            .cloned()
            .unwrap_or_else(|| format!("module#{raw}"))
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
    actual_tycon_names: &[FxHashMap<String, TyConId>],
    per_module_tycon_names: &[FxHashMap<String, TyConId>],
    stdlib_tycon_names: &FxHashMap<String, TyConId>,
    compiler_tycons: &FxHashMap<String, TyConId>,
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
                    // Prefer the producer's real id (recorded once it is checked, which
                    // `check_order` guarantees for every module this one imports); fall
                    // back to the pre-check prediction only if a producer is somehow not
                    // yet recorded. The prediction drifts from the real id whenever the
                    // producer or a module before it synthesizes types (`deriving`
                    // mirrors, insert companions) the source-item count cannot see.
                    let resolved = actual_tycon_names
                        .get(module.0 as usize)
                        .and_then(|m| m.get(&entry.name))
                        .or_else(|| {
                            per_module_tycon_names
                                .get(module.0 as usize)
                                .and_then(|m| m.get(&entry.name))
                        });
                    if let Some(&tid) = resolved {
                        out.insert(eb.local_name.clone(), tid);
                    }
                }
                // A type imported from a stdlib module. Reconciled stdlib types
                // resolve by name to their reserved-block id; everything else the
                // compiler declares itself — the taint wrappers (`Sql`, `Html`)
                // whose field access is gated (T036), and the nominal records
                // (`Error`, `Duration`, `Output`) — resolves to its built-in id.
                //
                // A name that resolves to neither is dropped, and dropping it is
                // what made the type invisible: the record-literal path in
                // `infer` falls back to a stub scheme and stops checking the
                // fields entirely.
                Binding::StdlibSymbol { name, .. } => {
                    if let Some(&tid) = stdlib_tycon_names
                        .get(name)
                        .or_else(|| compiler_tycons.get(name))
                    {
                        out.insert(eb.local_name.clone(), tid);
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

#[cfg(test)]
mod tycon_origins_tests {
    use super::TyConOrigins;
    use ridge_types::{BuiltinTyCons, TyConArena, TyConId};
    use rustc_hash::FxHashMap;

    /// No built-in type answers with a module a user could write code in.
    ///
    /// This is the assertion the orphan rule went without. It asked whether an
    /// id sat at or above a written-down bound of 17; the built-in table grew
    /// to 61 and nothing ever compared the two, so the rule stopped applying to
    /// forty-four types and no test changed colour. Stated over the whole arena
    /// rather than at the boundary, so it keeps holding as built-ins are added
    /// — which is the failure it exists to catch, not the one already fixed.
    #[test]
    fn no_builtin_type_is_owned_by_a_user_module() {
        let mut arena = TyConArena::new();
        let _ = BuiltinTyCons::allocate(&mut arena);
        let origins = TyConOrigins::new(arena.all(), &[], &[]);

        let owned: Vec<&str> = arena
            .all()
            .iter()
            .filter(|d| origins.declaring_module(d.id).is_some())
            .map(|d| d.name.as_str())
            .collect();

        assert!(
            owned.is_empty(),
            "built-in types reported as declared by a user module: {owned:?}"
        );
    }

    /// A user type answers with the module that declared it, and its neighbours
    /// in the same workspace do not answer for it.
    #[test]
    fn a_user_type_is_owned_by_its_declaring_module() {
        let mut mod0: FxHashMap<String, TyConId> = FxHashMap::default();
        mod0.insert("Color".to_string(), TyConId(61));
        let mut mod1: FxHashMap<String, TyConId> = FxHashMap::default();
        mod1.insert("Shape".to_string(), TyConId(62));

        let origins = TyConOrigins::new(
            &[],
            &[mod0, mod1],
            &["app.color".to_string(), "app.shape".to_string()],
        );

        assert_eq!(origins.declaring_module(TyConId(61)), Some(0));
        assert_eq!(origins.declaring_module(TyConId(62)), Some(1));
        assert_eq!(origins.type_name(TyConId(61)), "Color");
        assert_eq!(origins.module_name(1), "app.shape");
    }

    /// An id past the end of the table is unknown, and unknown must read as
    /// "no module owns this" — the direction that reports an orphan rather than
    /// permitting one.
    #[test]
    fn an_unknown_id_owns_nothing() {
        let origins = TyConOrigins::default();
        assert_eq!(origins.declaring_module(TyConId(9_999)), None);
    }
}
