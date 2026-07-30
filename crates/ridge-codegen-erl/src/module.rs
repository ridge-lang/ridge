//! §4.26–§4.27 — Module-level assembly and BEAM module-name mangling.
//!
//! `lower_module` walks the items of a `LoweredModule`, dispatches to
//! `item::lower_fn` / `item::lower_const`, and assembles the resulting
//! `CErlFn`s into a `CErlModule` with the correct exports list.
//!
//! `mangle_module_name` implements the BEAM module-name mangling rule
//! (plan line 405): replace `.` with `_`, prefix `ridge_`, reject collision
//! with the reserved `ridge_rt` atom (E006).

// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]
// lower_module_all and lower_module_with_name are called from T10's workspace-
// level codegen; dead_code fires until T10 wires them.
#![allow(dead_code)]

use crate::actor::lower_actor;
use crate::anf::normalise_module;
use crate::core_ast::{
    CErlAnn, CErlAtom, CErlAttribute, CErlExport, CErlExpr, CErlFn, CErlLit, CErlModule, CErlVar,
};
use crate::error::CodegenError;
use crate::item::{lower_const, lower_fn_with_module_name};
use ridge_ir::{IrFfiFn, IrItem, LoweredModule, LoweredWorkspace};
use ridge_resolve::ModuleId;
use rustc_hash::FxHashMap;

/// Build the workspace-wide arity table for cross-module symbol calls:
/// `module_id → (name → arity)`.
///
/// Each module contributes its fns (arity = parameter count), consts (arity 0),
/// and `@ffi` stubs (arity = parameter count) — the same shape the per-module
/// local arity table uses. A `SymbolRef::External` call carries only its
/// callee's module id and name; this table lets it recover the callee's arity
/// across the module boundary so a zero-arity call written `f ()` drops the
/// unit-paren punctuation instead of emitting an arity-1 call that would be
/// `undef` against the arity-0 callee.
pub(crate) fn build_external_arity(
    ws: &LoweredWorkspace,
) -> FxHashMap<ModuleId, FxHashMap<String, u32>> {
    let mut table: FxHashMap<ModuleId, FxHashMap<String, u32>> = FxHashMap::default();
    for slot in &ws.modules {
        let Some(m) = slot else { continue };
        let mut names: FxHashMap<String, u32> = FxHashMap::default();
        for item in &m.items {
            match item {
                IrItem::Fn(fn_) => {
                    #[allow(clippy::cast_possible_truncation)]
                    names.insert(fn_.name.clone(), fn_.params.len() as u32);
                }
                IrItem::Const(c) => {
                    names.insert(c.name.clone(), 0);
                }
                IrItem::Ffi(ffi) => {
                    #[allow(clippy::cast_possible_truncation)]
                    names.insert(ffi.name.clone(), ffi.params.len() as u32);
                }
                _ => {}
            }
        }
        table.insert(m.id, names);
    }
    table
}

// ── Name mangling (plan line 405) ────────────────────────────────────────────

/// The reserved BEAM module name that must not be produced by mangling.
const RESERVED_RT: &str = "ridge_rt";

/// Mangle a Ridge module-path slice into a BEAM atom string.
///
/// **Algorithm** (plan line 405):
/// 1. Join the path segments with `_`.
/// 2. Prefix `ridge_`.
/// 3. Reject equality with `ridge_rt` (reserved) → `E006`.
///
/// # Example
/// ```
/// # use ridge_codegen_erl::error::CodegenError;
/// // Tested via the module-level tests below.
/// ```
///
/// # Errors
/// Returns [`CodegenError::BeamModuleNameCollision`] (`E006`) if the mangled
/// name equals `ridge_rt` (reserved for the runtime bridge module).
pub fn mangle_module_name(
    module_path: &[&str],
    module_id: ModuleId,
) -> Result<String, CodegenError> {
    let joined = module_path.join("_");
    let mangled = format!("ridge_{joined}");

    if mangled == RESERVED_RT {
        return Err(CodegenError::BeamModuleNameCollision {
            // Both `left` and `right` are the same module in this single-module
            // collision check; the workspace-level dedup passes the pair.
            left: module_id,
            right: module_id,
            mangled,
        });
    }

    Ok(mangled)
}

/// Compute the stable BEAM module name for a dotted FQN (e.g.
/// `"acme.domain.Models"` → `ridge_acme_domain_Models`).
///
/// Used by the driver and CLI so every consumer derives beam names from the
/// FQN — never from `ModuleId` ordering — keeping names stable across builds.
///
/// # Errors
/// Returns [`CodegenError::BeamModuleNameCollision`] (`E006`) if the mangled
/// name collides with the reserved `ridge_rt` atom.
pub fn beam_name_for_fqn(fqn: &str, module_id: ModuleId) -> Result<String, CodegenError> {
    let segments: Vec<&str> = fqn.split('.').collect();
    mangle_module_name(&segments, module_id)
}

// ── Module assembly (§4.26 + §4.27) ─────────────────────────────────────────

