//! End-to-end check for the `SqlType Decimal` codec on the in-memory adapter.
//!
//! A `Decimal` field rides a row: `deriving (Row, Schema)` accepts it, the insert
//! path encodes it through `SqlType.toSql` into the typed `SqlDecimal` value (the
//! exact canonical text), and the read path decodes it back with `fromSql`. This
//! proves the loop:
//! - a decimal round-trips through the stored `SqlDecimal` and renders back to the
//!   exact text it went in as, including a value past Int/Float precision,
//! - the store orders and compares a decimal column by value, not by its text, so
//!   2.5 sorts before 10.25 (a lexical order would get this wrong), and
//! - `deriving (Schema)` reads the column type from `SqlType.dbType`, so the DDL
//!   names the column `numeric` (the unconstrained, exact Postgres form).
//!
//! The exact Postgres `numeric` (OID 1700) decode is covered separately in
//! `data_pg_e2e` against a real database.
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

-- An entity with a `Decimal` column. `deriving (Schema)` marks `id` an identity
-- column, so the insert shape `MoneyInsert` carries only `label` and `amount`.
pub type Money = { id: Int, label: Text, amount: Decimal } deriving (Row, Schema)

-- Parse or fall back to zero, so seeding is total.
fn dec (s: Text) -> Decimal =
    match Decimal.fromText s
        Ok d  -> d
        Err _ -> Decimal.fromInt 0

-- Comma-join the labels of a row list, so an order is observable as one string.
fn joinLabels (ms: List Money) -> Text =
    match ms
        []        -> ""
        m :: []   -> m.label
        m :: rest -> Text.concat m.label (Text.concat "," (joinLabels rest))

-- Seed three rows whose amounts are out of order and whose numeric order differs
-- from their lexical order (2.5 < 10.25 by value, but "10.25" < "2.5" as text).
pub fn db setup () -> Result (Repo Money MemAdapter) Error =
    let r: Repo Money MemAdapter = Repo.repo (memAdapter ()) "money"
    match Repo.insert (MoneyInsert { label = "a", amount = dec "2.5" }) r
        Err e -> Err e
        Ok _  ->
            match Repo.insert (MoneyInsert { label = "b", amount = dec "10.25" }) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insert (MoneyInsert { label = "c", amount = dec "1.999" }) r
                        Err e -> Err e
                        Ok _  -> Ok r

-- value round-trip: the stored decimal reads back and renders to the same text it
-- went in as. Row 2 is label "b", amount 10.25.
pub fn db roundTrip () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some m) -> Decimal.toText m.amount

-- ascending by the amount column orders by value: 1.999 < 2.5 < 10.25 -> "c,a,b".
-- A lexical order over the text would give "c,b,a", so this pins numeric ordering.
pub fn db ascOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (m: Money) -> m.amount) |> Repo.toList
                Err _ -> "list-err"
                Ok ms -> joinLabels ms

-- descending reverses it -> "b,a,c".
pub fn db descOrder () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (m: Money) -> m.amount) |> Repo.toList
                Err _ -> "list-err"
                Ok ms -> joinLabels ms

-- a value beyond Int/Float precision round-trips exactly through the stored value.
pub fn db bigExact () -> Text =
    let r: Repo Money MemAdapter = Repo.repo (memAdapter ()) "big"
    let big = "123456789012345678901234567890.123456789"
    match Repo.insert (MoneyInsert { label = "big", amount = dec big }) r
        Err _ -> "insert-err"
        Ok _  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some m) -> Decimal.toText m.amount

-- a decimal literal inside a quoted predicate: `amount > 2.5m` reifies the literal
-- into the query tree and compiles to a bound parameter, so only the 10.25 row
-- (label "b") passes -- 2.5 is not strictly greater than itself. Proves the
-- literal works in the query DSL, not only in plain expressions.
pub fn db filterLit () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (m: Money) -> m.amount > 2.5m) |> Repo.toList
                Err _ -> "list-err"
                Ok ms -> joinLabels ms

-- column-type dispatch: `deriving (Schema)` reads the `amount` column type from
-- SqlType.dbType, so the DDL names it `numeric` (the unconstrained exact form of
-- DbRaw "numeric"), proving the column type comes from the codec.
fn moneyWitness () -> Option Money = None

pub fn amountDdl () -> Text = schemaToDdl (schemaOf (moneyWitness ()))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-decimal-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn decimal_codec_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping decimal_codec_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-decimal-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-decimal-e2e-cache-")
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
         io:format(\"bigExact=~s~n\",[{module}:bigExact()]), \
         io:format(\"filterLit=~s~n\",[{module}:filterLit()]), \
         io:format(\"amountDdl=~s~n\",[{module}:amountDdl()]), \
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
            "roundTrip=10.25",
            "a Decimal round-trips through the stored SqlDecimal and renders back exactly",
        ),
        (
            "ascOrder=c,a,b",
            "a decimal column sorts ascending by value (1.999 < 2.5 < 10.25), not by text",
        ),
        (
            "descOrder=b,a,c",
            "the same column sorts descending by value",
        ),
        (
            "bigExact=123456789012345678901234567890.123456789",
            "a value beyond Int/Float precision round-trips exactly through the row",
        ),
        (
            "filterLit=b",
            "a decimal literal in a quoted predicate reifies and compiles to a bound param",
        ),
        (
            "numeric",
            "deriving (Schema) dispatches the amount column type through SqlType.dbType to numeric",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
