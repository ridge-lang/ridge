//! End-to-end check for the `SqlType Uuid` codec on the in-memory adapter.
//!
//! A `Uuid` field rides a row: `deriving (Row, Schema)` accepts it, the insert
//! path encodes it through `SqlType.toSql` into the typed `SqlUuid` value (the
//! canonical text), and the read path decodes it back with `fromSql`. This proves
//! the loop:
//! - a uuid round-trips through the stored `SqlUuid` and reads back as the same
//!   canonical text it went in as,
//! - the store orders a uuid column by the 128-bit value (its canonical text),
//! - a captured uuid in a quoted predicate compiles to a bound parameter, so a
//!   `token == target` filter matches the one row, and
//! - `deriving (Schema)` reads the column type from `SqlType.dbType`, so the DDL
//!   names the column `uuid`.
//!
//! The exact Postgres `uuid` (OID 2950) decode is covered separately in
//! `data_pg_uuid_e2e` against a real database.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.query (SortOrder, Asc, Desc)
import std.sql (toSql, SqlValue)
import std.schema (schemaOf, schemaToDdl)

-- An entity with a `Uuid` column. `deriving (Schema)` marks `id` an identity
-- column, so the insert shape `DocInsert` carries only `label` and `token`.
pub type Doc = { id: Int, label: Text, token: Uuid } deriving (Row, Schema)

-- Parse or fall back to the nil uuid, so seeding is total.
fn uu (s: Text) -> Uuid =
    match Uuid.fromText s
        Ok u  -> u
        Err _ -> Uuid.nil ()

-- Comma-join the labels of a row list, so an order is observable as one string.
fn joinLabels (ds: List Doc) -> Text =
    match ds
        []        -> ""
        d :: []   -> d.label
        d :: rest -> Text.concat d.label (Text.concat "," (joinLabels rest))

-- Seed three rows whose uuids sort a,c,b by value: 1111… < 2222… < 3333….
pub fn db setup () -> Result (Repo Doc MemAdapter) Error =
    let r: Repo Doc MemAdapter = Repo.repo (memAdapter ()) "docs"
    match Repo.insert (DocInsert { label = "a", token = uu "11111111-1111-1111-1111-111111111111" }) r
        Err e -> Err e
        Ok _  ->
            match Repo.insert (DocInsert { label = "b", token = uu "33333333-3333-3333-3333-333333333333" }) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insert (DocInsert { label = "c", token = uu "22222222-2222-2222-2222-222222222222" }) r
                        Err e -> Err e
                        Ok _  -> Ok r

-- value round-trip: the stored uuid reads back as the same canonical text. Row 2
-- is label "b", token 3333….
pub fn db roundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some d) -> Uuid.toText d.token

-- ascending by the uuid column orders by value: 1111 < 2222 < 3333 -> "a,c,b".
pub fn db ascOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (d: Doc) -> d.token) |> Repo.toList
                Err _ -> "list-err"
                Ok ds -> joinLabels ds

-- descending reverses it -> "b,c,a".
pub fn db descOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (d: Doc) -> d.token) |> Repo.toList
                Err _ -> "list-err"
                Ok ds -> joinLabels ds

-- a captured uuid in a quoted predicate compiles to a bound parameter, so only the
-- 2222 row (label "c") matches. Proves a uuid value flows through the query DSL as
-- a SqlUuid bind, not only through plain expressions.
pub fn db filterByToken () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            let target = uu "22222222-2222-2222-2222-222222222222"
            match r |> Repo.query |> Repo.filter (fn (d: Doc) -> d.token == target) |> Repo.toList
                Err _ -> "list-err"
                Ok ds -> joinLabels ds

-- column-type dispatch: `deriving (Schema)` reads the `token` column type from
-- SqlType.dbType, so the DDL names it `uuid`.
fn docWitness () -> Option Doc = None

pub fn tokenDdl () -> Text = schemaToDdl (schemaOf (docWitness ()))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-uuid-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn uuid_codec_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping uuid_codec_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-uuid-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-uuid-e2e-cache-")
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
         io:format(\"ascOrder=~s~n\",[{module}:ascOrder()]), \
         io:format(\"descOrder=~s~n\",[{module}:descOrder()]), \
         io:format(\"filterByToken=~s~n\",[{module}:filterByToken()]), \
         io:format(\"tokenDdl=~s~n\",[{module}:tokenDdl()]), \
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
            "roundTrip=33333333-3333-3333-3333-333333333333",
            "a Uuid round-trips through the stored SqlUuid and reads back as the same text",
        ),
        (
            "ascOrder=a,c,b",
            "a uuid column sorts ascending by the 128-bit value (1111 < 2222 < 3333)",
        ),
        (
            "descOrder=b,c,a",
            "the same column sorts descending by value",
        ),
        (
            "filterByToken=c",
            "a captured uuid in a quoted predicate compiles to a bound parameter",
        ),
        (
            "uuid",
            "deriving (Schema) dispatches the token column type through SqlType.dbType to uuid",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
