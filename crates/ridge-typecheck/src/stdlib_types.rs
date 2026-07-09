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
            name: "MigrationOp".to_string(),
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
                    // `CreateEntity` carries the entity-driven schema descriptor with its
                    // phantom erased to `Unit` (`EntitySchema`, this block's `base + 30`),
                    // so the backend renders the full `CREATE TABLE` rather than the
                    // constraint-poor column tuple the other variants carry.
                    UnionVariant {
                        name: "CreateEntity".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(
                            TyConId(base + 30),
                            vec![Type::Con(b.unit, vec![])],
                        )]),
                    },
                    // `AddEntityColumn` carries the table name and one full column
                    // descriptor (`ColumnSchema`, this block's `base + 29`, phantom erased
                    // to `Unit`), so the backend renders `ALTER TABLE … ADD COLUMN` with
                    // the column's type, default, and constraints rather than the
                    // constraint-poor tuple `AddColumn` carries.
                    UnionVariant {
                        name: "AddEntityColumn".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(TyConId(base + 29), vec![Type::Con(b.unit, vec![])]),
                        ]),
                    },
                    // `AlterColumn` carries the table name and the old and new column
                    // descriptors (`ColumnSchema`, this block's `base + 29`, phantom erased
                    // to `Unit`) of a column present in both snapshots whose type,
                    // nullability, or default changed, so the backend renders a minimal
                    // `ALTER TABLE … ALTER COLUMN` from the difference between the two.
                    UnionVariant {
                        name: "AlterColumn".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(TyConId(base + 29), vec![Type::Con(b.unit, vec![])]),
                            Type::Con(TyConId(base + 29), vec![Type::Con(b.unit, vec![])]),
                        ]),
                    },
                    // `DropIndex` carries the index name alone — the inverse of a
                    // `CreateIndex`, run by a rollback. Only the name is needed to drop
                    // an index, which is why a `DropIndex` cannot be auto-reversed.
                    UnionVariant {
                        name: "DropIndex".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(b.text, vec![])]),
                    },
                    // `SeedRows`/`UnseedRows` carry a table, its key columns, and the rows
                    // to seed (a `List (Map Text SqlValue)`). `SeedRows` writes them as an
                    // idempotent upsert keyed on the columns; `UnseedRows` is its reverse,
                    // deleting the same rows by key, so a seed step auto-reverses like a
                    // create. Data steps, unlike the schema variants above.
                    UnionVariant {
                        name: "SeedRows".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
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
                        name: "UnseedRows".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                            Type::Con(
                                b.list,
                                vec![Type::Con(
                                    b.map,
                                    vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
                                )],
                            ),
                        ]),
                    },
                    // `RunSql` carries a raw SQL statement run verbatim against the
                    // backend through the `rawExec` seam — the escape hatch for what the
                    // typed DSL cannot express. Like a lossy drop it has no derivable
                    // inverse, so a migration that uses it must supply an explicit `down`.
                    UnionVariant {
                        name: "RunSql".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(b.text, vec![])]),
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
        // the tracking table), the ordered `MigrationOp` steps, and `down` — the explicit
        // reverse steps (`Some`) or `None` to derive the reverse from `steps` at
        // rollback. Users construct it through `migration`/`reversibleMigration` or the
        // record literal; field order mirrors the source.
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
                    RecordField {
                        name: "down".to_string(),
                        ty: Type::Con(
                            b.option,
                            vec![Type::Con(
                                b.list,
                                vec![Type::Con(TyConId(base + 11), vec![])],
                            )],
                        ),
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
        // counterpart of `QueryPlan`. `MutInsert table rows identityCols` appends one or
        // more rows, the backend filling the database-generated `identityCols` the rows omit;
        // `MutUpsert table rows conflictCols updateCols` appends rows, resolving a
        // unique-constraint conflict over `conflictCols` by overwriting `updateCols`
        // (`ON CONFLICT … DO UPDATE`) or, with no update columns, leaving the row
        // (`DO NOTHING`); `MutUpdate table changes pred` sets the given columns on the
        // rows its predicate admits; `MutDelete table pred` removes them; `MutDeleteKeys
        // table keyCols rows` removes the rows whose `keyCols` match one of `rows`, a
        // value-keyed delete a seed rollback runs. `mutationToSql` renders it to one
        // parameterized statement, sharing the predicate renderer with `planToSql`, so a
        // correlated `EXISTS` in a mutation predicate compiles exactly as it does in a query.
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
                            // identityCols: the database-generated identity columns the
                            // rows omit, a `List Text` the backend fills in their place.
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                        ]),
                    },
                    UnionVariant {
                        name: "MutUpsert".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            Type::Con(
                                b.list,
                                vec![Type::Con(
                                    b.map,
                                    vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
                                )],
                            ),
                            // conflictCols / updateCols: two `List Text`.
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
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
                    UnionVariant {
                        name: "MutDeleteKeys".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.text, vec![]),
                            // keyCols: the columns the delete matches each stored row on.
                            Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                            // rows: the key-carrying rows to remove, matched by `keyCols`.
                            Type::Con(
                                b.list,
                                vec![Type::Con(
                                    b.map,
                                    vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
                                )],
                            ),
                        ]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.repo` — a single conflict-key column built by `onConflict`, the upsert
        // counterpart of `Setter`. An opaque record `{ column: Text }` declared in Ridge
        // (stdlib/repo.ridge). The entity `e` (param 0) is phantom — it ties the key to
        // the record whose column the quoted accessor named, so a `List (Conflict e)`
        // cannot mix entities and must match the repository's `e`. Opaque, so user code
        // builds one only through `onConflict`.
        TyConDecl {
            id: TyConId(base + 23),
            name: "Conflict".to_string(),
            arity: 1,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0)],
                vec![RecordField {
                    name: "column".to_string(),
                    ty: Type::Con(b.text, vec![]),
                }],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.data` — the typed kind of a database error. A plain nullary union
        // declared in Ridge (stdlib/data.ridge); `dbErrorKind` classifies a raw
        // `Error`'s code into one of these, so user code matches a failure's cause
        // (a unique violation, a connection fault, …) rather than its code string.
        // Kept last in the block so its id is the next free slot.
        TyConDecl {
            id: TyConId(base + 24),
            name: "DbErrorKind".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "UniqueViolation".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "ForeignKeyViolation".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "NotNullViolation".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "CheckViolation".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "ConnectionError".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DecodeError".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Unsupported".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "QueryError".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.sql` — the dialect-neutral SQL column type. A union declared in
        // Ridge (stdlib/sql.ridge, beside SqlValue so the SqlType codec class can
        // name it); a `columnType` step overrides the default a field type implies,
        // and `DbRaw` spells a dialect-specific type by hand. Variant order mirrors
        // the source. (Reconciled ids are name-matched, so the id is unchanged by
        // the module move.)
        TyConDecl {
            id: TyConId(base + 25),
            name: "DbType".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "DbBoolean".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbInt".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbBigInt".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbFloat".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbDecimal".to_string(),
                        kind: VariantPayload::Positional(vec![
                            Type::Con(b.int, vec![]),
                            Type::Con(b.int, vec![]),
                        ]),
                    },
                    UnionVariant {
                        name: "DbText".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbVarchar".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(b.int, vec![])]),
                    },
                    UnionVariant {
                        name: "DbUuid".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbTimestamp".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbTimestampTz".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbBytes".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbSmallInt".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbChar".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(b.int, vec![])]),
                    },
                    UnionVariant {
                        name: "DbJson".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbJsonb".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbDate".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbTime".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbInterval".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DbRaw".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(b.text, vec![])]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.schema` — how a column's value originates. A union declared in Ridge
        // (stdlib/schema.ridge); a non-`Supplied` variant marks the column
        // database-generated or default-filled, which drives omit-on-insert.
        // Variant order mirrors the source.
        TyConDecl {
            id: TyConId(base + 26),
            name: "Generation".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "Supplied".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Identity".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DefaultNow".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "DefaultLit".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(b.sql_value, vec![])]),
                    },
                    UnionVariant {
                        name: "DefaultRawSql".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Con(b.text, vec![])]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.schema` — the referential action of a foreign key. A union declared
        // in Ridge (stdlib/schema.ridge), mirroring SQL's `ON DELETE`/`ON UPDATE`.
        // Variant order mirrors the source.
        TyConDecl {
            id: TyConId(base + 27),
            name: "FkAction".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "NoAction".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Restrict".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Cascade".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "SetNull".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "SetDefault".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        },
        // `std.schema` — a foreign-key reference. An opaque record declared in Ridge
        // (stdlib/schema.ridge): the target table and column and the on-delete /
        // on-update actions. Opaque, built only through `references` and refined
        // with `onDelete`/`onUpdate`. Field order mirrors the source.
        TyConDecl {
            id: TyConId(base + 28),
            name: "ForeignKey".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "table".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "column".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "onDelete".to_string(),
                        ty: Type::Con(TyConId(base + 27), vec![]),
                    },
                    RecordField {
                        name: "onUpdate".to_string(),
                        ty: Type::Con(TyConId(base + 27), vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.schema` — one column's schema. An opaque record declared in Ridge
        // (stdlib/schema.ridge) with a phantom entity parameter `e` that ties a
        // column to the entity it describes. Opaque, built through `mkColumn` and
        // refined with the per-column steps. Field order mirrors the source.
        TyConDecl {
            id: TyConId(base + 29),
            name: "ColumnSchema".to_string(),
            arity: 1,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0)],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "column".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "ty".to_string(),
                        ty: Type::Con(TyConId(base + 25), vec![]),
                    },
                    RecordField {
                        name: "nullable".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                    RecordField {
                        name: "generation".to_string(),
                        ty: Type::Con(TyConId(base + 26), vec![]),
                    },
                    RecordField {
                        name: "primaryKey".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                    RecordField {
                        name: "unique".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                    RecordField {
                        name: "indexed".to_string(),
                        ty: Type::Con(b.bool, vec![]),
                    },
                    RecordField {
                        name: "foreignKey".to_string(),
                        ty: Type::Con(b.option, vec![Type::Con(TyConId(base + 28), vec![])]),
                    },
                    RecordField {
                        name: "check".to_string(),
                        ty: Type::Con(b.option, vec![Type::Con(b.q_expr, vec![])]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.schema` — an entity's full schema. An opaque record declared in Ridge
        // (stdlib/schema.ridge) with a phantom entity parameter `e`: the entity
        // name, its SQL table, and its `ColumnSchema e` columns in declaration
        // order. Opaque, built through `schema`/`withColumn`. Field order mirrors
        // the source.
        TyConDecl {
            id: TyConId(base + 30),
            name: "EntitySchema".to_string(),
            arity: 1,
            kind: TyConKind::Record(RecordSchema::new(
                vec![TyVid(0)],
                vec![
                    RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "table".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    RecordField {
                        name: "columns".to_string(),
                        ty: Type::Con(
                            b.list,
                            vec![Type::Con(TyConId(base + 29), vec![Type::Var(TyVid(0))])],
                        ),
                    },
                    RecordField {
                        name: "primaryKeyColumns".to_string(),
                        ty: Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                    },
                    RecordField {
                        name: "uniqueConstraints".to_string(),
                        ty: Type::Con(
                            b.list,
                            vec![Type::Con(b.list, vec![Type::Con(b.text, vec![])])],
                        ),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: true,
            is_anon: false,
        },
        // `std.decimal` — how a rounding or a division drops the digits it cannot
        // keep. A nullary union declared in Ridge (stdlib/decimal.ridge); its
        // constructors resolve through the module import like `Asc`/`Desc`. Appended
        // last so it disturbs no earlier reconciled id.
        TyConDecl {
            id: TyConId(base + 31),
            name: "RoundingMode".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "HalfEven".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "HalfUp".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "HalfDown".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Up".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Down".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Ceiling".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Floor".to_string(),
                        kind: VariantPayload::Nullary,
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
        // std.decimal `round : RoundingMode -> Int -> Decimal -> Decimal` — rounds to
        // a fixed scale with the given mode. Names the reconciled `RoundingMode`, so
        // the hand-curated table cannot express it.
        ("std.decimal", "round") => {
            let rounding = *reconciled.get("RoundingMode")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![
                        Type::Con(rounding, vec![]),
                        Type::Con(b.int, vec![]),
                        Type::Con(b.decimal, vec![]),
                    ],
                    ret: Box::new(Type::Con(b.decimal, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                },
                constraints: vec![],
            })
        }
        // std.decimal `div : RoundingMode -> Int -> Decimal -> Decimal -> Result
        // Decimal Error` — divides to a fixed scale with the given mode; a zero
        // divisor is an `Err`. Names the reconciled `RoundingMode`.
        ("std.decimal", "div") => {
            let rounding = *reconciled.get("RoundingMode")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![
                        Type::Con(rounding, vec![]),
                        Type::Con(b.int, vec![]),
                        Type::Con(b.decimal, vec![]),
                        Type::Con(b.decimal, vec![]),
                    ],
                    ret: Box::new(Type::Con(
                        b.result,
                        vec![Type::Con(b.decimal, vec![]), Type::Con(b.error, vec![])],
                    )),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                },
                constraints: vec![],
            })
        }
        // std.data `dbErrorKind : Error -> DbErrorKind` — classifies a raw storage
        // error by its code into a typed kind. Its return type names the reconciled
        // `DbErrorKind`, so the hand-curated signature table cannot express it.
        ("std.data", "dbErrorKind") => {
            let db_error_kind = *reconciled.get("DbErrorKind")?;
            Some(Scheme {
                vars: vec![],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(b.error, vec![])],
                    ret: Box::new(Type::Con(db_error_kind, vec![])),
                    caps: CapRow::Concrete(CapabilitySet::PURE),
                },
                constraints: vec![],
            })
        }
        // std.data `dbErrorConstraint`/`dbErrorColumn`/`dbErrorTable : Error -> Text`
        // — read the constraint, column, or table a backend named on a raw error.
        // Grouped with `dbErrorKind` as the typed-error reading of an `Error`; seeded
        // here rather than the hand-curated table to keep that reading in one place.
        ("std.data", "dbErrorConstraint" | "dbErrorColumn" | "dbErrorTable") => Some(Scheme {
            vars: vec![],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![Type::Con(b.error, vec![])],
                ret: Box::new(Type::Con(b.text, vec![])),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![],
        }),
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
        // factories `planInsert`/`planUpsert`/`planUpdate`/`planDelete` and `mutationToSql`.
        (
            "std.query",
            "planInsert"
            | "planUpsert"
            | "planUpdate"
            | "planDelete"
            | "mutationToSql"
            | "mutationReturningToSql",
        ) => reconciled_mutation_plan_fn_scheme(name, reconciled, b),
        ("std.data", "selectRows" | "fetch") => reconciled_data_fn_scheme(name, b, classes?),
        ("std.repo", _) => reconciled_repo_fn_scheme(name, reconciled, b, classes?),
        ("std.migrate", _) => reconciled_migrate_fn_scheme(name, reconciled, b, classes?),
        ("std.raw", _) => reconciled_raw_fn_scheme(name, b, classes?),
        ("std.schema", _) => reconciled_schema_fn_scheme(name, reconciled, b, classes?),
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
/// `planUpsert`/`planUpdate`/`planDelete` build a `MutationPlan` node, and `mutationToSql`
/// lowers one to a parameterized statement plus its ordered bind values (the write-side
/// dual of `planToSql`). The builders return the reconciled `MutationPlan`, so none is
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
    // A `List Text` — an upsert's conflict and update column names.
    let text_list = || Type::Con(b.list, vec![text()]);
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
        // planInsert : Text -> List (Map Text SqlValue) -> List Text -> MutationPlan
        "planInsert" => Some(pure(vec![text(), rows(), text_list()], plan())),
        // planUpsert : Text -> List (Map Text SqlValue) -> List Text -> List Text -> MutationPlan
        "planUpsert" => Some(pure(vec![text(), rows(), text_list(), text_list()], plan())),
        // planUpdate : Text -> Map Text SqlValue -> QExpr -> MutationPlan
        "planUpdate" => Some(pure(vec![text(), row(), qexpr()], plan())),
        // planDelete : Text -> QExpr -> MutationPlan
        "planDelete" => Some(pure(vec![text(), qexpr()], plan())),
        // planDeleteKeys : Text -> List Text -> List (Map Text SqlValue) -> MutationPlan
        "planDeleteKeys" => Some(pure(vec![text(), text_list(), rows()], plan())),
        // mutationToSql : MutationPlan -> (Sql, List SqlValue)
        "mutationToSql" => Some(pure(
            vec![plan()],
            Type::Tuple(vec![
                Type::Con(b.sql, vec![]),
                Type::Con(b.list, vec![Type::Con(b.sql_value, vec![])]),
            ]),
        )),
        // mutationReturningToSql : MutationPlan -> List Text -> (Sql, List SqlValue)
        "mutationReturningToSql" => Some(pure(
            vec![plan(), text_list()],
            Type::Tuple(vec![
                Type::Con(b.sql, vec![]),
                Type::Con(b.list, vec![Type::Con(b.sql_value, vec![])]),
            ]),
        )),
        _ => None,
    }
}

