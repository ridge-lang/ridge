//! End-to-end check that a `createView` migration step runs on SQLite through the real
//! repository API.
//!
//! A migration creates a table, a second migration saves a filtered query as a view
//! (`createView`), and the view is read back as the entity through a plain `Repo` bound to
//! the view name — a view is a relation, so the table read path works unchanged. The view's
//! filter is the point: with an active and an inactive account seeded, the view shows only
//! the active one. Rolling the view migration back exercises the auto-reverse — `createView`
//! inverts to `dropView` — so the view is gone and reading it fails.
//!
//! Gated on `beam-runtime` (real OTP + the baked SQLite NIF) plus a `which` guard.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (connectSqlite, sqliteMemory, Sqlite)
import std.migrate as Migrate
import std.repo as Repo
import std.schema (schemaOf)
import std.query (QueryPlan, planScan)
import std.list (length)

pub type Account = { id: Int, name: Text, active: Bool } deriving (Row, Schema)

fn accountWitness () -> Option Account = None

-- The view's SELECT: only the active accounts. Built from prelude `QExpr` constructors (the
-- shape the query builder reifies): a bool column compared to TRUE.
fn activeView () -> QueryPlan = planScan "accounts" (QEq (QCol "active") (QLitBool true)) [] (0 - 1) 0 false

-- Create the table, then the view over it — two migrations so a rollback can drop the view
-- alone.
fn setup (conn: Sqlite) -> Result (List Text) Error =
    Migrate.run conn [ Migrate.migration "0001_accounts" [ Migrate.createSchema (schemaOf (accountWitness ())) ], Migrate.migration "0002_active_view" [ Migrate.createView "active_accounts" (activeView ()) ] ]

fn addAccount (conn: Sqlite) (aname: Text) (act: Bool) -> Result Unit Error =
    let accounts: Repo Account Sqlite = Repo.repo conn "accounts"
    Repo.insert (AccountInsert { name = aname, active = act }) accounts

fn seedTwo (conn: Sqlite) -> Result Unit Error =
    match addAccount conn "ada" true
        Err e -> Err e
        Ok _  -> addAccount conn "lin" false

-- Read the view as `Account` through a plain `Repo` bound to the view name.
fn readView (conn: Sqlite) -> Result (List Account) Error =
    let view: Repo Account Sqlite = Repo.repo conn "active_accounts"
    Repo.all view

-- ada is active, lin is not, so the view shows one row.
pub fn db viewCount () -> Int =
    match connectSqlite (sqliteMemory ())
        Err _ -> 0 - 1
        Ok conn ->
            match setup conn
                Err _ -> 0 - 2
                Ok _  ->
                    match seedTwo conn
                        Err _ -> 0 - 3
                        Ok _  ->
                            match readView conn
                                Ok rows -> length rows
                                Err _   -> 0 - 4

-- The single visible row is the active account — the view's filter ran.
pub fn db viewFirstName () -> Text =
    match connectSqlite (sqliteMemory ())
        Err _ -> "conn-err"
        Ok conn ->
            match setup conn
                Err _ -> "setup-err"
                Ok _  ->
                    match seedTwo conn
                        Err _ -> "seed-err"
                        Ok _  ->
                            match readView conn
                                Ok (a :: _) -> a.name
                                Ok []       -> "empty"
                                Err _       -> "read-err"

-- Rolling the last migration back auto-reverses `createView` to `dropView`, so the view is
-- gone and reading it fails.
pub fn db viewDropped () -> Text =
    match connectSqlite (sqliteMemory ())
        Err _ -> "conn-err"
        Ok conn ->
            match setup conn
                Err _ -> "setup-err"
                Ok _  ->
                    let migs = [ Migrate.migration "0001_accounts" [ Migrate.createSchema (schemaOf (accountWitness ())) ], Migrate.migration "0002_active_view" [ Migrate.createView "active_accounts" (activeView ()) ] ]
                    match Migrate.rollback conn migs 1
                        Err _ -> "rollback-err"
                        Ok _  ->
                            match readView conn
                                Err _ -> "dropped"
                                Ok _  -> "still-there"
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"view-migration-sqlite-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"db\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn create_view_migration_runs_on_sqlite() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping create_view_migration_runs_on_sqlite");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-view-migration-sqlite-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-view-migration-sqlite-e2e-cache-")
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
        "io:format(\"count=~w~n\",[{module}:viewCount()]), \
         io:format(\"first=~s~n\",[{module}:viewFirstName()]), \
         io:format(\"rolled=~s~n\",[{module}:viewDropped()]), \
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

    // The view was created by the migration and filters to the one active account.
    assert!(
        stdout.contains("count=1"),
        "expected `count=1` — createView migration or the view read failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("first=ada"),
        "expected `first=ada` — the view did not filter to the active account\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Rolling back auto-reversed createView to dropView, so the view no longer resolves.
    assert!(
        stdout.contains("rolled=dropped"),
        "expected `rolled=dropped` — the view survived rollback or the reverse was wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
