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
//! Transaction isolation runs through the same stack: `transactionWith
//! Serializable` commits its rows; `transactionWith ReadUncommitted` turns the
//! connection's `read_uncommitted` pragma on for the transaction's span (read
//! back through the raw-query escape hatch); and a nested `transactionWith`
//! naming a different level fails with `db.tx.isolation_mismatch` (kind
//! `Unsupported`) and writes nothing.
//!
//! Gated on `beam-runtime` (real OTP + the baked NIF) plus a `which` guard.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (connectSqlite, sqliteMemory, Sqlite, IsolationLevel, ReadCommitted, ReadUncommitted, Serializable, dbErrorKind, Unsupported)
import std.migrate as Migrate
import std.repo as Repo
import std.raw as Raw
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

-- How many accounts the store holds, read back through the typed repository.
fn countAccounts (conn: Sqlite) -> Int =
    match readAccounts conn
        Ok rows -> length rows
        Err _   -> 0 - 9

-- A transaction body that inserts two rows and succeeds.
fn insertTwo (tx: Sqlite) -> Result Unit Error =
    match addAccount tx "ada" true
        Err e -> Err e
        Ok _  -> addAccount tx "lin" false

-- A one-row insert body, the name fixed up front so it is a plain
-- `Sqlite -> Result Unit Error` the transaction combinator can run.
fn insertOne (aname: Text) (tx: Sqlite) -> Result Unit Error =
    addAccount tx aname true

-- A deliberate failure with no SQL fault: a single-row query filtered to match
-- nothing answers `Err` ("matched no rows"), which a transaction body returns to
-- trigger a rollback. It is a plain SELECT, so it never aborts the transaction.
fn forceFail (conn: Sqlite) -> Result Unit Error =
    let accounts: Repo Account Sqlite = Repo.repo conn "accounts"
    match accounts |> Repo.query |> Repo.filter (fn (a: Account) -> a.id == 999999) |> Repo.singleOrError
        Err e -> Err e
        Ok _  -> Ok ()

-- The error-kind check for the mismatch probe.
fn isUnsupported (e: Error) -> Bool =
    match dbErrorKind e
        Unsupported -> true
        _           -> false

-- An explicit serializable transaction commits exactly like a plain one: both
-- inserts survive -> 2.
pub fn db sqliteIsoSerializable () -> Int =
    match connectSqlite (sqliteMemory ())
        Err _ -> 0 - 1
        Ok conn ->
            match setup conn
                Err _ -> 0 - 2
                Ok _  ->
                    match Repo.transactionWith Serializable conn insertTwo
                        Err _ -> 0 - 3
                        Ok _  -> countAccounts conn

-- The one row `PRAGMA read_uncommitted` answers.
pub type PragmaRow = { read_uncommitted: Int } deriving (Row)

-- A transaction body that reads the connection's read_uncommitted pragma back
-- through the raw-query escape hatch: 1 while a ReadUncommitted transaction is
-- open.
fn readUncommittedPragma (tx: Sqlite) -> Result Int Error =
    let q: Result (List PragmaRow) Error = Raw.query tx "PRAGMA read_uncommitted" []
    match q
        Err e -> Err e
        Ok rows ->
            match rows
                []     -> Ok (0 - 4)
                r :: _ -> Ok r.read_uncommitted

-- transactionWith ReadUncommitted turns the connection pragma on for the
-- transaction's span -> 1 read inside it.
pub fn db sqliteIsoReadUncommitted () -> Int =
    match connectSqlite (sqliteMemory ())
        Err _ -> 0 - 1
        Ok conn ->
            match setup conn
                Err _ -> 0 - 2
                Ok _  ->
                    match Repo.transactionWith ReadUncommitted conn readUncommittedPragma
                        Err _ -> 0 - 3
                        Ok n  -> n

-- A nested transactionWith naming a different level fails with the
-- isolation-mismatch error (kind Unsupported) and writes nothing -> 0.
fn sqliteMismatchBody (tx: Sqlite) -> Result Unit Error =
    match Repo.transactionWith ReadCommitted tx (insertOne "lin")
        Err e -> if isUnsupported e then Ok () else Err e
        Ok _  -> forceFail tx

pub fn db sqliteIsoMismatch () -> Int =
    match connectSqlite (sqliteMemory ())
        Err _ -> 0 - 1
        Ok conn ->
            match setup conn
                Err _ -> 0 - 2
                Ok _  ->
                    match Repo.transactionWith Serializable conn sqliteMismatchBody
                        Ok _  -> countAccounts conn
                        Err _ -> 0 - 3
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
         io:format(\"isoSerializable=~w~n\",[{module}:sqliteIsoSerializable()]), \
         io:format(\"isoReadUncommitted=~w~n\",[{module}:sqliteIsoReadUncommitted()]), \
         io:format(\"isoMismatch=~w~n\",[{module}:sqliteIsoMismatch()]), \
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
    // An explicit serializable transaction commits both inserts.
    assert!(
        stdout.contains("isoSerializable=2"),
        "expected `isoSerializable=2` — transactionWith Serializable did not commit both rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A ReadUncommitted transaction turns the connection pragma on for its span.
    assert!(
        stdout.contains("isoReadUncommitted=1"),
        "expected `isoReadUncommitted=1` — PRAGMA read_uncommitted did not read 1 inside the span\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A nested transactionWith naming a different level fails with the
    // isolation-mismatch error (kind Unsupported) and writes nothing.
    assert!(
        stdout.contains("isoMismatch=0"),
        "expected `isoMismatch=0` — level mismatch was not flagged Unsupported or wrote rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
