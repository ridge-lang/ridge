//! End-to-end check for the `SqlType Duration` codec on the in-memory adapter.
//!
//! A `Duration` field rides a row: `deriving (Row, Schema)` accepts it, the insert
//! path encodes it through `SqlType.toSql` into the typed `SqlInterval` value (the
//! whole-millisecond span), and the read path decodes it back with `fromSql`. This
//! proves the loop:
//! - a duration round-trips through the stored `SqlInterval` and reads back as the
//!   same span it went in as,
//! - the store orders an interval column by length,
//! - a scalar `min`/`max` folds by length and keeps the `Duration` type, and
//! - `deriving (Schema)` reads the column type from `SqlType.dbType`, so the DDL
//!   names the column `interval`.
//!
//! Ordering and `min`/`max` fold by the integer millisecond span the carrier holds,
//! so no text-sort approximation is involved. `sum`/`avg` over a `Duration` column and
//! a captured `Duration` in a quoted predicate are intentionally out of scope here —
//! both are rejected at type-check — since a `Duration` is not a quote scalar and its
//! aggregate fold is deferred.
//!
//! The exact Postgres `interval` (OID 1186) decode is covered separately in
//! `data_pg_interval_e2e` against a real database, and the text parser in
//! `pg_interval_decode_e2e` without one.
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
import std.time (ofMillis)

-- An entity with a `Duration` column. `deriving (Schema)` marks `id` an identity
-- column, so the insert shape `TaskInsert` carries only `label` and `took`.
pub type Task = { id: Int, label: Text, took: Duration } deriving (Row, Schema)

fn durText (d: Duration) -> Text = Int.toText d.ms

fn optMs (o: Option Duration) -> Text =
    match o
        None   -> "none"
        Some d -> durText d

-- Comma-join the labels of a row list, so an order is observable as one string.
fn joinLabels (ts: List Task) -> Text =
    match ts
        []        -> ""
        t :: []   -> t.label
        t :: rest -> Text.concat t.label (Text.concat "," (joinLabels rest))

-- Seed three rows whose durations sort a,c,b by length:
-- a = 500ms < c = 1500ms < b = 90000ms.
pub fn db setup () -> Result (Repo Task MemAdapter) Error =
    let r: Repo Task MemAdapter = Repo.repo (memAdapter ()) "tasks"
    match Repo.insert (TaskInsert { label = "a", took = ofMillis 500 }) r
        Err e -> Err e
        Ok _  ->
            match Repo.insert (TaskInsert { label = "b", took = ofMillis 90000 }) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insert (TaskInsert { label = "c", took = ofMillis 1500 }) r
                        Err e -> Err e
                        Ok _  -> Ok r

-- value round-trip: the stored duration reads back as the same span. Row 2 is label
-- "b", 90000 ms.
pub fn db roundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some t) -> durText t.took

-- ascending by the interval column orders by length -> "a,c,b".
pub fn db ascOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (t: Task) -> t.took) |> Repo.toList
                Err _ -> "list-err"
                Ok ts -> joinLabels ts

-- descending reverses it -> "b,c,a".
pub fn db descOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (t: Task) -> t.took) |> Repo.toList
                Err _ -> "list-err"
                Ok ts -> joinLabels ts

-- a scalar MIN/MAX over the interval column keeps the Duration type and folds by
-- length, so the least is 500 ms and the greatest 90000 ms. Reaching the aggregate at
-- all exercises the `SqlType Duration` dictionary its `Aggregable` instance threads to
-- decode the fold.
pub fn db minTook () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.minOf (fn (t: Task) -> t.took)
                Err _ -> "min-err"
                Ok o  -> optMs o

pub fn db maxTook () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.maxOf (fn (t: Task) -> t.took)
                Err _ -> "max-err"
                Ok o  -> optMs o

-- a scalar SUM over the interval column keeps the Duration type and folds to a total
-- span: 500 + 90000 + 1500 = 92000 ms. Reaching it exercises the `SqlType Duration`
-- dictionary its `Aggregable` instance threads to decode the folded interval.
pub fn db sumTook () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.sumOf (fn (t: Task) -> t.took)
                Err _ -> "sum-err"
                Ok o  -> optMs o

-- a captured Duration in a quoted predicate compiles to a bound parameter, so only
-- the 1500 ms row (label "c") matches. Proves a Duration flows through the query DSL
-- as a SqlInterval bind, not only through plain expressions.
pub fn db filterByTook () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            let target = ofMillis 1500
            match r |> Repo.query |> Repo.filter (fn (t: Task) -> t.took == target) |> Repo.toList
                Err _ -> "list-err"
                Ok ts -> joinLabels ts

-- column-type dispatch: `deriving (Schema)` reads the `took` column type from
-- SqlType.dbType, so the DDL names it `interval`.
fn taskWitness () -> Option Task = None

pub fn tookDdl () -> Text = schemaToDdl (schemaOf (taskWitness ()))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-interval-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn interval_codec_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping interval_codec_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-interval-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-interval-e2e-cache-")
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
         io:format(\"minTook=~s~n\",[{module}:minTook()]), \
         io:format(\"maxTook=~s~n\",[{module}:maxTook()]), \
         io:format(\"sumTook=~s~n\",[{module}:sumTook()]), \
         io:format(\"filterByTook=~s~n\",[{module}:filterByTook()]), \
         io:format(\"tookDdl=~s~n\",[{module}:tookDdl()]), \
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
            "roundTrip=90000",
            "a Duration round-trips through the stored SqlInterval and reads back as the same span",
        ),
        (
            "ascOrder=a,c,b",
            "an interval column sorts ascending by length (500 < 1500 < 90000 ms)",
        ),
        (
            "descOrder=b,c,a",
            "the same column sorts descending by length",
        ),
        (
            "minTook=500",
            "a scalar MIN over an interval column folds by length and keeps the Duration type",
        ),
        (
            "maxTook=90000",
            "a scalar MAX over an interval column folds by length and keeps the Duration type",
        ),
        (
            "sumTook=92000",
            "a scalar SUM over an interval column folds to a total and keeps the Duration type",
        ),
        (
            "filterByTook=c",
            "a captured Duration in a quoted predicate compiles to a bound SqlInterval parameter",
        ),
        (
            "interval",
            "deriving (Schema) dispatches the took column type through SqlType.dbType to interval",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
