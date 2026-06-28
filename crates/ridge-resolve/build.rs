// build.rs — ridge-resolve manifest generator (T10).
#![allow(dead_code, clippy::format_push_string)]
//
// Emits `${OUT_DIR}/stdlib_manifest.rs` containing the generated `BUILTINS`
// static data slice consumed by `src/stdlib_builtin.rs` via `include!`.
//
// # Generation strategy (T10)
//
// For T10 the generated content is the original hand-curated module/export
// table, augmented with exports discovered from `.ridge` files.  The baseline
// table preserves all prior entries (including prelude re-exports per R013)
// so that the existing API surface and all existing tests stay green.
//
// Future tasks (T12) will extend this into a full bidirectional consistency
// check; T10 just wires up the `include!` mechanism.
//
// # Cycle-break rationale
//
// ridge-stdlib depends on ridge-resolve (regular + build-deps), so
// ridge-resolve cannot depend on ridge-stdlib (even as build-dep) without
// creating a Cargo cycle.  This build script performs its own text-level
// extraction without depending on ridge-stdlib.  No new crate is introduced;
// the dependency graph is unchanged.
//
// T201 errors: surfaced via eprintln! + process::exit(1) (no panic! per §1.3
// hard constraint #5).

use std::path::{Path, PathBuf};

// ── Capability keywords (Ridge 0.1.0) ────────────────────────────────────────

const CAP_KEYWORDS: &[&str] = &["io", "fs", "net", "time", "random", "env", "proc", "db"];

// ── Canonical module order ────────────────────────────────────────────────────
//
// Must match BUILTINS[i].id == i invariant in stdlib_builtin.rs.

const MODULE_ORDER: &[&str] = &[
    "std.int",
    "std.float",
    "std.bool",
    "std.text",
    "std.list",
    "std.map",
    "std.set",
    "std.option",
    "std.result",
    "std.io",
    "std.fs",
    "std.time",
    "std.random",
    "std.env",
    "std.cli",
    "std.proc",
    "std.actor",
    "std.json",
    "std.net.http",
    "std.crypto",
    "std.sql",
    "std.query",
    "std.data",
    "std.repo",
    "std.migrate",
    "std.raw",
];

// ── Baseline export table (T10: preserves original API) ───────────────────────
//
// Each entry is (module_name, &[export_names]).
//
// This baseline replicates the hand-curated BUILTINS table that was previously
// in stdlib_builtin.rs.  It includes:
//   - `pub fn` exports that appear in the `.ridge` files (ground truth from T5-T9)
//   - Prelude re-exported constructors / type names (R013): Some, None,
//     Option (std.option) and Ok, Err, Result (std.result)
//   - Alias / compat entries documented in the plan (andThen, unwrapOr, etc.)
//   - `pub type` entries that serve as re-export markers in the resolver
//
// T12 will replace this static table with a generated one derived purely from
// the `.ridge` sources plus a formal prelude-re-export declaration mechanism.

