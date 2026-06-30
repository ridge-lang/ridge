//! End-to-end check that the typed insert path omits database-generated columns and
//! the in-memory store fills the omitted identity column — running on the BEAM.
//!
//! `deriving (Schema)` marks a non-null `Int` field named `id` an identity column by
//! convention. The typed `insert`/`insertMany`/`insertReturning` verbs read that schema,
//! drop the identity column from the row, and the in-memory store assigns the next
//! integer in its place. So an entity inserted with a placeholder `id` reads back with a
//! store-assigned one: a fresh store hands out 1, 2, 3 in insertion order, a bulk insert
//! a contiguous run, and `insertReturning` hands the generated id back.
//!
//! An entity with no `id` field names no generated column, so the insert omits nothing
//! and every caller-supplied value is written verbatim — the proof that omit is scoped to
//! the schema's generated set, not applied blanket.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc)
import std.sql (toSql, SqlValue)
import std.int as Int

-- `id` is an identity column by the `deriving (Schema)` convention (a non-null `Int`
-- named `id`), so the typed insert drops it and the in-memory store assigns the next
-- integer. The `id` in each literal below is a placeholder the store overwrites.
pub type User = { id: Int, name: Text } deriving (Row, Schema)

-- An entity with no `id` field: the convention marks nothing identity, so the insert
-- omits no column and the caller-supplied values are written as given.
pub type Tag = { label: Text, weight: Int } deriving (Row, Schema)

-- Render a user list's ids in order as "1,2,3", so the store-assigned ids are observable
-- as one string the probe asserts on.
fn idsOf (us: List User) -> Text =
    match us
        []        -> ""
        u :: []   -> Int.toText u.id
        u :: rest -> Text.concat (Int.toText u.id) (Text.concat "," (idsOf rest))

-- Two inserts with placeholder ids into a fresh store: the store drops each id and
-- assigns 1 then 2, so the round-trip reads "1,2". A non-omitting path would keep the
-- placeholder 0 and read "0,0".
pub fn db assignedIds () -> Text =
    let r: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    match Repo.insert (User { id = 0, name = "ada" }) r
        Err _ -> "insert-err"
        Ok _  ->
            match Repo.insert (User { id = 0, name = "lin" }) r
                Err _ -> "insert-err"
                Ok _  ->
                    match r |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.toList
                        Err _ -> "list-err"
                        Ok us -> idsOf us

-- A bulk insert assigns a contiguous run across the batch: three placeholder ids become
-- "1,2,3" in one multi-row statement.
pub fn db bulkIds () -> Text =
    let r: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    match r |> Repo.insertMany [ User { id = 0, name = "ada" }, User { id = 0, name = "lin" }, User { id = 0, name = "max" } ]
        Err _ -> "insert-err"
        Ok _  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.toList
                Err _ -> "list-err"
                Ok us -> idsOf us

-- insertReturning hands the stored row back, decoded, so the store-assigned id comes
-- back populated: the first insert into a fresh store returns id 1.
pub fn db returnedId () -> Int =
    let r: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    match r |> Repo.insertReturning (User { id = 0, name = "rex" })
        Err _ -> 0 - 1
        Ok u  -> u.id

-- The next id is one past the highest stored, not a blind row count: seed id 1, then a
-- second insert assigns 2. (Here it coincides with the count, but the store reads the
-- max — a later test could delete row 1 and still get 2.)
pub fn db sequentialId () -> Int =
    let r: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    match Repo.insert (User { id = 0, name = "ada" }) r
        Err _ -> 0 - 1
        Ok _  ->
            match r |> Repo.insertReturning (User { id = 0, name = "lin" })
                Err _ -> 0 - 2
                Ok u  -> u.id

-- A non-identity entity omits nothing: the caller-supplied weight is written as given and
-- reads back 42, proving the omit is scoped to the schema's generated columns.
pub fn db nonIdentityWeight () -> Int =
    let r: Repo Tag MemAdapter = Repo.repo (memAdapter ()) "tags"
    match Repo.insert (Tag { label = "x", weight = 42 }) r
        Err _ -> 0 - 1
        Ok _  ->
            match r |> Repo.getBy "label" (toSql "x")
                Err _       -> 0 - 2
                Ok None     -> 0 - 3
                Ok (Some t) -> t.weight
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"schema-insert-omit-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn identity_omit_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping identity_omit_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-schema-insert-omit-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-schema-insert-omit-e2e-cache-")
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
        "io:format(\"assignedIds=~s~n\",[{module}:assignedIds()]), \
         io:format(\"bulkIds=~s~n\",[{module}:bulkIds()]), \
         io:format(\"returnedId=~w~n\",[{module}:returnedId()]), \
         io:format(\"sequentialId=~w~n\",[{module}:sequentialId()]), \
         io:format(\"nonIdentityWeight=~w~n\",[{module}:nonIdentityWeight()]), \
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

    for (probe, why) in [
        (
            "assignedIds=1,2",
            "the insert drops the placeholder id and the store assigns 1 then 2",
        ),
        (
            "bulkIds=1,2,3",
            "a bulk insert assigns a contiguous run of ids across the batch",
        ),
        (
            "returnedId=1",
            "insertReturning hands the store-assigned id back from a fresh store",
        ),
        (
            "sequentialId=2",
            "the next id is one past the highest stored, not the placeholder",
        ),
        (
            "nonIdentityWeight=42",
            "a non-identity entity omits nothing, so the supplied value is written as given",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