/// Lower a [`LoweredModule`] to a [`CErlModule`] plus zero or more actor modules.
///
/// ## Item dispatch
/// - [`IrItem::Fn`]    → [`lower_fn`] → `CErlFn`; exported if `is_pub` or `is_main`.
/// - [`IrItem::Const`] → [`lower_const`] → zero-arity `CErlFn`; exported if `is_pub`.
/// - [`IrItem::Actor`] → [`lower_actor`] → **separate** `CErlModule` (`gen_server`).
///   Actor modules are collected in `actor_modules` (returned alongside the main
///   module) — they are separate BEAM compilation units.
///
/// ## Exports
/// An item is added to `CErlModule.exports` if:
/// - `IrFn.is_pub == true`, **or**
/// - `IrFn.is_main == true` (entry-point export, §4.26).
/// - `IrConst.is_pub == true` (0-arity call form, §4.27).
///
/// ## Module name
/// `module_path` segments are joined and prefixed with `ridge_` via
/// [`mangle_module_name`].
///
/// # Errors
/// Returns `Err` if `mangle_module_name` rejects the path (E006 collision),
/// or if any item lowering fails.
pub(crate) fn lower_module(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    module_path: &[&str],
) -> Result<CErlModule, CodegenError> {
    let beam_name = mangle_module_name(module_path, m.id)?;
    lower_module_with_name(m, ws, &beam_name)
}

/// Lower a module given an explicit BEAM module name (no mangling applied).
///
/// Exposed as `pub(crate)` so that `lib.rs::codegen_stdlib_module_with_fqn`
/// can compile stdlib modules with their dotted FQN (e.g. `"std.list"`) as the
/// BEAM atom, bypassing the `ridge_*` name-mangling used for user modules
/// (the dotted FQN is used for stdlib module atoms, not `ridge_*` mangling).
///
/// Returns the main `CErlModule`; actor sub-modules are emitted into the
/// `fns` list as a documentation note (the full actor modules are returned
/// by [`lower_module_all`]).  In the current implementation actors are lowered
/// as separate modules and the main module does not reference them directly.
#[allow(
    clippy::similar_names,
    reason = "fn_ (match-arm binding for IrItem::Fn) vs fns (Vec of lowered fns) — both are domain-correct conventional names"
)]
pub(crate) fn lower_module_with_name(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    beam_name: &str,
) -> Result<CErlModule, CodegenError> {
    // Build a fn/const arity table for this module so that SymbolRef::Local
    // used as a value can resolve to a LocalFnRef (T8 wiring).
    // Fns use params.len(); consts are always arity 0.
    // @ffi stubs (IrItem::Ffi) are included so that SymbolRef::Local calls
    // to them can be resolved as LocalFnRef — their wrapper is emitted below.
    let mut fn_arity: FxHashMap<String, u32> = FxHashMap::default();
    for item in &m.items {
        match item {
            IrItem::Fn(fn_) => {
                #[allow(clippy::cast_possible_truncation)]
                let arity = fn_.params.len() as u32;
                fn_arity.insert(fn_.name.clone(), arity);
            }
            IrItem::Const(c) => {
                fn_arity.insert(c.name.clone(), 0);
            }
            IrItem::Ffi(ffi) => {
                #[allow(clippy::cast_possible_truncation)]
                let arity = ffi.params.len() as u32;
                fn_arity.insert(ffi.name.clone(), arity);
            }
            _ => {}
        }
    }

    // If the module contains any actor, its parent module must expose every
    // top-level fn and const to the BEAM linker — actor sub-modules compile
    // to separate units and reach back into the parent via qualified
    // `call 'parent':'fn' (args…)` regardless of Ridge `pub` visibility.
    // Without this widening, calls from actor handlers (and the inner
    // lambdas they nest) to private parent fns would fail at erlc with
    // `undefined function fn/n`.  Ridge-level visibility is still enforced
    // by the resolver, so BEAM export pollution is the only cost.
    let module_has_actor = m.items.iter().any(|item| matches!(item, IrItem::Actor(_)));

    // Record version metadata for the whole workspace, computed once and
    // shared between the `ridge_meta` attribute and the migration-chain
    // exports below.
    let record_meta =
        crate::record_meta::build_record_meta(&ws.tycons, &ws.target_names, &ws.module_fqns);

    let mut fns = Vec::new();
    let mut exports = Vec::new();

    for item in &m.items {
        // `IrItem::Actor` and the wildcard arm both produce empty bodies on
        // purpose: actors are emitted as separate modules elsewhere, and the
        // wildcard is the defensive future-variant guard required by
        // `#[non_exhaustive]`.  Disable the `match_same_arms` lint here.
        #[allow(clippy::match_same_arms)]
        match item {
            IrItem::Fn(fn_) => {
                let cerl_fn = lower_fn_with_module_name(fn_, ws, &fn_arity, Some(beam_name))?;
                // §4.26: add to exports if pub or is_main (entry point), or
                // unconditionally when the module has an actor (see comment
                // above for the cross-module-call rationale).
                if fn_.is_pub || fn_.is_main || module_has_actor {
                    exports.push(CErlExport {
                        name: cerl_fn.name.clone(),
                        arity: cerl_fn.arity,
                    });
                }
                fns.push(cerl_fn);
            }
            IrItem::Const(c) => {
                let cerl_fn = lower_const(c, ws, &fn_arity)?;
                // §4.27: const → 0-arity fn; exported if is_pub, or
                // unconditionally when the module has an actor.
                if c.is_pub || module_has_actor {
                    exports.push(CErlExport {
                        name: cerl_fn.name.clone(),
                        arity: 0,
                    });
                }
                fns.push(cerl_fn);
            }
            // §4.28: IrItem::Actor → separate CErlModule via lower_actor.
            // Actor modules are separate BEAM compilation units collected by
            // lower_module_all.  Skip silently here (the actor is emitted as a
            // separate module by lower_module_all).
            IrItem::Actor(_) => {}
            // IrItem::Ffi → thin wrapper: `fun(V_P0, …) -> call 'mod':'fn'(…)`.
            // Emitted so that same-module SymbolRef::Local callers resolve to a
            // defined function (fixes E004 "undefined function X/N" from erlc).
            IrItem::Ffi(ffi) => {
                let cerl_fn = lower_ffi_wrapper(ffi);
                if ffi.is_pub {
                    exports.push(CErlExport {
                        name: cerl_fn.name.clone(),
                        #[allow(clippy::cast_possible_truncation)]
                        arity: ffi.params.len() as u32,
                    });
                }
                fns.push(cerl_fn);
            }
            // IrItem is #[non_exhaustive]; catch future variants defensively.
            _ => {}
        }
    }

    // ── Record version identity + migration chain (hot reload) ─────────────
    // Modules declaring at least one tagged record export two accessors the
    // runtime dispatcher reads: the current hash per record, and the
    // migration chain keyed by FROM hash.
    let record_metas: Vec<crate::record_meta::RecordMeta> = ws
        .tycons
        .iter()
        .filter(|d| d.def_module_raw == Some(m.id.0))
        .filter_map(|d| record_meta.get(&d.id).cloned())
        .collect();
    if !record_metas.is_empty() {
        exports.push(CErlExport {
            name: CErlAtom("__ridge_record_versions".into()),
            arity: 0,
        });
        exports.push(CErlExport {
            name: CErlAtom("__ridge_record_migrations".into()),
            arity: 0,
        });
        fns.push(emit_record_versions_fn(&record_metas));
        fns.push(emit_record_migrations_fn(
            m,
            ws,
            &record_metas,
            &fn_arity,
            beam_name,
        )?);
    }

    let mut module = CErlModule {
        name: CErlAtom(beam_name.into()),
        exports,
        attributes: vec![ridge_meta_attr(beam_name, m, ws, &record_meta)],
        fns,
    };
    // ANF normalisation: hoist non-atomic arguments in call/apply/case positions
    // so that `erlc` does not reject the emitted Core Erlang with "illegal expression".
    normalise_module(&mut module);
    Ok(module)
}

