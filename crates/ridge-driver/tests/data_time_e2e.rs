//! End-to-end check for the `SqlType Time` codec on the in-memory adapter.
//!
//! A `Time` field rides a row: `deriving (Row, Schema)` accepts it, the insert path
//! encodes it through `SqlType.toSql` into the typed `SqlTime` value (the ISO
//! `HH:MM:SS` text), and the read path decodes it back with `fromSql`. This proves
//! the loop:
//! - a time of day round-trips through the stored `SqlTime` and reads back as the
//!   same time it went in as,
//! - the store orders a time column chronologically,
//! - a captured Time in a quoted predicate compiles to a bound parameter, so an
//!   `at == target` filter matches the one row, and
//! - `deriving (Schema)` reads the column type from `SqlType.dbType`, so the DDL
//!   names the column `time`.
//!
//! The exact Postgres `time` (OID 1083) decode is covered separately in
//! `data_pg_time_e2e` against a real database.
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

-- An entity with a `Time` column. `deriving (Schema)` marks `id` an identity
-- column, so the insert shape `EventInsert` carries only `label` and `at`.
pub type Event = { id: Int, label: Text, at: Time } deriving (Row, Schema)

fn optTimeText (o: Option Time) -> Text =
    match o
        None   -> "none"
        Some t -> Time.toIso t

-- Comma-join the labels of a row list, so an order is observable as one string.
fn joinLabels (es: List Event) -> Text =
    match es
        []        -> ""
        e :: []   -> e.label
        e :: rest -> Text.concat e.label (Text.concat "," (joinLabels rest))

-- Seed three rows whose times sort a,c,b chronologically:
-- a = 08:15:00 < c = 13:45:30 < b = 23:59:59.
pub fn db setup () -> Result (Repo Event MemAdapter) Error =
    let r: Repo Event MemAdapter = Repo.repo (memAdapter ()) "events"
    match Time.fromHms 8 15 0
        Err e -> Err e
        Ok t1 ->
            match Time.fromHms 23 59 59
                Err e -> Err e
                Ok t2 ->
                    match Time.fromHms 13 45 30
                        Err e -> Err e
                        Ok t3 ->
                            match Repo.insert (EventInsert { label = "a", at = t1 }) r
                                Err e -> Err e
                                Ok _  ->
                                    match Repo.insert (EventInsert { label = "b", at = t2 }) r
                                        Err e -> Err e
                                        Ok _  ->
                                            match Repo.insert (EventInsert { label = "c", at = t3 }) r
                                                Err e -> Err e
                                                Ok _  -> Ok r

-- value round-trip: the stored time reads back as the same ISO text. Row 2 is label
-- "b", time 23:59:59.
pub fn db roundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some e) -> Time.toIso e.at

-- ascending by the time column orders chronologically -> "a,c,b".
pub fn db ascOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (e: Event) -> e.at) |> Repo.toList
                Err _ -> "list-err"
                Ok es -> joinLabels es

-- descending reverses it -> "b,c,a".
pub fn db descOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (e: Event) -> e.at) |> Repo.toList
                Err _ -> "list-err"
                Ok es -> joinLabels es

-- a captured Time in a quoted predicate compiles to a bound parameter, so only the
-- 13:45:30 row (label "c") matches. Proves a time flows through the query DSL as a
-- SqlTime bind, not only through plain expressions.
pub fn db filterByTime () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match Time.fromHms 13 45 30
                Err _ -> "time-err"
                Ok target ->
                    match r |> Repo.query |> Repo.filter (fn (e: Event) -> e.at == target) |> Repo.toList
                        Err _ -> "list-err"
                        Ok es -> joinLabels es

-- a scalar MIN/MAX over the time column keeps the Time type and folds chronologically,
-- so the least is 08:15:00 and the greatest 23:59:59.
pub fn db minTime () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.minOf (fn (e: Event) -> e.at)
                Err _ -> "min-err"
                Ok o  -> optTimeText o

pub fn db maxTime () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.maxOf (fn (e: Event) -> e.at)
                Err _ -> "max-err"
                Ok o  -> optTimeText o

-- time-of-day arithmetic wraps at midnight deterministically (23:59:30 + 60s is
-- 00:00:30), while the current time of day is always read with its zone stated:
-- `nowUtc` and a fixed-offset `now` each yield an in-range hour. The wrapping pins the
-- add/carry plumbing without depending on the wall clock; the range checks smoke-test
-- the clock reads (a continuously-changing time of day has no stable cross-call value).
pub fn time todCheck () -> Text =
    match Time.fromHms 23 59 30
        Err _ -> "hms-err"
        Ok t ->
            let wrapped = Time.toIso (Time.addSeconds 60 t)
            let hu = Time.hour (Time.nowUtc ())
            let ho = Time.hour (Time.now (-180))
            let hOk = (hu >= 0) && (hu <= 23) && (ho >= 0) && (ho <= 23)
            if (wrapped == "00:00:30") && hOk then "ok" else "mismatch"

-- column-type dispatch: `deriving (Schema)` reads the `at` column type from
-- SqlType.dbType, so the DDL names it `time`.
fn eventWitness () -> Option Event = None

pub fn atDdl () -> Text = schemaToDdl (schemaOf (eventWitness ()))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-time-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn time_codec_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping time_codec_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-time-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-time-e2e-cache-")
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
         io:format(\"filterByTime=~s~n\",[{module}:filterByTime()]), \
         io:format(\"minTime=~s~n\",[{module}:minTime()]), \
         io:format(\"maxTime=~s~n\",[{module}:maxTime()]), \
         io:format(\"todCheck=~s~n\",[{module}:todCheck()]), \
         io:format(\"atDdl=~s~n\",[{module}:atDdl()]), \
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
            "roundTrip=23:59:59",
            "a Time round-trips through the stored SqlTime and reads back as the same time",
        ),
        (
            "ascOrder=a,c,b",
            "a time column sorts ascending chronologically (08:15:00 < 13:45:30 < 23:59:59)",
        ),
        (
            "descOrder=b,c,a",
            "the same column sorts descending by time",
        ),
        (
            "filterByTime=c",
            "a captured Time in a quoted predicate compiles to a bound parameter",
        ),
        (
            "minTime=08:15:00",
            "a scalar MIN over a time column folds chronologically and keeps the Time type",
        ),
        (
            "maxTime=23:59:59",
            "a scalar MAX over a time column folds chronologically and keeps the Time type",
        ),
        (
            "todCheck=ok",
            "time arithmetic wraps at midnight and the current time of day reads with its zone stated",
        ),
        (
            "\"at\" time",
            "deriving (Schema) dispatches the at column type through SqlType.dbType to time",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