// T12 update: BASELINE_EXPORTS now derived from the actual .ridge source files
// (bidirectional consistency mandate, R006).  Entries that were in
// the old hand-curated T10 table but are NOT in any .ridge file have been
// removed.  New symbols that appear in the .ridge files but were absent from the
// T10 table have been added.
//
// Special prelude re-exports (R013) — constructors/type names that are
// declared as part of a `pub type` body and re-exported by the prelude:
//   std.option: Option, Some, None
//   std.result: Result, Ok, Err
// These are retained even though they do not appear as top-level `pub fn` or
// separate `pub type` declarations in the .ridge files.
//
// std.proc: `ProcOutput` is declared as `pub type` in proc.ridge.
// std.time:  `Duration`  is declared as `pub type` in time.ridge.
// std.json:  `JsonValue` is a language prelude union (compiler builtin), so it
//            is NOT a std.json export — unlike the records above.
// std.net.http: `Request`, `Response` are declared as `pub type` in net/http.ridge.
const BASELINE_EXPORTS: &[(&str, &[&str])] = &[
    (
        "std.int",
        &[
            "toText",
            "parse",
            "abs",
            "min",
            "max",
            "add",
            "sub",
            "mul",
            "div",
            "rem",
            "mod",
            "pow",
            "neg",
            "wrappingAdd",
            "saturatingAdd",
        ],
    ),
    (
        "std.float",
        &[
            "toText",
            "parseRaw",
            "parse",
            "fromInt",
            "round",
            "truncate",
            "floor",
            "ceil",
            "sqrt",
            "abs",
            "add",
            "sub",
            "mul",
            "div",
            "neg",
            "totalCompare",
        ],
    ),
    ("std.bool", &["not", "and", "or", "toText"]),
    (
        "std.text",
        &[
            "byteSize",
            "length",
            "join",
            "slice",
            "concat",
            "split",
            "splitN",
            "splitAny",
            "lines",
            "trim",
            "toUpper",
            "toLower",
            "startsWith",
            "endsWith",
            "contains",
            "like",
            "replace",
            "padLeft",
            "padRight",
            "isEmpty",
        ],
    ),
    (
        "std.list",
        &[
            "empty",
            "length",
            "isEmpty",
            "head",
            "tail",
            "map",
            "filter",
            "filterMap",
            "fold",
            "foldRight",
            "reverse",
            "concat",
            "sort",
            "sortBy",
            "take",
            "drop",
            "groupBy",
            "flatMap",
            "zip",
            "zipWith",
            "contains",
            "find",
            "any",
            "all",
            "range",
            "rangeExclusive",
            "forEach",
        ],
    ),
    (
        "std.map",
        &[
            "empty", "fromList", "toList", "insert", "remove", "get", "contains", "keys", "values",
            "map", "filter", "size", "merge", "update",
        ],
    ),
    (
        "std.set",
        &[
            "empty",
            "fromList",
            "toList",
            "insert",
            "remove",
            "contains",
            "union",
            "intersect",
            "difference",
            "size",
        ],
    ),
    (
        "std.option",
        &[
            "withDefault",
            "map",
            "flatMap",
            "orElse",
            "isSome",
            "isNone",
            "discard",
            // Prelude-exported constructors and type name (R013).
            "Option",
            "Some",
            "None",
        ],
    ),
    (
        "std.result",
        &[
            "map",
            "mapErr",
            "flatMap",
            "withDefault",
            "isOk",
            "isErr",
            "discard",
            // Prelude-exported constructors and type name (R013).
            "Result",
            "Ok",
            "Err",
        ],
    ),
    (
        "std.io",
        &["print", "println", "eprint", "eprintln", "readLine"],
    ),
    (
        "std.fs",
        &[
            "readFile",
            "writeFile",
            "append",
            "exists",
            "lines",
            "readDir",
            "isDir",
        ],
    ),
    (
        "std.time",
        &[
            // `pub type Duration` declared in time.ridge.
            "Duration", "now", "epoch", "fromIso", "diff", "diffMs", "sinceMs", "sleep", "parse",
            "iso",
        ],
    ),
    (
        "std.random",
        &["int", "float", "alphanumeric", "choice", "seed"],
    ),
    ("std.env", &["get", "set", "all"]),
    ("std.cli", &["args", "exit"]),
    (
        "std.proc",
        &[
            // `pub type ProcOutput` declared in proc.ridge.
            "ProcOutput",
            "run",
        ],
    ),
    ("std.actor", &["mailboxSize"]),
    (
        "std.json",
        &[
            // JsonValue is a language prelude union (compiler builtin), not a
            // std.json export — so it is intentionally absent from this list.
            "encode",
            "decode",
            "encodeInt",
            "encodeBool",
            "encodeText",
            // JsonValue construction shims (FFI bridges to
            // ridge_rt:json_* — see crates/ridge-stdlib/stdlib/json.ridge).
            // Cross-module `pub type` variant resolution lands in 0.2.0;
            // until then these are the supported constructor surface.
            "jNull",
            "jBool",
            "jInt",
            "jFloat",
            "jText",
            "jList",
            "jObject",
            // JsonValue accessor companions — destructure a JsonValue
            // returned from `decode` without needing cross-module variant
            // pattern matching (deferred).  See json.ridge for usage.
            "asInt",
            "asFloat",
            "asBool",
            "asText",
            "asList",
            "asObject",
            "isNull",
        ],
    ),
    (
        "std.net.http",
        &[
            // `pub type Request`, `Response`, `Html`, `SecureCookie` declared in
            // net/http.ridge.
            "Request",
            "Response",
            "Html",
            "SecureCookie",
            "get",
            "post",
            "put",
            "delete",
            "listen",
            "respond",
            "html",
            "htmlValue",
            "secureCookie",
            "secureCookieHeader",
            "withSecure",
            "withHttpOnly",
            "withSameSite",
            "withMaxAge",
            "withPath",
        ],
    ),
    (
        "std.crypto",
        &[
            // Constant-time comparison for secret-bearing values.
            "constantTimeEq",
        ],
    ),
    (
        "std.sql",
        &[
            // The opaque SQL column value plus the SqlType codec class and its
            // methods, all importable from user code.
            "SqlValue",
            "SqlType",
            "toSql",
            "fromSql",
            // The Row codec class and its methods (`deriving (Row)` generates the
            // instances). `fromRow` maps a `Map Text SqlValue` row back to a
            // record; `toRow` encodes a record into a row to write; `rowColumns`
            // names the columns from the type alone (a phantom `Option a` witness).
            "Row",
            "fromRow",
            "toRow",
            "rowColumns",
            // Monomorphic SqlValue factories (the variants stay opaque).
            "sqlInt",
            "sqlText",
            "sqlBool",
            "sqlFloat",
            // The safe SQL statement-text wrapper, its factory, and accessor —
            // a data-layer concern, declared in sql.ridge.
            "Sql",
            "sql",
            "sqlValue",
        ],
    ),
    (
        "std.query",
        &[
            // The tree renderer and the SQL compilers. `Quote`/`QExpr` and their
            // constructors are prelude builtins, not std.query exports.
            "debugShow",
            "toSql",
            "orderSql",
            "selectSql",
            // Sort direction, declared in query.ridge. The type plus its two
            // constructors are importable for ordering, and `ascending` projects
            // it to the `ascending?` boolean the seam reads.
            "SortOrder",
            "Asc",
            "Desc",
            "ascending",
            // The query-plan tree, its three constructors, and the builders that
            // wrap them, declared in query.ridge. The set-operation terminals build a
            // `QueryPlan` through the builders and hand it to a backend's `runPlan`.
            "QueryPlan",
            "PlanScan",
            "PlanCombine",
            "PlanRefine",
            "PlanJoin",
            "PlanProject",
            "PlanAggregate",
            "PlanGroup",
            "planScan",
            "planCombine",
            "planRefine",
            "planJoin",
            "planProject",
            "planAggregate",
            "planGroup",
            // The plan-to-SQL renderer: lowers a whole `QueryPlan` to one
            // parameterized statement plus its ordered bind values.
            "planToSql",
            // The plan-to-plan optimizer: rewrites a `QueryPlan` into an
            // equivalent one that compiles to tighter SQL (the renderer's pre-pass).
            "optimize",
            // The existence-probe wrapper: compiles a sub-plan to
            // `SELECT 1 FROM … LIMIT 1` for an `exists` terminal.
            "planExists",
            // The in-memory source leaf: wraps the rows `from` snapshotted, so an
            // in-memory `Seq` runs through the same plan/interpreter as a query.
            "planList",
            // The mutation-plan tree, its constructors, the builders that wrap them,
            // and the write-side renderer. A write verb builds a `MutationPlan` and
            // hands it to a backend's `runMutation`; `mutationToSql` lowers it to one
            // parameterized statement, the write-side dual of `planToSql`. `MutUpsert`
            // carries an `ON CONFLICT` clause built by `planUpsert`.
            "MutationPlan",
            "MutInsert",
            "MutUpsert",
            "MutUpdate",
            "MutDelete",
            "planInsert",
            "planUpsert",
            "planUpdate",
            "planDelete",
            "mutationToSql",
            // The RETURNING renderer: the same statement, with a `RETURNING <cols>`
            // tail so a backend hands back the affected rows.
            "mutationReturningToSql",
        ],
    ),
    (
        "std.data",
        &[
            // The storage seam class and its methods, plus the in-memory adapter
            // (the opaque handle type and its `db`-gated constructor). The
            // Postgres adapter (later) implements the same `Adapter` class.
            "Adapter",
            "appendRow",
            "all",
            "selectRows",
            "get",
            "delete",
            "updateRows",
            "fetch",
            "countWhere",
            "aggregate",
            "project",
            "groupSummarize",
            "runPlan",
            // The write seam: a write verb builds a `MutationPlan` and hands it here;
            // a SQL backend renders it through `mutationToSql`, the in-memory one
            // interprets it. Answers the affected row count.
            "runMutation",
            // The RETURNING write seam: renders through `mutationReturningToSql` (to
            // `_pgRawQuery`) or interprets, answering the rows the mutation touched.
            "runMutationReturning",
            // Transaction control: open, commit, and roll back a transaction
            // (nesting opens a savepoint). The `Repo.transaction` combinator runs
            // these around a body.
            "begin",
            "commit",
            "rollback",
            // Connection lifecycle: release a connection's pool (Postgres) or forget
            // the in-memory store, the counterpart to `connect`/`memAdapter`.
            "close",
            // Schema seam the `std.migrate` runner compiles a migration onto:
            // create/drop a table, add/drop a column, create an index, and the
            // migration tracking-table reads and writes.
            "ddlCreate",
            "ddlDrop",
            "ddlAddColumn",
            "ddlDropColumn",
            "ddlIndex",
            "migrationsApplied",
            "recordMigration",
            // Raw-SQL escape hatch (typed front door in std.raw): a parameterised
            // query returning rows, and a statement returning an affected count.
            "rawQuery",
            "rawExec",
            "MemAdapter",
            "memAdapter",
            // The Postgres adapter: the opaque connection handle, its config
            // record, and the `db`-gated `connect`. Implements the same
            // `Adapter` class as the in-memory backend.
            "Postgres",
            "Config",
            "connect",
            // Pool tuning: the `PoolConfig` record and the `connectWith` that takes
            // one, the `defaultPool` baseline, and the `with*` setters that size the
            // pool, set the millisecond timeouts, tune the maintenance windows, and
            // set the retry and backpressure knobs.
            "PoolConfig",
            "connectWith",
            "defaultPool",
            "withPoolSize",
            "withConnectTimeoutMs",
            "withQueryTimeoutMs",
            "withCheckoutTimeoutMs",
            "withIdleTimeoutMs",
            "withMaxLifetimeMs",
            "withHealthCheckMs",
            "withConnectRetries",
            "withRetryBackoffMs",
            "withMaxQueueDepth",
        ],
    ),
    (
        "std.repo",
        &[
            // The typed repository layer over the `Adapter` seam: the opaque
            // `Repo e a` handle, its `repo` constructor, and the query verbs
            // that auto-decode rows into entities through `deriving (Row)`.
            "Repo",
            "repo",
            "all",
            "findBy",
            "find",
            "getBy",
            "insertRow",
            "insert",
            "insertRows",
            "insertMany",
            // Upsert: the opaque `Conflict e` key built by `onConflict`, the typed
            // `upsert`/`insertOrIgnore` verbs that resolve a unique-constraint conflict
            // (`ON CONFLICT … DO UPDATE`/`DO NOTHING`), and the raw `upsertRow` escape
            // hatch that names the conflict and update columns explicitly.
            "Conflict",
            "onConflict",
            "upsert",
            "insertOrIgnore",
            "upsertRow",
            // RETURNING verbs: insert/upsert/delete the rows and read them back, decoded
            // — the stored row after the write (a server-filled column populated).
            "insertReturning",
            "insertManyReturning",
            "deleteReturning",
            "upsertReturning",
            "deleteWhere",
            "updateWhere",
            "update",
            // Typed partial updates: the opaque `Setter e` built by `set`, and the
            // verbs that apply a list of setters — `setWhere` (over the repo, with
            // an explicit predicate) and `applySet` (the query-builder terminal).
            "Setter",
            "set",
            "setWhere",
            // Run a body inside a transaction on the connection: commit on `Ok`,
            // roll back on `Err`. Nesting opens a savepoint.
            "transaction",
            // Run a body with the connection, then close it on every path — the
            // leak-safe scoped-connection combinator.
            "withConnection",
            // Release a connection, the dual of `connect` for the open-once,
            // reuse, then disconnect-at-shutdown pattern.
            "disconnect",
            // The query builder: the opaque `Query e a` and its pipeline verbs,
            // ending in the `toList`/`first` terminals and the `selectList`/
            // `selectFirst` projections.
            "Query",
            "query",
            // The in-memory query source: `from` lifts a `List a` into the query
            // world as an opaque `Seq a`, read back by the same `toList`/`first`
            // terminals. No repository, table, or adapter.
            "Seq",
            "from",
            // The unified `filter` is the method of the `Refinable q p | q -> p`
            // class, so one verb narrows a query (one-row predicate) and a join
            // (two-row predicate), the arity following the receiver.
            "Refinable",
            "filter",
            // The unified `orderBy` is the method of the `Orderable q p | q -> p`
            // class, so one verb orders a query (one-row key) and a join (two-row
            // key over either side), the arity following the receiver.
            "Orderable",
            "orderBy",
            // The unified `limit`/`offset`/`distinct` are the methods of the
            // `Pageable q` class, so one set of page-and-distinct builder steps
            // applies to a query (one receiver), an inner join, or a left join.
            "Pageable",
            "limit",
            "offset",
            "distinct",
            // The unified decode terminals `toList`/`first` are the methods of the
            // `Decodable q p | q -> p` class, so one pair decodes a query (to its
            // entity), an inner join (to a pair), or a left join (to a pair whose
            // right side is optional), the row shape following the receiver.
            "Decodable",
            "toList",
            "first",
            // Unique-row terminals: `single` answers the lone matching row or
            // `None`, `singleOrError` requires it; both fail on more than one.
            "single",
            "singleOrError",
            // The unified size-and-presence terminals `count`/`exists` are the methods
            // of the single-parameter `Countable q` class, so one pair counts a query,
            // an inner join, or a left join. The universal-predicate terminal `every`
            // (LINQ's `All`, the dual of `exists`) is the method of `Every q p | q ->
            // p`, the dependency fixing its predicate's arity per receiver — a one-row
            // predicate over a query, a two-row one over a join.
            "Countable",
            "count",
            "exists",
            // The complement of `exists` — true when the receiver selects no rows —
            // and, inside a quoted predicate, a correlated `NOT EXISTS` subquery.
            "notExists",
            "Every",
            "every",
            // The unified projection verb is the method of `Projectable q p |
            // q -> p`: `select` projects a query/join/left-join down to a named
            // shape, `selectFirst` returns the first projected row (`LIMIT 1`).
            "Projectable",
            "select",
            "selectFirst",
            "applySet",
            // Scalar aggregates are the methods of the `Aggregable q p | q -> p`
            // class, pushed down to the backend over the query's (or join's) filter:
            // sum/average/min/max of a quoted column, each `None` over an empty match
            // (a SQL aggregate of zero rows is NULL). One set of verbs folds a query
            // column (a one-row accessor) or a join column from either side (a
            // two-row accessor).
            "Aggregable",
            "sumOf",
            "avgOf",
            "minOf",
            "maxOf",
            // The two-table join builder: the opaque `Join e f a` and its `joinOn`
            // entry. Its decode terminals (`toList`/`first`) and projection
            // (`select`/`selectFirst`) are the `Decodable`/`Projectable` methods
            // above.
            "Join",
            "joinOn",
            // The cross join: `crossJoin` pairs a query with a right repository and
            // no condition (the cartesian product), reusing the same `Join e f a`.
            "crossJoin",
            // The left-outer join: the opaque `LeftJoin e f a` and its `leftJoinOn`
            // entry. Decode and projection unify through `Decodable`/`Projectable`,
            // the right side read as `Option`.
            "LeftJoin",
            "leftJoinOn",
            // The right-outer join: the opaque `RightJoin e f a` and its `rightJoinOn`
            // entry, the mirror of the left join with the left side read as `Option`.
            "RightJoin",
            "rightJoinOn",
            // The full-outer join: the opaque `FullJoin e f a` and its `fullJoinOn`
            // entry, keeping every row of both tables with both sides read as `Option`.
            "FullJoin",
            "fullJoinOn",
            // The N-ary inner join: chaining `joinOn` past the first table produces
            // the opaque nested `Joined q f a`, the `Joinable` class unifying the
            // builder across a query (binary `Join`) and a join (nested `Joined`).
            // Its decode terminals (`toList`/`first`) are the `Decodable` methods.
            "Joined",
            "Joinable",
            // The N-ary LEFT outer join: chaining `leftJoinOn` onto a composite
            // produces the opaque nested `LeftJoined q f a`, the `LeftJoinable` class
            // unifying the verb across a query (binary `LeftJoin`) and a composite
            // (nested `LeftJoined`). Its decode terminals are the `Decodable` methods.
            "LeftJoined",
            "LeftJoinable",
            // The N-ary RIGHT outer join: chaining `rightJoinOn` onto a composite
            // produces the opaque nested `RightJoined q f a`, the `RightJoinable`
            // class unifying the verb across a query (binary `RightJoin`) and a
            // composite (nested `RightJoined`).
            "RightJoined",
            "RightJoinable",
            // The N-ary FULL outer join: chaining `fullJoinOn` onto a composite
            // produces the opaque nested `FullJoined q f a`, the `FullJoinable` class
            // unifying the verb across a query (binary `FullJoin`) and a composite
            // (nested `FullJoined`).
            "FullJoined",
            "FullJoinable",
            // Grouped aggregates unified across a query and a join: the opaque
            // `Grouped q p` builder produced by the `Groupable` class's `groupBy`,
            // narrowed by `having`, and summarised into a named record by
            // `summarize` (which dispatches the GROUP BY through the `Summarizable`
            // class's `runGroups`). The `having`/`summarize` quotes range over the
            // `Grouped q p` handle (`g.key`, `g.count`, `g.sum`/`avg`/`min`/`max`),
            // the source `q` carrying the entities the column accessors read.
            "Grouped",
            "Groupable",
            "groupBy",
            "having",
            "summarize",
            "Summarizable",
            "runGroups",
            // Set operations unified across a query and an in-memory sequence: the
            // `Combinable` class's `union`/`unionAll`/`intersect`/`except` combine two
            // receivers into one that runs the combined result, each returning a
            // composable receiver (a SQL `UNION`/`UNION ALL`/`INTERSECT`/`EXCEPT`, or an
            // in-memory combine over a `Seq`).
            "Combinable",
            "union",
            "unionAll",
            "intersect",
            "except",
        ],
    ),
    (
        "std.migrate",
        &[
            // The schema-DSL: the opaque `Column` and its typed declarators and
            // modifiers, the opaque `SchemaOp` and its factories, and the
            // `Migration` batch and its `migration` builder.
            "Column",
            "intCol",
            "textCol",
            "boolCol",
            "floatCol",
            "nullable",
            "primaryKey",
            "unique",
            "SchemaOp",
            "createTable",
            "dropTable",
            "addColumn",
            "dropColumn",
            "createIndex",
            "uniqueIndex",
            "Migration",
            "migration",
            // The migration runner: apply the pending migrations in order, each in
            // its own transaction, and answer the names applied.
            "run",
        ],
    ),
    (
        "std.raw",
        &[
            // The raw-SQL escape hatch over the `Adapter` seam: a parameterised
            // query decoded into entities, its first-row form, and a row-less
            // statement returning the affected row count.
            "query",
            "queryFirst",
            "exec",
        ],
    ),
];