// ── ridge_meta beam attribute ────────────────────────────────────────────────

/// Lowercase atom name for a capability, in bit order (spec §3.5).
const fn capability_atom(c: ridge_ast::Capability) -> &'static str {
    match c {
        ridge_ast::Capability::Io => "io",
        ridge_ast::Capability::Fs => "fs",
        ridge_ast::Capability::Net => "net",
        ridge_ast::Capability::Time => "time",
        ridge_ast::Capability::Random => "random",
        ridge_ast::Capability::Env => "env",
        ridge_ast::Capability::Proc => "proc",
        ridge_ast::Capability::Spawn => "spawn",
        ridge_ast::Capability::Ffi => "ffi",
        ridge_ast::Capability::Db => "db",
    }
}

/// Build the `ridge_meta` module attribute as a structured constant term:
///
/// ```text
/// {'ridge_meta_v1', BeamName,
///   [{fn, Name, Arity, [CapAtom]}...],
///   [{record, Name, LayoutVersion}...]}
/// ```
///
/// The term is constant (atoms, ints, tuples, lists) so the Core Erlang
/// parser accepts it in attribute position — binary literals are rejected
/// there. Fn lines cover the exported fns (caps sorted); record lines cover
/// records declared in this module. Both lists are sorted for snapshot
/// determinism.
fn ridge_meta_attr(
    beam_name: &str,
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    meta: &FxHashMap<ridge_types::TyConId, crate::record_meta::RecordMeta>,
) -> CErlAttribute {
    let atom = |s: &str| CErlExpr::Lit(CErlLit::Atom(CErlAtom(s.to_owned())));

    let mut fn_entries: Vec<CErlExpr> = Vec::new();
    for item in &m.items {
        if let IrItem::Fn(fn_) = item {
            if !(fn_.is_pub || fn_.is_main) {
                continue;
            }
            let mut caps: Vec<CErlExpr> =
                fn_.caps.iter().map(|c| atom(capability_atom(c))).collect();
            caps.sort_by_key(|c| format!("{c:?}"));
            fn_entries.push(CErlExpr::Tuple(vec![
                atom("fn"),
                atom(&fn_.name),
                #[allow(clippy::cast_possible_wrap)]
                CErlExpr::Lit(CErlLit::Int(fn_.params.len() as i64)),
                CErlExpr::ListLit(caps),
            ]));
        }
    }
    fn_entries.sort_by_key(|e| format!("{e:?}"));

    let mut record_entries: Vec<CErlExpr> = ws
        .tycons
        .iter()
        .filter(|d| d.def_module_raw == Some(m.id.0))
        .filter_map(|d| meta.get(&d.id))
        .map(|rm| {
            CErlExpr::Tuple(vec![
                atom("record"),
                atom(&rm.name),
                #[allow(clippy::cast_possible_wrap)]
                CErlExpr::Lit(CErlLit::Int(rm.version as i64)),
            ])
        })
        .collect();
    record_entries.sort_by_key(|e| format!("{e:?}"));

    CErlAttribute {
        name: CErlAtom("ridge_meta".into()),
        value: CErlExpr::Tuple(vec![
            atom("ridge_meta_v1"),
            atom(beam_name),
            CErlExpr::ListLit(fn_entries),
            CErlExpr::ListLit(record_entries),
        ]),
    }
}

