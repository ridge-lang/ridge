//! End-to-end check that a Postgres `jsonb` column round-trips a `JsonValue`,
//! against a real database.
//!
//! Postgres delivers `json` (type OID 114) and `jsonb` (3802) as JSON text over the
//! wire. The adapter decodes it into the typed `SqlJson` value instead of letting it
//! fall through to `SqlText`, and the codec parses that back into the structured
//! `JsonValue` ADT; a bound `SqlJson` is sent as JSON text the database parses (and,
//! for jsonb, normalises). So a JSON document survives the insert/select loop as the
//! structured value it went in as. This is the decode path the in-memory adapter
//! cannot exercise (it has no OID wire form); `data_json_e2e` covers the codec logic
//! on the in-memory store.
//!
//! The program creates its own `ridge_pg_json` table with `Raw.exec`, so no
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
import std.data (connect, PostgresConfig, Postgres)
import std.repo as Repo
import std.raw as Raw
import std.sql (toSql, SqlValue)
import std.json as Json

-- A `serial` id lets `deriving (Schema)` omit it from the insert shape, so the
-- insert carries only the doc and Postgres assigns the id 1.
pub type Item = { id: Int, doc: JsonValue } deriving (Row, Schema)

fn pgConfig () -> PostgresConfig =
    PostgresConfig { host = "__PG_HOST__", port = __PG_PORT__, database = "__PG_DATABASE__", user = "__PG_USER__", password = "__PG_PASSWORD__", sslMode = "__PG_SSLMODE__" }

-- The first element of a JSON list read back out as an Int, or a marker for each way
-- the structure could fail to survive the round-trip.
fn firstInt (v: JsonValue) -> Text =
    match Json.asList v
        None    -> "not-list"
        Some xs ->
            match xs
                []     -> "empty"
                x :: _ ->
                    match Json.asInt x
                        None   -> "not-int"
                        Some n -> Int.toText n

-- Create a fresh jsonb table and seed one nested JSON value.
pub fn db setup () -> Result (Repo Item Postgres) Error =
    match connect (pgConfig ())
        Err e   -> Err e
        Ok conn ->
            let r: Repo Item Postgres = Repo.repo conn "ridge_pg_json"
            match Raw.exec conn "DROP TABLE IF EXISTS ridge_pg_json" []
                Err e -> Err e
                Ok _  ->
                    match Raw.exec conn "CREATE TABLE ridge_pg_json (id serial, doc jsonb)" []
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insert (ItemInsert { doc = Json.jList [Json.jInt 7, Json.jText "q"] }) r
                                Err e -> Err e
                                Ok _  -> Ok r

-- read the doc back: a `SqlText` decode would leave it as a string, but the typed
-- `SqlJson` decode parses jsonb into the structured value, so the nested Int is
-- recoverable through the accessors after the database's own normalisation.
pub fn db roundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some i) -> firstInt i.doc
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
        "[workspace]\nname = \"data-pg-json-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn postgres_jsonb_round_trips_a_json_value() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping postgres_jsonb_round_trips_a_json_value");
        return;
    }
    let url = match std::env::var("RIDGE_TEST_PG_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!(
                "RIDGE_TEST_PG_URL not set — skipping postgres_jsonb_round_trips_a_json_value"
            );
            return;
        }
    };
    let parts = parse_pg_url(&url)
        .unwrap_or_else(|| panic!("RIDGE_TEST_PG_URL is not a postgres:// URL: {url}"));

    let dir = tempfile::Builder::new()
        .prefix("ridge-pg-json-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-pg-json-e2e-cache-")
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

    let expr = format!("io:format(\"roundTrip=~s~n\",[{module}:roundTrip()]), halt().");
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

    assert!(
        stdout.contains("roundTrip=7"),
        "missing `roundTrip=7` (a jsonb column (OID 3802) decodes to the typed SqlJson and the nested Int survives the round-trip through the database)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