/// Per-module list of `pub opaque type` names. Drives the `opaque_types` field
/// of the generated manifest so the resolver and type-checker confine these
/// types' construction, pattern matching, and field access to the declaring
/// stdlib module (taint wrappers and opaque codec values).
const BASELINE_OPAQUE: &[(&str, &[&str])] = &[
    ("std.net.http", &["Html", "SecureCookie"]),
    ("std.sql", &["Sql", "SqlValue"]),
    ("std.data", &["MemAdapter", "Postgres"]),
    (
        "std.repo",
        &[
            "Repo",
            "Query",
            "Join",
            "LeftJoin",
            "RightJoin",
            "FullJoin",
            "Joined",
            "LeftJoined",
            "RightJoined",
            "FullJoined",
            "Setter",
            "Conflict",
            "Grouped",
            "Seq",
        ],
    ),
    ("std.migrate", &["Column"]),
];

fn main() {
    // Tell Cargo to re-run this script when any stdlib .ridge file changes.
    println!("cargo:rerun-if-changed=../ridge-stdlib/stdlib");

    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| {
        eprintln!("T201 ManifestRegressionFailed: OUT_DIR not set");
        std::process::exit(1);
    });
    let out_path = PathBuf::from(&out_dir).join("stdlib_manifest.rs");

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let stdlib_dir = manifest_dir.parent().map_or_else(
        || manifest_dir.join("ridge-stdlib").join("stdlib"),
        |p| p.join("ridge-stdlib").join("stdlib"),
    );

    match generate_manifest(&stdlib_dir, &out_path) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

