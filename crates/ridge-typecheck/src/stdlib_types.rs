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
#[expect(
    clippy::too_many_lines,
    reason = "one literal TyConDecl per reconciled stdlib type; the list reads best kept together"
)]
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
        // `std.data` — connection settings for `connect`. A plain (non-opaque)
        // record users construct directly; declared in Ridge (stdlib/data.ridge).
        // Field order mirrors the source declaration so the consistency check
        // holds.
        TyConDecl {
            id: TyConId(base + 3),
            name: "Config".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "host".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "port".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "database".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "user".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "password".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "sslMode".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "poolSize".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.data` — the Postgres connection handle. Opaque `{ id: Int }`,
        // declared in Ridge (stdlib/data.ridge); the `id` selects the connection
        // in the runtime handle registry, the same id-as-handle shape MemAdapter
        // uses.
        TyConDecl {
            id: TyConId(base + 4),
            name: "Postgres".to_string(),
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
        // `std.repo` — a query under construction over a repository. A generic
        // opaque record declared in Ridge (stdlib/repo.ridge): the repository, the
        // accumulated filter, the ordering as `(ascending?, column)` keys, and the
        // page (`lim`, `off`). Opaque, so user code only threads it through the
        // builder (`query`/`filter`/`orderBy`/`limit`/`offset`) into a terminal.
        // Field order mirrors the source so the consistency check holds.
        TyConDecl {
            id: TyConId(base + 5),
            name: "Query".to_string(),
            arity: 2,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1)],
                vec![
                    RecordField {
                        name: "repo".to_string(),
                        ty: Type::Con(
                            TyConId(base + 2),
                            vec![Type::Var(TyVid(0)), Type::Var(TyVid(1))],
                        ),
                    },
                    RecordField {
                        name: "pred".to_string(),
                        ty: Type::Con(
                            b.quote,
                            vec![Type::Fn {
                                params: vec![Type::Con(
                                    b.map,
                                    vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
                                )],
                                ret: Box::new(Type::Con(b.bool, vec![])),
                                caps: CapRow::Concrete(CapabilitySet::PURE),
                            }],
                        ),
                    },
                    RecordField {
                        name: "orders".to_string(),
                        ty: Type::Con(
                            b.list,
                            vec![Type::Tuple(vec![
                                Type::Con(b.bool, vec![]),
                                Type::Con(b.text, vec![]),
                            ])],
                        ),
                    },
                    RecordField {
                        name: "lim".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "off".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.repo` — a join under construction. A generic opaque record declared
        // in Ridge (stdlib/repo.ridge): the left query, the right repository, and
        // the quoted join condition over both entities. The entity `e` (param 0)
        // is the left side, `f` (param 1) the right, and `a` (param 2) the shared
        // adapter. Opaque, so user code only threads it from `joinOn` into a
        // terminal (`toPairs`/`selectJoin`). Field order mirrors the source.
        TyConDecl {
            id: TyConId(base + 6),
            name: "Join".to_string(),
            arity: 3,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1), TyVid(2)],
                vec![
                    RecordField {
                        name: "left".to_string(),
                        ty: Type::Con(
                            TyConId(base + 5),
                            vec![Type::Var(TyVid(0)), Type::Var(TyVid(2))],
                        ),
                    },
                    RecordField {
                        name: "right".to_string(),
                        ty: Type::Con(
                            TyConId(base + 2),
                            vec![Type::Var(TyVid(1)), Type::Var(TyVid(2))],
                        ),
                    },
                    // The join condition is stored as a captured tree over two
                    // row maps — the same row-map form `Query.pred` uses, not the
                    // entity form the user-facing `joinOn` scheme presents. The
                    // value is a `QExpr` either way; this is the field's static
                    // type, which must mirror the source repo.ridge declaration.
                    RecordField {
                        name: "cond".to_string(),
                        ty: Type::Con(
                            b.quote,
                            vec![Type::Fn {
                                params: vec![
                                    Type::Con(
                                        b.map,
                                        vec![
                                            Type::Con(b.text, vec![]),
                                            Type::Con(b.sql_value, vec![]),
                                        ],
                                    ),
                                    Type::Con(
                                        b.map,
                                        vec![
                                            Type::Con(b.text, vec![]),
                                            Type::Con(b.sql_value, vec![]),
                                        ],
                                    ),
                                ],
                                ret: Box::new(Type::Con(b.bool, vec![])),
                                caps: CapRow::Concrete(CapabilitySet::PURE),
                            }],
                        ),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.repo` — a left (outer) join under construction. Structurally a copy
        // of `Join`: the same left query, right repository, and quoted condition
        // over both entities. A distinct type so the terminal it admits differs —
        // `toLeftPairs` keeps every left row and returns the right side as
        // `Option f`, where `Join`'s `toPairs` returns it as `f`. Field order
        // mirrors the source.
        TyConDecl {
            id: TyConId(base + 7),
            name: "LeftJoin".to_string(),
            arity: 3,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1), TyVid(2)],
                vec![
                    RecordField {
                        name: "left".to_string(),
                        ty: Type::Con(
                            TyConId(base + 5),
                            vec![Type::Var(TyVid(0)), Type::Var(TyVid(2))],
                        ),
                    },
                    RecordField {
                        name: "right".to_string(),
                        ty: Type::Con(
                            TyConId(base + 2),
                            vec![Type::Var(TyVid(1)), Type::Var(TyVid(2))],
                        ),
                    },
                    RecordField {
                        name: "cond".to_string(),
                        ty: Type::Con(
                            b.quote,
                            vec![Type::Fn {
                                params: vec![
                                    Type::Con(
                                        b.map,
                                        vec![
                                            Type::Con(b.text, vec![]),
                                            Type::Con(b.sql_value, vec![]),
                                        ],
                                    ),
                                    Type::Con(
                                        b.map,
                                        vec![
                                            Type::Con(b.text, vec![]),
                                            Type::Con(b.sql_value, vec![]),
                                        ],
                                    ),
                                ],
                                ret: Box::new(Type::Con(b.bool, vec![])),
                                caps: CapRow::Concrete(CapabilitySet::PURE),
                            }],
                        ),
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
        // std.data `connect : Config -> Result Postgres Error` — opens a Postgres
        // connection. Like `memAdapter` it requires the `db` capability, and its
        // signature names the reconciled `Config` and `Postgres`, so the
        // hand-curated signature table (which only sees `BuiltinTyCons`) cannot
        // express it.
        ("std.data", "connect") => {
            let postgres = *reconciled.get("Postgres")?;
            let config = *reconciled.get("Config")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(config, vec![])],
                    ret: Box::new(Type::Con(
                        b.result,
                        vec![Type::Con(postgres, vec![]), Type::Con(b.error, vec![])],
                    )),
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
        // std.query `ascending : SortOrder -> Bool` — projects a sort direction
        // to the `ascending?` boolean the query builder and seam read.
        ("std.query", "ascending") => {
            let sort_order = *reconciled.get("SortOrder")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(sort_order, vec![])],
                    ret: Box::new(Type::Con(b.bool, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                },
                constraints: vec![],
            })
        }
        ("std.repo", _) => reconciled_repo_fn_scheme(name, reconciled, b, classes?),
        _ => None,
    }
}

/// The `std.repo` slice of [`reconciled_fn_scheme`]. Split out so the storage
/// repository's verbs sit together and share the `Repo`/class-id setup.
#[expect(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "one scheme per repository verb and query-builder fn; they read best together, and the single-letter locals mirror the type variables (e, f, a, s)"
)]
fn reconciled_repo_fn_scheme(
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
    classes: &ClassTable,
) -> Option<Scheme> {
    let repo_con = *reconciled.get("Repo")?;
    let query_con = *reconciled.get("Query")?;
    let adapter = classes.id_by_name("Adapter")?;
    let row = classes.id_by_name("Row")?;
    // Scheme-level placeholder vars: entity `e` and adapter `a`. Fresh copies
    // are made on each instantiation, so the fixed ids here are dummies.
    let e = TyVid(0);
    let a = TyVid(1);
    let repo_app = || Type::Con(repo_con, vec![Type::Var(e), Type::Var(a)]);
    let query_app = || Type::Con(query_con, vec![Type::Var(e), Type::Var(a)]);
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
        // query : ∀e a. Repo e a -> Query e a — start a query over a repository.
        // The builder verbs are pure: they assemble a query, and a terminal runs
        // it.
        "query" => method(vec![repo_app()], query_app(), vec![]),
        // filter : ∀e a. Quote (e -> Bool) -> Query e a -> Query e a
        "filter" => method(vec![quote_pred(), query_app()], query_app(), vec![]),
        // limit / offset : ∀e a. Int -> Query e a -> Query e a
        "limit" | "offset" => method(
            vec![Type::Con(b.int, vec![]), query_app()],
            query_app(),
            vec![],
        ),
        // orderBy : ∀e a k. SortOrder -> Quote (e -> k) -> Query e a -> Query e a.
        // The key quote names a column of any type `k` (the return is phantom —
        // only the column name is read), so this scheme carries the extra var.
        "orderBy" => {
            let sort_order = *reconciled.get("SortOrder")?;
            let k = TyVid(2);
            let key_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e)],
                    ret: Box::new(Type::Var(k)),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                }],
            );
            Some(Scheme {
                vars: vec![e, a, k],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(sort_order, vec![]), key_quote, query_app()],
                    ret: Box::new(query_app()),
                    caps: pure(),
                },
                constraints: vec![],
            })
        }
        // toList : ∀e a. Query e a -> Result (List e) Error where Adapter a, Row e
        "toList" => method(vec![query_app()], result(list_e()), with_adapter_row()),
        // first : ∀e a. Query e a -> Result (Option e) Error where Adapter a, Row e
        "first" => method(vec![query_app()], result(option_e()), with_adapter_row()),
        // selectList / selectFirst : ∀e a s. Quote (e -> s) -> Query e a
        //   -> Result (List s | Option s) Error where Adapter a, Row s.
        // The projection quote captures a record built by naming the result type
        // (`Summary { col = row.col }`); `s` is that named record, pinned at the
        // call from the constructor, and `Row s` decodes the projected columns.
        // `s` first appears (in the projection param) before the adapter `a`, so
        // the constraint order is `Row s` then `Adapter a`, matching how the
        // checker stores the source signature's constraints.
        "selectList" | "selectFirst" => {
            let s = TyVid(2);
            let proj_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e)],
                    ret: Box::new(Type::Var(s)),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                }],
            );
            let ok = if name == "selectList" {
                Type::Con(b.list, vec![Type::Var(s)])
            } else {
                Type::Con(b.option, vec![Type::Var(s)])
            };
            Some(Scheme {
                vars: vec![e, a, s],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![proj_quote, query_app()],
                    ret: Box::new(result(ok)),
                    caps: pure(),
                },
                constraints: vec![Constraint::single(row, s), Constraint::single(adapter, a)],
            })
        }
        // joinOn : ∀e f a. Repo f a -> Quote (e -> f -> Bool) -> Query e a
        //               -> Join e f a. A pure builder: it pairs the left query
        // with the right repository and a quoted join condition over both
        // entities. The condition's left columns range over `e`, its right over
        // `f`; the captured tree tags each side so the seam keeps them apart.
        "joinOn" => {
            let join_con = *reconciled.get("Join")?;
            let f = TyVid(2);
            let repo_f_a = Type::Con(repo_con, vec![Type::Var(f), Type::Var(a)]);
            let cond_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e), Type::Var(f)],
                    ret: Box::new(Type::Con(b.bool, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                }],
            );
            let join_e_f_a = Type::Con(join_con, vec![Type::Var(e), Type::Var(f), Type::Var(a)]);
            Some(Scheme {
                vars: vec![e, a, f],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![repo_f_a, cond_quote, query_app()],
                    ret: Box::new(join_e_f_a),
                    caps: pure(),
                },
                constraints: vec![],
            })
        }
        // toPairs : ∀e f a. Join e f a -> Result (List (e, f)) Error
        //                where Row e, Row f, Adapter a. Runs the inner join and
        // decodes each joined row pair into both entities. The constraint order
        // follows the variables' first appearance in the signature: e, then f,
        // then a (the three slots of `Join e f a`).
        "toPairs" => {
            let join_con = *reconciled.get("Join")?;
            let f = TyVid(2);
            let join_e_f_a = Type::Con(join_con, vec![Type::Var(e), Type::Var(f), Type::Var(a)]);
            let pair = Type::Tuple(vec![Type::Var(e), Type::Var(f)]);
            Some(Scheme {
                vars: vec![e, a, f],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![join_e_f_a],
                    ret: Box::new(result(Type::Con(b.list, vec![pair]))),
                    caps: pure(),
                },
                constraints: vec![
                    Constraint::single(row, e),
                    Constraint::single(row, f),
                    Constraint::single(adapter, a),
                ],
            })
        }
        // selectJoin : ∀e f a s. Quote (e -> f -> s) -> Join e f a
        //                  -> Result (List s) Error where Row s, Adapter a.
        // The projection names a result record built from columns of both
        // entities (`Line { who = u.name, title = p.title }`), which pins `s` to
        // that record and lists its (qualified) columns. `s` first appears in the
        // projection (param 0) before the adapter `a` (in `Join`, param 1), so
        // the constraint order is `Row s` then `Adapter a`, matching selectList.
        "selectJoin" => {
            let join_con = *reconciled.get("Join")?;
            let f = TyVid(2);
            let s = TyVid(3);
            let proj_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e), Type::Var(f)],
                    ret: Box::new(Type::Var(s)),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                }],
            );
            let join_e_f_a = Type::Con(join_con, vec![Type::Var(e), Type::Var(f), Type::Var(a)]);
            Some(Scheme {
                vars: vec![e, a, f, s],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![proj_quote, join_e_f_a],
                    ret: Box::new(result(Type::Con(b.list, vec![Type::Var(s)]))),
                    caps: pure(),
                },
                constraints: vec![Constraint::single(row, s), Constraint::single(adapter, a)],
            })
        }
        // leftJoinOn : ∀e f a. Repo f a -> Quote (e -> f -> Bool) -> Query e a
        //                  -> LeftJoin e f a. The left-outer builder, identical in
        // shape to `joinOn` but producing a `LeftJoin` so the terminal keeps every
        // left row.
        "leftJoinOn" => {
            let leftjoin_con = *reconciled.get("LeftJoin")?;
            let f = TyVid(2);
            let repo_f_a = Type::Con(repo_con, vec![Type::Var(f), Type::Var(a)]);
            let cond_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e), Type::Var(f)],
                    ret: Box::new(Type::Con(b.bool, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                }],
            );
            let leftjoin_e_f_a =
                Type::Con(leftjoin_con, vec![Type::Var(e), Type::Var(f), Type::Var(a)]);
            Some(Scheme {
                vars: vec![e, a, f],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![repo_f_a, cond_quote, query_app()],
                    ret: Box::new(leftjoin_e_f_a),
                    caps: pure(),
                },
                constraints: vec![],
            })
        }
        // toLeftPairs : ∀e f a. LeftJoin e f a -> Result (List (e, Option f)) Error
        //                   where Row e, Row f, Adapter a. Runs the left join and
        // decodes each row into the left entity paired with the right entity, or
        // with `None` where the left row matched no right row. Constraint order
        // follows the variables' first appearance (e, then f, then a), as `toPairs`.
        "toLeftPairs" => {
            let leftjoin_con = *reconciled.get("LeftJoin")?;
            let f = TyVid(2);
            let leftjoin_e_f_a =
                Type::Con(leftjoin_con, vec![Type::Var(e), Type::Var(f), Type::Var(a)]);
            let pair = Type::Tuple(vec![Type::Var(e), Type::Con(b.option, vec![Type::Var(f)])]);
            Some(Scheme {
                vars: vec![e, a, f],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![leftjoin_e_f_a],
                    ret: Box::new(result(Type::Con(b.list, vec![pair]))),
                    caps: pure(),
                },
                constraints: vec![
                    Constraint::single(row, e),
                    Constraint::single(row, f),
                    Constraint::single(adapter, a),
                ],
            })
        }
        // selectLeftJoin : ∀e f a s. Quote (e -> Option f -> s) -> LeftJoin e f a
        //                      -> Result (List s) Error where Row s, Adapter a.
        // The left-outer analogue of `selectJoin`. The right parameter is
        // `Option f`, so a column read off it (`p.title`) is `Option` of its
        // type; the named result record's right-derived fields are therefore
        // `Option`, and an unmatched left row projects them as `None`. `s` first
        // appears in the projection before the adapter `a`, so the constraint
        // order is `Row s` then `Adapter a`, as `selectJoin`.
        "selectLeftJoin" => {
            let leftjoin_con = *reconciled.get("LeftJoin")?;
            let f = TyVid(2);
            let s = TyVid(3);
            let proj_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e), Type::Con(b.option, vec![Type::Var(f)])],
                    ret: Box::new(Type::Var(s)),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                }],
            );
            let leftjoin_e_f_a =
                Type::Con(leftjoin_con, vec![Type::Var(e), Type::Var(f), Type::Var(a)]);
            Some(Scheme {
                vars: vec![e, a, f, s],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![proj_quote, leftjoin_e_f_a],
                    ret: Box::new(result(Type::Con(b.list, vec![Type::Var(s)]))),
                    caps: pure(),
                },
                constraints: vec![Constraint::single(row, s), Constraint::single(adapter, a)],
            })
        }
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
