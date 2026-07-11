//! End-to-end check for the `SqlType (List a)` codec on the in-memory adapter.
//!
//! A `List a` field rides a row as a native array column: `deriving (Row, Schema)`
//! accepts it, the insert path encodes it through the parametric `SqlType (List a)`
//! instance into the typed `SqlArray` value (a list of the elements' own `SqlValue`s),
//! and the read path decodes it back with `fromSql` into the `List a` it went in as.
//! This proves the loop for
//! - a `List Text` and a `List Int` column, whose elements round-trip through the
//!   stored `SqlArray` and read back in order,
//! - an empty list, which stores an empty array and reads back empty,
//! - an `Option (List Int)` column, which composes the nullable and array codecs — a
//!   present value rides the array codec and `None` reads back from SQL NULL, and
//! - `deriving (Schema)`, which reads each column type from `SqlType.dbType`, so the
//!   DDL names the columns `text[]` and `bigint[]`.
//!
//! The exact Postgres array (OID 1007/1009/…) decode is covered separately in
//! `data_pg_array_e2e` against a real database. Aggregates, ordering, and
//! captured-value predicates over an array column are out of scope for the codec (a
//! `List a` is not a scalar); the round-trip and column mapping are what this locks
//! down.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (toSql, SqlValue)
import std.schema (schemaOf, schemaToDdl)

-- An entity with two array columns and a nullable array column. `deriving (Schema)`
-- marks `id` an identity column, so the insert shape `PostInsert` carries `tags`,
-- `scores`, and `extra`.
pub type Post = { id: Int, tags: List Text, scores: List Int, extra: Option (List Int) } deriving (Row, Schema)

fn sumList (xs: List Int) -> Int =
    match xs
        []        -> 0
        x :: rest -> x + sumList rest

fn lenList (xs: List Int) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + lenList rest

-- Seed two rows: id 1 carries populated arrays and a present optional array; id 2
-- carries empty arrays and a NULL optional array. Two rows exercise both the
-- populated and the empty/absent shapes.
pub fn db setup () -> Result (Repo Post MemAdapter) Error =
    let r: Repo Post MemAdapter = Repo.repo (memAdapter ()) "posts"
    match Repo.insert (PostInsert { tags = ["x", "y", "z"], scores = [10, 20], extra = Some [7] }) r
        Err e -> Err e
        Ok _  ->
            match Repo.insert (PostInsert { tags = [], scores = [], extra = None }) r
                Err e -> Err e
                Ok _  -> Ok r

-- A List Text column round-trips: the tags read back in order and join to "x,y,z".
pub fn db tagsJoined () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some p) -> Text.join "," p.tags

-- A List Int column round-trips: the scores read back and sum to 30.
pub fn db scoresSum () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some p) -> Int.toText (sumList p.scores)

-- A present Option (List Int) column composes the nullable and array codecs: the
-- single element reads back through both.
pub fn db extraPresent () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some p) ->
                    match p.extra
                        None    -> "no-extra"
                        Some xs ->
                            match xs
                                []      -> "empty-extra"
                                v :: _  -> Int.toText v

-- An empty List Int column stores an empty array and reads back empty (length 0).
pub fn db emptyScores () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some p) -> if lenList p.scores == 0 then "empty" else "not-empty"

-- A NULL Option (List Int) column reads back as None.
pub fn db extraNull () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some p) ->
                    match p.extra
                        None   -> "none"
                        Some _ -> "some"

-- column-type dispatch: `deriving (Schema)` reads each array column type from
-- SqlType.dbType, so the DDL names them `text[]` and `bigint[]`.
fn postWitness () -> Option Post = None

pub fn postDdl () -> Text = schemaToDdl (schemaOf (postWitness ()))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-array-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn array_codec_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping array_codec_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-array-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-array-e2e-cache-")
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
        "io:format(\"tagsJoined=~s~n\",[{module}:tagsJoined()]), \
         io:format(\"scoresSum=~s~n\",[{module}:scoresSum()]), \
         io:format(\"extraPresent=~s~n\",[{module}:extraPresent()]), \
         io:format(\"emptyScores=~s~n\",[{module}:emptyScores()]), \
         io:format(\"extraNull=~s~n\",[{module}:extraNull()]), \
         io:format(\"postDdl=~s~n\",[{module}:postDdl()]), \
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

    // A List Text column round-trips in order.
    assert!(
        stdout.contains("tagsJoined=x,y,z"),
        "missing `tagsJoined=x,y,z` (a List Text column round-trips through the stored SqlArray)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A List Int column round-trips and folds.
    assert!(
        stdout.contains("scoresSum=30"),
        "missing `scoresSum=30` (a List Int column round-trips: 10 + 20)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A present Option (List Int) column composes the nullable and array codecs.
    assert!(
        stdout.contains("extraPresent=7"),
        "missing `extraPresent=7` (a present Option (List Int) reads back through both codecs)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // An empty list stores and reads back empty.
    assert!(
        stdout.contains("emptyScores=empty"),
        "missing `emptyScores=empty` (an empty List Int stores an empty array and reads back empty)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A NULL optional array reads back as None.
    assert!(
        stdout.contains("extraNull=none"),
        "missing `extraNull=none` (a NULL Option (List Int) reads back as None)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // deriving (Schema) dispatches each array column type through SqlType.dbType.
    assert!(
        stdout.contains("postDdl=") && stdout.contains("text[]") && stdout.contains("bigint[]"),
        "missing a `postDdl=` line naming the columns text[] and bigint[]\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