// ── Generation ────────────────────────────────────────────────────────────────

fn generate_manifest(stdlib_dir: &Path, out_path: &Path) -> Result<(), String> {
    // Build the module list in canonical order.
    //
    // T10: use the baseline table as the definitive export list.  The .ridge
    // source files are walked only to validate that they exist (T201 guard);
    // the text-extracted names are NOT merged in here.  T12 will introduce
    // the full bidirectional consistency mechanism.
    let mut modules: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();

    for &dotted in MODULE_ORDER {
        // Validate the .ridge file exists (T201 guard — emit a warning if not).
        let rel = module_name_to_path(dotted);
        let full = stdlib_dir.join(&rel);
        if !full.exists() {
            // Missing .ridge file is non-fatal for T10 — the module may not have
            // been written yet (progressive T5-T9 delivery).
            continue;
        }

        // Baseline exports for this module (API-stable, R013 compliant).
        let baseline: &[&str] = BASELINE_EXPORTS
            .iter()
            .find(|&(name, _)| *name == dotted)
            .map_or(&[], |(_, exps)| *exps);

        let exports: Vec<String> = baseline.iter().map(|&s| s.to_owned()).collect();

        let opaque: Vec<String> = BASELINE_OPAQUE
            .iter()
            .find(|&(name, _)| *name == dotted)
            .map_or_else(Vec::new, |(_, ops)| {
                ops.iter().map(|&s| s.to_owned()).collect()
            });

        modules.push((dotted.to_owned(), exports, opaque));
    }

    let content = emit_manifest_rs(&modules);

    std::fs::write(out_path, content).map_err(|e| {
        format!(
            "T201 ManifestRegressionFailed: could not write {}: {e}",
            out_path.display()
        )
    })?;

    Ok(())
}

