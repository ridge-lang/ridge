//! End-to-end check for the `SqlType Date` codec on the in-memory adapter.
//!
//! A `Date` field rides a row: `deriving (Row, Schema)` accepts it, the insert path
//! encodes it through `SqlType.toSql` into the typed `SqlDate` value (the ISO
//! `YYYY-MM-DD` text), and the read path decodes it back with `fromSql`. This proves
//! the loop:
//! - a calendar date round-trips through the stored `SqlDate` and reads back as the
//!   same date it went in as,
//! - the store orders a date column chronologically,
//! - a captured Date in a quoted predicate compiles to a bound parameter, so a
//!   `dueOn == target` filter matches the one row, and
//! - `deriving (Schema)` reads the column type from `SqlType.dbType`, so the DDL
//!   names the column `date`.
//!
//! The exact Postgres `date` (OID 1082) decode is covered separately in
//! `data_pg_date_e2e` against a real database.
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

-- An entity with a `Date` column. `deriving (Schema)` marks `id` an identity
-- column, so the insert shape `EventInsert` carries only `label` and `dueOn`.
pub type Event = { id: Int, label: Text, dueOn: Date } deriving (Row, Schema)

fn optDateText (o: Option Date) -> Text =
    match o
        None   -> "none"
        Some d -> Date.toIso d

-- Comma-join the labels of a row list, so an order is observable as one string.
fn joinLabels (es: List Event) -> Text =
    match es
        []        -> ""
        e :: []   -> e.label
        e :: rest -> Text.concat e.label (Text.concat "," (joinLabels rest))

-- Seed three rows whose dates sort a,c,b chronologically:
-- a = 2026-01-15 < c = 2026-07-04 < b = 2026-12-31.
pub fn db setup () -> Result (Repo Event MemAdapter) Error =
    let r: Repo Event MemAdapter = Repo.repo (memAdapter ()) "events"
    match Date.fromYmd 2026 1 15
        Err e -> Err e
        Ok d1 ->
            match Date.fromYmd 2026 12 31
                Err e -> Err e
                Ok d2 ->
                    match Date.fromYmd 2026 7 4
                        Err e -> Err e
                        Ok d3 ->
                            match Repo.insert (EventInsert { label = "a", dueOn = d1 }) r
                                Err e -> Err e
                                Ok _  ->
                                    match Repo.insert (EventInsert { label = "b", dueOn = d2 }) r
                                        Err e -> Err e
                                        Ok _  ->
                                            match Repo.insert (EventInsert { label = "c", dueOn = d3 }) r
                                                Err e -> Err e
                                                Ok _  -> Ok r

-- value round-trip: the stored date reads back as the same ISO text. Row 2 is label
-- "b", date 2026-12-31.
pub fn db roundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some e) -> Date.toIso e.dueOn

-- ascending by the date column orders chronologically -> "a,c,b".
pub fn db ascOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (e: Event) -> e.dueOn) |> Repo.toList
                Err _ -> "list-err"
                Ok es -> joinLabels es

-- descending reverses it -> "b,c,a".
pub fn db descOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (e: Event) -> e.dueOn) |> Repo.toList
                Err _ -> "list-err"
                Ok es -> joinLabels es

-- a captured Date in a quoted predicate compiles to a bound parameter, so only the
-- 2026-07-04 row (label "c") matches. Proves a date flows through the query DSL as a
-- SqlDate bind, not only through plain expressions.
pub fn db filterByDate () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match Date.fromYmd 2026 7 4
                Err _ -> "date-err"
                Ok target ->
                    match r |> Repo.query |> Repo.filter (fn (e: Event) -> e.dueOn == target) |> Repo.toList
                        Err _ -> "list-err"
                        Ok es -> joinLabels es

-- a scalar MIN/MAX over the date column keeps the Date type and folds chronologically,
-- so the least is 2026-01-15 and the greatest 2026-12-31. Reaching the aggregate at all
-- exercises the `SqlType Date` dictionary its `Aggregable` instance threads to decode
-- the fold.
pub fn db minDate () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.minOf (fn (e: Event) -> e.dueOn)
                Err _ -> "min-err"
                Ok o  -> optDateText o

pub fn db maxDate () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.maxOf (fn (e: Event) -> e.dueOn)
                Err _ -> "max-err"
                Ok o  -> optDateText o

-- today's-date zone handling is always explicit: `todayUtc` equals `today 0` (both
-- UTC), and a +/-24h (1440-minute) offset shifts the calendar date by exactly one
-- day whatever the current time of day, so these identities hold on any run. This
-- pins that the offset plumbing is right without depending on the wall clock.
pub fn time todayCheck () -> Text =
    let z = Date.today 0
    let a = Date.eq (Date.todayUtc ()) z
    let b = Date.eq (Date.today 1440) (Date.addDays 1 z)
    let c = Date.eq (Date.today (-1440)) (Date.addDays (-1) z)
    if a && b && c then "ok" else "mismatch"

-- column-type dispatch: `deriving (Schema)` reads the `due_on` column type from
-- SqlType.dbType, so the DDL names it `date`.
fn eventWitness () -> Option Event = None

pub fn dueOnDdl () -> Text = schemaToDdl (schemaOf (eventWitness ()))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-date-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"db\", \"time\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn date_codec_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping date_codec_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-date-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-date-e2e-cache-")
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
         io:format(\"filterByDate=~s~n\",[{module}:filterByDate()]), \
         io:format(\"minDate=~s~n\",[{module}:minDate()]), \
         io:format(\"maxDate=~s~n\",[{module}:maxDate()]), \
         io:format(\"todayCheck=~s~n\",[{module}:todayCheck()]), \
         io:format(\"dueOnDdl=~s~n\",[{module}:dueOnDdl()]), \
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
            "roundTrip=2026-12-31",
            "a Date round-trips through the stored SqlDate and reads back as the same date",
        ),
        (
            "ascOrder=a,c,b",
            "a date column sorts ascending chronologically (2026-01-15 < 2026-07-04 < 2026-12-31)",
        ),
        (
            "descOrder=b,c,a",
            "the same column sorts descending by date",
        ),
        (
            "filterByDate=c",
            "a captured Date in a quoted predicate compiles to a bound parameter",
        ),
        (
            "minDate=2026-01-15",
            "a scalar MIN over a date column folds chronologically and keeps the Date type",
        ),
        (
            "maxDate=2026-12-31",
            "a scalar MAX over a date column folds chronologically and keeps the Date type",
        ),
        (
            "todayCheck=ok",
            "today's date states its zone: todayUtc == today 0, and a +/-24h offset shifts the date by one day",
        ),
        (
            "date",
            "deriving (Schema) dispatches the due_on column type through SqlType.dbType to date",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
