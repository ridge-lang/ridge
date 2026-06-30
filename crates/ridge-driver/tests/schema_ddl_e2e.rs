//! End-to-end check for the `std.schema` DDL renderer — running on the BEAM.
//!
//! `schemaToDdl` turns the dialect-neutral schema descriptor into a Postgres
//! `CREATE TABLE`: a database-generated `Identity` column becomes a `serial`/
//! `bigserial` pseudo-type, and the per-column modifiers render inline — `NOT NULL`,
//! `PRIMARY KEY`, `UNIQUE`, a `REFERENCES … ON DELETE` foreign key, a `DEFAULT`
//! clause, and a `CHECK` whose predicate is the captured `QExpr` rendered with its
//! literals inline. The `createTableDdl`/`addColumnDdl`/`dropTableDdl`/
//! `dropColumnDdl`/`indexDdl` family renders the migration runner's schema steps from
//! the seam tuples, the same statements the retired Erlang builder used to assemble.
//!
//! This proves the whole renderer lowers and runs: the SQL text is built in Ridge,
//! so the storage adapter only executes it. The live-Postgres oracle
//! (`data_pg_e2e`) covers that the rendered statements actually run on a database.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.schema (DbBigInt, DbInt, DbText, DbTimestampTz, Identity, DefaultNow, Cascade, mkColumn, withColumn, schema, generated, primaryKey, unique, indexed, foreignKey, references, onDelete, check, schemaToDdl, schemaIndexDdls, createTableDdl, addColumnDdl, dropTableDdl, dropColumnDdl, indexDdl)
import std.text as Text

-- The domain records the schemas below describe. Persistence-ignorant — the
-- descriptor is their separate mapping companion.
type User = { id: Int, email: Text, age: Int }
type Post = { id: Int, author: Int, bio: Text }

-- A schema exercising the rich column features: an identity primary key (renders as
-- bigserial), a unique non-null text column, and a checked column.
fn userSchema () =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "email" "email" DbText false |> unique)
      |> withColumn (mkColumn "age" "age" DbInt false |> check (fn (u: User) -> u.age >= 0))

pub fn userDdl () -> Text = schemaToDdl (userSchema ())

-- A schema exercising a foreign key with an ON DELETE action, a DEFAULT now()
-- timestamp, and a nullable column.
fn postSchema () =
    schema "Post" "posts"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "author" "author" DbBigInt false |> foreignKey (references "users" "id" |> onDelete Cascade))
      |> withColumn (mkColumn "created_at" "created_at" DbTimestampTz false |> generated DefaultNow)
      |> withColumn (mkColumn "bio" "bio" DbText true)

pub fn postDdl () -> Text = schemaToDdl (postSchema ())

-- A schema with a non-unique indexed column: the index is a separate statement.
fn indexedSchema () =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "email" "email" DbText false |> indexed)

pub fn userIndexes () -> Text = Text.join " ; " (schemaIndexDdls (indexedSchema ()))

-- The migration step renderers over the seam tuple (name, base-type, nullable,
-- primaryKey, unique) — the same DDL the retired Erlang builder produced.
pub fn migrateCreateDdl () -> Text =
    createTableDdl "widgets" [("id", "int", false, true, false), ("name", "text", false, false, false)]

pub fn migrateAddColDdl () -> Text = addColumnDdl "widgets" ("note", "text", true, false, false)

pub fn migrateDropTableDdl () -> Text = dropTableDdl "widgets"

pub fn migrateDropColDdl () -> Text = dropColumnDdl "widgets" "note"

pub fn migrateIndexDdl () -> Text = indexDdl "widgets_name_idx" "widgets" ["name"] false

pub fn migrateUniqueIndexDdl () -> Text = indexDdl "uq_widgets" "widgets" ["name", "id"] true
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"schema-ddl-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn schema_ddl_renders_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping schema_ddl_renders_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-schema-ddl-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-schema-ddl-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile, got diagnostics: {:?}",
        artefacts.diagnostics
    );

    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|stem| stem.starts_with("ridge_module_"))
        .expect("a user module")
        .to_owned();

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['userDdl','postDdl','userIndexes','migrateCreateDdl',\
         'migrateAddColDdl','migrateDropTableDdl','migrateDropColDdl','migrateIndexDdl',\
         'migrateUniqueIndexDdl']), halt()."
    );
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let want = |needle: &str| {
        assert!(
            stdout.contains(needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    };

    // An identity primary key renders as `bigserial` (no explicit NOT NULL — serial
    // implies it); a non-null unique text column carries both modifiers inline; a
    // checked column renders the captured predicate with its literal inline. The
    // predicate keeps its own parentheses inside the CHECK clause's, the same doubled
    // form Postgres itself normalises a CHECK to.
    want(
        r#"userDdl=CREATE TABLE "users" ("id" bigserial PRIMARY KEY, "email" text NOT NULL UNIQUE, "age" integer NOT NULL CHECK (("age" >= 0)))"#,
    );

    // A foreign key renders `REFERENCES … ON DELETE CASCADE`; a DefaultNow column
    // renders `DEFAULT now()`; a nullable column carries no NOT NULL.
    want(
        r#"postDdl=CREATE TABLE "posts" ("id" bigserial PRIMARY KEY, "author" bigint NOT NULL REFERENCES "users" ("id") ON DELETE CASCADE, "created_at" timestamptz NOT NULL DEFAULT now(), "bio" text)"#,
    );

    // A non-unique indexed column emits a separate CREATE INDEX, named
    // `<table>_<column>_idx`.
    want(r#"userIndexes=CREATE INDEX "users_email_idx" ON "users" ("email")"#);

    // The migration step renderers reproduce the statements the Erlang builder used
    // to assemble: `int` maps to `bigint`, the modifiers render in order, and an
    // index renders `CREATE [UNIQUE] INDEX … ON … (cols)`.
    want(
        r#"migrateCreateDdl=CREATE TABLE "widgets" ("id" bigint NOT NULL PRIMARY KEY, "name" text NOT NULL)"#,
    );
    want(r#"migrateAddColDdl=ALTER TABLE "widgets" ADD COLUMN "note" text"#);
    want(r#"migrateDropTableDdl=DROP TABLE "widgets""#);
    want(r#"migrateDropColDdl=ALTER TABLE "widgets" DROP COLUMN "note""#);
    want(r#"migrateIndexDdl=CREATE INDEX "widgets_name_idx" ON "widgets" ("name")"#);
    want(r#"migrateUniqueIndexDdl=CREATE UNIQUE INDEX "uq_widgets" ON "widgets" ("name", "id")"#);
}
