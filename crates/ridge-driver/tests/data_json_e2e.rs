//! End-to-end check for the `SqlType JsonValue` codec on the in-memory adapter.
//!
//! A `JsonValue` field rides a row: `deriving (Row, Schema)` accepts it, the insert
//! path encodes it through `SqlType.toSql` into the typed `SqlJson` value (the encoded
//! JSON text), and the read path decodes it back with `fromSql` into the structured
//! `JsonValue` ADT. This proves the loop:
//! - a nested JSON value round-trips through the stored `SqlJson` and reads back as the
//!   same structure it went in as (accessible through the `std.json` accessors), and
//! - `deriving (Schema)` reads the column type from `SqlType.dbType`, so the DDL names
//!   the column `jsonb`.
//!
//! The exact Postgres `json`/`jsonb` (OID 114/3802) decode is covered separately in
//! `data_pg_json_e2e` against a real database. Aggregates, ordering, and captured-value
//! predicates over a JSON column are out of scope for the codec (JSON has no natural
//! order); the round-trip and column mapping are what this locks down.
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
import std.json as Json

-- An entity with a `JsonValue` column. `deriving (Schema)` marks `id` an identity
-- column, so the insert shape `DocInsert` carries only `label` and `body`.
pub type Doc = { id: Int, label: Text, body: JsonValue } deriving (Row, Schema)

-- A small nested JSON value: [<n>, "z"]. A list keeps the encoded order deterministic,
-- so the round-trip is exact without depending on map key order.
fn doc (n: Int) -> JsonValue = Json.jList [Json.jInt n, Json.jText "z"]

pub fn db setup () -> Result (Repo Doc MemAdapter) Error =
    let r: Repo Doc MemAdapter = Repo.repo (memAdapter ()) "docs"
    match Repo.insert (DocInsert { label = "a", body = doc 42 }) r
        Err e -> Err e
        Ok _  -> Ok r

-- The first element of a JSON list read back out as an Int, or a marker for each way
-- the structure could fail to survive the round-trip.
fn firstInt (v: JsonValue) -> Text =
    match Json.asList v
        None    -> "not-list"
        Some xs ->
            match xs
                []       -> "empty"
                x :: _   ->
                    match Json.asInt x
                        None   -> "not-int"
                        Some n -> Int.toText n

-- structured round-trip: the stored JsonValue reads back and its nested Int is
-- recoverable through the accessors — proving the ADT survived, not just its text.
pub fn db roundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some d) -> firstInt d.body

-- text round-trip: the value re-encodes to JSON text carrying the same data.
pub fn db encoded () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some d) -> Json.encode d.body

-- column-type dispatch: `deriving (Schema)` reads the `body` column type from
-- SqlType.dbType, so the DDL names it `jsonb`.
fn docWitness () -> Option Doc = None

pub fn bodyDdl () -> Text = schemaToDdl (schemaOf (docWitness ()))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-json-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn json_codec_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping json_codec_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-json-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-json-e2e-cache-")
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
        "io:format(\"roundTrip=~s~n\",[{module}:roundTrip()]), \
         io:format(\"encoded=~s~n\",[{module}:encoded()]), \
         io:format(\"bodyDdl=~s~n\",[{module}:bodyDdl()]), \
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

    // The nested Int survives the round-trip through the stored SqlJson and is
    // recovered through the accessors, so the structured value came back intact.
    assert!(
        stdout.contains("roundTrip=42"),
        "missing `roundTrip=42` (a JsonValue round-trips through the stored SqlJson and reads back as the same structure)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The re-encoded text carries the same data (42 and the "z" element).
    assert!(
        stdout.contains("encoded=") && stdout.contains("42") && stdout.contains('z'),
        "missing an `encoded=` line carrying 42 and z (the value re-encodes to JSON text)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // deriving (Schema) dispatches the body column type through SqlType.dbType to jsonb.
    assert!(
        stdout.contains("bodyDdl=") && stdout.contains("jsonb"),
        "missing a `bodyDdl=` line naming the column jsonb\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
