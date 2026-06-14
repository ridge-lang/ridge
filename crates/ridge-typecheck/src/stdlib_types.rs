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
        // accumulated filter, the ordering as `(ascending?, column)` keys, the
        // page (`lim`, `off`), the `dist` flag (a `SELECT DISTINCT`), and the
        // optional set-operation `plan`. Opaque, so user code only threads it
        // through the builder (`query`/`filter`/`orderBy`/`limit`/`offset`/
        // `distinct`/`union`/`intersect`/`except`) into a terminal.
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
                    RecordField {
                        name: "dist".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                    RecordField {
                        name: "plan".to_string(),
                        ty: Type::Con(b.option, vec![Type::Con(b.q_expr, vec![])]),
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
        // terminal (`toList`/`select`). Field order mirrors the source.
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
        // over both entities. A distinct type so the row its `toList`/`first`
        // decode into differs — a left join keeps every left row and returns the
        // right side as `Option f`, where an inner `Join` returns it as `f`. Field
        // order mirrors the source.
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
        // `std.repo` — a typed column assignment built by `set`. An opaque record
        // `{ column: Text, value: SqlValue }` declared in Ridge (stdlib/repo.ridge).
        // The entity `e` (param 0) is phantom — it ties the setter to the record
        // whose column the quoted accessor named, so a `List (Setter e)` cannot mix
        // entities and must match the repository's `e`, the same phantom shape as
        // `Repo`. Opaque, so user code builds one only through `set`. Field order
        // mirrors the source.
        TyConDecl {
            id: TyConId(base + 8),
            name: "Setter".to_string(),
            arity: 1,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0)],
                vec![
                    RecordField {
                        name: "column".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "value".to_string(),
                        ty: Type::Con(b.sql_value, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.repo` — the group handle the grouped-aggregate quotes range over.
        // Never constructed by user code: it only names the parameter of a
        // `having`/`summarize` lambda so the group vocabulary (`g.key`, `g.count`,
        // `g.sum`/`avg`/`min`/`max`) is available inside the quote. The entity `e`
        // (param 0) is phantom, like `Repo`'s; `k` (param 1) is the group-key type
        // `g.key` answers. Field order mirrors the source.
        TyConDecl {
            id: TyConId(base + 9),
            name: "Group".to_string(),
            arity: 2,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1)],
                vec![RecordField {
                    name: "key".to_string(),
                    ty: Type::Var(TyVid(1)),
                }],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.repo` — a grouped query under construction. A generic opaque record
        // declared in Ridge (stdlib/repo.ridge): the repository, the accumulated
        // filter, the group-key column, and the captured `HAVING` tree. The entity
        // `e` (param 0), the key `k` (param 1), and the adapter `a` (param 2).
        // Opaque, so user code only threads it from `groupBy` through `having` into
        // `summarize`. Field order mirrors the source.
        TyConDecl {
            id: TyConId(base + 10),
            name: "GroupedQuery".to_string(),
            arity: 3,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1), TyVid(2)],
                vec![
                    RecordField {
                        name: "repo".to_string(),
                        ty: Type::Con(
                            TyConId(base + 2),
                            vec![Type::Var(TyVid(0)), Type::Var(TyVid(2))],
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
                        name: "keyCol".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "having".to_string(),
                        ty: Type::Con(b.q_expr, vec![]),
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
        // `count` and `exists` are no longer reconciled here: they became the methods
        // of the `Countable q p | q -> p` class (std.repo), one count-and-test pair
        // over a query, an inner join, or a left join. A qualified `Repo.count`/
        // `Repo.exists` resolves to that class method, typed by the seeded `∀q p. q ->
        // Result Int/Bool Error where Countable q p` scheme (see
        // `seed_countable_scheme`), the receiver pinning the instance and the
        // dependency fixing the predicate arity for the sibling `every`. Omitting the
        // arm routes them through the class-method path; the old `countBy` (count over
        // a predicate) is gone with them — it is `query |> filter pred |> count`.
        // `deleteWhere` keeps its own scheme (it removes the matching rows, not a
        // count, and is unrelated to the receiver-polymorphic query builder).
        "deleteWhere" => method(
            vec![quote_pred(), repo_app()],
            result(Type::Con(b.int, vec![])),
            with_adapter(),
        ),
        // insertRow : ∀e a. Map Text SqlValue -> Repo e a
        //                  -> Result Unit Error where Adapter a
        "insertRow" => method(
            vec![map_row(), repo_app()],
            result(Type::Con(b.unit, vec![])),
            with_adapter(),
        ),
        // insert : ∀e a. e -> Repo e a -> Result Unit Error where Adapter a, Row e.
        // The typed dual of `insertRow`: encodes the entity through `toRow` and
        // appends it. Carries `Row e` because it derives the row.
        "insert" => method(
            vec![Type::Var(e), repo_app()],
            result(Type::Con(b.unit, vec![])),
            with_adapter_row(),
        ),
        // updateWhere : ∀e a. Map Text SqlValue -> Quote (e -> Bool) -> Repo e a
        //   -> Result Int Error where Adapter a. Sets the columns of a partial map
        //   on the matching rows and answers how many changed.
        "updateWhere" => method(
            vec![map_row(), quote_pred(), repo_app()],
            result(Type::Con(b.int, vec![])),
            with_adapter(),
        ),
        // update : ∀e a. e -> Quote (e -> Bool) -> Repo e a -> Result Int Error
        //   where Adapter a, Row e. Overwrites every column of the matching rows
        //   with the entity, encoded through `toRow`.
        "update" => method(
            vec![Type::Var(e), quote_pred(), repo_app()],
            result(Type::Con(b.int, vec![])),
            with_adapter_row(),
        ),
        // set : ∀e a v. Quote (e -> v) -> v -> Setter e where SqlType v. Builds a
        // typed column assignment: the accessor quote names a single column whose
        // type `v` is read off the entity (exactly as an `orderBy` key), and the
        // value must match it. `a` is unused but kept so the quantifier shape lines
        // up with the other repository schemes; `v` is the column/value type, which
        // the `SqlType v` constraint encodes to a `SqlValue`.
        "set" => {
            let setter_con = *reconciled.get("Setter")?;
            let sqltype = classes.id_by_name("SqlType")?;
            let v = TyVid(2);
            let col_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e)],
                    ret: Box::new(Type::Var(v)),
                    caps: pure(),
                }],
            );
            let setter_e = Type::Con(setter_con, vec![Type::Var(e)]);
            Some(Scheme {
                vars: vec![e, a, v],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![col_quote, Type::Var(v)],
                    ret: Box::new(setter_e),
                    caps: pure(),
                },
                constraints: vec![Constraint::single(sqltype, v)],
            })
        }
        // setWhere : ∀e a. List (Setter e) -> Quote (e -> Bool) -> Repo e a
        //   -> Result Int Error where Adapter a. The typed front door to the partial
        //   update: a list of typed setters in place of `updateWhere`'s raw map.
        "setWhere" => {
            let setter_con = *reconciled.get("Setter")?;
            let list_setter = Type::Con(b.list, vec![Type::Con(setter_con, vec![Type::Var(e)])]);
            method(
                vec![list_setter, quote_pred(), repo_app()],
                result(Type::Con(b.int, vec![])),
                with_adapter(),
            )
        }
        // applySet : ∀e a. List (Setter e) -> Query e a -> Result Int Error
        //   where Adapter a. The query-builder write terminal: the accumulated
        //   filter selects the rows, the setters assign their columns — the pipeline
        //   form of `setWhere`.
        "applySet" => {
            let setter_con = *reconciled.get("Setter")?;
            let list_setter = Type::Con(b.list, vec![Type::Con(setter_con, vec![Type::Var(e)])]);
            method(
                vec![list_setter, query_app()],
                result(Type::Con(b.int, vec![])),
                with_adapter(),
            )
        }
        // `sumOf` / `avgOf` / `minOf` / `maxOf` are no longer reconciled here: they
        // became the methods of the `Aggregable q p | q -> p` class (std.repo), one
        // set of scalar aggregates over a query, an inner join, or a left join. A
        // qualified `Repo.sumOf` resolves to that class method, typed by the seeded
        // `∀q p. Quote p -> q -> Result (Option (Ret p)) Error where Aggregable q p`
        // scheme (with `avgOf` answering `Option Float`; see `seed_aggregable_scheme`),
        // the fundep fixing the accessor's arity per receiver and a two-row accessor
        // naming a column from either side of a join. Returning `None` here (falling
        // through to the final arm) routes them through the class-method path rather
        // than the old single-receiver pub fns — and removes the dict-order fragility
        // those reconciled schemes carried, since instance dispatch now threads the
        // `Adapter a`/`SqlType n` context.
        // query : ∀e a. Repo e a -> Query e a — start a query over a repository.
        // The builder verbs are pure: they assemble a query, and a terminal runs
        // it.
        "query" => method(vec![repo_app()], query_app(), vec![]),
        // `filter` is no longer reconciled here: it became the method of the
        // `Refinable q p | q -> p` class (std.repo), one verb over a query or a
        // join. A qualified `Repo.filter` resolves to that class method and is
        // typed by the seeded `∀q p. Quote p -> q -> q where Refinable q p`
        // scheme (see `seed_refinable_scheme`), the fundep fixing the predicate's
        // arity per receiver. Returning `None` here routes it through the
        // class-method path rather than the old single-receiver pub fn.
        // `distinct` is no longer reconciled here: it became a method of the
        // `Pageable q` class (std.repo), one of `limit`/`offset`/`distinct` over a
        // query, an inner join, or a left join. A qualified `Repo.distinct` resolves
        // to that class method, typed by the seeded `∀q. q -> q where Pageable q`
        // scheme (see `seed_pageable_scheme`), the single receiver parameter pinning
        // the instance. Returning `None` here (falling through to the final arm)
        // routes it through the class-method path rather than the old single-receiver
        // pub fn.
        // union / unionAll / intersect / except : ∀e a. Query e a -> Query e a
        //   -> Query e a — combine two queries with a set operation. Pure builders
        // like `filter`: they capture a query plan, and a terminal runs it. Both
        // branches share the entity `e` and adapter `a`, so the column shapes align.
        "union" | "unionAll" | "intersect" | "except" => {
            method(vec![query_app(), query_app()], query_app(), vec![])
        }
        // `limit` / `offset` are no longer reconciled here: they joined `distinct`
        // as methods of the `Pageable q` class (std.repo), typed by the seeded
        // `∀q. Int -> q -> q where Pageable q` scheme (see `seed_pageable_scheme`).
        // Omitting the arm routes them through the class-method path rather than the
        // old single-receiver pub fns.
        // `orderBy` is no longer reconciled here: it became the method of the
        // `Orderable q p | q -> p` class (std.repo), one verb over a query or a
        // join. A qualified `Repo.orderBy` resolves to that class method, typed by
        // the seeded `∀q p. SortOrder -> Quote p -> q -> q where Orderable q p`
        // scheme (see `seed_orderable_scheme`), the fundep fixing the key's arity
        // per receiver and a two-row key naming a column from either side of a
        // join. Returning `None` here routes it through the class-method path
        // rather than the old single-receiver pub fn.
        // `toList` / `first` are no longer reconciled here: they became the methods
        // of the `Decodable q p | q -> p` class (std.repo), one pair of terminals
        // that decode a query, an inner join, or a left join. A qualified
        // `Repo.toList`/`Repo.first` resolves to that class method, typed by the
        // seeded `∀q p. q -> Result (List (Ret p)) Error where Decodable q p` scheme
        // (see `seed_decodable_scheme`), the fundep fixing the row shape per receiver
        // and `Ret p` naming the decoded element. Omitting the arm routes them
        // through the class-method path rather than the old single-receiver pub fns.
        // single : ∀e a. Query e a -> Result (Option e) Error where Adapter a, Row e.
        // The unique-row terminal stays a reconciled pub fn: it fetches a second row
        // to reject a non-unique result, so it is not part of the decode family.
        "single" => method(vec![query_app()], result(option_e()), with_adapter_row()),
        // singleOrError : ∀e a. Query e a -> Result e Error where Adapter a, Row e.
        // The strict `single`: it answers the bare entity, turning the empty match
        // into an error rather than `None`; otherwise the same constraints in the
        // same order.
        "singleOrError" => method(vec![query_app()], result(Type::Var(e)), with_adapter_row()),
        // `every` is no longer reconciled here: it joined `count`/`exists` as a method
        // of the `Countable q p | q -> p` class (std.repo), the universal dual of
        // `exists` over a query, an inner join, or a left join. A qualified
        // `Repo.every` resolves to that class method, typed by the seeded `∀q p. Quote
        // p -> q -> Result Bool Error where Countable q p` scheme (see
        // `seed_countable_scheme`), the dependency fixing the predicate's arity per
        // receiver. Omitting the arm routes it through the class-method path.
        // `select` / `selectFirst` are no longer reconciled here: they became the
        // methods of the `Projectable q p | q -> p` class (std.repo), one verb
        // over a query, an inner join, or a left join. A qualified `Repo.select`
        // resolves to that class method, typed by the seeded `∀q p. Quote p -> q
        // -> Result (List (Ret p)) Error where Projectable q p` scheme (see
        // `seed_projectable_scheme`), the fundep fixing the projection per
        // receiver and `Ret p` naming the projected element. Returning `None`
        // routes them through the class-method path rather than the old pub fns.
        // groupBy : ∀e a k. Quote (e -> k) -> Query e a -> GroupedQuery e k a.
        // A pure builder: it pins the group-key column (named by the accessor
        // quote, exactly as an `orderBy` key) and carries the query's filter into a
        // grouped query. `k` is the key type `g.key` later answers.
        "groupBy" => {
            let grouped_con = *reconciled.get("GroupedQuery")?;
            let k = TyVid(2);
            let key_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e)],
                    ret: Box::new(Type::Var(k)),
                    caps: pure(),
                }],
            );
            let grouped = Type::Con(grouped_con, vec![Type::Var(e), Type::Var(k), Type::Var(a)]);
            Some(Scheme {
                vars: vec![e, a, k],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![key_quote, query_app()],
                    ret: Box::new(grouped),
                    caps: pure(),
                },
                constraints: vec![],
            })
        }
        // having : ∀e a k. Quote (Group e k -> Bool) -> GroupedQuery e k a
        //   -> GroupedQuery e k a. A pure builder: it captures a predicate over the
        // group aggregates (`g.count`, `g.sum(col)`, …) and stores it as the query's
        // `HAVING`. The quote ranges over the `Group e k` handle, not a row.
        "having" => {
            let grouped_con = *reconciled.get("GroupedQuery")?;
            let group_con = *reconciled.get("Group")?;
            let k = TyVid(2);
            let group = Type::Con(group_con, vec![Type::Var(e), Type::Var(k)]);
            let having_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![group],
                    ret: Box::new(Type::Con(b.bool, vec![])),
                    caps: pure(),
                }],
            );
            let grouped = Type::Con(grouped_con, vec![Type::Var(e), Type::Var(k), Type::Var(a)]);
            Some(Scheme {
                vars: vec![e, a, k],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![having_quote, grouped.clone()],
                    ret: Box::new(grouped),
                    caps: pure(),
                },
                constraints: vec![],
            })
        }
        // summarize : ∀e a k s. Quote (Group e k -> s) -> GroupedQuery e k a
        //   -> Result (List s) Error where Row s, Adapter a. The projection names a
        // result record built from group aggregates (`Stats { dept = g.key, n =
        // g.count, total = g.sum (fn u -> u.salary) }`), which pins `s` to that
        // record; the backend pushes the GROUP BY down and `Row s` decodes each
        // summarised row. `s` first appears in the projection (param 0) before the
        // adapter `a` (in `GroupedQuery`, param 2), so the constraint order is
        // `Row s` then `Adapter a`, matching `selectList`.
        "summarize" => {
            let grouped_con = *reconciled.get("GroupedQuery")?;
            let group_con = *reconciled.get("Group")?;
            let k = TyVid(2);
            let s = TyVid(3);
            let group = Type::Con(group_con, vec![Type::Var(e), Type::Var(k)]);
            let proj_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![group],
                    ret: Box::new(Type::Var(s)),
                    caps: pure(),
                }],
            );
            let grouped = Type::Con(grouped_con, vec![Type::Var(e), Type::Var(k), Type::Var(a)]);
            Some(Scheme {
                vars: vec![e, a, k, s],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![proj_quote, grouped],
                    ret: Box::new(result(Type::Con(b.list, vec![Type::Var(s)]))),
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
        // `toPairs` is gone: an inner join's `toList`/`first` are now the
        // `Decodable (Join e f a) …` methods (std.repo), so the join shares the
        // query's decode terminals. `Repo.toList` over a `Join` resolves to that
        // class method (see `seed_decodable_scheme`), `Ret p` naming the decoded
        // pair `(e, f)`; omitting the arm routes it through the class-method path.
        // (`selectJoin` is gone: an inner join's projection is now the
        // `Projectable (Join e f a) (fn e f -> s)` instance — see
        // `seed_projectable_scheme`.)
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
        // `toLeftPairs` is gone: a left join's `toList`/`first` are now the
        // `Decodable (LeftJoin e f a) …` methods (std.repo). `Repo.toList` over a
        // `LeftJoin` resolves to that class method (see `seed_decodable_scheme`),
        // `Ret p` naming the decoded pair `(e, Option f)` — the right side `None`
        // where a left row matched none; omitting the arm routes it through the
        // class-method path.
        // (`selectLeftJoin` is gone: a left join's projection is now the
        // `Projectable (LeftJoin e f a) (fn e (Option f) -> s)` instance, the
        // right side read as `Option` — see `seed_projectable_scheme`.)
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