// ── Record version + migration chain accessors (hot reload) ──────────────────

/// `'__ridge_record_versions'/0` — `#{RecordName => CurrentHash}` for every
/// record declared in this module. The runtime dispatcher reads it to tell a
/// current tag from a stale one.
fn emit_record_versions_fn(metas: &[crate::record_meta::RecordMeta]) -> CErlFn {
    let pairs = metas
        .iter()
        .map(|rm| {
            (
                CErlExpr::Lit(CErlLit::Atom(CErlAtom(rm.name.clone()))),
                #[allow(clippy::cast_possible_wrap)]
                CErlExpr::Lit(CErlLit::Int(rm.version as i64)),
            )
        })
        .collect();
    CErlFn {
        name: CErlAtom("__ridge_record_versions".into()),
        arity: 0,
        anns: vec![CErlAnn(
            "%% Current shape hash per record (read by the runtime migration dispatcher)".into(),
        )],
        body: CErlExpr::Fun {
            params: vec![],
            body: Box::new(CErlExpr::MapLit(pairs)),
        },
    }
}

/// `'__ridge_record_migrations'/0` — the full migration chain
/// `[{FromHash, fun((Old) -> New)}]`: user `migrate` blocks in source order,
/// then compiler-derived structural edges (rename/keep/drop) for every
/// history entry with no user edge and a hole-free plan.
fn emit_record_migrations_fn(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    metas: &[crate::record_meta::RecordMeta],
    fn_arity: &FxHashMap<String, u32>,
    beam_name: &str,
) -> Result<CErlFn, CodegenError> {
    let mut entries: Vec<CErlExpr> = Vec::new();
    let mut covered_hashes: Vec<u64> = Vec::new();

    // ── User edges, in source order ─────────────────────────────────────────
    for item in &m.items {
        let IrItem::Migration(mig) = item else {
            continue;
        };
        let Some(from_hash) = mig.from_hash else {
            continue; // Typecheck already reported the unknown version; no edge for it.
        };
        covered_hashes.push(from_hash);
        entries.push(migration_entry(
            from_hash,
            user_migration_fun(mig, fn_arity, beam_name, ws)?,
        ));
    }

    // ── Derived edges: one per hole-free history entry, keyed by its hash ───
    let fqn = ws.module_fqns.get(m.id.0 as usize).cloned().unwrap_or_default();
    for rm in metas {
        let Some(history) = ws
            .version_history
            .records
            .get(&(fqn.clone(), rm.name.clone()))
        else {
            continue;
        };
        let current_shape: Vec<(String, String)> = current_rendered_shape(ws, rm);
        for entry in history {
            if entry.hash == rm.version || covered_hashes.contains(&entry.hash) {
                continue; // Current shape, or a user hook already covers it.
            }
            let Some((renames, removed)) = derive_plan(&entry.shape, &current_shape) else {
                continue; // Holes are not derivable — a chain gap by design.
            };
            covered_hashes.push(entry.hash);
            entries.push(migration_entry(
                entry.hash,
                derived_migration_fun(rm, &renames, &removed),
            ));
        }
    }

    Ok(CErlFn {
        name: CErlAtom("__ridge_record_migrations".into()),
        arity: 0,
        anns: vec![CErlAnn(
            "%% Record migration chain: [{FromHash, fun((Old) -> New)}] — user migrate blocks plus derived structural edges (read by the runtime migration dispatcher)".into(),
        )],
        body: CErlExpr::Fun {
            params: vec![],
            body: Box::new(CErlExpr::ListLit(entries)),
        },
    })
}

/// `{FromHash, Fun}` as a Core Erlang tuple.
fn migration_entry(from_hash: u64, fun: CErlExpr) -> CErlExpr {
    CErlExpr::Tuple(vec![
        #[allow(clippy::cast_possible_wrap)]
        CErlExpr::Lit(CErlLit::Int(from_hash as i64)),
        fun,
    ])
}