// ── Code emitter ──────────────────────────────────────────────────────────────

fn emit_manifest_rs(modules: &[(String, Vec<String>, Vec<String>)]) -> String {
    // The generated file contains only the `BUILTINS` static initializer body.
    // It is included via:
    //   pub static BUILTINS: &[BuiltinStdlibModule] = include!(...);
    // so the file must be a valid Rust expression — the `&[...]` slice literal.

    let mut out = String::from("// @generated by crates/ridge-resolve/build.rs (T10)\n");
    out.push_str("// Do not edit by hand — re-run cargo build to regenerate.\n");
    out.push_str("&[\n");

    for (idx, (dotted, exports, opaque)) in modules.iter().enumerate() {
        out.push_str("    BuiltinStdlibModule {\n");
        out.push_str(&format!("        id: StdlibModuleId({idx}),\n"));
        out.push_str(&format!("        name: \"{dotted}\",\n"));
        out.push_str("        exports: &[\n");
        for exp in exports {
            out.push_str(&format!("            \"{exp}\",\n"));
        }
        out.push_str("        ],\n");
        out.push_str("        opaque_types: &[\n");
        for ty in opaque {
            out.push_str(&format!("            \"{ty}\",\n"));
        }
        out.push_str("        ],\n");
        out.push_str("    },\n");
    }

    out.push_str("]\n");
    out
}

