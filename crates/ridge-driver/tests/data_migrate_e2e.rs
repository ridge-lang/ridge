//! End-to-end check for std.migrate on the in-memory adapter — proves the
//! migration runner applies a schema, tracks what it applied, and is idempotent
//! on the BEAM.
//!
//! `Migrate.run` reads the tracking table for the migrations already applied,
//! runs each pending one in its own transaction (its schema changes and the
//! record of it landing commit together), and answers the names applied on this
//! run. This program drives three scenarios and reports an integer from each:
//! - `firstApplied` — a fresh store runs a two-migration schema, so both apply (2).
//! - `idempotent` — running the same schema twice applies nothing the second time (0).
//! - `usable` — after the schema runs, the created table accepts and counts rows (2).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

const SOURCE: &str = r#"
import std.data (memAdapter, MemAdapter)
import std.migrate as Migrate
import std.migrate (SchemaOp)
import std.repo as Repo
import std.list (length)
import std.sql (SqlValue)

-- `deriving (Schema)` makes `id` an identity column by convention, so the typed
-- `insert` omits it and the store assigns it; the probes count rows rather than
-- assert a specific id.
pub type User = { id: Int, name: Text } deriving (Row, Schema)

-- Each table is built in its own helper (a statement-level `createTable` with its
-- columns), and the schema list stays flat, so the entry points never name the
-- `Migration` type and the nested literal never spans lines.
fn usersTable () -> SchemaOp =
    Migrate.createTable "users"
        [ Migrate.intCol  "id"   |> Migrate.primaryKey
        , Migrate.textCol "name" ]

fn postsTable () -> SchemaOp =
    Migrate.createTable "posts"
        [ Migrate.intCol "id"     |> Migrate.primaryKey
        , Migrate.intCol "author" ]

fn applyAll (conn: MemAdapter) -> Result (List Text) Error =
    let schema = [ Migrate.migration "0001_users" [ usersTable () ], Migrate.migration "0002_posts" [ postsTable (), Migrate.createIndex "posts_author_idx" "posts" ["author"] ] ]
    Migrate.run conn schema

-- A migration exercising every other schema verb: add and drop a column, a unique
-- index, and a create/drop of a throwaway table. On the schemaless in-memory store
-- the column and index changes are no-ops and create/drop touch table existence;
-- the point is that each verb runs and the migration commits.
fn alterAll (conn: MemAdapter) -> Result (List Text) Error =
    let ops = [ Migrate.addColumn "users" (Migrate.intCol "age" |> Migrate.nullable), Migrate.dropColumn "users" "bio", Migrate.uniqueIndex "users_name_idx" "users" ["name"], Migrate.createTable "temp" [ Migrate.intCol "id" ], Migrate.dropTable "temp" ]
    Migrate.run conn [ Migrate.migration "0003_alter" ops ]

-- Insert one user into the migrated table.
fn addUser (conn: MemAdapter) (_uid: Int) (uname: Text) -> Result Unit Error =
    let users: Repo User MemAdapter = Repo.repo conn "users"
    Repo.insert (UserInsert { name = uname }) users

-- How many users the migrated table holds, read back through the repository.
fn countUsers (conn: MemAdapter) -> Int =
    let users: Repo User MemAdapter = Repo.repo conn "users"
    match users |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 1

-- A fresh store applies the whole schema: both migrations run, so two names come back.
pub fn db firstApplied () -> Int =
    let conn = memAdapter ()
    match applyAll conn
        Ok names -> length names
        Err _    -> 0 - 1

-- Running the same schema a second time applies nothing: every migration is already
-- recorded, so the run answers an empty list.
pub fn db idempotent () -> Int =
    let conn = memAdapter ()
    match applyAll conn
        Err _ -> 0 - 1
        Ok _  ->
            match applyAll conn
                Ok names -> length names
                Err _    -> 0 - 2

-- After the schema runs, the created table is usable: two inserts land and count back.
pub fn db usable () -> Int =
    let conn = memAdapter ()
    match applyAll conn
        Err _ -> 0 - 1
        Ok _  ->
            match addUser conn 1 "ada"
                Err _ -> 0 - 2
                Ok _  ->
                    match addUser conn 2 "lin"
                        Err _ -> 0 - 3
                        Ok _  -> countUsers conn

-- Every other schema verb runs and commits: the alter migration applies on top of
-- the base schema, so one name comes back.
pub fn db altered () -> Int =
    let conn = memAdapter ()
    match applyAll conn
        Err _ -> 0 - 1
        Ok _  ->
            match alterAll conn
                Ok names -> length names
                Err _    -> 0 - 2
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-migrate-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"db\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn migrations_apply_track_and_are_idempotent_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping migrations_apply_track_and_are_idempotent_on_beam"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-migrate-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-migrate-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    if !artefacts.diagnostics.is_empty() {
        eprintln!("COMPILE DIAGNOSTICS:");
        for d in &artefacts.diagnostics {
            eprintln!("  {d:?}");
        }
    }
    assert!(
        artefacts.diagnostics.is_empty(),
        "no compile errors expected; got {:?}",
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
        "io:format(\"firstApplied=~w~n\",[{module}:firstApplied()]), \
         io:format(\"idempotent=~w~n\",[{module}:idempotent()]), \
         io:format(\"usable=~w~n\",[{module}:usable()]), \
         io:format(\"altered=~w~n\",[{module}:altered()]), \
         halt()."
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

    // A fresh store applies both migrations.
    assert!(
        stdout.contains("firstApplied=2"),
        "expected `firstApplied=2` — both migrations should apply on a fresh store\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The second run applies nothing — every migration is already recorded.
    assert!(
        stdout.contains("idempotent=0"),
        "expected `idempotent=0` — a re-run must apply nothing\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The migrated table accepts and counts rows.
    assert!(
        stdout.contains("usable=2"),
        "expected `usable=2` — the created table should accept two inserts\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Every other schema verb (add/drop column, unique index, create/drop table) runs.
    assert!(
        stdout.contains("altered=1"),
        "expected `altered=1` — the alter migration's schema verbs should all run and commit\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
