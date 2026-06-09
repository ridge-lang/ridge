//! Reconciled stdlib type declarations.
//!
//! A handful of stdlib types must be visible across module boundaries — an
//! `import std.m (T)` followed by `(x: T)` has to resolve `T` to a stable
//! `TyConId`, and `import std.m (MkT)` has to type and lower `MkT` as a real
//! constructor. Historically every such type was hand-interned as a built-in
//! (see [`ridge_types::BuiltinTyCons`]) with its constructors hand-listed in the
//! prelude, the lowering pass, and several manifests. That couples each new
//! stdlib data type to a Rust edit in many places.
//!
//! This module reserves a contiguous block of arena ids for stdlib `pub type`
//! declarations that are made available *by declaration* instead of by built-in
//! interning. The block is interned in [`typecheck_workspace`] right after the
//! built-ins and before any user type, so:
//!
//! - the block occupies `[builtins_len, builtins_len + N)`, and `builtins_len`
//!   (computed as `arena.all().len()` after this pass) shifts the user-type
//!   prediction base past it automatically, so user `TyConId`s land after the
//!   reserved block with no other change;
//! - references between reconciled types name `TyConId(base + offset)` and are
//!   stable because the order is fixed.
//!
//! The decl table here is the single source of truth the type checker consumes
//! at runtime. During the standard library's *own* build the source `.ridge`
//! declarations are authoritative, so the reservation is skipped there (see the
//! `is_stdlib` guard at the call site); a consistency test compares the two so
//! the table cannot silently drift from the declarations it mirrors.
//!
//! [`typecheck_workspace`]: crate::typecheck_workspace

use ridge_ast::Capability;
use ridge_types::{
    BuiltinTyCons, CapRow, CapabilitySet, Constraint, RecordField, RecordSchema, Scheme,
    TyConArena, TyConDecl, TyConId, TyConKind, TyVid, Type, UnionSchema, UnionVariant,
    VariantPayload,
};
use rustc_hash::FxHashMap;

use crate::class_env::ClassTable;

/// Intern the reconciled stdlib type block into `arena` and return its
/// `name -> TyConId` map.
///
/// Must be called immediately after [`BuiltinTyCons::allocate`] and before any
/// user type is collected, so the reserved block is contiguous with the
/// built-ins. The returned map seeds cross-module name resolution for these
/// types (see [`crate::cross_module::imported_tycon_names`]) and identifies the
/// reconciled decls for constructor scheme/lowering lookups.
pub(crate) fn intern_stdlib_types(
    arena: &mut TyConArena,
    b: &BuiltinTyCons,
) -> FxHashMap<String, TyConId> {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "built-in TyCon count is a small constant well under u32::MAX"
    )]
    let base = arena.all().len() as u32;
    let mut names = FxHashMap::default();
    for decl in reconciled_decls(b, base) {
        let name = decl.name.clone();
        let id = arena.intern(decl);
        names.insert(name, id);
    }
    names
}

