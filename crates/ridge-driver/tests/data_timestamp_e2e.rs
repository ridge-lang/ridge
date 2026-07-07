//! End-to-end check for the `SqlType Timestamp` codec on the in-memory adapter.
//!
//! A `Timestamp` field now rides a row: `deriving (Row, Schema)` accepts it, the
//! insert path encodes it through `SqlType.toSql` into the typed `SqlInstant` value
//! (epoch microseconds), and the read path decodes it back with `fromSql`. This
//! proves the whole loop:
//! - a wall-clock value round-trips through a stored `SqlInstant` and formats back
//!   to the exact ISO-8601 string it went in as, and
//! - the store orders rows by a timestamp column, so a `SqlInstant` sorts by its
//!   epoch value in both directions.
//!
//! Column-type dispatch (a timestamp field mapping to `DbTimestampTz`) is proved
//! separately by the schema descriptor tests; here the focus is the value codec.
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
import std.time as Time

-- An entity with a `Timestamp` column. `deriving (Schema)` marks `id` an identity
-- column, so the insert shape `EventInsert` carries only `name` and `at`.
pub type Event = { id: Int, name: Text, at: Timestamp } deriving (Row, Schema)

-- A fixed instant from an ISO string, so every probe is deterministic. A parse
-- failure would collapse to the epoch, which the assertions would catch.
fn instant (iso: Text) -> Timestamp =
    match Time.fromIso iso
        Ok t  -> t
        Err _ -> Time.epoch ()

-- Comma-join the names of an event list, so an order is observable as one string.
fn joinNames (es: List Event) -> Text =
    match es
        []        -> ""
        e :: []   -> e.name
        e :: rest -> Text.concat e.name (Text.concat "," (joinNames rest))

-- Seed two events out of chronological order (the 2026 row is inserted last), so a
-- sort has something to reorder. Each probe seeds its own isolated store.
pub fn db setup () -> Result (Repo Event MemAdapter) Error =
    let r: Repo Event MemAdapter = Repo.repo (memAdapter ()) "events"
    match Repo.insert (EventInsert { name = "old", at = instant "2020-01-01T00:00:00Z" }) r
        Err e -> Err e
        Ok _  ->
            match Repo.insert (EventInsert { name = "new", at = instant "2026-07-06T18:09:05Z" }) r
                Err e -> Err e
                Ok _  -> Ok r

-- value round-trip: the stored instant reads back and formats to the same ISO
-- string it went in as, at microsecond precision. Proves toSql/fromSql.
pub fn db roundTripIso () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some e) -> Time.iso e.at

-- ascending by the timestamp column: the 2020 row precedes the 2026 one ->
-- "old,new". Proves a SqlInstant sorts by its epoch value.
pub fn db ascOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (e: Event) -> e.at) |> Repo.toList
                Err _ -> "list-err"
                Ok es -> joinNames es

-- descending by the same column reverses the order -> "new,old".
pub fn db descOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (e: Event) -> e.at) |> Repo.toList
                Err _ -> "list-err"
                Ok es -> joinNames es
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-timestamp-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn timestamp_codec_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping timestamp_codec_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-timestamp-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-timestamp-e2e-cache-")
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
        "io:format(\"roundTripIso=~s~n\",[{module}:roundTripIso()]), \
         io:format(\"ascOrder=~s~n\",[{module}:ascOrder()]), \
         io:format(\"descOrder=~s~n\",[{module}:descOrder()]), \
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
            "roundTripIso=2026-07-06T18:09:05.000000Z",
            "a Timestamp round-trips through the stored SqlInstant and formats back to its ISO string",
        ),
        (
            "ascOrder=old,new",
            "a timestamp column sorts ascending by its epoch value",
        ),
        (
            "descOrder=new,old",
            "the same column sorts descending",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
