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
use crate::core_ast::{CErlAtom, CErlExport, CErlExpr, CErlFn, CErlModule, CErlVar};
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
pub(crate) fn mangle_module_name(
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

    let mut module = CErlModule {
        name: CErlAtom(beam_name.into()),
        exports,
        attributes: vec![],
        fns,
    };
    // ANF normalisation: hoist non-atomic arguments in call/apply/case positions
    // so that `erlc` does not reject the emitted Core Erlang with "illegal expression".
    normalise_module(&mut module);
    Ok(module)
}

/// Lower a [`LoweredModule`] to the main [`CErlModule`] **plus** all actor
/// sub-modules.
///
/// Returns `(main_module, actor_modules)`.  The actor modules must each be
/// compiled separately by `erlc +from_core` (they are separate BEAM modules).
///
/// This is the T9 entry point wired into the workspace-level codegen.
/// `lower_module` (the original T8 entry point) remains available for
/// snapshot tests and the LSP hot-path that only needs the main module.
pub(crate) fn lower_module_all(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    module_path: &[&str],
) -> Result<(CErlModule, Vec<CErlModule>), CodegenError> {
    let beam_name = mangle_module_name(module_path, m.id)?;
    let main_module = lower_module_with_name(m, ws, &beam_name)?;

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
            let mut actor_module = lower_actor(actor, &beam_name, &fn_arity_for_actors)?;
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
}
