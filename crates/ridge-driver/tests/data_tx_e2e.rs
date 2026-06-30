//! End-to-end check for std.data transactions on the in-memory adapter — proves
//! `Repo.transaction` commits, rolls back, and nests as a savepoint on the BEAM.
//!
//! The combinator runs a body on the connection: it commits when the body answers
//! `Ok`, rolls back when it answers `Err`, and a nested `transaction` opens a
//! savepoint so an inner failure rewinds only the inner work. This program drives
//! four scenarios and reports the surviving row count of each:
//! - `committed` — a body inserts two rows and succeeds, so both persist (2).
//! - `rolledBack` — a committed baseline row plus a failing transaction whose insert is undone (1).
//! - `innerRollback` — a nested transaction inserts a row and fails (rewinding to its savepoint) while the outer commits its own row (1).
//! - `outerRollback` — a nested transaction commits (releasing its savepoint), then the outer fails and unwinds everything (0).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

const SOURCE: &str = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (SqlValue)

-- `deriving (Schema)` makes `id` an identity column by convention, so the typed
-- `insert` omits it and the store assigns it; the transaction probes count rows and
-- never assert a specific id, so the database-assigned ids do not affect them.
pub type User = { id: Int, name: Text } deriving (Row, Schema)

fn mkUser (uid: Int) (uname: Text) -> User =
    User { id = uid, name = uname }

-- How many users the store holds, read back through the typed repository.
fn countAll (conn: MemAdapter) -> Int =
    let users: Repo User MemAdapter = Repo.repo conn "users"
    match users |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 1

-- A deliberate failure with no SQL fault: a single-row query filtered to match
-- nothing answers `Err` ("matched no rows"), which a transaction body returns to
-- trigger a rollback. It is a plain SELECT, so it never aborts the session.
fn forceFail (conn: MemAdapter) -> Result Unit Error =
    let users: Repo User MemAdapter = Repo.repo conn "users"
    match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 999999) |> Repo.singleOrError
        Err e -> Err e
        Ok _  -> Ok ()

-- Insert one user, returning the result.
fn addUser (conn: MemAdapter) (uid: Int) (uname: Text) -> Result Unit Error =
    let users: Repo User MemAdapter = Repo.repo conn "users"
    Repo.insert (mkUser uid uname) users

-- A transaction body that inserts two rows and succeeds.
fn insertTwo (tx: MemAdapter) -> Result Unit Error =
    match addUser tx 1 "ada"
        Err e -> Err e
        Ok _  -> addUser tx 2 "lin"

-- A transaction body that inserts a row and then fails, so it rolls back.
fn insertThenFail (tx: MemAdapter) -> Result Unit Error =
    match addUser tx 2 "lin"
        Err e -> Err e
        Ok _  -> forceFail tx

-- A transaction body whose nested transaction inserts a row and then fails: the
-- nested failure rewinds to its savepoint, and this body commits its own row.
fn outerKeepsInnerRollsBack (tx: MemAdapter) -> Result Unit Error =
    match addUser tx 1 "ada"
        Err e -> Err e
        Ok _  ->
            let _inner = Repo.transaction tx insertThenFail
            Ok ()

-- A transaction body whose nested transaction inserts a row and commits (releasing
-- its savepoint), and this body then fails: the outer rollback unwinds both rows.
fn outerFailsAfterInnerCommit (tx: MemAdapter) -> Result Unit Error =
    match addUser tx 1 "ada"
        Err e -> Err e
        Ok _  ->
            match Repo.transaction tx (insertOne 2 "lin")
                Err e -> Err e
                Ok _  -> forceFail tx

-- A one-row insert body, the id and name fixed up front so it is a plain
-- `MemAdapter -> Result Unit Error` the transaction combinator can run.
fn insertOne (uid: Int) (uname: Text) (tx: MemAdapter) -> Result Unit Error =
    addUser tx uid uname

-- A successful transaction: insert two rows and commit. Both survive.
pub fn db committed () -> Int =
    let conn = memAdapter ()
    match Repo.transaction conn insertTwo
        Ok _  -> countAll conn
        Err _ -> 0 - 1

-- A failing transaction over a committed baseline: the baseline row stays, the
-- transaction's insert is rolled back.
pub fn db rolledBack () -> Int =
    let conn = memAdapter ()
    match addUser conn 1 "ada"
        Err _ -> 0 - 2
        Ok _  ->
            match Repo.transaction conn insertThenFail
                Ok _  -> 0 - 3
                Err _ -> countAll conn

-- Nested: the outer commits, the inner fails. The inner insert rewinds to the
-- savepoint; the outer insert survives.
pub fn db innerRollback () -> Int =
    let conn = memAdapter ()
    match Repo.transaction conn outerKeepsInnerRollsBack
        Ok _  -> countAll conn
        Err _ -> 0 - 1

-- Nested: the inner commits (releasing its savepoint), then the outer fails. The
-- outer rollback unwinds everything, the released savepoint's work included.
pub fn db outerRollback () -> Int =
    let conn = memAdapter ()
    match Repo.transaction conn outerFailsAfterInnerCommit
        Ok _  -> 0 - 5
        Err _ -> countAll conn
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-tx-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn transactions_commit_rollback_and_nest_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping transactions_commit_rollback_and_nest_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-tx-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-tx-e2e-cache-")
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
        "io:format(\"committed=~w~n\",[{module}:committed()]), \
         io:format(\"rolledBack=~w~n\",[{module}:rolledBack()]), \
         io:format(\"innerRollback=~w~n\",[{module}:innerRollback()]), \
         io:format(\"outerRollback=~w~n\",[{module}:outerRollback()]), \
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

    // A committed transaction persists both inserts.
    assert!(
        stdout.contains("committed=2"),
        "expected `committed=2` — commit did not persist both rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A failed transaction rolls back its insert, leaving only the baseline row.
    assert!(
        stdout.contains("rolledBack=1"),
        "expected `rolledBack=1` — rollback did not undo the transaction's insert\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A failed inner transaction rewinds to the savepoint; the outer commit keeps its row.
    assert!(
        stdout.contains("innerRollback=1"),
        "expected `innerRollback=1` — nested savepoint rollback was wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // An outer rollback unwinds everything, including a released savepoint's work.
    assert!(
        stdout.contains("outerRollback=0"),
        "expected `outerRollback=0` — outer rollback did not unwind the savepoint\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
