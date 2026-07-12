# The data layer

Ridge talks to a SQL database through `std.data` and its companion modules —
`std.repo` for queries and writes, `std.schema` for entity descriptions, and
`std.migrate` for schema changes. The same code runs against SQLite or
Postgres: you describe a table once as a typed record, and the repository
encodes and decodes rows for you.

This guide builds up from a first program to the full query and write surface.
The runnable version of the first program lives in
[`examples/data/users-crud`](../examples/data/users-crud).

## Contents

- [Prerequisites](#prerequisites)
- [A first program](#a-first-program)
- [Entities](#entities)
- [Connecting](#connecting)
- [Migrations](#migrations)
- [Reading](#reading)
- [Writing](#writing)
- [Errors and transactions](#errors-and-transactions)
- [Running against Postgres with Docker](#running-against-postgres-with-docker)

## Prerequisites

A Ridge program runs on the Erlang/BEAM runtime, so `erl` must be on your
`PATH` — the [tutorial](tutorial.md) covers installing it. The database access
itself needs the `db` capability, granted in the project manifest:

```toml
[capabilities]
allow = ["db", "io"]
```

The SQLite backend is compiled into the released `ridge` binaries, so an
installed toolchain has it with nothing extra to build. Postgres needs no
special build at all. (If you build the compiler from source with
`cargo install`, pass `--features beam-runtime` to include the SQLite driver;
Postgres works either way.)

## A first program

Here is the whole shape of a data program: open a connection, create a table,
and read and write rows. It uses an in-memory SQLite database, so it needs no
setup and leaves nothing behind.

```ridge
import std.data (connectSqlite, sqliteMemory, Sqlite)
import std.migrate as Migrate
import std.repo as Repo
import std.schema (schemaOf)
import std.io as Io

pub type User = { id: Int, name: Text, email: Text } deriving (Row, Schema)

fn userWitness () -> Option User = None

fn setup (conn: Sqlite) -> Result (List Text) Error =
    Migrate.run conn
        [ Migrate.migration "0001_create_users"
            [ Migrate.createSchema (schemaOf (userWitness ())) ] ]

fn db io run (conn: Sqlite) -> Result Unit Error =
    let _ = setup conn ?
    let users: Repo User Sqlite = Repo.repo conn "users"
    Repo.insert (UserInsert { name = "Ada Lovelace", email = "ada@example.com" }) users ?
    let _ = Io.println "inserted one user"
    Ok ()

fn db io main () -> Unit =
    match connectSqlite (sqliteMemory ())
        Err e   -> Io.eprintln $"could not open the database (${e.code})"
        Ok conn ->
            match run conn
                Err e -> Io.eprintln $"database error (${e.code})"
                Ok _  -> ()
```

The rest of this guide takes each piece — the entity, the connection, the
migration, and the read and write verbs — in turn.

## Entities

An entity is a plain record that derives two instances:

```ridge
pub type User = { id: Int, name: Text, email: Text } deriving (Row, Schema)
```

- **`Row`** is what a repository uses to turn a database row into a `User` and
  back. Every entity you read or write needs it.
- **`Schema`** describes the table — its columns, their SQL types, and which
  column is the key — so a migration can create it and the insert path knows
  which columns the database fills in.

### The insert shape

`deriving (Schema)` treats an `id: Int` field as the primary key the database
assigns, by convention. So the generated **insert shape** — `UserInsert`, the
entity name with an `Insert` suffix — leaves `id` out: you provide the columns
you own, and the database fills in the rest.

```ridge
Repo.insert (UserInsert { name = "Ada Lovelace", email = "ada@example.com" }) users
```

### Nullable columns

A field of type `Option a` is a nullable column. `None` is written as SQL
`NULL` and reads back as `None`; `Some x` round-trips to `x`.

```ridge
pub type User = { id: Int, name: Text, nick: Option Text } deriving (Row, Schema)
```

### Column types

Beyond `Int`, `Text`, `Bool`, and `Float`, an entity field can be a `Decimal`,
`Uuid`, `Bytes`, `Date`, `Time`, `Timestamp`, or `Duration`, each mapped to the
column type the backend uses for it. Reach for `Decimal` rather than `Float`
whenever exactness matters (money, especially): it keeps every digit.

## Connecting

### SQLite

`connectSqlite` takes a config. Two presets cover the common cases:

```ridge
import std.data (connectSqlite, sqliteMemory, sqliteFile)

connectSqlite (sqliteMemory ())        -- a fresh in-memory database
connectSqlite (sqliteFile "app.db")    -- a file on disk, kept between runs
```

`sqliteFile` opens the database in write-ahead-log mode with foreign keys on;
`sqliteMemory` is a private database that lasts as long as the connection. Both
return a `Result Sqlite Error`.

### Postgres

`connect` takes a `PostgresConfig` record and returns a `Result Postgres Error`:

```ridge
import std.data (connect, PostgresConfig)

fn pgConfig () -> PostgresConfig =
    PostgresConfig { host = "127.0.0.1", port = 5432, database = "app",
             user = "app", password = "secret", sslMode = "disable" }

match connect (pgConfig ())
    Err e   -> ...
    Ok conn -> ...
```

`sslMode` is `"disable"`, `"require"`, or `"verify-full"`. For a tuned
connection pool, `connectWith` takes a `PoolConfig` built with
`defaultPool ()` and refined with steps like `withPoolSize` and
`withQueryTimeoutMs`.

### Releasing a connection

`Repo.withConnection` runs a body and closes the connection on the way out,
even if the body fails:

```ridge
Repo.withConnection conn (fn c -> ...)
```

`Repo.disconnect conn` releases a handle explicitly.

Because the connection type is part of a repository's type (`Repo User Sqlite`
vs `Repo User Postgres`), the same query and write code compiles against either
backend — only the `connect` call changes.

## Migrations

A migration is a named, ordered batch of schema changes. `Migrate.run` applies
the migrations a project declares, in order, and records each one in a tracking
table, so running the same list again applies nothing. Each migration runs in a
transaction: it lands whole or not at all.

```ridge
import std.migrate as Migrate
import std.schema (schemaOf)

fn userWitness () -> Option User = None

fn migrations () -> List Migration =
    [ Migrate.migration "0001_create_users"
        [ Migrate.createSchema (schemaOf (userWitness ())) ]
    , Migrate.migration "0002_users_email_index"
        [ Migrate.uniqueIndex "users_email_idx" "users" ["email"] ] ]

match Migrate.run conn (migrations ())
    Ok applied -> ...   -- the names applied on this run (empty on a repeat)
    Err e      -> ...
```

### From an entity, or by hand

`createSchema (schemaOf (witness))` builds a table from an entity's derived
schema — the full-fidelity path, and the one to prefer, since the table then
matches the record it decodes into. For a table with no entity, the tuple DSL
spells the columns out:

```ridge
Migrate.createTable "users"
    [ Migrate.intCol "id" |> Migrate.primaryKey
    , Migrate.textCol "name"
    , Migrate.textCol "email" |> Migrate.unique
    , Migrate.textCol "bio" |> Migrate.nullable ]
```

Columns are declared by base type — `intCol`, `textCol`, `boolCol`, `floatCol`
— and refined with the pipe-friendly `nullable`, `primaryKey`, and `unique`.

### Views

A view saves a query as a database object so a repository can read it back as
if it were a table. The saved query is a `QueryPlan` — the value the query
builder reifies — built here directly with `planScan` and the `QExpr`
constructors:

```ridge
import std.query (QueryPlan, planScan)

-- The view's SELECT: the active users only. The predicate is a QExpr (QEq,
-- QCol, QLitBool, and so on — prelude constructors, no import needed).
fn activeUsers () -> QueryPlan =
    planScan "users" (QEq (QCol "active") (QLitBool true)) [] (0 - 1) 0 false

Migrate.createView "active_users" (activeUsers ())
```

Read it back by binding a repository to the view's name — the rows decode into
the entity exactly as a table's would:

```ridge
let view: Repo User Sqlite = Repo.repo conn "active_users"
Repo.all view
```

`Migrate.dropView "active_users"` removes it, and `createView` reverses to
`dropView` automatically on rollback.

### Computed columns

A stored generated column is computed by the database from other columns on
every write. `deriving (Schema)` can't mark a field computed, so a table with
one is described by hand, and the `computed` refiner carries the expression:

```ridge
import std.schema (EntitySchema, schema, withColumn, mkColumn, generated, computed, primaryKey, Identity)
import std.sql (DbBigInt)

pub type Order = { id: Int, qty: Int, price: Int, total: Int } deriving (Row)

fn orderSchema () -> EntitySchema Order =
    schema "Order" "orders"
        |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
        |> withColumn (mkColumn "qty" "qty" DbBigInt false)
        |> withColumn (mkColumn "price" "price" DbBigInt false)
        |> withColumn (mkColumn "total" "total" DbBigInt true
                         |> computed (fn (o: Order) -> o.qty * o.price))
```

`total` renders as `GENERATED ALWAYS AS ("qty" * "price") STORED`. Like an
identity column, it is left out of the insert shape — the database fills it in.
Feed `orderSchema ()` to a migration with `Migrate.createSchema`.

### Rollback and the CLI

A migration built with `migration` derives its reverse from its steps: a
`createTable` reverses to a `dropTable`, an `addColumn` to a `dropColumn`. A
step that loses information — a `dropTable`, a raw `runSql` — has no derivable
reverse, so spell it out with `reversibleMigration name [up] [down]`.

`Migrate.rollback conn migrations n` reverses the last `n` applied migrations;
`Migrate.revertTo conn migrations name` rolls back to a chosen migration. The
`ridge migrate` CLI drives the same engine from the shell (`apply`, `status`,
`rollback`), reading its connection from `RIDGE_DB_*` environment variables.

### Seeds

`Migrate.seed [ ... ]` writes reference rows as an idempotent upsert keyed on
the entity's primary key, so re-running converges rather than duplicating:

```ridge
Migrate.seed [ Currency { code = "USD", name = "US Dollar" }
             , Currency { code = "EUR", name = "Euro" } ]
```

## Reading

Bind a repository to a table, then read through it. Every read returns a
`Result`, and decodes rows into the entity.

```ridge
let users: Repo User Sqlite = Repo.repo conn "users"
```

### Whole rows

```ridge
Repo.all users                                  -- every row
Repo.find (fn (u: User) -> u.age > 28) users    -- the first match, as Option
Repo.findBy (fn (u: User) -> u.age >= 25) users -- every match, as a list
Repo.getBy "id" (toSql 2) users                 -- one row by a key column
```

### The query builder

`Repo.query` starts a builder that composes with `|>`:

```ridge
users
    |> Repo.query
    |> Repo.filter (fn (u: User) -> u.age >= 18)
    |> Repo.orderBy Asc (fn (u: User) -> u.name)
    |> Repo.limit 10
    |> Repo.toList
```

The terminals are `toList` (every row), `first` (the first, as `Option`),
`count`, and `exists`. `filter` narrows, `orderBy Asc`/`Desc` sorts, and
`limit`/`offset` page. A captured local variable in a predicate becomes a bound
parameter, so `List.contains u.age ages` renders to a SQL `IN`.

### Projections

`select` projects each row into a named shape rather than the whole entity:

```ridge
pub type Summary = { who: Text, years: Int } deriving (Row)

users
    |> Repo.query
    |> Repo.select (fn (u: User) -> Summary { who = u.name, years = u.age })
```

### Aggregates

`sumOf` folds a column; `groupBy` with `summarize` projects one record per
group, and `having` narrows by an aggregate.

```ridge
users |> Repo.query |> Repo.sumOf (fn (u: User) -> u.age)
```

### Joins

`joinOn` is an inner join; `leftJoinOn` keeps every left row and reads the right
entity as `Option`. `toList` decodes both sides of each matched pair.

```ridge
users
    |> Repo.query
    |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author)
    |> Repo.toList
```

## Writing

Every write returns a `Result`; the mutating verbs answer how many rows changed.

### Insert

```ridge
Repo.insert (UserInsert { name = "Ada", email = "ada@example.com" }) users
Repo.insertMany [ UserInsert { ... }, UserInsert { ... } ] users
Repo.insertReturning (UserInsert { ... }) users   -- returns the stored entity
```

### Update

`update` overwrites whole rows; `updateWhere` and the typed `setWhere` change
only the named columns.

```ridge
users |> Repo.setWhere [ Repo.set (fn (u: User) -> u.name) "Ada L." ]
                       (fn (u: User) -> u.id == 1)
```

`set` is checked against the column at compile time, so a type mismatch is a
build error, not a runtime one.

### Delete

```ridge
users |> Repo.delete (fn (u: User) -> u.age < 18)
users |> Repo.deleteReturning (fn (u: User) -> u.id == 2)  -- returns the removed rows
```

### Upsert

`upsert` inserts or, on a key conflict, overwrites; `insertOrIgnore` inserts or
does nothing.

```ridge
users |> Repo.upsert (User { id = 1, name = "Ada", email = "ada@example.com" })
                     [ Repo.onConflict (fn (u: User) -> u.id) ]
```

## Errors and transactions

Every database call returns `Result a Error`. The `?` operator threads the
happy path and returns early on the first error, so a sequence of steps reads
top to bottom:

```ridge
fn db seed (users: Repo User Sqlite) -> Result Unit Error =
    Repo.insert (UserInsert { name = "Ada", email = "ada@example.com" }) users ?
    Repo.insert (UserInsert { name = "Alan", email = "alan@example.com" }) users ?
    Ok ()
```

To act on a specific failure, `dbErrorKind` sorts an `Error` into a kind —
`UniqueViolation`, `ForeignKeyViolation`, `NotNullViolation`, `CheckViolation`,
and so on:

```ridge
import std.data (dbErrorKind, UniqueViolation)

match Repo.insert (UserInsert { name = "Ada", email = "ada@example.com" }) users
    Ok _  -> Ok ()
    Err e ->
        match dbErrorKind e
            UniqueViolation -> Ok ()          -- already there; treat as success
            _               -> Err e
```

`Repo.transaction conn (fn c -> ...)` runs a body in a transaction: it commits
if the body returns `Ok`, and rolls back on `Err` or a failure inside.

## Running against Postgres with Docker

The first program runs on SQLite with no setup. To try the same code against
Postgres, bring one up with the `docker-compose.yml` under
[`examples/data`](../examples/data):

```sh
cd examples/data
docker compose up -d
```

That starts Postgres on `localhost:5432` with database, user, and password all
set to `app` — matching the `PostgresConfig` in the [Connecting](#connecting) section.
Point the program at Postgres by swapping the connection:

```ridge
import std.data (connect, PostgresConfig, Postgres)

fn pgConfig () -> PostgresConfig =
    PostgresConfig { host = "127.0.0.1", port = 5432, database = "app",
             user = "app", password = "app", sslMode = "disable" }
```

The entity, migration, and every read and write stay exactly the same — only
the connection and the repository's backend type (`Repo User Postgres`) change.
Stop the database with `docker compose down` when you're done.
