//! Implicit prelude type bindings (T4).
//!
//! [`prelude_types`] wires the `Scheme`s and `TyConId`s for every name injected
//! by Phase 3's [`ridge_resolve::prelude_resolutions`].  The two maps it returns are consumed by
//! T6's inference engine to give types to unqualified prelude references.
//!
//! # Design
//!
//! - **Value map** (`FxHashMap<String, Scheme>`): constructor names that have
//!   function types — `Some`, `None`, `Ok`, `Err`.
//! - **`TyCon` map** (`FxHashMap<String, TyConId>`): type names that resolve to
//!   built-in `TyCons` — `Int`, `Float`, `Bool`, `Text`, `List`, `Map`, `Set`,
//!   `Option`, `Result`.
//!
//! `Option` and `Result` appear only in the tycon map (they are type-level names
//! in Ridge, not value-level names; their constructors `Some`/`None`/`Ok`/`Err`
//! carry the value-level schemes).
//!
//! # Note on `Json`
//!
//! `Json` is injected by Phase 3 as a `Binding::ModuleAlias` (pointing to
//! `std.json`), not as a `TyCon`.  `BuiltinTyCons` has no `json` field, so `Json`
//! has no `TyConId` entry here.  The property test (test 6) skips `ModuleAlias`
//! bindings that have no corresponding `TyCon` — this is tracked as a known T4
//! drift item pending a Phase-5/7 `Json` `TyCon` definition.
//!
//! # Stability variables
//!
//! The bound `TyVid`s in each scheme use the same small fixed indices (`0`, `1`)
//! as the schema-level placeholders in [`BuiltinTyCons::allocate`].  They are
//! stable generalised slots — not unification variables — and are replaced by
//! fresh variables at each instantiation call (T6).

use ridge_types::{BuiltinTyCons, CapRow, CapabilitySet, Scheme, TyConId, TyVid, Type};
use rustc_hash::FxHashMap;

#[cfg(test)]
use ridge_resolve::{prelude_resolutions, Binding};

// ── Stable TyVid slots ────────────────────────────────────────────────────────
//
// Mirror the schema-level TyVid(0)/TyVid(1) placeholders used in
// `BuiltinTyCons::allocate` (builtins.rs §4.3).  These are generalised variables
// in the Scheme body, always replaced by fresh TyVids on instantiation (T6).

const A: TyVid = TyVid(0); // first type parameter (e.g. `a` in `Option a`)
const E: TyVid = TyVid(1); // second type parameter (e.g. `e` in `Result a e`)

// ── Helper builders ───────────────────────────────────────────────────────────

/// Builds `Type::Con(id, args)`.
#[inline]
const fn ty_con(id: TyConId, args: Vec<Type>) -> Type {
    Type::Con(id, args)
}

/// Builds a pure (empty-caps) function type.
#[inline]
fn ty_fn_pure(params: Vec<Type>, ret: Type) -> Type {
    Type::Fn {
        params,
        ret: Box::new(ret),
        caps: CapRow::Concrete(CapabilitySet::PURE),
    }
}

/// Builds a `Scheme` with one universally-quantified type variable.
#[inline]
fn poly1(var: TyVid, ty: Type) -> Scheme {
    Scheme {
        vars: vec![var],
        cap_vars: vec![],
        ty,
    }
}

