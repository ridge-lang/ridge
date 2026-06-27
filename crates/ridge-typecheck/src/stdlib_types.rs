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
    BuiltinTyCons, CapRow, CapVid, CapabilitySet, Constraint, RecordField, RecordSchema, Scheme,
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
                        // `Option QueryPlan` — the captured set-operation plan, a
                        // typed `QueryPlan` (TyConId base + 15) declared in
                        // query.ridge, `None` for a plain single-table query.
                        ty: Type::Con(b.option, vec![Type::Con(TyConId(base + 15), vec![])]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.repo` — a transparent alias for the 2-table inner join.
        // `pub type Join e f a = Joined (Query e a) f a`
        //
        // The alias keeps the slot at base + 6 so no downstream offset shifts.
        // Type-checking resolves `Join e f a` to `Joined (Query e a) f a` via
        // shallow_resolve, so the existing `Joined`-family class instances cover
        // the 2-table case without any separate verb implementations.
        TyConDecl {
            id: TyConId(base + 6),
            name: "Join".to_string(),
            arity: 3,
            kind: TyConKind::Alias {
                params: vec![TyVid(0), TyVid(1), TyVid(2)],
                // body = Joined (Query e a) f a
                //   = TyConId(base + 16) applied to [Query e a, f, a]
                //   = TyConId(base + 16) [ TyConId(base+5) [TyVid(0), TyVid(2)],
                //                          TyVid(1), TyVid(2) ]
                body: Type::Con(
                    TyConId(base + 16), // Joined
                    vec![
                        Type::Con(
                            TyConId(base + 5),                              // Query
                            vec![Type::Var(TyVid(0)), Type::Var(TyVid(2))], // Query e a
                        ),
                        Type::Var(TyVid(1)), // f
                        Type::Var(TyVid(2)), // a
                    ],
                ),
            },
            def_span: None,
            def_module_raw: None,
            opaque: false,
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
        // `std.repo` — a grouped builder under construction, unified across a query
        // and a join. A generic opaque record declared in Ridge (stdlib/repo.ridge):
        // it carries the source queryable it groups (`source`, of type `q`), the
        // group-key column and which side of a join it belongs to (`keyCol`,
        // `keySide`), and the captured `HAVING` tree. The source type `q` (param 0)
        // is the `Query`/`Join`/`LeftJoin` being grouped; the key-accessor type `p`
        // (param 1) is phantom, kept only so the `having`/`summarize` quotes can read
        // the key type off it. Opaque, so user code only threads it from `groupBy`
        // through `having` into `summarize`.
        TyConDecl {
            id: TyConId(base + 9),
            name: "Grouped".to_string(),
            arity: 2,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1)],
                vec![
                    RecordField {
                        name: "source".to_string(),
                        ty: Type::Var(TyVid(0)),
                    },
                    RecordField {
                        name: "keyCol".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "keySide".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                    RecordField {
                        name: "havingTree".to_string(),
                        ty: Type::Con(b.q_expr, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.migrate` — a column in a table definition. An opaque record declared
        // in Ridge (stdlib/migrate.ridge): the column name, its base-type name
        // (`"int"`/`"text"`/`"bool"`/`"float"`), and the three schema modifiers
        // (`nullable`, `primaryKey`, `unique`). Opaque, so user code builds one only
        // through the `intCol`/`textCol`/… declarators and the modifier steps. Field
        // order mirrors the source.
        TyConDecl {
            id: TyConId(base + 10),
            name: "Column".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "ty".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "nullable".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                    RecordField {
                        name: "primaryKey".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                    RecordField {
                        name: "unique".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.migrate` — a single schema change. An opaque union declared in Ridge
        // (stdlib/migrate.ridge); its variants are built only through the
        // `createTable`/`dropTable`/`addColumn`/`dropColumn`/`createIndex` factories
        // and decomposed onto the adapter's schema seam by the migration runner, so
        // the constructors stay confined to the module. Variant order mirrors the
        // source.
        TyConDecl {
            id: TyConId(base + 11),
            name: "SchemaOp".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "CreateTable".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.list, vec![Type::Con(TyConId(base + 10), vec![])]),
                        ]),
                    },
                    UnionVariant {
                        name: "DropTable".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(b.text, vec![])]),
                    },
                    UnionVariant {
                        name: "AddColumn".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(TyConId(base + 10), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "DropColumn".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.text, vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "CreateIndex".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.text, vec![]),
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                            Type::Con(b.bool, vec![]),
                        ]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.migrate` — a named, ordered batch of schema changes. A plain record
        // declared in Ridge (stdlib/migrate.ridge): the migration name (its key in
        // the tracking table) and the ordered `SchemaOp` steps. Users construct it
        // through `migration` or the record literal; field order mirrors the source.
        TyConDecl {
            id: TyConId(base + 12),
            name: "Migration".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "steps".to_string(),
                        ty: Type::Con(b.list, vec![Type::Con(TyConId(base + 11), vec![])]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.repo` — a right (outer) join under construction. The mirror of
        // `LeftJoin`: structurally a copy of `Join` (same left query, right
        // repository, and quoted condition), a distinct nominal type so the row its
        // `toList`/`first` decode into differs — a right join keeps every right row
        // and returns the left side as `Option e`, where a `LeftJoin` returns the
        // right side as `Option f`. Opaque, so only the arity and the field skeleton
        // matter here; the real field set lives in repo.ridge.
        TyConDecl {
            id: TyConId(base + 13),
            name: "RightJoin".to_string(),
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
        // `std.repo` — a full (outer) join under construction. The union of `LeftJoin`
        // and `RightJoin`: structurally a copy of `Join` (same left query, right
        // repository, and quoted condition), a distinct nominal type so the row its
        // `toList`/`first` decode into differs — a full join keeps every row of both
        // tables and returns BOTH sides as `Option` (`(Option e, Option f)`). Opaque,
        // so only the arity and the field skeleton matter here; the real field set
        // lives in repo.ridge.
        TyConDecl {
            id: TyConId(base + 14),
            name: "FullJoin".to_string(),
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
        // `std.query` — a captured query plan, the dual at the plan layer of what
        // `QExpr` is at the predicate layer. A plain (non-opaque) union declared in
        // Ridge (stdlib/query.ridge): a `PlanScan` single-table read, a `PlanCombine`
        // set operation over two plans, or a `PlanRefine` wrapping an outer filter/
        // order/page/distinct on a plan. The set-operation terminals build one through
        // the `planScan`/`planCombine`/`planRefine` factories and hand it to a
        // backend's `runPlan`. Variant order mirrors the source.
        TyConDecl {
            id: TyConId(base + 15),
            name: "QueryPlan".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "PlanScan".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.q_expr, vec![]),
                            Type::Con(
                                b.list,
                                vec![Type::Tuple(vec![
                                    Type::Con(b.bool, vec![]),
                                    Type::Con(b.text, vec![]),
                                ])],
                            ),
                            Type::Con(b.int, vec![]),
                            Type::Con(b.int, vec![]),
                            Type::Con(b.bool, vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "PlanCombine".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(TyConId(base + 15), vec![]),
                            Type::Con(TyConId(base + 15), vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "PlanRefine".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(TyConId(base + 15), vec![]),
                            Type::Con(b.q_expr, vec![]),
                            Type::Con(
                                b.list,
                                vec![Type::Tuple(vec![
                                    Type::Con(b.bool, vec![]),
                                    Type::Con(b.text, vec![]),
                                ])],
                            ),
                            Type::Con(b.int, vec![]),
                            Type::Con(b.int, vec![]),
                            Type::Con(b.bool, vec![]),
                        ]),
                    },
                    // `PlanJoin kind left right cond where2 orders lim off dist leftCols
                    // rightCols` — two sub-plans paired on a join. `orders` is the
                    // leaf-tagged `(ascending?, leaf, column)` ordering keys;
                    // `leftCols`/`rightCols` are each source entity's column names (from
                    // `Row.rowColumns`), spelled into the renderer's prefixed select list
                    // and ignored by the in-memory backend.
                    UnionVariant {
                        name: "PlanJoin".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(TyConId(base + 15), vec![]),
                            Type::Con(TyConId(base + 15), vec![]),
                            Type::Con(b.q_expr, vec![]),
                            Type::Con(b.q_expr, vec![]),
                            Type::Con(
                                b.list,
                                vec![Type::Tuple(vec![
                                    Type::Con(b.bool, vec![]),
                                    Type::Con(b.int, vec![]),
                                    Type::Con(b.text, vec![]),
                                ])],
                            ),
                            Type::Con(b.int, vec![]),
                            Type::Con(b.int, vec![]),
                            Type::Con(b.bool, vec![]),
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                        ]),
                    },
                    // `PlanProject proj child lim off dist` — project a sub-plan's rows
                    // through the projection tree (a `QProj`) into rows keyed by its
                    // output aliases, then de-duplicate and page. Wraps a `PlanJoin`.
                    UnionVariant {
                        name: "PlanProject".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.q_expr, vec![]),
                            Type::Con(TyConId(base + 15), vec![]),
                            Type::Con(b.int, vec![]),
                            Type::Con(b.int, vec![]),
                            Type::Con(b.bool, vec![]),
                        ]),
                    },
                    // `PlanAggregate func column isRight child` — reduce a sub-plan to a
                    // single scalar (`COUNT`/`SUM`/`AVG`/`MIN`/`MAX` of `column`, on the
                    // join side `isRight` selects). Yields one row carrying the scalar,
                    // or none when the aggregate is SQL NULL.
                    UnionVariant {
                        name: "PlanAggregate".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.text, vec![]),
                            Type::Con(b.bool, vec![]),
                            Type::Con(TyConId(base + 15), vec![]),
                        ]),
                    },
                    // `PlanGroup keyCol keyLeaf cols having child` — group a sub-plan's
                    // rows by `keyCol` (on leaf `keyLeaf`), summarise each group into the
                    // `(alias, func, column, leaf)` aggregate columns, keep the groups
                    // `having` admits. One row per group. Wraps a `PlanJoin`.
                    UnionVariant {
                        name: "PlanGroup".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.int, vec![]),
                            Type::Con(
                                b.list,
                                vec![Type::Tuple(vec![
                                    Type::Con(b.text, vec![]),
                                    Type::Con(b.text, vec![]),
                                    Type::Con(b.text, vec![]),
                                    Type::Con(b.int, vec![]),
                                ])],
                            ),
                            Type::Con(b.q_expr, vec![]),
                            Type::Con(TyConId(base + 15), vec![]),
                        ]),
                    },
                    // `PlanExists child` — wrap a sub-plan in an existence probe, asking
                    // only whether it yields any row. Compiles to `SELECT 1 FROM … LIMIT
                    // 1`; the backend answers one trivial row or none.
                    UnionVariant {
                        name: "PlanExists".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(
                            TyConId(base + 15),
                            vec![],
                        )]),
                    },
                    // `PlanList rows` — the in-memory `Seq` source: the rows `from`
                    // snapshotted, carried inline. Mem-only; the interpreter returns them
                    // as-is and the verbs wrap this leaf to refine it.
                    UnionVariant {
                        name: "PlanList".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(
                            b.list,
                            vec![Type::Con(
                                b.map,
                                vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
                            )],
                        )]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.repo` — a nested inner join of three or more tables, declared in
        // Ridge (stdlib/repo.ridge). The left side `source` is itself a join (a
        // binary `Join` or another `Joined`), `f` the newly joined entity, `a` the
        // shared adapter. Opaque, so user code only threads it from `joinOn` into a
        // terminal (`toList`/`first`). Interned after `QueryPlan` so every existing
        // reconciled offset is unchanged.
        TyConDecl {
            id: TyConId(base + 16),
            name: "Joined".to_string(),
            arity: 3,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1), TyVid(2)],
                vec![
                    RecordField {
                        name: "source".to_string(),
                        ty: Type::Var(TyVid(0)),
                    },
                    RecordField {
                        name: "right".to_string(),
                        ty: Type::Con(
                            TyConId(base + 2),
                            vec![Type::Var(TyVid(1)), Type::Var(TyVid(2))],
                        ),
                    },
                    // Same captured-tree-over-two-row-maps form `Join.cond` uses,
                    // mirroring the source field; the entity view the user-facing
                    // `joinOn` presents is the `JoinCond` projection, reconciled
                    // separately.
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
        // `std.repo` — a nested LEFT outer join of three or more tables, declared in
        // Ridge (stdlib/repo.ridge). The same shape as `Joined`: the left `source` is
        // a composite, `f` the newly left-joined entity (read optional in the result),
        // `a` the shared adapter. Opaque; threaded from `leftJoinOn` into a terminal.
        // Interned after `Joined` so every existing reconciled offset is unchanged.
        TyConDecl {
            id: TyConId(base + 17),
            name: "LeftJoined".to_string(),
            arity: 3,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1), TyVid(2)],
                vec![
                    RecordField {
                        name: "source".to_string(),
                        ty: Type::Var(TyVid(0)),
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
        // `std.repo` — a nested RIGHT outer join of three or more tables. The same
        // shape as `LeftJoined`: `source` the composite, `f` the new table (always
        // present), `a` the adapter — but the terminal keeps every new row and reads
        // the whole composite as `Option`. Opaque; threaded from `rightJoinOn` into a
        // terminal. Interned after `LeftJoined` so existing offsets are unchanged.
        TyConDecl {
            id: TyConId(base + 18),
            name: "RightJoined".to_string(),
            arity: 3,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1), TyVid(2)],
                vec![
                    RecordField {
                        name: "source".to_string(),
                        ty: Type::Var(TyVid(0)),
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
        // `std.repo` — a nested FULL outer join of three or more tables. The same
        // shape as `Left`/`RightJoined`: `source` the composite, `f` the new table,
        // `a` the adapter — but the terminal keeps every row of both sides, reading
        // the composite (as a unit) and the new table each as `Option`. Opaque;
        // threaded from `fullJoinOn` into a terminal. Interned after `RightJoined` so
        // existing offsets are unchanged.
        TyConDecl {
            id: TyConId(base + 19),
            name: "FullJoined".to_string(),
            arity: 3,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0), TyVid(1), TyVid(2)],
                vec![
                    RecordField {
                        name: "source".to_string(),
                        ty: Type::Var(TyVid(0)),
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
        // `std.repo` — an in-memory sequence of records lifted into the query world
        // by `from`. Opaque; mirrors `Query`'s builder fields one-for-one minus the
        // repository: `source` is the inline `PlanList` of snapshotted rows, then the
        // accumulated `pred`/`orders`/`lim`/`off`/`dist` a terminal materialises into one
        // `planRefine`. The element `a` is phantom (carried in the type so `Rows (Seq a)`
        // reduces to `a`), not stored. Field order mirrors the source so the consistency
        // check holds. Interned after `FullJoined` so existing offsets are unchanged.
        TyConDecl {
            id: TyConId(base + 20),
            name: "Seq".to_string(),
            arity: 1,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0)],
                vec![
                    RecordField {
                        name: "source".to_string(),
                        ty: Type::Con(TyConId(base + 15), vec![]),
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
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.data` — pool tuning for `connectWith`. A plain (non-opaque) record
        // built through `defaultPool`/`with*`; declared in Ridge
        // (stdlib/data.ridge). Field order mirrors the source declaration so the
        // consistency check holds.
        TyConDecl {
            id: TyConId(base + 21),
            name: "PoolConfig".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "size".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "connectTimeoutMs".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "queryTimeoutMs".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "checkoutTimeoutMs".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "idleTimeoutMs".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "maxLifetimeMs".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "healthCheckMs".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "connectRetries".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "retryBackoffMs".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                    RecordField {
                        name: "maxQueueDepth".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.query` — the mutation-plan tree (stdlib/query.ridge), the write-side
        // counterpart of `QueryPlan`. `MutInsert table rows` appends one or more rows;
        // `MutUpdate table changes pred` sets the given columns on the rows its
        // predicate admits; `MutDelete table pred` removes them. `mutationToSql`
        // renders it to one parameterized statement, sharing the predicate renderer
        // with `planToSql`, so a correlated `EXISTS` in a mutation predicate compiles
        // exactly as it does in a query.
        TyConDecl {
            id: TyConId(base + 22),
            name: "MutationPlan".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "MutInsert".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(
                                b.list,
                                vec![Type::Con(
                                    b.map,
                                    vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
                                )],
                            ),
                        ]),
                    },
                    UnionVariant {
                        name: "MutUpdate".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(
                                b.map,
                                vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
                            ),
                            Type::Con(b.q_expr, vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "MutDelete".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.q_expr, vec![]),
                        ]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
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
#[expect(
    clippy::too_many_lines,
    reason = "one match arm per reconciled stdlib function; the arms read best kept together"
)]
pub(crate) fn reconciled_fn_scheme(
    module: &str,
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
    classes: Option<&ClassTable>,
) -> Option<Scheme> {
    match (module, name) {
        // std.query `orderSql : ∀f. SortOrder -> Quote f -> (Sql, List SqlValue)`
        // — compiles a quoted ordering key plus a direction into an `ORDER BY`
        // fragment and its ordered bind values (a computed key may carry literals,
        // each a placeholder rather than interpolated text).
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
                    ret: Box::new(Type::Tuple(vec![
                        Type::Con(b.sql, vec![]),
                        Type::Con(b.list, vec![Type::Con(b.sql_value, vec![])]),
                    ])),
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
        // std.data `connectWith : Config -> PoolConfig -> Result Postgres Error` —
        // `connect` with an explicit pool. Names the reconciled `Config`,
        // `PoolConfig`, and `Postgres`, so the hand-curated table cannot express it.
        ("std.data", "connectWith") => {
            let postgres = *reconciled.get("Postgres")?;
            let config = *reconciled.get("Config")?;
            let pool = *reconciled.get("PoolConfig")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(config, vec![]), Type::Con(pool, vec![])],
                    ret: Box::new(Type::Con(
                        b.result,
                        vec![Type::Con(postgres, vec![]), Type::Con(b.error, vec![])],
                    )),
                    caps: CapRow::Concrete(CapabilitySet::singleton(Capability::Db)),
                },
                constraints: vec![],
            })
        }
        // std.data `defaultPool : Unit -> PoolConfig` — the pure pool baseline.
        // Returns the reconciled `PoolConfig`.
        ("std.data", "defaultPool") => {
            let pool = *reconciled.get("PoolConfig")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(b.unit, vec![])],
                    ret: Box::new(Type::Con(pool, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                },
                constraints: vec![],
            })
        }
        // std.data `with* : Int -> PoolConfig -> PoolConfig` — the pure pool-config
        // setters (size, the millisecond timeouts, the maintenance windows, and the
        // retry and backpressure knobs). Each names the reconciled `PoolConfig` on
        // both sides.
        (
            "std.data",
            "withPoolSize"
            | "withConnectTimeoutMs"
            | "withQueryTimeoutMs"
            | "withCheckoutTimeoutMs"
            | "withIdleTimeoutMs"
            | "withMaxLifetimeMs"
            | "withHealthCheckMs"
            | "withConnectRetries"
            | "withRetryBackoffMs"
            | "withMaxQueueDepth",
        ) => {
            let pool = *reconciled.get("PoolConfig")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(b.int, vec![]), Type::Con(pool, vec![])],
                    ret: Box::new(Type::Con(pool, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
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
        // std.query `planScan`/`planCombine`/`planRefine`/`planJoin` — the `QueryPlan`
        // factories.
        (
            "std.query",
            "planScan" | "planCombine" | "planRefine" | "planJoin" | "planProject"
            | "planAggregate" | "planGroup" | "planToSql" | "optimize" | "planExists",
        ) => reconciled_query_plan_fn_scheme(name, reconciled, b),
        // std.query mutation builders + the write-side renderer — the `MutationPlan`
        // factories `planInsert`/`planUpdate`/`planDelete` and `mutationToSql`.
        ("std.query", "planInsert" | "planUpdate" | "planDelete" | "mutationToSql") => {
            reconciled_mutation_plan_fn_scheme(name, reconciled, b)
        }
        ("std.repo", _) => reconciled_repo_fn_scheme(name, reconciled, b, classes?),
        ("std.migrate", _) => reconciled_migrate_fn_scheme(name, reconciled, b, classes?),
        ("std.raw", _) => reconciled_raw_fn_scheme(name, b, classes?),
        _ => None,
    }
}

/// The `std.query` plan-builder slice of [`reconciled_fn_scheme`]: `planScan`/
/// `planCombine`/`planRefine`/`planJoin`/`planProject`/`planAggregate`/`planGroup`, the
/// factories that build a `QueryPlan` node. Each is pure and returns the reconciled
/// `QueryPlan`, so none is expressible in the hand-curated signature table.
fn reconciled_query_plan_fn_scheme(
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
) -> Option<Scheme> {
    let query_plan = *reconciled.get("QueryPlan")?;
    let plan = || Type::Con(query_plan, vec![]);
    let text = || Type::Con(b.text, vec![]);
    let int = || Type::Con(b.int, vec![]);
    let bool_ = || Type::Con(b.bool, vec![]);
    let qexpr = || Type::Con(b.q_expr, vec![]);
    // The ordering keys: `List (Bool, QExpr)` — the (ascending?, key) pairs, the
    // key a column or a computed expression over the columns.
    let orders = || Type::Con(b.list, vec![Type::Tuple(vec![bool_(), qexpr()])]);
    // The leaf-tagged join ordering keys: `List (Bool, Int, QExpr)` — the
    // (ascending?, leaf, key) triples; the leaf is the base side a bare column
    // names, while a computed key qualifies each of its columns to its own side.
    let join_orders = || Type::Con(b.list, vec![Type::Tuple(vec![bool_(), int(), qexpr()])]);
    // A `List Text` — a join's per-source column names (`leftCols`/`rightCols`).
    let text_list = || Type::Con(b.list, vec![text()]);
    // The grouped-aggregate columns: `List (Text, Text, QExpr, Int)` — the
    // (alias, func, value, leaf) quadruples a `GROUP BY` summary projects, where the
    // value is a `QExpr` (a column or a computed expression the fold evaluates) and
    // the leaf index names which join leaf a bare column folds.
    let group_cols = || {
        Type::Con(
            b.list,
            vec![Type::Tuple(vec![text(), text(), qexpr(), int()])],
        )
    };
    let pure = |params: Vec<Type>| Scheme {
        vars: vec![],
        cap_vars: vec![],
        row_vars: vec![],
        ty: Type::Fn {
            params,
            ret: Box::new(plan()),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        },
        constraints: vec![],
    };
    match name {
        // planScan : Text -> QExpr -> List (Bool, QExpr) -> Int -> Int -> Bool -> QueryPlan
        "planScan" => Some(pure(vec![text(), qexpr(), orders(), int(), int(), bool_()])),
        // planCombine : Text -> QueryPlan -> QueryPlan -> QueryPlan
        "planCombine" => Some(pure(vec![text(), plan(), plan()])),
        // planRefine : QueryPlan -> QExpr -> List (Bool, QExpr) -> Int -> Int -> Bool -> QueryPlan
        "planRefine" => Some(pure(vec![plan(), qexpr(), orders(), int(), int(), bool_()])),
        // planJoin : Text -> QueryPlan -> QueryPlan -> QExpr -> QExpr ->
        //            List (Bool, Int, QExpr) -> Int -> Int -> Bool ->
        //            List Text -> List Text -> QueryPlan
        "planJoin" => Some(pure(vec![
            text(),
            plan(),
            plan(),
            qexpr(),
            qexpr(),
            join_orders(),
            int(),
            int(),
            bool_(),
            text_list(),
            text_list(),
        ])),
        // planProject : QExpr -> QueryPlan -> Int -> Int -> Bool -> QueryPlan
        "planProject" => Some(pure(vec![qexpr(), plan(), int(), int(), bool_()])),
        // planAggregate : Text -> QExpr -> Int -> QueryPlan -> QueryPlan
        "planAggregate" => Some(pure(vec![text(), qexpr(), int(), plan()])),
        // planGroup : Text -> Int -> List (Text, Text, QExpr, Int) -> QExpr ->
        //             QueryPlan -> QueryPlan
        "planGroup" => Some(pure(vec![text(), int(), group_cols(), qexpr(), plan()])),
        // QueryPlan -> QueryPlan, both: `optimize` is the renderer's plan-to-plan pre-pass,
        // `planExists` the existence-probe wrapper an `exists` terminal builds.
        "optimize" | "planExists" => Some(pure(vec![plan()])),
        // planList : List (Map Text SqlValue) -> QueryPlan — the in-memory `Seq`
        // source leaf, wrapping the rows `from` snapshotted inline.
        "planList" => Some(pure(vec![Type::Con(
            b.list,
            vec![Type::Con(
                b.map,
                vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
            )],
        )])),
        // planToSql : QueryPlan -> (Sql, List SqlValue) — the renderer, lowering a
        // whole plan to one parameterized statement plus its ordered bind values.
        // Unlike the builders it does not return a `QueryPlan`, so its scheme is
        // spelled out rather than built through `pure`.
        "planToSql" => Some(Scheme {
            vars: vec![],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![plan()],
                ret: Box::new(Type::Tuple(vec![
                    Type::Con(b.sql, vec![]),
                    Type::Con(b.list, vec![Type::Con(b.sql_value, vec![])]),
                ])),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        }),
        _ => None,
    }
}

/// The `std.query` mutation-builder slice of [`reconciled_fn_scheme`]: `planInsert`/
/// `planUpdate`/`planDelete` build a `MutationPlan` node, and `mutationToSql` lowers one
/// to a parameterized statement plus its ordered bind values (the write-side dual of
/// `planToSql`). The builders return the reconciled `MutationPlan`, so none is
/// expressible in the hand-curated signature table.
fn reconciled_mutation_plan_fn_scheme(
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
) -> Option<Scheme> {
    let mutation_plan = *reconciled.get("MutationPlan")?;
    let plan = || Type::Con(mutation_plan, vec![]);
    let text = || Type::Con(b.text, vec![]);
    let qexpr = || Type::Con(b.q_expr, vec![]);
    // A `Map Text SqlValue` — one row's columns, and the changes map of an update.
    let row = || {
        Type::Con(
            b.map,
            vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
        )
    };
    let rows = || Type::Con(b.list, vec![row()]);
    let pure = |params: Vec<Type>, ret: Type| Scheme {
        vars: vec![],
        cap_vars: vec![],
        row_vars: vec![],
        ty: Type::Fn {
            params,
            ret: Box::new(ret),
            caps: CapRow::Concrete(CapabilitySet::PURE),
        },
        constraints: vec![],
    };
    match name {
        // planInsert : Text -> List (Map Text SqlValue) -> MutationPlan
        "planInsert" => Some(pure(vec![text(), rows()], plan())),
        // planUpdate : Text -> Map Text SqlValue -> QExpr -> MutationPlan
        "planUpdate" => Some(pure(vec![text(), row(), qexpr()], plan())),
        // planDelete : Text -> QExpr -> MutationPlan
        "planDelete" => Some(pure(vec![text(), qexpr()], plan())),
        // mutationToSql : MutationPlan -> (Sql, List SqlValue)
        "mutationToSql" => Some(pure(
            vec![plan()],
            Type::Tuple(vec![
                Type::Con(b.sql, vec![]),
                Type::Con(b.list, vec![Type::Con(b.sql_value, vec![])]),
            ]),
        )),
        _ => None,
    }
}

/// The `std.migrate` slice of [`reconciled_fn_scheme`]: the schema-DSL builders and
/// the migration runner. The builders are pure and reference the reconciled
/// `Column`/`SchemaOp`/`Migration` types; `run` is the only constrained verb
/// (`where Adapter a`, to reach the schema seam).
fn reconciled_migrate_fn_scheme(
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
    classes: &ClassTable,
) -> Option<Scheme> {
    let column = *reconciled.get("Column")?;
    let schema_op = *reconciled.get("SchemaOp")?;
    let migration = *reconciled.get("Migration")?;
    let text = || Type::Con(b.text, vec![]);
    let list = |x: Type| Type::Con(b.list, vec![x]);
    let pure = || CapRow::Concrete(CapabilitySet::PURE);
    let result = |ok: Type| Type::Con(b.result, vec![ok, Type::Con(b.error, vec![])]);
    let column_ty = || Type::Con(column, vec![]);
    let schema_op_ty = || Type::Con(schema_op, vec![]);
    // A monomorphic pure builder: `params -> ret`, no quantified vars or constraints.
    let mono = |params: Vec<Type>, ret: Type| {
        Some(Scheme {
            vars: vec![],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params,
                ret: Box::new(ret),
                caps: pure(),
            },
            constraints: vec![],
        })
    };
    match name {
        // intCol / textCol / boolCol / floatCol : Text -> Column — the typed column
        // declarators, each pinning the base type.
        "intCol" | "textCol" | "boolCol" | "floatCol" => mono(vec![text()], column_ty()),
        // nullable / primaryKey / unique : Column -> Column
        "nullable" | "primaryKey" | "unique" => mono(vec![column_ty()], column_ty()),
        // createTable : Text -> List Column -> SchemaOp
        "createTable" => mono(vec![text(), list(column_ty())], schema_op_ty()),
        // dropTable : Text -> SchemaOp
        "dropTable" => mono(vec![text()], schema_op_ty()),
        // addColumn : Text -> Column -> SchemaOp
        "addColumn" => mono(vec![text(), column_ty()], schema_op_ty()),
        // dropColumn : Text -> Text -> SchemaOp
        "dropColumn" => mono(vec![text(), text()], schema_op_ty()),
        // createIndex / uniqueIndex : Text -> Text -> List Text -> SchemaOp
        "createIndex" | "uniqueIndex" => mono(vec![text(), text(), list(text())], schema_op_ty()),
        // migration : Text -> List SchemaOp -> Migration
        "migration" => mono(
            vec![text(), list(schema_op_ty())],
            Type::Con(migration, vec![]),
        ),
        // run : ∀a. a -> List Migration -> Result (List Text) Error where Adapter a.
        // The runner reaches the schema seam through the `Adapter a` dictionary, the
        // same shape `transaction` carries; `a` is the only quantified variable.
        "run" => {
            let adapter = classes.id_by_name("Adapter")?;
            let a = TyVid(0);
            Some(Scheme {
                vars: vec![a],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Var(a), list(Type::Con(migration, vec![]))],
                    ret: Box::new(result(list(text()))),
                    caps: pure(),
                },
                constraints: vec![Constraint::single(adapter, a)],
            })
        }
        _ => None,
    }
}

/// The `std.raw` slice of [`reconciled_fn_scheme`]: the raw-SQL escape hatch.
/// `query`/`queryFirst` run a raw statement and decode the returned rows into the
/// entity (`where Adapter a, Row e`); `exec` runs a row-less statement for its
/// affected-row count (`where Adapter a`). Every verb takes the connection, the
/// SQL text, and a `List SqlValue` of bound parameters.
fn reconciled_raw_fn_scheme(name: &str, b: &BuiltinTyCons, classes: &ClassTable) -> Option<Scheme> {
    let adapter = classes.id_by_name("Adapter")?;
    let row = classes.id_by_name("Row")?;
    // Placeholder scheme vars: entity `e` and adapter `a`. The constraint order
    // must mirror what the stdlib build stores when it compiles the `std.raw`
    // source, since the lowering prepends one dictionary parameter per constraint
    // in that order on both the callee and the call site. The build generalises the
    // entity `e` (the decoded result) ahead of the adapter `a`, so `Row e` precedes
    // `Adapter a` — the same order the repository verbs use (`with_adapter_row`),
    // even though `a` appears first in the parameter list. The data-raw BEAM e2e is
    // what catches a flipped order: the adapter dictionary lands in the row slot.
    let e = TyVid(0);
    let a = TyVid(1);
    let pure = || CapRow::Concrete(CapabilitySet::PURE);
    let result = |ok: Type| Type::Con(b.result, vec![ok, Type::Con(b.error, vec![])]);
    // conn, sql, params — the three arguments shared by every raw verb.
    let raw_params = || {
        vec![
            Type::Var(a),
            Type::Con(b.text, vec![]),
            Type::Con(b.list, vec![Type::Con(b.sql_value, vec![])]),
        ]
    };
    let with_adapter_row = || vec![Constraint::single(row, e), Constraint::single(adapter, a)];
    match name {
        // query : ∀a e. a -> Text -> List SqlValue -> Result (List e) Error
        //              where Adapter a, Row e
        "query" => Some(Scheme {
            vars: vec![e, a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: raw_params(),
                ret: Box::new(result(Type::Con(b.list, vec![Type::Var(e)]))),
                caps: pure(),
            },
            constraints: with_adapter_row(),
        }),
        // queryFirst : ∀a e. a -> Text -> List SqlValue -> Result (Option e) Error
        //                   where Adapter a, Row e
        "queryFirst" => Some(Scheme {
            vars: vec![e, a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: raw_params(),
                ret: Box::new(result(Type::Con(b.option, vec![Type::Var(e)]))),
                caps: pure(),
            },
            constraints: with_adapter_row(),
        }),
        // exec : ∀a. a -> Text -> List SqlValue -> Result Int Error where Adapter a
        "exec" => Some(Scheme {
            vars: vec![a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: raw_params(),
                ret: Box::new(result(Type::Con(b.int, vec![]))),
                caps: pure(),
            },
            constraints: vec![Constraint::single(adapter, a)],
        }),
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
        // from : ∀e. List e -> Seq e where Row e — lift an in-memory list into the
        // query world. The element `e` needs only `Row` (auto-synthesised for a
        // record of SqlType fields); no adapter, since the rows are snapshotted in
        // hand. Single-var scheme — there is no adapter `a`.
        "from" => {
            let seq_con = *reconciled.get("Seq")?;
            Some(Scheme {
                vars: vec![e],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(b.list, vec![Type::Var(e)])],
                    ret: Box::new(Type::Con(seq_con, vec![Type::Var(e)])),
                    caps: pure(),
                },
                constraints: vec![Constraint::single(row, e)],
            })
        }
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
        // insertRows : ∀e a. List (Map Text SqlValue) -> Repo e a
        //   -> Result Unit Error where Adapter a. The bulk dual of `insertRow`:
        //   one multi-row INSERT over hand-built column maps that share the columns.
        "insertRows" => method(
            vec![Type::Con(b.list, vec![map_row()]), repo_app()],
            result(Type::Con(b.unit, vec![])),
            with_adapter(),
        ),
        // insertMany : ∀e a. List e -> Repo e a -> Result Unit Error
        //   where Adapter a, Row e. The bulk dual of `insert`: encodes each entity
        //   through `toRow` and appends the whole batch in one statement.
        "insertMany" => method(
            vec![list_e(), repo_app()],
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
        // transaction / withConnection : ∀a r. a -> (fn a -> Result r Error)
        //   -> Result r Error where Adapter a. Two Adapter-constrained HOFs sharing one
        // reconciled scheme. `transaction` runs the body inside a transaction (`begin`,
        // body, then `commit` on `Ok` or `rollback` on `Err`); `withConnection` runs the
        // body then `close`s the connection on every path, returning the body's own
        // result so a scoped connection is never leaked. The body is a live callback
        // (the first reconciled repo fns that take one), so like the std.list/std.result
        // HOFs its capability row is a fresh cap var the call site absorbs — a pure body
        // keeps the call pure. `r` is the body's own success type, threaded straight out;
        // `a` carries the `Adapter` dictionary the methods dispatch on.
        "transaction" | "withConnection" => {
            let r = TyVid(2);
            let cap_c = CapVid(0);
            let body = Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(result(Type::Var(r))),
                caps: CapRow::Var(cap_c),
            };
            Some(Scheme {
                vars: vec![a, r],
                cap_vars: vec![cap_c],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Var(a), body],
                    ret: Box::new(result(Type::Var(r))),
                    caps: pure(),
                },
                constraints: with_adapter(),
            })
        }
        // `disconnect` releases a connection — `close conn` over the `Adapter` seam.
        // One argument, no body, answering `Result Unit Error`; `a` carries the
        // `Adapter` dictionary `close` dispatches on. The handle is the proof of
        // access, so releasing it is capability-free like the query methods.
        "disconnect" => Some(Scheme {
            vars: vec![a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Var(a)],
                ret: Box::new(result(Type::Con(b.unit, vec![]))),
                caps: pure(),
            },
            constraints: with_adapter(),
        }),
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
        // `union`/`unionAll`/`intersect`/`except` are no longer reconciled here: they
        // became the methods of the `Combinable q` class (std.repo), one set of
        // set-operation builders over a query or an in-memory sequence. A qualified
        // `Repo.union` resolves to that class method, typed by the seeded `∀q. q -> q ->
        // q where Combinable q` scheme (see `seed_combinable_scheme`), the single receiver
        // parameter pinning the instance — so one binding serves a query and a `Seq`
        // alike, and a `Seq` gains the same set operations the query path has. Omitting
        // the arm routes them through the class-method path rather than the old
        // single-receiver pub fns.
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
        // `groupBy` is no longer reconciled here: it became the method of the
        // `Groupable q p | q -> p` class (std.repo), one verb that groups a query,
        // an inner join, or a left join by a key column the accessor names. A
        // qualified `Repo.groupBy` resolves to that class method, typed by the
        // seeded `∀q p. Quote p -> q -> Grouped q p where Groupable q p` scheme
        // (see `seed_groupable_scheme`), the fundep fixing the key accessor's arity
        // per receiver. Omitting the arm routes it through the class-method path.
        // having : ∀q p. Quote (Grouped q p -> Bool) -> Grouped q p -> Grouped q p.
        // A pure builder: it captures a predicate over the group aggregates
        // (`g.count`, `g.sum(col)`, …) and stores it as the grouped builder's
        // `HAVING`. The quote ranges over the `Grouped q p` handle, not a row; the
        // source `q` (a query or a join) carries the entities the column accessors
        // read.
        "having" => {
            let grouped_con = *reconciled.get("Grouped")?;
            let q = TyVid(0);
            let p = TyVid(1);
            let grouped = Type::Con(grouped_con, vec![Type::Var(q), Type::Var(p)]);
            let having_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![grouped.clone()],
                    ret: Box::new(Type::Con(b.bool, vec![])),
                    caps: pure(),
                }],
            );
            Some(Scheme {
                vars: vec![q, p],
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
        // summarize : ∀q p s. Quote (Grouped q p -> s) -> Grouped q p
        //   -> Result (List s) Error where Summarizable q, Row s. The projection
        // names a result record built from group aggregates (`Stats { dept = g.key,
        // n = g.count, total = g.sum (fn u -> u.salary) }`), which pins `s`; the
        // `Summarizable q` instance runs the GROUP BY against the source's seam (a
        // query, an inner join, or a left join) and `Row s` decodes each summarised
        // row. `q` first appears in the `Grouped q p` handle (param 0) before `s` (the
        // projection's result), so the constraint order is `Summarizable q` then
        // `Row s`.
        "summarize" => {
            let grouped_con = *reconciled.get("Grouped")?;
            let summarizable = classes.id_by_name("Summarizable")?;
            let q = TyVid(0);
            let p = TyVid(1);
            let s = TyVid(2);
            let grouped = Type::Con(grouped_con, vec![Type::Var(q), Type::Var(p)]);
            let proj_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![grouped.clone()],
                    ret: Box::new(Type::Var(s)),
                    caps: pure(),
                }],
            );
            Some(Scheme {
                vars: vec![q, p, s],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![proj_quote, grouped],
                    ret: Box::new(result(Type::Con(b.list, vec![Type::Var(s)]))),
                    caps: pure(),
                },
                constraints: vec![
                    Constraint::single(row, s),
                    Constraint::single(summarizable, q),
                ],
            })
        }
        // `joinOn` is no longer a standalone scheme: it is the `Joinable` class
        // method, seeded by `seed_joinable_scheme` so its condition (`JoinCond q f`)
        // and result (`JoinResult q f`) follow the receiver. Omitting the arm routes
        // it through that class path, the same way `toList`/`first` route through
        // `seed_decodable_scheme`.
        // crossJoin : ∀e f a. Repo f a -> Query e a -> Join e f a. The cartesian
        // builder: it pairs the left query with the right repository and no
        // condition, so it carries no quoted predicate. A cross join is an inner
        // join whose `ON` is always true, so it produces the same `Join e f a` and
        // shares its terminals, projection, and the rest of the join vocabulary.
        "crossJoin" => {
            let join_con = *reconciled.get("Join")?;
            let f = TyVid(2);
            let repo_f_a = Type::Con(repo_con, vec![Type::Var(f), Type::Var(a)]);
            let join_e_f_a = Type::Con(join_con, vec![Type::Var(e), Type::Var(f), Type::Var(a)]);
            Some(Scheme {
                vars: vec![e, a, f],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![repo_f_a, query_app()],
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
        // `leftJoinOn` is no longer a standalone scheme: it became the `LeftJoinable`
        // class method, seeded by `seed_leftjoinable_scheme` so its condition
        // (`JoinCond q f`) and result (`LeftJoinResult q f`) follow the receiver — a
        // binary `LeftJoin` from a query, the nested `LeftJoined` from a composite.
        // Omitting the arm routes it through that class path, the same way `joinOn`
        // routes through `seed_joinable_scheme`.
        // `rightJoinOn` is no longer a standalone scheme: it became the
        // `RightJoinable` class method, seeded by `seed_rightjoinable_scheme` so its
        // condition (`JoinCond q f`) and result (`RightJoinResult q f`) follow the
        // receiver — a binary `RightJoin` from a query, the nested `RightJoined` from
        // a composite. Omitting the arm routes it through that class path.
        // `fullJoinOn` is no longer a standalone scheme: it became the `FullJoinable`
        // class method, seeded by `seed_fulljoinable_scheme` so its condition
        // (`JoinCond q f`) and result (`FullJoinResult q f`) follow the receiver — a
        // binary `FullJoin` from a query, the nested `FullJoined` from a composite.
        // Omitting the arm routes it through that class path.
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
