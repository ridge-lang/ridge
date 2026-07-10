//! End-to-end check for the `std.schema` DDL renderer in the SQLite dialect —
//! running on the BEAM.
//!
//! `schemaToDdlFor SqliteDialect` turns the same dialect-neutral schema descriptor
//! `schemaToDdl` renders for Postgres into a SQLite `CREATE TABLE`. The dialects
//! agree on most syntax and part where SQLite spells things differently: a
//! database-generated `Identity` column is an `INTEGER PRIMARY KEY AUTOINCREMENT`
//! rowid rather than a `serial` pseudo-type (and carries no second inline key), the
//! type affinities collapse the rich types onto `INTEGER`/`REAL`/`TEXT`/`BLOB` (an
//! exact `decimal` and the temporal types ride `TEXT`, never lossy `REAL`), and a
//! `DEFAULT now()` becomes `DEFAULT CURRENT_TIMESTAMP`. Composite primary keys,
//! multi-column unique constraints, foreign keys, checks, and the migration-tuple
//! `createTableDdlFor` render as they do on Postgres, the shared skeleton.
//!
//! The live-Postgres oracle (`data_pg_e2e`) covers that the Postgres statements run
//! on a database; this proves the SQLite dialect lowers and renders on the BEAM. The
//! SQLite runtime that executes these statements lands with the NIF adapter.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.sql (DbBigInt, DbInt, DbText, DbTimestampTz, DbSmallInt, DbChar, DbDecimal, DbUuid, DbBytes, DbFloat, DbArray, SqliteDialect)
import std.schema (Identity, DefaultNow, Cascade, mkColumn, withColumn, schema, generated, primaryKey, unique, foreignKey, references, onDelete, check, compositePrimaryKey, uniqueConstraint, schemaToDdlFor, createTableDdlFor)

type User = { id: Int, email: Text, age: Int }

-- An identity primary key renders as `INTEGER PRIMARY KEY AUTOINCREMENT` (no
-- separate serial type, no second inline PRIMARY KEY), a unique non-null text column
-- carries both modifiers inline, and a checked column renders its captured predicate.
fn userSchema () =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "email" "email" DbText false |> unique)
      |> withColumn (mkColumn "age" "age" DbInt false |> check (fn (u: User) -> u.age >= 0))

pub fn userDdl () -> Text = schemaToDdlFor SqliteDialect (userSchema ())

-- A foreign key with an ON DELETE action, a DEFAULT now() timestamp (which becomes
-- CURRENT_TIMESTAMP over a TEXT column), and a nullable column.
fn postSchema () =
    schema "Post" "posts"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "author" "author" DbBigInt false |> foreignKey (references "users" "id" |> onDelete Cascade))
      |> withColumn (mkColumn "created_at" "created_at" DbTimestampTz false |> generated DefaultNow)
      |> withColumn (mkColumn "bio" "bio" DbText true)

pub fn postDdl () -> Text = schemaToDdlFor SqliteDialect (postSchema ())

-- A junction table whose key is two columns: the composite primary key and the
-- multi-column unique constraint render as table-level clauses after the columns.
fn membershipSchema () =
    schema "Membership" "memberships"
      |> withColumn (mkColumn "user_id" "user_id" DbBigInt false)
      |> withColumn (mkColumn "group_id" "group_id" DbBigInt false)
      |> withColumn (mkColumn "role" "role" DbText false)
      |> compositePrimaryKey ["user_id", "group_id"]
      |> uniqueConstraint ["user_id", "role"]

pub fn membershipDdl () -> Text = schemaToDdlFor SqliteDialect (membershipSchema ())

-- A smallint identity still renders as an INTEGER rowid (SQLite has no narrow serial),
-- and the narrow types collapse onto their affinities: char(n) is TEXT, smallint is
-- INTEGER.
fn narrowSchema () =
    schema "Ticket" "tickets"
      |> withColumn (mkColumn "id" "id" DbSmallInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "code" "code" (DbChar 8) false)
      |> withColumn (mkColumn "priority" "priority" DbSmallInt false)