/// Builds a `Scheme` with two universally-quantified type variables.
#[inline]
fn poly2(v0: TyVid, v1: TyVid, ty: Type) -> Scheme {
    Scheme {
        vars: vec![v0, v1],
        cap_vars: vec![],
        ty,
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Returns `(value_bindings, tycon_bindings)` for the implicit prelude.
///
/// **Value bindings** — constructor schemes for `Some`, `None`, `Ok`, `Err`:
/// - `Some : ∀ a. (a) -> Option a`
/// - `None : ∀ a. () -> Option a`
/// - `Ok   : ∀ a e. (a) -> Result a e`
/// - `Err  : ∀ a e. (e) -> Result a e`
///
/// **`TyCon` bindings** — maps each prelude type name to its [`TyConId`]:
/// `Int`, `Float`, `Bool`, `Text`, `List`, `Map`, `Set`,
/// `Option`, `Result`.
///
/// The returned maps are intended to seed the inference context in T6.
#[must_use]
pub fn prelude_types(b: &BuiltinTyCons) -> (FxHashMap<String, Scheme>, FxHashMap<String, TyConId>) {
    // ── Constructor value schemes ─────────────────────────────────────────────

    // Some : ∀ a. (a) -> Option a
    let scheme_some = poly1(
        A,
        ty_fn_pure(vec![Type::Var(A)], ty_con(b.option, vec![Type::Var(A)])),
    );

    // None : ∀ a. () -> Option a
    // Nullary constructor: no parameters.
    let scheme_none = poly1(A, ty_fn_pure(vec![], ty_con(b.option, vec![Type::Var(A)])));

    // Ok : ∀ a e. (a) -> Result a e
    // Result a e — TyVid(0)=a is the Ok-payload, TyVid(1)=e is the Err-payload
    // (matching the arena allocation order in BuiltinTyCons::allocate).
    let scheme_ok = poly2(
        A,
        E,
        ty_fn_pure(
            vec![Type::Var(A)],
            ty_con(b.result, vec![Type::Var(A), Type::Var(E)]),
        ),
    );

    // Err : ∀ a e. (e) -> Result a e
    let scheme_err = poly2(
        A,
        E,
        ty_fn_pure(
            vec![Type::Var(E)],
            ty_con(b.result, vec![Type::Var(A), Type::Var(E)]),
        ),
    );

    let mut values: FxHashMap<String, Scheme> = FxHashMap::default();
    values.insert("Some".to_string(), scheme_some);
    values.insert("None".to_string(), scheme_none);
    values.insert("Ok".to_string(), scheme_ok);
    values.insert("Err".to_string(), scheme_err);

    // ── TyCon bindings ────────────────────────────────────────────────────────
    //
    // Every name that Phase 3 injects as a module alias for the pure-data
    // stdlib modules (Int, Float, Bool, Text, List, Map, Set) maps to the
    // corresponding BuiltinTyCons field so the type system can resolve them.
    //
    // `Json` has no TyConId in BuiltinTyCons (it is a module alias only);
    // it is intentionally omitted here — see the module-level note.
    //
    // Option and Result are type-level names; their value-level constructors
    // are in the values map above.

    let mut tycons: FxHashMap<String, TyConId> = FxHashMap::default();
    tycons.insert("Int".to_string(), b.int);
    tycons.insert("Float".to_string(), b.float);
    tycons.insert("Bool".to_string(), b.bool);
    tycons.insert("Text".to_string(), b.text);
    tycons.insert("List".to_string(), b.list);
    tycons.insert("Map".to_string(), b.map);
    tycons.insert("Set".to_string(), b.set);
    tycons.insert("Option".to_string(), b.option);
    tycons.insert("Result".to_string(), b.result);
    tycons.insert("Handle".to_string(), b.handle);
    // Stdlib record types — pre-allocated in BuiltinTyCons (§3.11, §3.12, §3.16).
    // These are registered here so that `ast_type_to_ridge_type` can resolve
    // `Error`, `Duration`, and `ProcOutput` in stdlib `.rg` type annotations.
    tycons.insert("Error".to_string(), b.error);
    tycons.insert("Duration".to_string(), b.duration);
    tycons.insert("ProcOutput".to_string(), b.proc_output);

    (values, tycons)
}

/// Looks up a constructor scheme from the implicit prelude value map.
///
/// Returns `Some(scheme)` for `Some`, `None`, `Ok`, `Err`; `None` otherwise.
#[must_use]
pub fn lookup_prelude(b: &BuiltinTyCons, name: &str) -> Option<Scheme> {
    let (values, _) = prelude_types(b);
    values.into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// Looks up a type-constructor id from the implicit prelude tycon map.
///
/// Returns `Some(id)` for `Int`, `Float`, `Bool`, `Text`, `List`, `Map`,
/// `Set`, `Option`, `Result`; `None` for anything else.
#[must_use]
pub fn lookup_prelude_tycon(b: &BuiltinTyCons, name: &str) -> Option<TyConId> {
    let (_, tycons) = prelude_types(b);
    tycons.into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// Returns the [`ridge_types::UnionSchema`] for a prelude union `TyCon`
/// (`Option` or `Result`).
///
/// This is used by T9's pattern-matching dispatch to retrieve the canonical
/// schema for prelude union types without access to a full `TyConArena`.
///
/// # Panics (debug only)
///
/// Panics in debug builds if `id` is neither `b.option` nor `b.result`.
#[must_use]
pub fn get_prelude_union_schema(b: &BuiltinTyCons, id: TyConId) -> ridge_types::UnionSchema {
    use ridge_types::{TyVid, Type, UnionSchema, UnionVariant, VariantPayload};

    if id == b.option {
        // Option a = Some a | None
        UnionSchema {
            params: vec![TyVid(0)],
            variants: vec![
                UnionVariant {
                    name: "Some".to_string(),
                    kind: VariantPayload::Positional(vec![Type::Var(TyVid(0))]),
                },
                UnionVariant {
                    name: "None".to_string(),
                    kind: VariantPayload::Nullary,
                },
            ],
        }
    } else if id == b.result {
        // Result a e = Ok a | Err e
        UnionSchema {
            params: vec![TyVid(0), TyVid(1)],
            variants: vec![
                UnionVariant {
                    name: "Ok".to_string(),
                    kind: VariantPayload::Positional(vec![Type::Var(TyVid(0))]),
                },
                UnionVariant {
                    name: "Err".to_string(),
                    kind: VariantPayload::Positional(vec![Type::Var(TyVid(1))]),
                },
            ],
        }
    } else {
        debug_assert!(
            false,
            "get_prelude_union_schema called with non-prelude TyConId {id:?}"
        );
        // Fallback: empty schema to avoid panic in release builds.
        UnionSchema {
            params: vec![],
            variants: vec![],
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_types::TyConArena;

    fn make_builtins() -> BuiltinTyCons {
        let mut arena = TyConArena::new();
        BuiltinTyCons::allocate(&mut arena)
    }

    // ── Test 1: Some scheme exact shape ──────────────────────────────────────
    //
    // `Some : ∀ a. (a) -> Option a`
    // Scheme { vars: [TyVid(0)], cap_vars: [], ty: Fn { params: [Var(0)],
    //          ret: Con(option, [Var(0)]), caps: Concrete(PURE) } }

    #[test]
    fn some_scheme_exact_shape() {
        let b = make_builtins();
        let scheme = lookup_prelude(&b, "Some").expect("Some must be in prelude");

        // vars: exactly [TyVid(0)]
        assert_eq!(scheme.vars, vec![TyVid(0)], "Some must be ∀ a");
        assert!(scheme.cap_vars.is_empty(), "Some has no cap vars");

        match &scheme.ty {
            Type::Fn { params, ret, caps } => {
                // params: [Var(a)]
                assert_eq!(params.len(), 1, "Some takes 1 argument");
                assert!(
                    matches!(&params[0], Type::Var(TyVid(0))),
                    "Some param must be Var(0)"
                );
                // ret: Option a = Con(b.option, [Var(0)])
                match ret.as_ref() {
                    Type::Con(id, args) => {
                        assert_eq!(*id, b.option, "Some ret must be Option");
                        assert_eq!(args.len(), 1, "Option takes 1 arg");
                        assert!(
                            matches!(&args[0], Type::Var(TyVid(0))),
                            "Option arg must be Var(0)"
                        );
                    }
                    other => panic!("expected Con for Option, got: {other:?}"),
                }
                // caps: Concrete(PURE)
                assert_eq!(
                    *caps,
                    CapRow::Concrete(CapabilitySet::PURE),
                    "Some must be pure"
                );
            }
            other => panic!("Some ty must be Fn, got: {other:?}"),
        }
    }

    // ── Test 2: Int maps to b.int ─────────────────────────────────────────────

    #[test]
    fn int_tycon_is_b_int() {
        let b = make_builtins();
        let id = lookup_prelude_tycon(&b, "Int").expect("Int must be in prelude tycons");
        assert_eq!(id, b.int);
    }

    // ── Test 3: List maps to b.list ───────────────────────────────────────────

    #[test]
    fn list_tycon_is_b_list() {
        let b = make_builtins();
        let id = lookup_prelude_tycon(&b, "List").expect("List must be in prelude tycons");
        assert_eq!(id, b.list);
    }

    // ── Test 4: Option maps to b.option ──────────────────────────────────────

    #[test]
    fn option_tycon_is_b_option() {
        let b = make_builtins();
        let id = lookup_prelude_tycon(&b, "Option").expect("Option must be in prelude tycons");
        assert_eq!(id, b.option);
    }

    // ── Test 5: unknown name returns None ─────────────────────────────────────

    #[test]
    fn bogus_returns_none_from_value_map() {
        let b = make_builtins();
        assert!(
            lookup_prelude(&b, "Bogus").is_none(),
            "Bogus must not be in the prelude value map"
        );
    }

    // ── Test 6: property — StdlibSymbol prelude names have a prelude entry ────
    //
    // Walks prelude_resolutions() and asserts that every name bound as a
    // StdlibSymbol (Option, Some, None, Result, Ok, Err) has an entry in
    // either the value map or the tycon map.
    //
    // ModuleAlias bindings (Int, Float, Bool, Text, List, Map, Set, Json) are
    // also checked, but Json is excluded because it has no TyConId in
    // BuiltinTyCons (see module-level note).  All other module-alias names DO
    // have TyConId entries in the tycon map.

    #[test]
    fn all_prelude_resolution_names_covered() {
        // Names known to lack a TyConId in the current BuiltinTyCons.
        // These are module-alias-only names that will be addressed in a future
        // phase when a Json TyCon is added.
        const KNOWN_MODULE_ALIAS_ONLY: &[&str] = &["Json"];
        let b = make_builtins();
        let (values, tycons) = prelude_types(&b);
        let resolutions = prelude_resolutions();

        for ir in &resolutions {
            for eb in &ir.effective_bindings {
                let name = &eb.local_name;

                // Skip bindings that are known module-alias-only (no TyConId yet).
                if KNOWN_MODULE_ALIAS_ONLY.contains(&name.as_str()) {
                    // Verify it really is a ModuleAlias binding (not accidentally
                    // a StdlibSymbol that we're incorrectly skipping).
                    assert!(
                        matches!(&eb.binding, Binding::ModuleAlias { .. }),
                        "'{name}' is in KNOWN_MODULE_ALIAS_ONLY but is not a ModuleAlias"
                    );
                    continue;
                }

                let in_values = values.contains_key(name.as_str());
                let in_tycons = tycons.contains_key(name.as_str());
                assert!(
                    in_values || in_tycons,
                    "prelude name '{name}' has no entry in prelude_types()"
                );
            }
        }
    }

    // ── Extra: None scheme shape ──────────────────────────────────────────────

    #[test]
    fn none_scheme_has_no_params() {
        let b = make_builtins();
        let scheme = lookup_prelude(&b, "None").expect("None must be in prelude");
        assert_eq!(scheme.vars, vec![TyVid(0)], "None must be ∀ a");
        match &scheme.ty {
            Type::Fn { params, ret, caps } => {
                assert!(params.is_empty(), "None takes no arguments");
                assert!(
                    matches!(ret.as_ref(), Type::Con(id, _) if *id == b.option),
                    "None ret must be Option _"
                );
                assert_eq!(*caps, CapRow::Concrete(CapabilitySet::PURE));
            }
            other => panic!("None ty must be Fn, got: {other:?}"),
        }
    }

    // ── Extra: Ok/Err scheme shapes ───────────────────────────────────────────

    #[test]
    fn ok_scheme_has_two_vars_and_result_ret() {
        let b = make_builtins();
        let scheme = lookup_prelude(&b, "Ok").expect("Ok must be in prelude");
        assert_eq!(scheme.vars.len(), 2, "Ok must be ∀ a e");
        match &scheme.ty {
            Type::Fn { params, ret, caps } => {
                assert_eq!(params.len(), 1, "Ok takes 1 argument");
                assert!(
                    matches!(ret.as_ref(), Type::Con(id, args) if *id == b.result && args.len() == 2),
                    "Ok ret must be Result _ _"
                );
                assert_eq!(*caps, CapRow::Concrete(CapabilitySet::PURE));
            }
            other => panic!("Ok ty must be Fn, got: {other:?}"),
        }
    }

    #[test]
    fn err_scheme_has_two_vars_and_result_ret() {
        let b = make_builtins();
        let scheme = lookup_prelude(&b, "Err").expect("Err must be in prelude");
        assert_eq!(scheme.vars.len(), 2, "Err must be ∀ a e");
        match &scheme.ty {
            Type::Fn { params, ret, .. } => {
                assert_eq!(params.len(), 1, "Err takes 1 argument");
                assert!(
                    matches!(ret.as_ref(), Type::Con(id, args) if *id == b.result && args.len() == 2),
                    "Err ret must be Result _ _"
                );
            }
            other => panic!("Err ty must be Fn, got: {other:?}"),
        }
    }

    // ── Extra: all 7 primitive/builtin tycons present ─────────────────────────
    // (Int, Float, Bool, Text, List, Map, Set — not Json which has no TyConId)

    #[test]
    fn seven_primitive_tycons_present() {
        let b = make_builtins();
        for name in &["Int", "Float", "Bool", "Text", "List", "Map", "Set"] {
            assert!(
                lookup_prelude_tycon(&b, name).is_some(),
                "tycon '{name}' missing from prelude"
            );
        }
    }

    // ── Extra: lookup by wrong case returns None ──────────────────────────────

    #[test]
    fn lowercase_some_is_not_in_prelude() {
        let b = make_builtins();
        assert!(lookup_prelude(&b, "some").is_none());
        assert!(lookup_prelude(&b, "none").is_none());
        assert!(lookup_prelude(&b, "ok").is_none());
        assert!(lookup_prelude(&b, "err").is_none());
    }
}
