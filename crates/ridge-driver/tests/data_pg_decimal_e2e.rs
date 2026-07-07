//! End-to-end check that a Postgres `numeric` column round-trips a `Decimal`
//! exactly, against a real database.
//!
//! Postgres delivers `numeric` (type OID 1700) as decimal text over the wire. The
//! adapter now decodes it into the typed `SqlDecimal` value verbatim instead of
//! narrowing it through a float, and encodes a bound `SqlDecimal` back as its
//! canonical text. So a value with more significant digits than a double can hold,
//! and a fraction a binary float cannot represent, both survive the insert/select
//! loop digit for digit. This is the decode path the in-memory adapter cannot
//! exercise (it has no OID wire form); `data_decimal_e2e` covers the codec logic
//! on the in-memory store.
//!
//! The program creates its own `ridge_pg_decimals` table with `Raw.exec`, so no
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

-- A `serial` id lets `deriving (Schema)` omit it from the insert shape (it is a
-- database-generated column), so each insert carries only the amount and Postgres
-- assigns the id 1, 2, … in order.
pub type Amount = { id: Int, amount: Decimal } deriving (Row, Schema)

fn dec (s: Text) -> Decimal =
    match Decimal.fromText s
        Ok d  -> d
        Err _ -> Decimal.fromInt 0

fn pgConfig () -> Config =
    Config { host = "__PG_HOST__", port = __PG_PORT__, database = "__PG_DATABASE__", user = "__PG_USER__", password = "__PG_PASSWORD__", sslMode = "__PG_SSLMODE__" }

-- Create a fresh numeric table and seed two decimals: one with more significant
-- digits than a double holds, one whose fraction a binary float cannot represent.
pub fn db setup () -> Result (Repo Amount Postgres) Error =
    match connect (pgConfig ())
        Err e   -> Err e
        Ok conn ->
            let r: Repo Amount Postgres = Repo.repo conn "ridge_pg_decimals"
            match Raw.exec conn "DROP TABLE IF EXISTS ridge_pg_decimals" []
                Err e -> Err e
                Ok _  ->
                    match Raw.exec conn "CREATE TABLE ridge_pg_decimals (id serial, amount numeric)" []
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insert (AmountInsert { amount = dec "12345678901234567.89" }) r
                                Err e -> Err e
                                Ok _  ->
                                    match Repo.insert (AmountInsert { amount = dec "0.10" }) r
                                        Err e -> Err e
                                        Ok _  -> Ok r

-- read back the 19-significant-digit value: a float decode would corrupt the last
-- digits; the exact SqlDecimal decode returns it verbatim.
pub fn db exactBig () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some a) -> Decimal.toText a.amount

-- read back a value whose fraction a float cannot hold exactly; the scale
-- survives the numeric round-trip.
pub fn db exactFrac () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some a) -> Decimal.toText a.amount
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
        "[workspace]\nname = \"data-pg-decimal-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn postgres_numeric_round_trips_a_decimal() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping postgres_numeric_round_trips_a_decimal");
        return;
    }
    let url = match std::env::var("RIDGE_TEST_PG_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("RIDGE_TEST_PG_URL not set — skipping postgres_numeric_round_trips_a_decimal");
            return;
        }
    };
    let parts =
        parse_pg_url(&url).unwrap_or_else(|| panic!("RIDGE_TEST_PG_URL is not a postgres:// URL: {url}"));

    let dir = tempfile::Builder::new()
        .prefix("ridge-pg-decimal-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-pg-decimal-e2e-cache-")
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
        "io:format(\"exactBig=~s~n\",[{module}:exactBig()]), \
         io:format(\"exactFrac=~s~n\",[{module}:exactFrac()]), \
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
            "exactBig=12345678901234567.89",
            "a 19-significant-digit numeric decodes exactly, not narrowed through a float",
        ),
        (
            "exactFrac=0.10",
            "a numeric fraction a float cannot represent round-trips with its scale intact",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
