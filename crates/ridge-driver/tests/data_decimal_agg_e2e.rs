//! End-to-end check for grouped SUM/AVG over a `Decimal` column on the in-memory
//! adapter.
//!
//! Before this, `mem_num` (the SUM/AVG fold) knew only `SqlInt`/`SqlFloat`, so a
//! decimal aggregate crashed with `function_clause`. This proves the fold now:
//! - a grouped `sum` keeps the column's type and folds exactly, so a decimal column
//!   sums to a `Decimal` with every digit preserved (a naive float fold of
//!   0.10 + 0.20 + 0.03 drifts to 0.33000000000000007; the exact fold stays 0.33),
//! - a grouped `avg` is a `Float` even over a decimal column, matching how the SQL
//!   backend casts `AVG(numeric)` to `float8`, and
//! - a decimal `HAVING` threshold reads the same folds.
//!
//! This is the in-memory adapter, which folds the aggregate itself; Postgres folds
//! `SUM`/`AVG(numeric)` in the database, and `data_pg_decimal_e2e` covers the exact
//! `numeric` wire decode the results ride back on.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (memAdapter, MemAdapter)
import std.repo as Repo
import std.sql (toSql, SqlValue)

-- An entity with a `Decimal` amount and a repeated `dept` key, so a GROUP BY folds
-- several rows per group. `deriving (Schema)` marks `id` identity, so the insert
-- shape `SaleInsert` carries only `dept` and `amount`.
pub type Sale = { id: Int, dept: Text, amount: Decimal } deriving (Row, Schema)

-- The summarised shapes a `groupBy` projects into: a decimal SUM keeps the column
-- type, an AVG is always a Float.
pub type DeptSum = { dept: Text, total: Decimal } deriving (Row)
pub type DeptAvg = { dept: Text, mean: Float } deriving (Row)

-- Parse or fall back to zero, so seeding is total.
fn dec (s: Text) -> Decimal =
    match Decimal.fromText s
        Ok d  -> d
        Err _ -> Decimal.fromInt 0

-- Render the grouped result rows as `key:value` cells joined by commas. The backend
-- returns the groups ordered by the key, so the rendered string is deterministic.
fn sumCells (rows: List DeptSum) -> Text =
    match rows
        []        -> ""
        r :: []   -> Text.concat r.dept (Text.concat ":" (Decimal.toText r.total))
        r :: rest -> Text.concat r.dept (Text.concat ":" (Text.concat (Decimal.toText r.total) (Text.concat "," (sumCells rest))))

fn avgCells (rows: List DeptAvg) -> Text =
    match rows
        []        -> ""
        r :: []   -> Text.concat r.dept (Text.concat ":" (Float.toText r.mean))
        r :: rest -> Text.concat r.dept (Text.concat ":" (Text.concat (Float.toText r.mean) (Text.concat "," (avgCells rest))))

-- Seed two departments with binary-exact amounts so the float AVG lands cleanly:
-- eng {1.25, 2.75} sums to 4.00 (avg 2.0), ops {0.50, 10.50} sums to 11.00 (avg 5.5).
pub fn db setup () -> Result (Repo Sale MemAdapter) Error =
    let r: Repo Sale MemAdapter = Repo.repo (memAdapter ()) "sales"
    match Repo.insert (SaleInsert { dept = "eng", amount = dec "1.25" }) r
        Err e -> Err e
        Ok _  ->
            match Repo.insert (SaleInsert { dept = "eng", amount = dec "2.75" }) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insert (SaleInsert { dept = "ops", amount = dec "0.50" }) r
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insert (SaleInsert { dept = "ops", amount = dec "10.50" }) r
                                Err e -> Err e
                                Ok _  -> Ok r

-- grouped SUM per dept, decimal and exact: eng 4.00, ops 11.00.
pub fn db deptSums () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (s: Sale) -> s.dept) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (s: Sale) -> s.amount) })
                Err _ -> "sum-err"
                Ok rows -> sumCells rows

-- grouped AVG per dept, a Float: eng 2.0, ops 5.5.
pub fn db deptAvgs () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (s: Sale) -> s.dept) |> Repo.summarize (fn g -> DeptAvg { dept = g.key, mean = g.avg (fn (s: Sale) -> s.amount) })
                Err _ -> "avg-err"
                Ok rows -> avgCells rows

-- grouped SUM behind a HAVING on the decimal aggregate: keep depts whose total is at
-- least 5.00, so eng (4.00) drops and only ops (11.00) survives.
pub fn db havingSum () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (s: Sale) -> s.dept) |> Repo.having (fn g -> g.sum (fn (s: Sale) -> s.amount) >= 5.00m) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (s: Sale) -> s.amount) })
                Err _ -> "having-err"
                Ok rows -> sumCells rows

-- SUM stays exact where a naive float fold would drift: 0.10 + 0.20 + 0.03 = 0.33
-- exactly (folding the nearest floats gives 0.33000000000000007). A separate one-group
-- table isolates the case, read through the same grouped SUM.
pub fn db sumExact () -> Text =
    let r: Repo Sale MemAdapter = Repo.repo (memAdapter ()) "cents"
    match Repo.insert (SaleInsert { dept = "x", amount = dec "0.10" }) r
        Err _ -> "insert-err"
        Ok _  ->
            match Repo.insert (SaleInsert { dept = "x", amount = dec "0.20" }) r
                Err _ -> "insert-err"
                Ok _  ->
                    match Repo.insert (SaleInsert { dept = "x", amount = dec "0.03" }) r
                        Err _ -> "insert-err"
                        Ok _  ->
                            match r |> Repo.query |> Repo.groupBy (fn (s: Sale) -> s.dept) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (s: Sale) -> s.amount) })
                                Err _ -> "sum-err"
                                Ok rows -> sumCells rows
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-decimal-agg-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn decimal_aggregates_run_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping decimal_aggregates_run_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-decimal-agg-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-decimal-agg-e2e-cache-")
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
        "io:format(\"deptSums=~s~n\",[{module}:deptSums()]), \
         io:format(\"deptAvgs=~s~n\",[{module}:deptAvgs()]), \
         io:format(\"havingSum=~s~n\",[{module}:havingSum()]), \
         io:format(\"sumExact=~s~n\",[{module}:sumExact()]), \
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
            "deptSums=eng:4.00,ops:11.00",
            "a grouped SUM folds each decimal group exactly and keeps its Decimal type",
        ),
        (
            "deptAvgs=eng:2.0,ops:5.5",
            "a grouped AVG over a decimal column is a Float per group",
        ),
        (
            "havingSum=ops:11.00",
            "a HAVING threshold on a decimal SUM drops the group below 5.00",
        ),
        (
            "sumExact=x:0.33",
            "the decimal fold stays exact where a float fold would drift to 0.33000000000000007",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