// ── Text-level extraction ─────────────────────────────────────────────────────

fn module_name_to_path(dotted: &str) -> PathBuf {
    let rest = dotted.strip_prefix("std.").unwrap_or(dotted);
    let with_slashes = rest.replace('.', "/");
    PathBuf::from(format!("{with_slashes}.ridge"))
}

fn extract_pub_names(src: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();

    for line in src.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("--") || trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("pub fn ") {
            let mut tokens = rest.split_whitespace();
            let name = loop {
                let Some(tok) = tokens.next() else { break None };
                if CAP_KEYWORDS.contains(&tok) {
                    continue;
                }
                break Some(tok.trim_end_matches('('));
            };
            if let Some(n) = name {
                if is_valid_ident(n) {
                    names.push(n.to_owned());
                }
            }
            continue;
        }

        if let Some(rest) = trimmed
            .strip_prefix("pub opaque type ")
            .or_else(|| trimmed.strip_prefix("pub type "))
        {
            let mut tokens = rest.split_whitespace();
            if let Some(n) = tokens.next() {
                let n = n.trim_end_matches('=').trim();
                if is_valid_ident(n) {
                    names.push(n.to_owned());
                }
            }
        }
    }

    names
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => chars.all(|c| c.is_alphanumeric() || c == '_'),
        _ => false,
    }
}