/// Lower a user migrate block to `fun(V_OldParam) -> <body>`.
///
/// The scope mirrors module-level fn bodies (same-module local calls stay
/// unqualified applies; the shared tables carry record metadata so the NEW
/// record value the body builds gets its `__ridge_v` tag).
fn user_migration_fun(
    mig: &ridge_ir::item::IrMigration,
    fn_arity: &FxHashMap<String, u32>,
    beam_name: &str,
    ws: &LoweredWorkspace,
) -> Result<CErlExpr, CodegenError> {
    let mut scope =
        crate::scope::LocalScope::with_arity_and_module(fn_arity.clone(), beam_name);
    scope.external_arity = std::sync::Arc::new(build_external_arity(ws));
    scope.tables = crate::scope::CodegenTables::from_workspace(ws);
    let param = ridge_ir::IrParam {
        name: mig.param_name.clone(),
        ty: ridge_types::Type::Error, // Erased at codegen; see IrParam uses in tests.
        span: mig.span,
    };
    crate::expr::lower_lambda(&[param], &mig.body, &scope)
}

/// The record's current shape as `(name, rendered-type)` pairs — the same
/// rendering `record_meta` hashes, so derived plans compare like with like.
fn current_rendered_shape(
    ws: &LoweredWorkspace,
    rm: &crate::record_meta::RecordMeta,
) -> Vec<(String, String)> {
    let ctx = ridge_types::render::RenderCtx {
        tycons: &ws.tycons,
        module_fqns: &ws.module_fqns,
    };
    ws.tycons
        .iter()
        .find(|d| !d.is_anon && d.name == rm.name && d.def_module_raw.is_some())
        .and_then(|d| match &d.kind {
            ridge_types::TyConKind::Record(schema) => Some(
                schema
                    .record_fields()
                    .iter()
                    .map(|f| (f.name.clone(), ridge_types::render::render_type(&ctx, &f.ty)))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

/// A derived migration plan: field renames `(old, new)` plus dropped fields.
type MigrationPlan = (Vec<(String, String)>, Vec<String>);

/// The mechanical migration plan from an old shape to the current one,
/// mirroring the scaffold's rename heuristic: a field present in both with
/// the same type keeps its value; exactly one removal + one addition with
/// the same type is a rename; removals drop. Any other addition or retype is
/// a hole — NOT derivable — and the whole edge is a chain gap by design
/// (`None`).
fn derive_plan(old: &[(String, String)], new: &[(String, String)]) -> Option<MigrationPlan> {
    let removed: Vec<&(String, String)> = old
        .iter()
        .filter(|(n, _)| !new.iter().any(|(nn, _)| nn == n))
        .collect();
    let added: Vec<&(String, String)> = new
        .iter()
        .filter(|(n, _)| !old.iter().any(|(on, _)| on == n))
        .collect();
    // Retyped field (same name, different rendered type): not derivable.
    if old
        .iter()
        .any(|(n, t)| new.iter().any(|(nn, nt)| nn == n && nt != t))
    {
        return None;
    }
    match (removed.as_slice(), added.as_slice()) {
        ([], []) => Some((vec![], vec![])),
        ([(on, _)], []) => Some((vec![], vec![on.clone()])),
        ([(on, ot)], [(nn, nt)]) if ot == nt => Some((vec![(on.clone(), nn.clone())], vec![])),
        _ => None,
    }
}

/// `fun(V_Old) -> call 'ridge_rt':'derive_record_migration'(V_Old, Renames, Removed, NewTag)`
fn derived_migration_fun(
    rm: &crate::record_meta::RecordMeta,
    renames: &[(String, String)],
    removed: &[String],
) -> CErlExpr {
    let atom = |s: &str| CErlExpr::Lit(CErlLit::Atom(CErlAtom(s.into())));
    let rename_list = CErlExpr::ListLit(
        renames
            .iter()
            .map(|(f, t)| CErlExpr::Tuple(vec![atom(f), atom(t)]))
            .collect(),
    );
    let removed_list = CErlExpr::ListLit(removed.iter().map(|f| atom(f)).collect());
    let tag = CErlExpr::Tuple(vec![
        atom(&rm.fqn),
        atom(&rm.name),
        #[allow(clippy::cast_possible_wrap)]
        CErlExpr::Lit(CErlLit::Int(rm.version as i64)),
    ]);
    CErlExpr::Fun {
        params: vec![CErlVar("V_Old".into())],
        body: Box::new(CErlExpr::Call {
            module: CErlAtom("ridge_rt".into()),
            fn_name: CErlAtom("derive_record_migration".into()),
            args: vec![
                CErlExpr::Var(CErlVar("V_Old".into())),
                rename_list,
                removed_list,
                tag,
            ],
        }),
    }
}

pub(crate) fn lower_module_all(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    module_path: &[&str],
) -> Result<(CErlModule, Vec<CErlModule>), CodegenError> {
    let beam_name = mangle_module_name(module_path, m.id)?;
    lower_module_all_named(m, ws, &beam_name)
}

/// Lower a [`LoweredModule`] like [`lower_module_all`], but with the BEAM
/// module name supplied directly (already FQN-derived by the caller) instead
/// of being mangled from path segments.
pub(crate) fn lower_module_all_named(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    beam_name: &str,
) -> Result<(CErlModule, Vec<CErlModule>), CodegenError> {
    let main_module = lower_module_with_name(m, ws, beam_name)?;

    // Collect actor sub-modules.
    // Rebuild fn_arity to pass to lower_actor so handlers can reference
    // module-level fns and constants via SymbolRef::Local.
    // Include @ffi stubs (IrItem::Ffi) so actors can reference them too.
    let mut fn_arity_for_actors: FxHashMap<String, u32> = FxHashMap::default();
    for item in &m.items {
        match item {
            IrItem::Fn(fn_) => {
                #[allow(clippy::cast_possible_truncation)]
                let arity = fn_.params.len() as u32;
                fn_arity_for_actors.insert(fn_.name.clone(), arity);
            }
            IrItem::Const(c) => {
                fn_arity_for_actors.insert(c.name.clone(), 0);
            }
            IrItem::Ffi(ffi) => {
                #[allow(clippy::cast_possible_truncation)]
                let arity = ffi.params.len() as u32;
                fn_arity_for_actors.insert(ffi.name.clone(), arity);
            }
            _ => {}
        }
    }
    let mut actor_modules = Vec::new();
    for item in &m.items {
        if let IrItem::Actor(actor) = item {
            let tables = crate::scope::CodegenTables::from_workspace(ws);
            let mut actor_module = lower_actor(actor, beam_name, &fn_arity_for_actors, &tables)?;
            normalise_module(&mut actor_module);
            actor_modules.push(actor_module);
        }
    }

    Ok((main_module, actor_modules))
}

// ── @ffi wrapper emission ─────────────────────────────────────────────────────

/// Emit a thin wrapper `CErlFn` for an `IrItem::Ffi` stub.
///
/// The generated Core Erlang looks like:
/// ```text
/// 'truncate'/1 =
///   fun (V_P0) ->
///     call 'erlang':'trunc' (V_P0)
/// ```
///
/// This makes the function available in the module so that same-module
/// `SymbolRef::Local` calls do not produce "undefined function X/N" from
/// `erlc +from_core`.
fn lower_ffi_wrapper(ffi: &IrFfiFn) -> CErlFn {
    // Build param variable names: V_P0, V_P1, … matching the Ridge param count.
    let params: Vec<CErlVar> = ffi
        .params
        .iter()
        .map(|p| CErlVar(format!("V_{}", p.to_uppercase().replace('-', "_"))))
        .collect();

    // Forward only the first `ffi_call_arity` params to the foreign call.
    // This handles the Ridge convention where 0-arity foreign functions are
    // wrapped with a dummy `_unit: Unit` Ridge param — e.g.
    //   `@ffi("maps","new",0) fn _mapsNew (_unit: Unit)` emits
    //   `fun(V_P0) -> call 'maps':'new'()` — discarding the dummy arg.
    let call_args: Vec<CErlExpr> = params
        .iter()
        .take(ffi.ffi_call_arity as usize)
        .map(|v| CErlExpr::Var(v.clone()))
        .collect();

    let body = CErlExpr::Call {
        module: CErlAtom(ffi.ffi_module.clone()),
        fn_name: CErlAtom(ffi.ffi_fn.clone()),
        args: call_args,
    };

    #[allow(clippy::cast_possible_truncation)]
    let arity = ffi.params.len() as u32;

    CErlFn {
        name: CErlAtom(ffi.name.clone()),
        arity,
        anns: vec![],
        body: CErlExpr::Fun {
            params,
            body: Box::new(body),
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::Span;
    use ridge_ir::{
        CapabilitySet, IrConst, IrExpr, IrFn, IrItem, IrLit, IrNodeId, IrParam, LoweredModule,
        LoweredWorkspace, ModuleId, NodeId, Scheme, Type,
    };
    use rustc_hash::FxHashMap;

    fn sp() -> Span {
        Span::point(0)
    }

    fn lit_unit() -> IrExpr {
        IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Unit,
            span: sp(),
        }
    }

    fn lit_int(n: i64) -> IrExpr {
        IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Int(n),
            span: sp(),
        }
    }

    fn make_fn(name: &str, is_pub: bool, is_main: bool, params: Vec<IrParam>) -> IrFn {
        IrFn {
            name: name.into(),
            module: ModuleId(0),
            params,
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_unit(),
            origin: NodeId(0),
            span: sp(),
            is_pub,
            is_main,
            doc: None,
        }
    }

    fn make_const(name: &str, is_pub: bool, value: IrExpr) -> IrConst {
        IrConst {
            name: name.into(),
            ty: Type::Error,
            value,
            origin: NodeId(0),
            span: sp(),
            is_pub,
        }
    }

    fn make_module(id: u32, items: Vec<IrItem>) -> LoweredModule {
        LoweredModule::new(ModuleId(id), items, vec![], FxHashMap::default())
    }

    fn empty_ws() -> LoweredWorkspace {
        LoweredWorkspace::empty(1, 0)
    }

    // ── mangle_module_name tests ──────────────────────────────────────────────

    #[test]
    fn beam_name_uses_fqn_not_module_id() {
        // Same FQN, two different ModuleIds → identical beam name. This is the
        // stability contract hot code reloading depends on: names must not
        // shift when modules are added, removed, or reordered.
        let a = mangle_module_name(&["blog_engine", "models"], ModuleId(3)).unwrap();
        let b = mangle_module_name(&["blog_engine", "models"], ModuleId(17)).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, "ridge_blog_engine_models");
    }

    #[test]
    fn beam_name_for_fqn_splits_dotted_path() {
        let name = beam_name_for_fqn("acme.domain.Models", ModuleId(0)).unwrap();
        assert_eq!(name, "ridge_acme_domain_Models");
    }

    #[test]
    fn mangle_happy_path() {
        let result = mangle_module_name(&["examples", "log_analyzer"], ModuleId(0)).unwrap();
        assert_eq!(result, "ridge_examples_log_analyzer");
    }

    #[test]
    fn mangle_single_segment() {
        let result = mangle_module_name(&["main"], ModuleId(0)).unwrap();
        assert_eq!(result, "ridge_main");
    }

    #[test]
    fn mangle_rejects_rt_collision() {
        // ["rt"] → "ridge_rt" → E006.
        let err = mangle_module_name(&["rt"], ModuleId(1)).unwrap_err();
        assert!(
            matches!(
                err,
                CodegenError::BeamModuleNameCollision { ref mangled, .. }
                if mangled == "ridge_rt"
            ),
            "expected E006 BeamModuleNameCollision, got {err:?}"
        );
    }

    // ── lower_module tests ────────────────────────────────────────────────────

    #[test]
    fn lower_module_pub_fn_exported() {
        let items = vec![IrItem::Fn(make_fn("do_work", true, false, vec![]))];
        let m = make_module(0, items);
        let ws = empty_ws();
        let result = lower_module(&m, &ws, &["examples", "work"]).unwrap();

        assert_eq!(result.exports.len(), 1);
        assert_eq!(result.exports[0].name.0, "do_work");
        assert_eq!(result.exports[0].arity, 0);
    }

    #[test]
    fn lower_module_private_fn_not_exported() {
        let items = vec![IrItem::Fn(make_fn("helper", false, false, vec![]))];
        let m = make_module(0, items);
        let ws = empty_ws();
        let result = lower_module(&m, &ws, &["examples", "work"]).unwrap();

        assert!(
            result.exports.is_empty(),
            "private fn must not appear in exports"
        );
        assert_eq!(result.fns.len(), 1, "fn must still be emitted");
    }

    #[test]
    fn lower_module_main_fn_exported_even_when_private() {
        // §4.26: is_main adds to exports regardless of is_pub.
        let params = vec![IrParam {
            name: "args".into(),
            ty: Type::Error,
            span: sp(),
        }];
        let items = vec![IrItem::Fn(make_fn("main", false, true, params))];
        let m = make_module(0, items);
        let ws = empty_ws();
        let result = lower_module(&m, &ws, &["app", "main"]).unwrap();

        assert_eq!(result.exports.len(), 1);
        assert_eq!(result.exports[0].name.0, "main");
        assert_eq!(result.exports[0].arity, 1);
    }

    #[test]
    fn lower_module_const_zero_arity_exported_if_pub() {
        let items = vec![IrItem::Const(make_const("timeout", true, lit_int(5000)))];
        let m = make_module(0, items);
        let ws = empty_ws();
        let result = lower_module(&m, &ws, &["cfg"]).unwrap();

        assert_eq!(result.exports.len(), 1);
        assert_eq!(result.exports[0].name.0, "timeout");
        assert_eq!(result.exports[0].arity, 0);
    }

    #[test]
    fn lower_module_const_private_not_exported() {
        let items = vec![IrItem::Const(make_const(
            "internal_limit",
            false,
            lit_int(10),
        ))];
        let m = make_module(0, items);
        let ws = empty_ws();
        let result = lower_module(&m, &ws, &["cfg"]).unwrap();

        assert!(
            result.exports.is_empty(),
            "private const must not be exported"
        );
        assert_eq!(result.fns.len(), 1, "const fn must still be emitted");
    }

    #[test]
    fn lower_module_mixed_items() {
        // One pub fn, one private fn, one const, one main fn.
        let params_main = vec![IrParam {
            name: "args".into(),
            ty: Type::Error,
            span: sp(),
        }];
        let items = vec![
            IrItem::Fn(make_fn("process", true, false, vec![])), // pub → exported
            IrItem::Fn(make_fn("_internal", false, false, vec![])), // private → not exported
            IrItem::Const(make_const("version", true, lit_int(1))), // pub const → exported
            IrItem::Fn(make_fn("main", false, true, params_main)), // main → exported
        ];
        let m = make_module(0, items);
        let ws = empty_ws();
        let result = lower_module(&m, &ws, &["app"]).unwrap();

        // 3 exported: process/0, version/0, main/1.
        assert_eq!(result.exports.len(), 3);
        let exported_names: Vec<&str> = result.exports.iter().map(|e| e.name.0.as_str()).collect();
        assert!(
            exported_names.contains(&"process"),
            "process should be exported"
        );
        assert!(
            exported_names.contains(&"version"),
            "version should be exported"
        );
        assert!(exported_names.contains(&"main"), "main should be exported");
        assert!(
            !exported_names.contains(&"_internal"),
            "_internal must not be exported"
        );

        // All 4 fns emitted.
        assert_eq!(result.fns.len(), 4);
    }

    #[test]
    fn lower_module_beam_name_mangled() {
        let m = make_module(0, vec![]);
        let ws = empty_ws();
        let result = lower_module(&m, &ws, &["examples", "log_analyzer"]).unwrap();

        assert_eq!(result.name.0, "ridge_examples_log_analyzer");
    }

    #[test]
    fn lower_module_rt_collision_returns_error() {
        let m = make_module(0, vec![]);
        let ws = empty_ws();
        let err = lower_module(&m, &ws, &["rt"]).unwrap_err();

        assert!(
            matches!(err, CodegenError::BeamModuleNameCollision { .. }),
            "expected E006 error"
        );
    }

    // ── Record version + migration chain exports ──────────────────────────────

    fn user_record_tycons() -> Vec<ridge_types::TyConDecl> {
        use ridge_types::{RecordField, RecordSchema, TyConDecl, TyConId, TyConKind};
        let text = TyConDecl {
            id: TyConId(1),
            name: "Text".to_owned(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        };
        let user = TyConDecl {
            id: TyConId(0),
            name: "User".to_owned(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![RecordField {
                    name: "name".to_owned(),
                    ty: Type::Con(TyConId(1), vec![]),
                }],
            )),
            def_span: None,
            def_module_raw: Some(0),
            opaque: false,
            is_anon: false,
        };
        vec![user, text]
    }

    /// Workspace with one record `User { name: Text }` in module 0, beam name
    /// `ridge_app_models`, fqn "app.models", empty history.
    fn fixture_workspace_with_user_record(items: Vec<IrItem>) -> (LoweredModule, LoweredWorkspace) {
        let mut ws = LoweredWorkspace::empty(1, 2);
        ws.tycons = user_record_tycons();
        ws.target_names = vec!["ridge_app_models".to_owned()];
        ws.module_fqns = vec!["app.models".to_owned()];
        (make_module(0, items), ws)
    }

    /// Adds a two-entry history for `User` and one user hook covering @1
    /// (`from_hash` Some(111)); @2 (hash 222) drops a field, which yields a
    /// derived edge.
    fn fixture_workspace_with_user_record_and_history() -> (LoweredModule, LoweredWorkspace) {
        use ridge_types::history::VersionEntry;
        let mig = ridge_ir::item::IrMigration {
            owner: ridge_types::TyConId(0),
            from_ordinal: 1,
            from_hash: Some(111),
            param_name: "old".into(),
            body: lit_unit(),
            span: sp(),
        };
        let (m, mut ws) = fixture_workspace_with_user_record(vec![IrItem::Migration(mig)]);
        ws.version_history.records.insert(
            ("app.models".to_owned(), "User".to_owned()),
            vec![
                VersionEntry {
                    ordinal: 1,
                    hash: 111,
                    shape: vec![("name".to_owned(), "Text".to_owned())],
                },
                VersionEntry {
                    ordinal: 2,
                    hash: 222,
                    shape: vec![
                        ("name".to_owned(), "Text".to_owned()),
                        ("nick".to_owned(), "Text".to_owned()),
                    ],
                },
            ],
        );
        (m, ws)
    }

    #[test]
    fn module_with_records_exports_version_accessors() {
        let (m, ws) = fixture_workspace_with_user_record(vec![]);
        let module = lower_module_all_named(&m, &ws, "ridge_app_models")
            .expect("lower")
            .0;
        for wanted in ["__ridge_record_versions", "__ridge_record_migrations"] {
            assert!(
                module.exports.iter().any(|e| e.name.0 == wanted && e.arity == 0),
                "missing export {wanted}/0"
            );
        }
    }

    #[test]
    fn migrations_fn_lists_user_edges_then_derived() {
        let (m, ws) = fixture_workspace_with_user_record_and_history();
        let module = lower_module_all_named(&m, &ws, "ridge_app_models")
            .expect("lower")
            .0;
        let f = module
            .fns
            .iter()
            .find(|f| f.name.0 == "__ridge_record_migrations")
            .expect("migrations fn");
        let text = format!("{f:?}");
        assert!(text.contains("111"), "user edge keyed by from_hash: {text}");
        assert!(text.contains("222"), "derived edge keyed by history hash: {text}");
        assert!(
            text.contains("derive_record_migration"),
            "derived edge delegates to the runtime: {text}"
        );
        let user_pos = text.find("111").expect("user edge");
        let derived_pos = text.find("222").expect("derived edge");
        assert!(
            user_pos < derived_pos,
            "user edges come before derived edges: {text}"
        );
    }

    #[test]
    fn module_without_records_has_no_migration_exports() {
        let items = vec![IrItem::Fn(make_fn("do_work", true, false, vec![]))];
        let m = make_module(0, items);
        let ws = empty_ws();
        let module = lower_module_all_named(&m, &ws, "ridge_app_main")
            .expect("lower")
            .0;
        assert!(
            !module
                .exports
                .iter()
                .any(|e| e.name.0 == "__ridge_record_migrations")
        );
        assert!(
            !module
                .exports
                .iter()
                .any(|e| e.name.0 == "__ridge_record_versions")
        );
    }
}
