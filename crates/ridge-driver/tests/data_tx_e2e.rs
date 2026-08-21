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
//! `Repo.transactionWith` adds an explicit isolation level. The in-memory keeper
//! records the level of the outermost transaction, so four more probes pin the
//! rules:
//! - `withLevelCommits` — an explicit level commits exactly like a plain transaction (2).
//! - `nestedSameLevel` — a nested `transactionWith` naming the outer level opens a savepoint and commits (2).
//! - `nestedPlainInsideWith` — a plain nested `transaction` demands no level and commits inside any outer level (2).
//! - `nestedMismatch` — a nested `transactionWith` naming a different level fails with the isolation-mismatch error (kind `Unsupported`) and writes nothing (0).
//!
//! `Repo.retryWhen` runs an attempt function under a policy, retrying while the
//! classifier calls the failure transient, and `Repo.transactionWithRetry` wraps
//! a whole transaction in that loop. Five more probes pin the rules:
//! - `retrySucceedsOnSecond` — a transient failure is retried and the second attempt's value is the result (42).
//! - `retryStopsAtMax` — the loop stops at `maxAttempts` and returns the last error unwrapped (1).
//! - `retryNonTransientStops` — a permanent failure comes back at once, with no second attempt (1).
//! - `txRetryCommits` — `transactionWithRetry` commits like a plain transaction, `None` reading the adapter's default policy (2).
//! - `txRetryRollsBack` — a non-transient body failure rolls back and is not retried, so nothing persists (0).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

// Each test below is one end-to-end scenario: a Ridge program, a real OTP
// run, and the assertions on its output. A split by line count would put
// half a scenario out of sight of the half that explains it.
#![allow(clippy::too_many_lines)]
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

const SOURCE: &str = r#"
import std.data (DbError, memAdapter, MemAdapter, IsolationLevel, ReadCommitted, Serializable, dbErrorKind, Unsupported, RetryPolicy)
import std.repo as Repo
import std.sql (SqlValue)

-- `deriving (Schema)` makes `id` an identity column by convention, so the typed
-- `insert` omits it and the store assigns it; the transaction probes count rows and
-- never assert a specific id, so the database-assigned ids do not affect them.
pub type User = { id: Int, name: Text } deriving (Row, Schema)

-- `id` is a `deriving (Schema)` identity column, so the insert shape drops it and
-- the store assigns it; the probes only count rows, so a name is all they carry.
fn mkUser (uname: Text) -> UserInsert =
    UserInsert { name = uname }

-- How many users the store holds, read back through the typed repository.
fn countAll (conn: MemAdapter) -> Int =
    let users: Repo User MemAdapter = Repo.repo conn "users"
    match users |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 1

-- A deliberate failure with no SQL fault: a single-row query filtered to match
-- nothing answers `Err` ("matched no rows"), which a transaction body returns to
-- trigger a rollback. It is a plain SELECT, so it never aborts the session.
fn forceFail (conn: MemAdapter) -> Result Unit DbError =
    let users: Repo User MemAdapter = Repo.repo conn "users"
    match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 999999) |> Repo.singleOrError
        Err e -> Err e
        Ok _  -> Ok ()

-- Insert one user, returning the result.
fn addUser (conn: MemAdapter) (uname: Text) -> Result Unit DbError =
    let users: Repo User MemAdapter = Repo.repo conn "users"
    Repo.insert (mkUser uname) users

-- A transaction body that inserts two rows and succeeds.
fn insertTwo (tx: MemAdapter) -> Result Unit DbError =
    match addUser tx "ada"
        Err e -> Err e
        Ok _  -> addUser tx "lin"

-- A transaction body that inserts a row and then fails, so it rolls back.
fn insertThenFail (tx: MemAdapter) -> Result Unit DbError =
    match addUser tx "lin"
        Err e -> Err e
        Ok _  -> forceFail tx

-- A transaction body whose nested transaction inserts a row and then fails: the
-- nested failure rewinds to its savepoint, and this body commits its own row.
fn outerKeepsInnerRollsBack (tx: MemAdapter) -> Result Unit DbError =
    match addUser tx "ada"
        Err e -> Err e
        Ok _  ->
            let _inner = Repo.transaction tx insertThenFail
            Ok ()

-- A transaction body whose nested transaction inserts a row and commits (releasing
-- its savepoint), and this body then fails: the outer rollback unwinds both rows.
fn outerFailsAfterInnerCommit (tx: MemAdapter) -> Result Unit DbError =
    match addUser tx "ada"
        Err e -> Err e
        Ok _  ->
            match Repo.transaction tx (insertOne "lin")
                Err e -> Err e
                Ok _  -> forceFail tx

