//! End-to-end check for the std.data SQLite adapter — proves the whole stack
//! runs on the BEAM through the real repository API: a connection opened by
//! `connectSqlite`, a table built from a `deriving (Schema)` entity for the
//! SQLite dialect, a row written and read back through `Repo`, and the record
//! decoded through the storage-tolerant codecs.
//!
//! The `active` field is the point: a Ridge `Bool` is stored as SQLite's integer
//! 0/1 and reads back as a raw integer, so materialising the record proves the
//! codec accepts SQLite's dynamically-typed storage form.
//!
//! Gated on `beam-runtime` (real OTP + the baked NIF) plus a `which` guard.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (connectSqlite, sqliteMemory, Sqlite)
import std.migrate as Migrate
import std.repo as Repo
import std.schema (schemaOf)
import std.list (length)

pub type Account = { id: Int, name: Text, active: Bool } deriving (Row, Schema)

fn accountWitness () -> Option Account = None

fn setup (conn: Sqlite) -> Result (List Text) Error =
    Migrate.run conn [ Migrate.migration "0001_accounts" [ Migrate.createSchema (schemaOf (accountWitness ())) ] ]

fn addAccount (conn: Sqlite) (aname: Text) (act: Bool) -> Result Unit Error =
    let accounts: Repo Account Sqlite = Repo.repo conn "accounts"
    Repo.insert (AccountInsert { name = aname, active = act }) accounts

fn readAccounts (conn: Sqlite) -> Result (List Account) Error =
    let accounts: Repo Account Sqlite = Repo.repo conn "accounts"
    Repo.all accounts

pub fn db accountCount () -> Int =
    match connectSqlite (sqliteMemory ())
        Err _ -> 0 - 1
        Ok conn ->
            match setup conn
                Err _ -> 0 - 2
                Ok _  ->
                    match addAccount conn "ada" true
                        Err _ -> 0 - 3
                        Ok _  ->
                            match addAccount conn "lin" false
                                Err _ -> 0 - 4
                                Ok _  ->
                                    match readAccounts conn
                                        Ok rows -> length rows
                                        Err _   -> 0 - 5

pub fn db firstActive () -> Text =
    match connectSqlite (sqliteMemory ())
        Err _ -> "conn-err"
        Ok conn ->
            match setup conn
                Err _ -> "setup-err"
                Ok _  ->
                    match addAccount conn "ada" true
                        Err _ -> "insert-err"
                        Ok _  ->
                            match readAccounts conn
                                Ok (a :: _) -> if a.active then "active" else "inactive"
                                Ok []       -> "empty"
                                Err _       -> "read-err"
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-sqlite-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn sqlite_adapter_roundtrips_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping sqlite_adapter_roundtrips_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-sqlite-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-sqlite-e2e-cache-")
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
        "io:format(\"count=~w~n\",[{module}:accountCount()]), \
         io:format(\"first=~s~n\",[{module}:firstActive()]), \
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

    // Two rows written and read back through the repository over SQLite.
    assert!(
        stdout.contains("count=2"),
        "expected `count=2` — connect/migrate/insert/read failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The bool stored as SQLite integer 0/1 decodes back to a Ridge Bool.
    assert!(
        stdout.contains("first=active"),
        "expected `first=active` — bool did not round-trip through the tolerant codec\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