pub fn narrowDdl () -> Text = schemaToDdlFor SqliteDialect (narrowSchema ())

-- The rich column types map onto SQLite storage classes: an exact decimal and a uuid
-- ride TEXT (a decimal never REAL, whose float rounding would corrupt money), bytes a
-- BLOB, a float REAL, and an array its JSON text.
fn richSchema () =
    schema "Rec" "recs"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "amount" "amount" (DbDecimal 12 2) false)
      |> withColumn (mkColumn "token" "token" DbUuid false)
      |> withColumn (mkColumn "blob" "blob" DbBytes true)
      |> withColumn (mkColumn "ratio" "ratio" DbFloat false)
      |> withColumn (mkColumn "tags" "tags" (DbArray DbText) false)

pub fn richDdl () -> Text = schemaToDdlFor SqliteDialect (richSchema ())

-- The migration-tuple CREATE TABLE in the SQLite dialect: `int` maps to INTEGER, and a
-- non-identity integer primary key carries an inline PRIMARY KEY.
pub fn migrateCreateDdl () -> Text =
    createTableDdlFor SqliteDialect "widgets" [("id", "int", false, true, false), ("name", "text", false, false, false)]
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"sqlite-ddl-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn sqlite_ddl_renders_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping sqlite_ddl_renders_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-sqlite-ddl-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-sqlite-ddl-e2e-cache-")
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
         lists:foreach(F,['userDdl','postDdl','membershipDdl','narrowDdl','richDdl',\
         'migrateCreateDdl']), halt()."
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

    // An identity primary key is an `INTEGER PRIMARY KEY AUTOINCREMENT` rowid, carrying
    // no second inline key; a non-null unique text column carries both modifiers; a
    // checked column renders the captured predicate with its literal inline.
    want(
        r#"userDdl=CREATE TABLE "users" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "email" TEXT NOT NULL UNIQUE, "age" INTEGER NOT NULL CHECK (("age" >= 0)))"#,
    );

    // A foreign key renders `REFERENCES … ON DELETE CASCADE`; a DefaultNow column over a
    // TEXT timestamp renders `DEFAULT CURRENT_TIMESTAMP`; a nullable column carries no
    // NOT NULL.
    want(
        r#"postDdl=CREATE TABLE "posts" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "author" INTEGER NOT NULL REFERENCES "users" ("id") ON DELETE CASCADE, "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, "bio" TEXT)"#,
    );

    // A composite primary key and a multi-column unique constraint render as table-level
    // clauses after the columns; no column carries an inline PRIMARY KEY.
    want(
        r#"membershipDdl=CREATE TABLE "memberships" ("user_id" INTEGER NOT NULL, "group_id" INTEGER NOT NULL, "role" TEXT NOT NULL, PRIMARY KEY ("user_id", "group_id"), UNIQUE ("user_id", "role"))"#,
    );

    // A smallint identity is still an INTEGER rowid; char(n) and smallint collapse onto
    // their TEXT/INTEGER affinities.
    want(
        r#"narrowDdl=CREATE TABLE "tickets" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "code" TEXT NOT NULL, "priority" INTEGER NOT NULL)"#,
    );

    // The rich types map onto storage classes: decimal and uuid to TEXT, bytes to BLOB,
    // float to REAL, an array to TEXT.
    want(
        r#"richDdl=CREATE TABLE "recs" ("id" INTEGER PRIMARY KEY AUTOINCREMENT, "amount" TEXT NOT NULL, "token" TEXT NOT NULL, "blob" BLOB, "ratio" REAL NOT NULL, "tags" TEXT NOT NULL)"#,
    );

    // The migration-tuple renderer in the SQLite dialect: `int` maps to INTEGER, and a
    // non-identity integer primary key carries an inline PRIMARY KEY.
    want(
        r#"migrateCreateDdl=CREATE TABLE "widgets" ("id" INTEGER NOT NULL PRIMARY KEY, "name" TEXT NOT NULL)"#,
    );
}
