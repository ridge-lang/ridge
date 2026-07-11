//! End-to-end check that Postgres array columns round-trip a `List a`, against a
//! real database.
//!
//! Postgres delivers an array (type OID 1007 for `int[]`, 1009 for `text[]`, …) as its
//! array output text `{a,b,c}` over the wire. The adapter decodes that into the typed
//! `SqlArray` value — splitting the elements and decoding each through its own element
//! OID — instead of letting it fall through to `SqlText`, and the codec rebuilds the
//! `List a`; a bound `SqlArray` is sent as the array literal `{…}`, which Postgres parses
//! back to the same array. So a list survives the insert/select loop as the value it went
//! in as. An element that contains the array delimiter (a comma) is double-quoted on the
//! way out and un-quoted on the way back, which this exercises with a `p,q` element. An
//! empty list round-trips as an empty array. This is the decode path the in-memory
//! adapter cannot exercise (it has no OID wire form); `data_array_e2e` covers the codec
//! logic on the in-memory store.
//!
//! The program creates its own `ridge_pg_array` table with `Raw.exec`, so no
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

-- A `serial` id lets `deriving (Schema)` omit it from the insert shape, so the insert
-- carries the two array columns and Postgres assigns the id.
pub type Item = { id: Int, tags: List Text, nums: List Int } deriving (Row, Schema)

fn sumList (xs: List Int) -> Int =
    match xs
        []        -> 0
        x :: rest -> x + sumList rest

fn lenList (xs: List Int) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + lenList rest

fn pgConfig () -> Config =
    Config { host = "__PG_HOST__", port = __PG_PORT__, database = "__PG_DATABASE__", user = "__PG_USER__", password = "__PG_PASSWORD__", sslMode = "__PG_SSLMODE__" }

-- Create a fresh array table and seed two rows: id 1 with populated arrays, id 2 with a
-- comma-carrying text element and an empty int array.
pub fn db setup () -> Result (Repo Item Postgres) Error =
    match connect (pgConfig ())
        Err e   -> Err e
        Ok conn ->
            let r: Repo Item Postgres = Repo.repo conn "ridge_pg_array"
            match Raw.exec conn "DROP TABLE IF EXISTS ridge_pg_array" []
                Err e -> Err e
                Ok _  ->
                    match Raw.exec conn "CREATE TABLE ridge_pg_array (id serial, tags text[], nums int[])" []
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insert (ItemInsert { tags = ["a", "b"], nums = [1, 2, 3] }) r
                                Err e -> Err e
                                Ok _  ->
                                    match Repo.insert (ItemInsert { tags = ["p,q", "r"], nums = [] }) r
                                        Err e -> Err e
                                        Ok _  -> Ok r

-- a text[] column decodes through the typed SqlArray: the elements read back in order
-- and join to "a,b".
pub fn db tagsRoundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some i) -> Text.join "," i.tags

-- an int[] column decodes through the typed SqlArray: the elements read back and sum
-- to 6.
pub fn db numsSum () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some i) -> Int.toText (sumList i.nums)

-- a text element carrying the array delimiter round-trips: `p,q` is quoted on the way
-- out and un-quoted on the way back, so joining by "|" reads back "p,q|r".
pub fn db specialTags () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some i) -> Text.join "|" i.tags

-- an empty int[] round-trips as an empty list (length 0).
pub fn db emptyNums () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some i) -> if lenList i.nums == 0 then "empty" else "not-empty"
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
        "[workspace]\nname = \"data-pg-array-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn postgres_array_round_trips_a_list() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping postgres_array_round_trips_a_list");
        return;
    }
    let url = match std::env::var("RIDGE_TEST_PG_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("RIDGE_TEST_PG_URL not set — skipping postgres_array_round_trips_a_list");
            return;
        }
    };
    let parts = parse_pg_url(&url)
        .unwrap_or_else(|| panic!("RIDGE_TEST_PG_URL is not a postgres:// URL: {url}"));

    let dir = tempfile::Builder::new()
        .prefix("ridge-pg-array-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-pg-array-e2e-cache-")
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
        "io:format(\"tagsRoundTrip=~s~n\",[{module}:tagsRoundTrip()]), \
         io:format(\"numsSum=~s~n\",[{module}:numsSum()]), \
         io:format(\"specialTags=~s~n\",[{module}:specialTags()]), \
         io:format(\"emptyNums=~s~n\",[{module}:emptyNums()]), \
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

    assert!(
        stdout.contains("tagsRoundTrip=a,b"),
        "missing `tagsRoundTrip=a,b` (a text[] column (OID 1009) decodes to the typed SqlArray and the elements survive the round-trip through the database)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("numsSum=6"),
        "missing `numsSum=6` (an int[] column (OID 1007) decodes to the typed SqlArray: 1 + 2 + 3)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("specialTags=p,q|r"),
        "missing `specialTags=p,q|r` (a text element carrying the array delimiter is quoted on the way out and un-quoted on the way back)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("emptyNums=empty"),
        "missing `emptyNums=empty` (an empty int[] round-trips as an empty list)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