-- A one-row insert body, the id and name fixed up front so it is a plain
-- `MemAdapter -> Result Unit DbError` the transaction combinator can run.
fn insertOne (uname: Text) (tx: MemAdapter) -> Result Unit DbError =
    addUser tx uname

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
    match addUser conn "ada"
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

-- The error-kind check for the nested-mismatch probes.
fn isUnsupported (e: DbError) -> Bool =
    match dbErrorKind e
        Unsupported -> true
        _           -> false

-- An explicit level commits exactly like a plain transaction.
pub fn db withLevelCommits () -> Int =
    let conn = memAdapter ()
    match Repo.transactionWith Serializable conn insertTwo
        Ok _  -> countAll conn
        Err _ -> 0 - 1

-- A nested transactionWith naming the same level opens a savepoint and commits.
fn nestedSameLevelBody (tx: MemAdapter) -> Result Unit DbError =
    match addUser tx "ada"
        Err e -> Err e
        Ok _  -> Repo.transactionWith Serializable tx (insertOne "lin")

pub fn db nestedSameLevel () -> Int =
    let conn = memAdapter ()
    match Repo.transactionWith Serializable conn nestedSameLevelBody
        Ok _  -> countAll conn
        Err _ -> 0 - 1

-- A plain transaction nested inside a transactionWith demands no level: it
-- opens a savepoint regardless of the outer level.
fn nestedPlainInsideWithBody (tx: MemAdapter) -> Result Unit DbError =
    match addUser tx "ada"
        Err e -> Err e
        Ok _  -> Repo.transaction tx (insertOne "lin")

pub fn db nestedPlainInsideWith () -> Int =
    let conn = memAdapter ()
    match Repo.transactionWith Serializable conn nestedPlainInsideWithBody
        Ok _  -> countAll conn
        Err _ -> 0 - 1

-- A nested transactionWith naming a different level fails with the
-- isolation-mismatch error (kind Unsupported) and writes nothing.
fn nestedMismatchBody (tx: MemAdapter) -> Result Unit DbError =
    match Repo.transactionWith ReadCommitted tx (insertOne "lin")
        Err e -> if isUnsupported e then Ok () else Err e
        Ok _  -> forceFail tx

pub fn db nestedMismatch () -> Int =
    let conn = memAdapter ()
    match Repo.transactionWith Serializable conn nestedMismatchBody
        Ok _  -> countAll conn
        Err _ -> 0 - 1

-- ── Retry probes ────────────────────────────────────────────────────────────
--
-- The loop is exercised through the generic `retryWhen` with `Text` errors —
-- the nominal `Error` has no constructor, so a fabricated failure is a `Text`
-- — and `transactionWithRetry` is exercised over the in-memory adapter.

-- A two-attempt policy with negligible backoffs, so the probes stay fast.
fn twoAttemptPolicy -> RetryPolicy =
    RetryPolicy { maxAttempts = 2, baseBackoffMs = 1, maxBackoffMs = 4 }

fn textIsTransient (err: Text) -> Bool =
    err == "boom"

fn neverTransient (err: Text) -> Bool =
    false

-- Fails transiently on attempt 1, succeeds on attempt 2.
fn succeedsOnSecond (n: Int) -> Result Int Text =
    if n < 2 then Err "boom" else Ok 42

-- Fails transiently on attempts 1-2, succeeds on attempt 3.
fn succeedsOnThird (n: Int) -> Result Int Text =
    if n < 3 then Err "boom" else Ok 42

-- A transient failure is retried: the second attempt's `Ok` is the result.
-- Proves the loop runs past attempt 1.
pub fn time random retrySucceedsOnSecond () -> Int =
    match Repo.retryWhen (twoAttemptPolicy ()) textIsTransient succeedsOnSecond
        Ok n  -> n
        Err _ -> 0 - 1

-- `maxAttempts` bounds the loop: a success on attempt 3 is never reached when
-- only 2 attempts are allowed, and the last error comes back unwrapped. With
-- `retrySucceedsOnSecond` (≥ 2 attempts ran), this pins the count at exactly 2.
pub fn time random retryStopsAtMax () -> Int =
    match Repo.retryWhen (twoAttemptPolicy ()) textIsTransient succeedsOnThird
        Ok _    -> 0 - 1
        Err err -> if err == "boom" then 1 else 0 - 2

