# users-crud

A guided tour of the `std.data` repository API. It opens a SQLite database,
creates a `users` table from a typed entity, and walks the four data
operations — create, read, update, delete — printing each step.

## Run it

```sh
cd examples/data/users-crud
ridge run
```

You'll see three users inserted, one renamed, one deleted, and the table
printed after each change.

## What it shows

- **An entity** — `deriving (Row, Schema)` maps the `User` record to a table
  and, by convention, treats `id` as the database-assigned primary key. That
  is why the generated `UserInsert` shape has no `id` field.
- **A migration** — `Migrate.run` creates the table and records that it did,
  so running the program again doesn't recreate it.
- **Create** — `Repo.insertMany` writes a batch of `UserInsert` values.
- **Read** — the `Repo.query` builder with `orderBy`/`toList`, and `getBy` for
  a single row by key.
- **Update** — `Repo.setWhere` with a typed `set`, checked against the column
  at compile time.
- **Delete** — `Repo.delete` with a predicate.
- **`?`** — every step returns a `Result`, and `?` threads the happy path so
  the program reads top to bottom without nested `match`.

## Making it real

The database lives in memory, so each run starts clean. Two one-line changes
in `src/Main.ridge` take the same code to a database that persists:

- `connectSqlite (sqliteFile "users.db")` keeps the data between runs;
- `connect (PostgresConfig { ... })` runs it against Postgres, unchanged.

The [data guide](../../../docs/data.md) walks through both, including running
against Postgres with Docker, and the one prerequisite worth knowing up front:
the SQLite backend is built into the release binaries, so an installed `ridge`
has it out of the box.
