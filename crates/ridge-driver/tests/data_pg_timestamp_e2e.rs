//! End-to-end check that a Postgres `timestamptz`/`timestamp` column round-trips a
//! `Timestamp`, against a real database.
//!
//! Postgres delivers `timestamptz` (type OID 1184) and `timestamp` (OID 1114) as
//! ISO text over the wire. The adapter now decodes both into the typed
//! `SqlInstant` value (epoch microseconds) instead of leaving them as text, so a
//! `Timestamp` field reads back through `fromSql` rather than failing to decode.
//! `timestamptz` carries a zone offset the decoder normalises to UTC; a plain
//! `timestamp` has none and is read as UTC. Both columns are seeded from the same
//! instant and must format back to the exact ISO string it went in as. This is the
//! decode path the in-memory adapter cannot exercise (it has no OID wire form);
//! `data_timestamp_e2e` covers the codec logic on the in-memory store, and
//! `pg_timestamp_decode_e2e` pins the text parser without a database.
//!
//! The program creates its own `ridge_pg_instants` table with `Raw.exec`, so no
//! CI-provisioned table is needed. Gated three ways like `data_pg_e2e`: the
//! `beam-runtime` feature, a `which` guard for `erl`/`erlc`, and the
//! `RIDGE_TEST_PG_URL` environment variable. Without a reachable database the test
//! skips rather than fails.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// The program source, with connection settings spliced in as sentinels so the
/// Ridge record braces never collide with Rust string formatting.
const SOURCE_TEMPLATE: &str = r#"
import std.data (connect, Config, Postgres)
import std.repo as Repo
import std.raw as Raw
import std.sql (toSql, SqlValue)
import std.time as Time

-- A `serial` id lets `deriving (Schema)` omit it from the insert shape (it is a
-- database-generated column), so each insert carries only the two instants and
-- Postgres assigns the id. `tz` maps to a `timestamptz` column, `naive` to a plain
-- `timestamp`, so a single seed exercises both decode paths.
pub type Event = { id: Int, tz: Timestamp, naive: Timestamp } deriving (Row, Schema)

-- A fixed instant from an ISO string, so the probe is deterministic. A parse
-- failure would collapse to the epoch, which the assertions would catch.
fn instant (iso: Text) -> Timestamp =
    match Time.fromIso iso
        Ok t  -> t
        Err _ -> Time.epoch ()

fn pgConfig () -> Config =
    Config { host = "__PG_HOST__", port = __PG_PORT__, database = "__PG_DATABASE__", user = "__PG_USER__", password = "__PG_PASSWORD__", sslMode = "__PG_SSLMODE__" }

-- Create a fresh table with one timezone-aware and one naive timestamp column,
-- then seed both from the same UTC instant.
pub fn db setup () -> Result (Repo Event Postgres) Error =
    match connect (pgConfig ())
        Err e   -> Err e
        Ok conn ->
            let r: Repo Event Postgres = Repo.repo conn "ridge_pg_instants"
            match Raw.exec conn "DROP TABLE IF EXISTS ridge_pg_instants" []
                Err e -> Err e
                Ok _  ->
                    match Raw.exec conn "CREATE TABLE ridge_pg_instants (id serial, tz timestamptz, naive timestamp)" []
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insert (EventInsert { tz = instant "2026-07-06T18:09:05Z", naive = instant "2026-07-06T18:09:05Z" }) r
                                Err e -> Err e
                                Ok _  -> Ok r

-- read back the timestamptz column: it arrives with a zone offset the decoder
-- normalises to UTC, and formats back to the ISO string it went in as.
pub fn db tzRoundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some e) -> Time.toIso e.tz

-- read back the plain timestamp column: it arrives without an offset and is read
-- as UTC, so it formats back to the same ISO string.
pub fn db naiveRoundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some e) -> Time.toIso e.naive
"#;

struct PgParts<'a> {
    host: &'a str,
    port: u16,
    user: &'a str,
    password: &'a str,
    database: &'a str,
    sslmode: &'a str,
}

fn parse_pg_url(url: &str) -> Option<PgParts<'_>> {
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))?;
    let (main, query) = match rest.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (rest, None),
    };
    let (userinfo, host_port_db) = main.split_once('@')?;
    let (user, password) = match userinfo.split_once(':') {
        Some((u, p)) => (u, p),
        None => (userinfo, ""),
    };
    let (host_port, database) = host_port_db.split_once('/')?;
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (host_port, 5432u16),
    };
    let sslmode = query
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("sslmode=")))
        .unwrap_or("disable");
    Some(PgParts {
        host,
        port,
        user,
        password,
        database,
        sslmode,
    })
}

fn render_source(parts: &PgParts) -> String {
    SOURCE_TEMPLATE
        .replace("__PG_HOST__", parts.host)
        .replace("__PG_PORT__", &parts.port.to_string())
        .replace("__PG_DATABASE__", parts.database)
        .replace("__PG_USER__", parts.user)
        .replace("__PG_PASSWORD__", parts.password)
        .replace("__PG_SSLMODE__", parts.sslmode)
}

fn write_workspace(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-pg-timestamp-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"db\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

#[test]
fn postgres_timestamp_round_trips_an_instant() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping postgres_timestamp_round_trips_an_instant");
        return;
    }
    let url = match std::env::var("RIDGE_TEST_PG_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!(
                "RIDGE_TEST_PG_URL not set — skipping postgres_timestamp_round_trips_an_instant"
            );
            return;
        }
    };
    let parts = parse_pg_url(&url)
        .unwrap_or_else(|| panic!("RIDGE_TEST_PG_URL is not a postgres:// URL: {url}"));

    let dir = tempfile::Builder::new()
        .prefix("ridge-pg-timestamp-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-pg-timestamp-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), &render_source(&parts));

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

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
        "io:format(\"tzRoundTrip=~s~n\",[{module}:tzRoundTrip()]), \
         io:format(\"naiveRoundTrip=~s~n\",[{module}:naiveRoundTrip()]), \
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
            "tzRoundTrip=2026-07-06T18:09:05.000000Z",
            "a timestamptz column decodes to SqlInstant and formats back to its ISO string",
        ),
        (
            "naiveRoundTrip=2026-07-06T18:09:05.000000Z",
            "a naive timestamp column decodes to the same UTC instant",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