-- A non-transient error comes back at once, with no second attempt (this same
-- attempt fn succeeds on attempt 2 when one is allowed).
pub fn time random retryNonTransientStops () -> Int =
    match Repo.retryWhen (twoAttemptPolicy ()) neverTransient succeedsOnSecond
        Ok _    -> 0 - 1
        Err err -> if err == "boom" then 1 else 0 - 2

-- `transactionWithRetry` over the in-memory adapter commits like a plain
-- transaction; `None` reads the adapter's default policy.
pub fn db time random txRetryCommits () -> Int =
    let conn = memAdapter ()
    match Repo.transactionWithRetry None conn insertTwo
        Ok _  -> countAll conn
        Err _ -> 0 - 1

-- A body that fails with a non-transient error (the store's "matched no rows")
-- rolls back and is not retried: nothing persists.
pub fn db time random txRetryRollsBack () -> Int =
    let conn = memAdapter ()
    match Repo.transactionWithRetry (Some (twoAttemptPolicy ())) conn insertThenFail
        Ok _  -> 0 - 3
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
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = [\"db\", \"time\", \"random\"]\n",
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
        .find(|stem| {
            stem.starts_with("ridge_")
                && !matches!(
                    *stem,
                    "ridge_rt"
                        | "ridge_main_runner"
                        | "ridge_test_runner"
                        | "ridge_pg"
                        | "ridge_sup"
                        | "ridge_sqlite"
                        | "ridge_bench_runner"
                )
        })
        .expect("a user module")
        .to_owned();

    let expr = format!(
        "io:format(\"committed=~w~n\",[{module}:committed()]), \
         io:format(\"rolledBack=~w~n\",[{module}:rolledBack()]), \
         io:format(\"innerRollback=~w~n\",[{module}:innerRollback()]), \
         io:format(\"outerRollback=~w~n\",[{module}:outerRollback()]), \
         io:format(\"withLevelCommits=~w~n\",[{module}:withLevelCommits()]), \
         io:format(\"nestedSameLevel=~w~n\",[{module}:nestedSameLevel()]), \
         io:format(\"nestedPlainInsideWith=~w~n\",[{module}:nestedPlainInsideWith()]), \
         io:format(\"nestedMismatch=~w~n\",[{module}:nestedMismatch()]), \
         io:format(\"retrySucceedsOnSecond=~w~n\",[{module}:retrySucceedsOnSecond()]), \
         io:format(\"retryStopsAtMax=~w~n\",[{module}:retryStopsAtMax()]), \
         io:format(\"retryNonTransientStops=~w~n\",[{module}:retryNonTransientStops()]), \
         io:format(\"txRetryCommits=~w~n\",[{module}:txRetryCommits()]), \
         io:format(\"txRetryRollsBack=~w~n\",[{module}:txRetryRollsBack()]), \
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
    // An explicit isolation level commits both inserts, like a plain transaction.
    assert!(
        stdout.contains("withLevelCommits=2"),
        "expected `withLevelCommits=2` — transactionWith did not commit both rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A nested transactionWith naming the outer level commits as a savepoint.
    assert!(
        stdout.contains("nestedSameLevel=2"),
        "expected `nestedSameLevel=2` — same-level nested transactionWith did not commit\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A plain nested transaction demands no level and commits inside transactionWith.
    assert!(
        stdout.contains("nestedPlainInsideWith=2"),
        "expected `nestedPlainInsideWith=2` — plain nested transaction inside transactionWith did not commit\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A nested transactionWith naming a different level fails with the
    // isolation-mismatch error (kind Unsupported) and writes nothing.
    assert!(
        stdout.contains("nestedMismatch=0"),
        "expected `nestedMismatch=0` — level mismatch was not flagged Unsupported or wrote rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A transient failure is retried; the second attempt's value is the result.
    assert!(
        stdout.contains("retrySucceedsOnSecond=42"),
        "expected `retrySucceedsOnSecond=42` — transient failure was not retried\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The loop stops at maxAttempts and returns the last error unwrapped.
    assert!(
        stdout.contains("retryStopsAtMax=1"),
        "expected `retryStopsAtMax=1` — the loop did not stop at maxAttempts\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A non-transient error is returned with no retry.
    assert!(
        stdout.contains("retryNonTransientStops=1"),
        "expected `retryNonTransientStops=1` — a permanent error was retried\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // transactionWithRetry commits like a plain transaction (None → default policy).
    assert!(
        stdout.contains("txRetryCommits=2"),
        "expected `txRetryCommits=2` — transactionWithRetry did not commit both rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A non-transient body failure rolls back and is not retried.
    assert!(
        stdout.contains("txRetryRollsBack=0"),
        "expected `txRetryRollsBack=0` — a permanent body failure persisted rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