/// The `std.schema` slice of [`reconciled_fn_scheme`]: the schema-descriptor
/// builders and read accessors. Every verb is pure; the column steps and entity
/// builders are polymorphic in the phantom entity `e`, the foreign-key builders
/// monomorphic. All reference the reconciled `DbType`/`Generation`/`FkAction`/
/// `ForeignKey`/`ColumnSchema`/`EntitySchema` types, so the hand-curated
/// signature table (which only sees `BuiltinTyCons`) cannot express them.
#[expect(
    clippy::too_many_lines,
    reason = "one scheme per schema-descriptor verb plus the shared type-builder closures; they read best kept together"
)]
fn reconciled_schema_fn_scheme(
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
    classes: &ClassTable,
) -> Option<Scheme> {
    // The probe-driven column-set readers `generatedColumnsOf`/`identityColumnsOf`:
    // `∀e. e -> List Text where HasSchema e`. They reference no descriptor TyCon — only
    // the `HasSchema` class and builtins — so they are resolved before the schema-type
    // lookups below, which are absent while the stdlib bundle itself is being built (the
    // reconciled type map is empty then). Resolving them up front keeps the write path's
    // dictionary-passing working in that self-build, where the later `?` guards would
    // otherwise short-circuit the whole function to `None`. The argument is a probe entity
    // (its value ignored) that pins the `HasSchema e` dictionary by its type.
    if matches!(
        name,
        "generatedColumnsOf" | "identityColumnsOf" | "identityColumnsOfShape"
    ) {
        let has_schema = classes.id_by_name("HasSchema")?;
        let e = TyVid(0);
        // `identityColumnsOfShape` reads the columns from an insert shape rather than a
        // probe entity, so the typed insert verbs resolve them from the companion value
        // they already hold; the other two take a probe entity `e`.
        let arg = if name == "identityColumnsOfShape" {
            Type::Con(b.insert_shape, vec![Type::Var(e)])
        } else {
            Type::Var(e)
        };
        return Some(Scheme {
            vars: vec![e],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params: vec![arg],
                ret: Box::new(Type::Con(b.list, vec![Type::Con(b.text, vec![])])),
                caps: CapRow::Concrete(CapabilitySet::PURE),
            },
            constraints: vec![Constraint::single(has_schema, e)],
        });
    }
    let db_type = *reconciled.get("DbType")?;
    let generation = *reconciled.get("Generation")?;
    let fk_action = *reconciled.get("FkAction")?;
    let foreign_key = *reconciled.get("ForeignKey")?;
    let column_schema = *reconciled.get("ColumnSchema")?;
    let entity_schema = *reconciled.get("EntitySchema")?;
    let e = TyVid(0);
    let text = || Type::Con(b.text, vec![]);
    let boolean = || Type::Con(b.bool, vec![]);
    let option = |x: Type| Type::Con(b.option, vec![x]);
    let list = |x: Type| Type::Con(b.list, vec![x]);
    let qexpr = || Type::Con(b.q_expr, vec![]);
    let dbtype_ty = || Type::Con(db_type, vec![]);
    let gen_ty = || Type::Con(generation, vec![]);
    let fk_act_ty = || Type::Con(fk_action, vec![]);
    let fk_ty = || Type::Con(foreign_key, vec![]);
    let col_e = || Type::Con(column_schema, vec![Type::Var(e)]);
    let ent_e = || Type::Con(entity_schema, vec![Type::Var(e)]);
    // `EntitySchema Unit` — the phantom-erased schema a migration step carries.
    let ent_unit = || Type::Con(entity_schema, vec![Type::Con(b.unit, vec![])]);
    // The migration seam's column tuple `(Text, Text, Bool, Bool, Bool)` — the
    // `(name, base-type, nullable, primaryKey, unique)` the runner flattens a column to.
    let col_tuple = || Type::Tuple(vec![text(), text(), boolean(), boolean(), boolean()]);
    let pure = || CapRow::Concrete(CapabilitySet::PURE);
    // A monomorphic pure builder: `params -> ret`, no quantified vars.
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
    // A pure builder polymorphic in the phantom entity `e`: `∀e. params -> ret`.
    let poly1 = |params: Vec<Type>, ret: Type| {
        Some(Scheme {
            vars: vec![e],
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
        // Foreign-key reference builders (monomorphic).
        "references" => mono(vec![text(), text()], fk_ty()),
        "onDelete" | "onUpdate" => mono(vec![fk_act_ty(), fk_ty()], fk_ty()),
        // Column constructor and refinement steps (∀e. … -> ColumnSchema e).
        "mkColumn" => poly1(vec![text(), text(), dbtype_ty(), boolean()], col_e()),
        // column : ∀e v. Quote (e -> v) -> DbType -> Bool -> ColumnSchema e. Builds a
        //   column from an accessor quote naming a single field of type `v` (phantom —
        //   only the column name is kept), the same capture `onConflict`/`set` read.
        "column" => {
            let v = TyVid(1);
            let accessor = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e)],
                    ret: Box::new(Type::Var(v)),
                    caps: pure(),
                }],
            );
            Some(Scheme {
                vars: vec![e, v],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![accessor, dbtype_ty(), boolean()],
                    ret: Box::new(col_e()),
                    caps: pure(),
                },
                constraints: vec![],
            })
        }
        "named" => poly1(vec![text(), col_e()], col_e()),
        "columnType" => poly1(vec![dbtype_ty(), col_e()], col_e()),
        "generated" => poly1(vec![gen_ty(), col_e()], col_e()),
        "foreignKey" => poly1(vec![fk_ty(), col_e()], col_e()),
        // check : ∀e. Quote (e -> Bool) -> ColumnSchema e -> ColumnSchema e. Attaches a
        //   CHECK constraint as a captured predicate over the column's own entity, so
        //   the predicate can only read that entity's fields.
        "check" => {
            let pred = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e)],
                    ret: Box::new(boolean()),
                    caps: pure(),
                }],
            );
            poly1(vec![pred, col_e()], col_e())
        }
        // checkRaw : ∀e. QExpr -> ColumnSchema e -> ColumnSchema e. Attaches a CHECK from an
        // already-built predicate tree (the escape hatch the source renderer rebuilds a check
        // through, since a phantom-erased schema cannot restore the original quote).
        "checkRaw" => poly1(vec![qexpr(), col_e()], col_e()),
        "nullable" | "required" | "primaryKey" | "unique" | "indexed" => {
            poly1(vec![col_e()], col_e())
        }
        // Column read accessors (∀e. ColumnSchema e -> …). `columnToSource` shares the
        // `ColumnSchema e -> Text` shape: it renders a column back to the `mkColumn … |> …`
        // source that rebuilds it, the source dual of `columnDdl`.
        "colName" | "colColumn" | "columnToSource" => poly1(vec![col_e()], text()),
        "colType" => poly1(vec![col_e()], dbtype_ty()),
        "colGeneration" => poly1(vec![col_e()], gen_ty()),
        "colNullable" | "colPrimaryKey" | "colUnique" | "colIndexed" | "colGenerated" => {
            poly1(vec![col_e()], boolean())
        }
        "colForeignKey" => poly1(vec![col_e()], option(fk_ty())),
        // fkTable : ForeignKey -> Text — the reference's target table, read by the snapshot
        // diff to order `CREATE TABLE`s topologically (a referenced table before its referrer).
        "fkTable" => mono(vec![fk_ty()], text()),
        "colCheck" => poly1(vec![col_e()], option(qexpr())),
        // Entity schema builders and read accessors (∀e. … over EntitySchema e).
        "schema" => poly1(vec![text(), text()], ent_e()),
        "withColumn" => poly1(vec![col_e(), ent_e()], ent_e()),
        // `compositePrimaryKey`/`uniqueConstraint` : ∀e. List Text -> EntitySchema e ->
        // EntitySchema e — the multi-column table-constraint steps, each naming its
        // columns by SQL column name (the composite key, or one unique constraint).
        "compositePrimaryKey" | "uniqueConstraint" => poly1(vec![list(text()), ent_e()], ent_e()),
        // `schemaToDdl` renders an entity's `CREATE TABLE`; it shares the
        // `EntitySchema e -> Text` shape with the name accessors. `schemaIndexDdls`
        // renders its non-unique indexes, sharing the `-> List Text` shape with the
        // generated-column readers.
        // `schemaToSource` renders an entity's `schema … |> withColumn …` builder
        // source; it shares the `EntitySchema e -> Text` shape with the name accessors
        // and `schemaToDdl`.
        "schemaName" | "schemaTable" | "schemaToDdl" | "schemaToSource" => {
            poly1(vec![ent_e()], text())
        }
        "schemaColumns" => poly1(vec![ent_e()], list(col_e())),
        // The table-constraint read accessors: the composite primary key's columns
        // (empty when the key is a single per-column one, or absent), and the
        // multi-column unique constraints (a column list each).
        "schemaPrimaryKey" => poly1(vec![ent_e()], list(text())),
        "schemaUniqueConstraints" => poly1(vec![ent_e()], list(list(text()))),
        // eraseSchema : ∀e. EntitySchema e -> EntitySchema Unit — drop the phantom
        // entity so a non-parametric migration step can carry the descriptor.
        "eraseSchema" => poly1(vec![ent_e()], ent_unit()),
        "generatedColumns" | "identityColumns" | "schemaIndexDdls" => {
            poly1(vec![ent_e()], list(text()))
        }
        // The migration step renderers over the seam tuple `(name, base-type, nullable,
        // primaryKey, unique)` — the DDL the retired Erlang builder produced.
        "createTableDdl" => mono(vec![text(), list(col_tuple())], text()),
        "addColumnDdl" => mono(vec![text(), col_tuple()], text()),
        // addColumnSchemaDdl : ∀e. Text -> ColumnSchema e -> Text — the entity-driven
        // ADD COLUMN renderer, keeping the descriptor's type/default/constraints.
        "addColumnSchemaDdl" => poly1(vec![text(), col_e()], text()),
        // alterColumnDdl : ∀e. Text -> ColumnSchema e -> ColumnSchema e -> Text — the
        // entity-driven ALTER COLUMN renderer, taking a column's old and new descriptors and
        // emitting the minimal statement for the facets (type, nullability, default) that differ.
        "alterColumnDdl" => poly1(vec![text(), col_e(), col_e()], text()),
        // columnAltered : ∀e. ColumnSchema e -> ColumnSchema e -> Bool — the snapshot-diff
        // predicate: whether a column's type, nullability, or default changed (a serial/identity
        // column is excluded), i.e. whether the diff emits an `AlterColumn` for it.
        "columnAltered" => poly1(vec![col_e(), col_e()], boolean()),
        // dropTableDdl / dropIndexDdl : Text -> Text — a name-only `DROP TABLE` /
        // `DROP INDEX IF EXISTS`; `dropIndexDdl` is the inverse of `indexDdl`.
        "dropTableDdl" | "dropIndexDdl" => mono(vec![text()], text()),
        // dropColumnDdl / indexName : Text -> Text -> Text — a two-name `DROP COLUMN` renderer
        // (table, column) and the conventional `<table>_<column>_idx` index name (the single
        // naming source the entity create and the snapshot diff share); both take two texts.
        "dropColumnDdl" | "indexName" => mono(vec![text(), text()], text()),
        "indexDdl" => mono(vec![text(), text(), list(text()), boolean()], text()),
        // schemaOf : ∀e. Option e -> EntitySchema e where HasSchema e. The single
        // method of the `HasSchema` binding class, dispatched by a phantom
        // `Option e` witness (the same shape `Row.rowColumns` uses). The
        // `HasSchema e` constraint is what makes the lowering pass the instance
        // dictionary — without it, a bare `schemaOf` is read as a plain stdlib
        // symbol and codegen reports a missing bridge.
        "schemaOf" => {
            let has_schema = classes.id_by_name("HasSchema")?;
            Some(Scheme {
                vars: vec![e],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![option(Type::Var(e))],
                    ret: Box::new(ent_e()),
                    caps: pure(),
                },
                constraints: vec![Constraint::single(has_schema, e)],
            })
        }
        // toInsertRow : ∀e. InsertShape e -> Map Text SqlValue where HasSchema e. The second
        // `HasSchema` method: encodes the entity's insert shape — the entity minus its
        // database-generated columns — to a row, so the write path turns a companion value into
        // columns without ever encoding a generated key. The `HasSchema e` constraint passes the
        // instance dictionary the same way `schemaOf` does.
        "toInsertRow" => {
            let has_schema = classes.id_by_name("HasSchema")?;
            let map_row = Type::Con(
                b.map,
                vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
            );
            Some(Scheme {
                vars: vec![e],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![Type::Con(b.insert_shape, vec![Type::Var(e)])],
                    ret: Box::new(map_row),
                    caps: pure(),
                },
                constraints: vec![Constraint::single(has_schema, e)],
            })
        }
        _ => None,
    }
}