/// The committed reconciled stdlib type table.
///
/// `base` is the first `TyConId` this block occupies (the arena length right
/// after the built-ins). Self- and cross-references inside the block name
/// `TyConId(base + offset)`, where `offset` is the declaration's position in the
/// returned vector.
fn reconciled_decls(b: &BuiltinTyCons, base: u32) -> Vec<TyConDecl> {
    vec![
        // `std.query` — sort direction for query ordering. A plain nullary union
        // declared in Ridge (stdlib/query.ridge) rather than as a built-in.
        TyConDecl {
            id: TyConId(base),
            name: "SortOrder".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "Asc".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Desc".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.data` — the in-memory adapter handle. An opaque record `{ id: Int }`
        // declared in Ridge (stdlib/data.ridge); the `id` selects the handle's
        // private store. Opaque, so user code reaches it only through `memAdapter`
        // and the `Adapter` methods, never by constructing the record.
        TyConDecl {
            id: TyConId(base + 1),
            name: "MemAdapter".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![RecordField {
                    name: "id".to_string(),
                    ty: Type::Con(b.int, vec![]),
                }],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.repo` — the typed repository handle. A generic opaque record
        // `{ adapter: a, table: Text }` declared in Ridge (stdlib/repo.ridge).
        // The entity `e` (param 0) is phantom — it names what the repository
        // stores without appearing in a field, the same shape as `Quote f`; the
        // adapter `a` (param 1) is the stored connection handle. Opaque, so user
        // code builds one only through `repo` and threads it as a handle.
        TyConDecl {
            id: TyConId(base + 2),
            name: "Repo".to_string(),
            arity: 2,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1)],
                vec![
                    RecordField {
                        name: "adapter".to_string(),
                        ty: Type::Var(TyVid(1)),
                    },
                    RecordField {
                        name: "table".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
    ]
}

/// Build the value scheme for a constructor named `ctor_name` declared by one of
/// the reconciled stdlib types, or `None` if no reconciled type declares it.
///
/// `decls` is the full arena snapshot; `reconciled` maps reconciled type names
/// to their ids, so only those decls are scanned. A union variant `MkT a b` of a
/// type `T p…` yields `∀ p…. (a, b) -> T p…`; a nullary variant yields
/// `() -> T p…`. Record-payload variants and reconciled record auto-constructors
/// are not yet emitted here.
pub(crate) fn reconciled_ctor_scheme(
    decls: &[TyConDecl],
    reconciled: &FxHashMap<String, TyConId>,
    ctor_name: &str,
) -> Option<Scheme> {
    for &tid in reconciled.values() {
        let Some(decl) = decls.get(tid.0 as usize) else {
            continue;
        };
        if let TyConKind::Union(u) = &decl.kind {
            let Some(variant) = u.variants.iter().find(|v| v.name == ctor_name) else {
                continue;
            };
            let params = match &variant.kind {
                VariantPayload::Nullary => vec![],
                VariantPayload::Positional(tys) => tys.clone(),
                // Record-payload variants are constructed with record syntax; a
                // function scheme does not model them. Deferred.
                VariantPayload::Record(_) => return None,
            };
            let ret = Type::Con(decl.id, u.params.iter().map(|&p| Type::Var(p)).collect());
            return Some(Scheme {
                vars: u.params.clone(),
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params,
                    ret: Box::new(ret),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                },
                constraints: vec![],
            });
        }
    }
    None
}

/// Build the value scheme for a stdlib function whose signature references a
/// reconciled type, so the hand-curated `stdlib_signature` table (which only
/// sees [`BuiltinTyCons`]) cannot express it. Returns `None` for any
/// `(module, name)` pair not in the table.
///
/// Keyed on the declaring module as well as the name: `std.repo`'s query verbs
/// (`all`, `get`, `delete`) share names with the `std.data` `Adapter` methods,
/// so a name-only lookup would resolve one module's import to the other's
/// scheme. `classes` supplies the `Adapter`/`Row` class ids the repository
/// methods are constrained over; it is `None` only in contexts without a class
/// table, where those methods cannot be seeded and resolve to `None`.
pub(crate) fn reconciled_fn_scheme(
    module: &str,
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
    classes: Option<&ClassTable>,
) -> Option<Scheme> {
    match (module, name) {
        // std.query `orderSql : ∀f. SortOrder -> Quote f -> Sql` — compiles a
        // quoted ordering key plus a direction into an `ORDER BY` fragment.
        ("std.query", "orderSql") => {
            let sort_order = *reconciled.get("SortOrder")?;
            let f = TyVid(0);
            Some(Scheme {
                vars: vec![f],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![
                        Type::Con(sort_order, vec![]),
                        Type::Con(b.quote, vec![Type::Var(f)]),
                    ],
                    ret: Box::new(Type::Con(b.sql, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                },
                constraints: vec![],
            })
        }
        // std.data `memAdapter : Unit -> MemAdapter` — opens a fresh in-memory
        // adapter. Requires the `db` capability (opening a store is the gated act;
        // the handle returned is the proof of access for the cap-free methods).
        // Its return type names the reconciled `MemAdapter`, so the hand-curated
        // signature table (which only sees `BuiltinTyCons`) cannot express it.
        ("std.data", "memAdapter") => {
            let mem_adapter = *reconciled.get("MemAdapter")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(b.unit, vec![])],
                    ret: Box::new(Type::Con(mem_adapter, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::singleton(Capability::Db)),
                },
                constraints: vec![],
            })
        }
        // std.repo — the typed repository over the `Adapter` seam. Every method
        // takes (or returns) the reconciled `Repo e a`, and the read verbs are
        // constrained over `Adapter a` (to reach the storage primitives) and
        // `Row e` (to decode rows into the entity), so none is expressible in
        // the hand-curated table.
        ("std.repo", _) => reconciled_repo_fn_scheme(name, reconciled, b, classes?),
        _ => None,
    }
}

/// The `std.repo` slice of [`reconciled_fn_scheme`]. Split out so the storage
/// repository's verbs sit together and share the `Repo`/class-id setup.
fn reconciled_repo_fn_scheme(
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
    classes: &ClassTable,
) -> Option<Scheme> {
    let repo_con = *reconciled.get("Repo")?;
    let adapter = classes.id_by_name("Adapter")?;
    let row = classes.id_by_name("Row")?;
    // Scheme-level placeholder vars: entity `e` and adapter `a`. Fresh copies
    // are made on each instantiation, so the fixed ids here are dummies.
    let e = TyVid(0);
    let a = TyVid(1);
    let repo_app = || Type::Con(repo_con, vec![Type::Var(e), Type::Var(a)]);
    let pure = || CapRow::Concrete(CapabilitySet::PURE);
    let result = |ok: Type| Type::Con(b.result, vec![ok, Type::Con(b.error, vec![])]);
    // A list of decoded entities `List e`.
    let list_e = || Type::Con(b.list, vec![Type::Var(e)]);
    // An optional decoded entity `Option e`.
    let option_e = || Type::Con(b.option, vec![Type::Var(e)]);
    // A raw column map `Map Text SqlValue`.
    let map_row = || {
        Type::Con(
            b.map,
            vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
        )
    };
    // A quoted predicate `Quote (e -> Bool)`. The entity `e` is the queried
    // record at the call site; it is pinned from the predicate's parameter
    // annotation when the lambda is captured, exactly as at the adapter seam.
    let quote_pred = || {
        Type::Con(
            b.quote,
            vec![Type::Fn {
                params: vec![Type::Var(e)],
                ret: Box::new(Type::Con(b.bool, vec![])),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            }],
        )
    };
    // Constraint shorthands. Read verbs decode, so they carry `Row e`; the
    // aggregate and write verbs touch only the adapter. The order must mirror
    // the source signatures' constraint order as the type checker stores it —
    // by the order the constrained variables first appear, so the entity `e`
    // (in the predicate / `Repo e a`) precedes the adapter `a`. The lowering
    // prepends one dict parameter per constraint in this order on both the
    // callee (stdlib build) and the call site, so the two must agree.
    let with_adapter = || vec![Constraint::single(adapter, a)];
    let with_adapter_row = || vec![Constraint::single(row, e), Constraint::single(adapter, a)];
    // Assemble a method scheme: `∀e a. params -> ret`, pure, with `constraints`.
    let method = |params: Vec<Type>, ret: Type, constraints: Vec<Constraint>| {
        Some(Scheme {
            vars: vec![e, a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params,
                ret: Box::new(ret),
                caps: pure(),
            },
            constraints,
        })
    };
    match name {
        // repo : ∀e a. a -> Text -> Repo e a — bind a repository to a table.
        "repo" => method(
            vec![Type::Var(a), Type::Con(b.text, vec![])],
            repo_app(),
            vec![],
        ),
        // all : ∀e a. Repo e a -> Result (List e) Error where Adapter a, Row e
        "all" => method(vec![repo_app()], result(list_e()), with_adapter_row()),
        // findBy : ∀e a. Quote (e -> Bool) -> Repo e a
        //               -> Result (List e) Error where Adapter a, Row e
        "findBy" => method(
            vec![quote_pred(), repo_app()],
            result(list_e()),
            with_adapter_row(),
        ),
        // find : ∀e a. Quote (e -> Bool) -> Repo e a
        //             -> Result (Option e) Error where Adapter a, Row e
        "find" => method(
            vec![quote_pred(), repo_app()],
            result(option_e()),
            with_adapter_row(),
        ),
        // getBy : ∀e a. Text -> SqlValue -> Repo e a
        //              -> Result (Option e) Error where Adapter a, Row e
        "getBy" => method(
            vec![
                Type::Con(b.text, vec![]),
                Type::Con(b.sql_value, vec![]),
                repo_app(),
            ],
            result(option_e()),
            with_adapter_row(),
        ),
        // count : ∀e a. Repo e a -> Result Int Error where Adapter a
        "count" => method(
            vec![repo_app()],
            result(Type::Con(b.int, vec![])),
            with_adapter(),
        ),
        // countBy / deleteWhere : ∀e a. Quote (e -> Bool) -> Repo e a
        //   -> Result Int Error where Adapter a. One counts the matching rows,
        //   the other removes them and answers how many — the same scheme.
        "countBy" | "deleteWhere" => method(
            vec![quote_pred(), repo_app()],
            result(Type::Con(b.int, vec![])),
            with_adapter(),
        ),
        // exists : ∀e a. Quote (e -> Bool) -> Repo e a
        //               -> Result Bool Error where Adapter a
        "exists" => method(
            vec![quote_pred(), repo_app()],
            result(Type::Con(b.bool, vec![])),
            with_adapter(),
        ),
        // insertRow : ∀e a. Map Text SqlValue -> Repo e a
        //                  -> Result Unit Error where Adapter a
        "insertRow" => method(
            vec![map_row(), repo_app()],
            result(Type::Con(b.unit, vec![])),
            with_adapter(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtins() -> (TyConArena, BuiltinTyCons) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        (arena, b)
    }

    #[test]
    fn reserved_block_follows_builtins_and_shifts_user_base() {
        let (mut arena, b) = builtins();
        let builtins_len = arena.all().len();
        let names = intern_stdlib_types(&mut arena, &b);

        // SortOrder lands at the first reserved id, immediately after the
        // built-ins, so a subsequent user type would start one slot later.
        let so = names.get("SortOrder").copied().expect("SortOrder interned");
        assert_eq!(so.0 as usize, builtins_len);
        assert_eq!(arena.all().len(), builtins_len + names.len());
    }

    #[test]
    fn sort_order_is_a_two_variant_nullary_union() {
        let (mut arena, b) = builtins();
        let names = intern_stdlib_types(&mut arena, &b);
        let so = names["SortOrder"];
        match &arena.get(so).kind {
            TyConKind::Union(u) => {
                let variants: Vec<&str> = u.variants.iter().map(|v| v.name.as_str()).collect();
                assert_eq!(variants, vec!["Asc", "Desc"]);
                assert!(u.params.is_empty(), "SortOrder takes no type params");
                assert!(
                    u.variants
                        .iter()
                        .all(|v| matches!(v.kind, VariantPayload::Nullary)),
                    "both variants are nullary"
                );
            }
            other => panic!("SortOrder must be a Union, got {other:?}"),
        }
    }

    #[test]
    fn ctor_scheme_is_nullary_returning_the_owner() {
        let (mut arena, b) = builtins();
        let names = intern_stdlib_types(&mut arena, &b);
        let decls = arena.all().to_vec();
        let scheme = reconciled_ctor_scheme(&decls, &names, "Asc").expect("Asc has a ctor scheme");
        assert!(scheme.vars.is_empty());
        match &scheme.ty {
            Type::Fn { params, ret, .. } => {
                assert!(params.is_empty(), "Asc is nullary");
                assert!(
                    matches!(ret.as_ref(), Type::Con(id, args)
                        if *id == names["SortOrder"] && args.is_empty()),
                    "Asc returns SortOrder"
                );
            }
            other => panic!("ctor scheme must be a Fn, got {other:?}"),
        }
        assert!(
            reconciled_ctor_scheme(&decls, &names, "Nope").is_none(),
            "unknown ctor yields no scheme"
        );
    }
}