/// The `std.migrate` slice of [`reconciled_fn_scheme`]: the schema-DSL builders and
/// the migration runner. The builders are pure and reference the reconciled
/// `Column`/`MigrationOp`/`Migration` types; `run` and `applied` are the constrained
/// verbs (`where Adapter a`, to reach the schema seam) — `applied` touches no
/// reconciled type itself, but every `std.migrate` export is dispatched through
/// this table (see the `("std.migrate", _)` arm in [`reconciled_fn_scheme`]), and
/// a typeclass-constrained cross-module scheme needs the same hand-built
/// `Constraint`/`ClassTable` wiring `run` does.
#[expect(
    clippy::too_many_lines,
    reason = "one scheme per std.migrate verb plus the shared type-builder closures; they read best kept together"
)]
fn reconciled_migrate_fn_scheme(
    name: &str,
    reconciled: &FxHashMap<String, TyConId>,
    b: &BuiltinTyCons,
    classes: &ClassTable,
) -> Option<Scheme> {
    let column = *reconciled.get("Column")?;
    let migration_op = *reconciled.get("MigrationOp")?;
    let migration = *reconciled.get("Migration")?;
    let entity_schema = *reconciled.get("EntitySchema")?;
    let column_schema = *reconciled.get("ColumnSchema")?;
    let text = || Type::Con(b.text, vec![]);
    let list = |x: Type| Type::Con(b.list, vec![x]);
    let pure = || CapRow::Concrete(CapabilitySet::PURE);
    let result = |ok: Type| Type::Con(b.result, vec![ok, Type::Con(b.error, vec![])]);
    let column_ty = || Type::Con(column, vec![]);
    let migration_op_ty = || Type::Con(migration_op, vec![]);
    let e = TyVid(0);
    let ent_e = || Type::Con(entity_schema, vec![Type::Var(e)]);
    let ent_unit = || Type::Con(entity_schema, vec![Type::Con(b.unit, vec![])]);
    // `ColumnSchema Unit` — the phantom-erased column an entity-driven step carries.
    let col_unit = || Type::Con(column_schema, vec![Type::Con(b.unit, vec![])]);
    // `List (Map Text SqlValue)` — the seed rows a data step carries, the same erased
    // row shape the mutation plan carries.
    let rows = || {
        list(Type::Con(
            b.map,
            vec![text(), Type::Con(b.sql_value, vec![])],
        ))
    };
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
    // A pure builder polymorphic in the phantom entity `e`: `∀e. params -> ret`.
    let poly_e = |params: Vec<Type>, ret: Type| {
        Some(Scheme {
            vars: vec![e],
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
    // A pure builder over the `Adapter a` dictionary: `∀a. a -> rest -> ret where
    // Adapter a`. The receiver `a` is prepended; `run`/`applied`/`rollback`/`revertTo`
    // share this shape and differ only in the parameters after the receiver.
    let adapter_scheme = |rest: Vec<Type>, ret: Type| -> Option<Scheme> {
        let adapter = classes.id_by_name("Adapter")?;
        let a = TyVid(0);
        let mut params = vec![Type::Var(a)];
        params.extend(rest);
        Some(Scheme {
            vars: vec![a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params,
                ret: Box::new(ret),
                caps: pure(),
            },
            constraints: vec![Constraint::single(adapter, a)],
        })
    };
    match name {
        // intCol / textCol / boolCol / floatCol : Text -> Column — the typed column
        // declarators, each pinning the base type.
        "intCol" | "textCol" | "boolCol" | "floatCol" => mono(vec![text()], column_ty()),
        // nullable / primaryKey / unique : Column -> Column
        "nullable" | "primaryKey" | "unique" => mono(vec![column_ty()], column_ty()),
        // createTable : Text -> List Column -> MigrationOp
        "createTable" => mono(vec![text(), list(column_ty())], migration_op_ty()),
        // dropTable / dropIndex / runSql : Text -> MigrationOp — the name-only drop steps
        // and the raw-SQL escape hatch (a statement run verbatim through the `rawExec` seam
        // for a schema change the typed DSL cannot express); all three take a single `Text`.
        "dropTable" | "dropIndex" | "runSql" => mono(vec![text()], migration_op_ty()),
        // addColumn : Text -> Column -> MigrationOp
        "addColumn" => mono(vec![text(), column_ty()], migration_op_ty()),
        // dropColumn : Text -> Text -> MigrationOp
        "dropColumn" => mono(vec![text(), text()], migration_op_ty()),
        // createIndex / uniqueIndex : Text -> Text -> List Text -> MigrationOp
        "createIndex" | "uniqueIndex" => {
            mono(vec![text(), text(), list(text())], migration_op_ty())
        }
        // createSchema / dropSchema : ∀e. EntitySchema e -> MigrationOp — the entity-driven
        // table create and drop, taking the schema descriptor in place of a column tuple.
        "createSchema" | "dropSchema" => poly_e(vec![ent_e()], migration_op_ty()),
        // addEntityColumn : Text -> ColumnSchema Unit -> MigrationOp — the entity-driven ADD
        // COLUMN factory (a phantom-erased descriptor); the diff's added-column step.
        "addEntityColumn" => mono(vec![text(), col_unit()], migration_op_ty()),
        // alterColumn : Text -> ColumnSchema Unit -> ColumnSchema Unit -> MigrationOp — the
        // ALTER COLUMN factory carrying a column's old and new descriptors; the diff's
        // altered-column step.
        "alterColumn" => mono(vec![text(), col_unit(), col_unit()], migration_op_ty()),
        // seed : ∀e. List e -> MigrationOp where Row e, HasSchema e — the typed data step.
        // The entity is encoded to a row through `Row` and its table and key columns read
        // from `HasSchema`, then erased into a `SeedRows` op. Constraint order is [Row,
        // HasSchema] to match the dictionary order the body threads (the row encode takes
        // the first dictionary, `schemaOf` the second); the reconciled order must equal the
        // self-built body's or the runtime reads a method from the wrong dictionary.
        "seed" => {
            let row = classes.id_by_name("Row")?;
            let has_schema = classes.id_by_name("HasSchema")?;
            Some(Scheme {
                vars: vec![e],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![list(Type::Var(e))],
                    ret: Box::new(migration_op_ty()),
                    caps: pure(),
                },
                constraints: vec![
                    Constraint::single(row, e),
                    Constraint::single(has_schema, e),
                ],
            })
        }
        // seedRows : Text -> List Text -> List (Map Text SqlValue) -> MigrationOp — the raw
        // data step: a table, its key columns, and hand-built rows, for a table with no
        // typed entity.
        "seedRows" => mono(vec![text(), list(text()), rows()], migration_op_ty()),
        // diffSchemas : List (EntitySchema Unit) -> List (EntitySchema Unit) -> List MigrationOp —
        // the pure snapshot diff: the schema steps that turn the `prev` model into `next`.
        "diffSchemas" => mono(
            vec![list(ent_unit()), list(ent_unit())],
            list(migration_op_ty()),
        ),
        // migration : Text -> List MigrationOp -> Migration
        "migration" => mono(
            vec![text(), list(migration_op_ty())],
            Type::Con(migration, vec![]),
        ),
        // modelToSource : List (EntitySchema Unit) -> Text renders a model snapshot to the
        // `[ schema … , … ]` expression; snapshotModule wraps it in the whole `.ridge`
        // snapshot module (imports + `model ()`). Same `List (EntitySchema Unit) -> Text`.
        "modelToSource" | "snapshotModule" => mono(vec![list(ent_unit())], text()),
        // migrationToSource : Migration -> Text renders a migration to the
        // `migration "name" [ … ]` expression; migrationModule wraps it in the whole
        // `.ridge` migration module (imports + `up ()`). Same `Migration -> Text`.
        "migrationToSource" | "migrationModule" => mono(vec![Type::Con(migration, vec![])], text()),
        // run : ∀a. a -> List Migration -> Result (List Text) Error where Adapter a.
        // The runner reaches the schema seam through the `Adapter a` dictionary, the
        // same shape `transaction` carries; `a` is the only quantified variable.
        "run" => adapter_scheme(
            vec![list(Type::Con(migration, vec![]))],
            result(list(text())),
        ),
        // applied : ∀a. a -> Result (List Text) Error where Adapter a. A plain
        // top-level re-export of the `Adapter` class method `migrationsApplied`,
        // for a caller that only wants the applied set (`ridge migrate status`)
        // without importing `std.data` or naming the class method directly.
        "applied" => adapter_scheme(vec![], result(list(text()))),
        // reversibleMigration : Text -> List MigrationOp -> List MigrationOp -> Migration
        "reversibleMigration" => mono(
            vec![text(), list(migration_op_ty()), list(migration_op_ty())],
            Type::Con(migration, vec![]),
        ),
        // rollback : ∀a. a -> List Migration -> Int -> Result (List Text) Error where Adapter a
        "rollback" => adapter_scheme(
            vec![list(Type::Con(migration, vec![])), Type::Con(b.int, vec![])],
            result(list(text())),
        ),
        // revertTo : ∀a. a -> List Migration -> Text -> Result (List Text) Error where Adapter a
        "revertTo" => adapter_scheme(
            vec![list(Type::Con(migration, vec![])), text()],
            result(list(text())),
        ),
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

/// The `std.data` slice of [`reconciled_fn_scheme`]: the `selectRows`/`fetch` read
/// helpers — standalone functions over the `Adapter` seam. Each takes a connection and
/// a quoted predicate (`fetch` adds the ordering keys, the page bounds, and a distinct
/// flag), builds a single-table scan, and answers the raw row maps, constrained over
/// `Adapter a`. The predicate pins the entity `e` from the call-site lambda, so neither
/// is expressible in the hand-curated signature table.
fn reconciled_data_fn_scheme(
    name: &str,
    b: &BuiltinTyCons,
    classes: &ClassTable,
) -> Option<Scheme> {
    let adapter = classes.id_by_name("Adapter")?;
    // Scheme-level placeholder vars: entity `e` (in the predicate) and adapter `a`.
    // Fresh copies are made on each instantiation, so the fixed ids here are dummies.
    let e = TyVid(0);
    let a = TyVid(1);
    let pure = || CapRow::Concrete(CapabilitySet::PURE);
    // A raw column map `Map Text SqlValue`, and the `Result (List …) Error` both verbs answer.
    let map_row = || {
        Type::Con(
            b.map,
            vec![Type::Con(b.text, vec![]), Type::Con(b.sql_value, vec![])],
        )
    };
    let result_rows = || {
        Type::Con(
            b.result,
            vec![
                Type::Con(b.list, vec![map_row()]),
                Type::Con(b.error, vec![]),
            ],
        )
    };
    // A quoted predicate `Quote (e -> Bool)`. The entity `e` is pinned from the
    // predicate's parameter annotation when the lambda is captured at the call site.
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
    // Assemble a scheme `∀e a. params -> Result (List (Map Text SqlValue)) Error`,
    // pure, constrained over `Adapter a` — the verbs touch only the adapter, no decode.
    let scheme = |params: Vec<Type>| {
        Some(Scheme {
            vars: vec![e, a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Fn {
                params,
                ret: Box::new(result_rows()),
                caps: pure(),
            },
            constraints: vec![Constraint::single(adapter, a)],
        })
    };
    match name {
        // selectRows : ∀e a. a -> Text -> Quote (e -> Bool)
        //                  -> Result (List (Map Text SqlValue)) Error where Adapter a
        "selectRows" => scheme(vec![Type::Var(a), Type::Con(b.text, vec![]), quote_pred()]),
        // fetch : ∀e a. a -> Text -> Quote (e -> Bool) -> List (Bool, QExpr)
        //              -> Int -> Int -> Bool
        //              -> Result (List (Map Text SqlValue)) Error where Adapter a.
        // The order keys are `(ascending?, column)` pairs; the two Ints are the limit
        // (negative for none) and offset (non-positive for none); the Bool is `distinct`.
        "fetch" => {
            let orders = Type::Con(
                b.list,
                vec![Type::Tuple(vec![
                    Type::Con(b.bool, vec![]),
                    Type::Con(b.q_expr, vec![]),
                ])],
            );
            scheme(vec![
                Type::Var(a),
                Type::Con(b.text, vec![]),
                quote_pred(),
                orders,
                Type::Con(b.int, vec![]),
                Type::Con(b.int, vec![]),
                Type::Con(b.bool, vec![]),
            ])
        }
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
    let has_schema = classes.id_by_name("HasSchema")?;
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
    // The insert shape of entity `e` — `InsertShape e`, the entity minus its
    // database-generated columns. The typed insert verbs take this rather than `e`,
    // so a serial/identity column cannot be set by hand. It reduces to `e` itself
    // when the entity has no generated column.
    let insert_shape_e = || Type::Con(b.insert_shape, vec![Type::Var(e)]);
    let list_insert_shape_e = || Type::Con(b.list, vec![insert_shape_e()]);
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
    // The typed insert verbs encode the insert shape and read the entity's schema
    // through `HasSchema e` — `toInsertRow` for the row and `identityColumnsOfShape`
    // for the auto-increment columns — so the plain inserts carry only `HasSchema e`
    // and `Adapter a`. The dict order is `[HasSchema e, Adapter a]`: the `e`-constrained
    // dictionary precedes the `a`-constrained one, the order the stdlib build lowers
    // the dictionaries, which the call site must match exactly.
    let with_adapter_schema = || {
        vec![
            Constraint::single(has_schema, e),
            Constraint::single(adapter, a),
        ]
    };
    // The RETURNING insert verbs also decode the stored row back (`fromRow`/`decodeRows`),
    // so they carry `Row e` too. The build lowers their dictionaries in the order
    // `[Row e, HasSchema e, Adapter a]` — `Row` ahead of `HasSchema`, the e-constrained
    // dictionaries before the a-constrained one. The plain (non-decoding) inserts use
    // `with_adapter_schema` above; the two orders are not interchangeable, so each group
    // takes the one its body produces.
    let with_adapter_schema_returning = || {
        vec![
            Constraint::single(row, e),
            Constraint::single(has_schema, e),
            Constraint::single(adapter, a),
        ]
    };
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
        //
        // `deleteReturning` shares this exact scheme — a quoted predicate over the repo
        // answering a decoded list — and is reconciled here with it; only the SQL the two
        // emit differs (a `SELECT` versus a `DELETE … RETURNING *`), which is the runtime
        // body, not the type.
        "findBy" | "deleteReturning" => method(
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
        // insert : ∀e a. InsertShape e -> Repo e a -> Result Unit Error where Adapter a, HasSchema e.
        // The typed dual of `insertRow`: takes the entity's insert shape — the entity minus its
        // database-generated columns — encodes it through `HasSchema`'s `toInsertRow`, and appends
        // it. Carries only `HasSchema e`: a serial/identity column is absent from the shape, so no
        // `Row e` (the encode lives on the schema), and the column the in-memory store fills comes
        // from `identityColumnsOfShape`.
        "insert" => method(
            vec![insert_shape_e(), repo_app()],
            result(Type::Con(b.unit, vec![])),
            with_adapter_schema(),
        ),
        // insertRows : ∀e a. List (Map Text SqlValue) -> Repo e a
        //   -> Result Unit Error where Adapter a. The bulk dual of `insertRow`:
        //   one multi-row INSERT over hand-built column maps that share the columns.
        "insertRows" => method(
            vec![Type::Con(b.list, vec![map_row()]), repo_app()],
            result(Type::Con(b.unit, vec![])),
            with_adapter(),
        ),
        // insertMany : ∀e a. List (InsertShape e) -> Repo e a -> Result Unit Error
        //   where Adapter a, HasSchema e. The bulk dual of `insert`: encodes each insert
        //   shape through `HasSchema`'s `toInsertRow` and appends the whole batch in one
        //   statement.
        "insertMany" => method(
            vec![list_insert_shape_e(), repo_app()],
            result(Type::Con(b.unit, vec![])),
            with_adapter_schema(),
        ),
        // insertReturning : ∀e a. InsertShape e -> Repo e a -> Result e Error
        //   where Adapter a, Row e, HasSchema e. Insert the entity's insert shape and read the
        //   stored row back, decoded (an INSERT … RETURNING *), so a server-filled column comes
        //   back populated. Carries `Row e` for the decode on top of `HasSchema e` for the encode;
        //   the dict order is `[Row e, HasSchema e, Adapter a]`.
        "insertReturning" => method(
            vec![insert_shape_e(), repo_app()],
            result(Type::Var(e)),
            with_adapter_schema_returning(),
        ),
        // insertManyReturning : ∀e a. List (InsertShape e) -> Repo e a -> Result (List e) Error
        //   where Adapter a, Row e, HasSchema e. The bulk dual of `insertReturning`; same dict
        //   order.
        "insertManyReturning" => method(
            vec![list_insert_shape_e(), repo_app()],
            result(list_e()),
            with_adapter_schema_returning(),
        ),
        // deleteReturning (∀e a. Quote (e -> Bool) -> Repo e a -> Result (List e) Error
        //   where Adapter a, Row e — remove the matching rows and read each back, decoded,
        //   a DELETE … RETURNING *) shares `findBy`'s scheme exactly and is reconciled in
        //   that arm above.
        // upsertReturning : ∀e a. e -> List (Conflict e) -> Repo e a -> Result e Error
        //   where Adapter a, Row e. Upsert the entity and read the resulting row back,
        //   decoded (INSERT … ON CONFLICT … DO UPDATE … RETURNING *). Unlike `upsert` —
        //   which carries the same `List (Conflict e)` parameter yet generalises `[Adapter
        //   a, Row e]` — this verb decodes the returned row with `fromRow` *after* the
        //   adapter call, and that trailing `Row e` use makes the source generalise `e`
        //   first, so it takes the ordinary `with_adapter_row` `[Row e, Adapter a]` order,
        //   matching the other RETURNING verbs. Verified by the `data_write` BEAM e2e.
        "upsertReturning" => {
            let conflict_con = *reconciled.get("Conflict")?;
            let list_conflict =
                Type::Con(b.list, vec![Type::Con(conflict_con, vec![Type::Var(e)])]);
            method(
                vec![Type::Var(e), list_conflict, repo_app()],
                result(Type::Var(e)),
                with_adapter_row(),
            )
        }
        // onConflict : ∀e a v. Quote (e -> v) -> Conflict e. Builds a typed conflict key
        //   from an accessor quote, the upsert counterpart of `set`: the quote names a
        //   single column whose type `v` is read off the entity (phantom — only the
        //   column name is kept). `a` is unused but kept so the quantifier shape lines up
        //   with the other repository schemes. No constraint: a conflict key names a
        //   column, it does not encode a value.
        "onConflict" => {
            let conflict_con = *reconciled.get("Conflict")?;
            let v = TyVid(2);
            let col_quote = Type::Con(
                b.quote,
                vec![Type::Fn {
                    params: vec![Type::Var(e)],
                    ret: Box::new(Type::Var(v)),
                    caps: pure(),
                }],
            );
            Some(Scheme {
                vars: vec![e, a, v],
                cap_vars: vec![],
                row_vars: vec![],
                ty: Type::Fn {
                    params: vec![col_quote],
                    ret: Box::new(Type::Con(conflict_con, vec![Type::Var(e)])),
                    caps: pure(),
                },
                constraints: vec![],
            })
        }
        // upsert : ∀e a. e -> List (Conflict e) -> Repo e a -> Result Int Error
        //   where Adapter a, Row e. Insert the entity or, on a unique-constraint conflict
        //   over the key columns, overwrite every other column with its values; answers
        //   how many rows were written. Carries `Row e` because it derives the row.
        //
        //   Dict order is `[Adapter a, Row e]`, NOT `with_adapter_row` — the
        //   `List (Conflict e)` parameter makes the source generalise `a` before `e`
        //   (where `update`/`insert`, whose constrained params mention only `e` and
        //   `Repo e a`, generalise `e` first). The lowering threads dicts in the source's
        //   generalised-variable order, so this hand-written scheme must match it or the
        //   `toRow`/`runMutation` dicts swap at the call boundary. Verified by the
        //   `data_write` BEAM e2e (an entity round-trips through upsert).
        "upsert" => {
            let conflict_con = *reconciled.get("Conflict")?;
            let list_conflict =
                Type::Con(b.list, vec![Type::Con(conflict_con, vec![Type::Var(e)])]);
            method(
                vec![Type::Var(e), list_conflict, repo_app()],
                result(Type::Con(b.int, vec![])),
                vec![Constraint::single(adapter, a), Constraint::single(row, e)],
            )
        }
        // insertOrIgnore : ∀e a. e -> List (Conflict e) -> Repo e a -> Result Int Error
        //   where Adapter a, Row e. The `DO NOTHING` companion of `upsert`: insert the
        //   entity or, on a conflict over the key columns, leave the existing row. Same
        //   `[Adapter a, Row e]` dict order as `upsert` (same `List (Conflict e)` shape).
        "insertOrIgnore" => {
            let conflict_con = *reconciled.get("Conflict")?;
            let list_conflict =
                Type::Con(b.list, vec![Type::Con(conflict_con, vec![Type::Var(e)])]);
            method(
                vec![Type::Var(e), list_conflict, repo_app()],
                result(Type::Con(b.int, vec![])),
                vec![Constraint::single(adapter, a), Constraint::single(row, e)],
            )
        }
        // upsertRow : ∀e a. List Text -> List Text -> Map Text SqlValue -> Repo e a
        //   -> Result Int Error where Adapter a. The raw, explicit-control upsert: a
        //   hand-built row, the conflict columns, and the update columns named directly.
        "upsertRow" => method(
            vec![
                Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                Type::Con(b.list, vec![Type::Con(b.text, vec![])]),
                map_row(),
                repo_app(),
            ],
            result(Type::Con(b.int, vec![])),
            with_adapter(),
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
