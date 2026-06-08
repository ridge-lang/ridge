//! End-to-end check that a quoted predicate compiles to parameterized SQL on
//! the BEAM.
//!
//! Builds on the capture chain (a native lambda passed where a `Quote` is
//! expected is reified into a `QExpr` tree) and exercises the next step:
//! `Query.toSql` walks that tree into a `(Sql, List SqlValue)` pair — the SQL
//! string with a `?` placeholder per literal, plus the literal values collected
//! as bind parameters in left-to-right order.
//!
//! The decisive details: the statement reads back through `Sql.sqlValue` exactly
//! as the renderer shows it, each literal contributes one bind (so a one-literal
//! predicate yields a one-element bind list), and a bare boolean column adds no
//! bind.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.query as Query (SortOrder, Asc, Desc)
import std.sql (Sql, SqlValue, sqlValue)
import std.int as Int
import std.list as List

pub type User = { id: Int, age: Int, active: Bool, signupYear: Int } deriving (Table)

fn compiled (q: Quote (User -> Bool)) -> (Sql, List SqlValue) = Query.toSql q

fn orderKey (q: Quote (User -> Int)) -> Sql = Query.orderSql Desc q

fn orderKeyAsc (q: Quote (User -> Int)) -> Sql = Query.orderSql Asc q

fn selectCols (q: Quote (User -> { id: Int, signupYear: Int })) -> Sql = Query.selectSql q

fn selectRenamed (q: Quote (User -> { year: Int })) -> Sql = Query.selectSql q

pub fn projectCols () -> Text = sqlValue (selectCols (fn u -> { id = u.id, signupYear = u.signupYear }))

pub fn projectRenamed () -> Text = sqlValue (selectRenamed (fn u -> { year = u.signupYear }))

pub fn recentFirst () -> Text = sqlValue (orderKey (fn u -> u.signupYear))

pub fn oldestFirst () -> Text = sqlValue (orderKeyAsc (fn u -> u.signupYear))

pub fn adultSql () -> Text =
    match compiled (fn u -> u.age >= 18)
        (s, _) -> sqlValue s

pub fn adultBinds () -> Text =
    match compiled (fn u -> u.age >= 18)
        (_, ps) -> Int.toText (List.length ps)

pub fn activeAdultSql () -> Text =
    match compiled (fn u -> u.age >= 18 && u.active)
        (s, _) -> sqlValue s

pub fn activeAdultBinds () -> Text =
    match compiled (fn u -> u.age >= 18 && u.active)
        (_, ps) -> Int.toText (List.length ps)

pub fn recentSql () -> Text =
    match compiled (fn u -> u.signupYear >= 2020)
        (s, _) -> sqlValue s
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"query-sql-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn quoted_predicate_compiles_to_parameterized_sql() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping quoted_predicate_compiles_to_parameterized_sql");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-query-sql-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-query-sql-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    // The query module must compile clean: `Query.toSql`, `Query.orderSql`, and
    // `Query.selectSql` all resolve as qualified calls (orderSql is seeded from
    // the reconciled block, so its qualified form must be in the env too).
    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile, got diagnostics: {:?}",
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
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['adultSql','adultBinds','activeAdultSql','activeAdultBinds','recentSql','recentFirst','oldestFirst','projectCols','projectRenamed']), halt()."
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

    // The predicate compiles to a parameterized statement: the column reads by
    // its SQL name, the literal becomes a `?`, and exactly one bind is collected.
    assert!(
        stdout.contains("adultSql=(age >= ?)"),
        "expected `adultSql=(age >= ?)`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("adultBinds=1"),
        "expected `adultBinds=1`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A boolean column is a predicate on its own and contributes no bind, so the
    // `&&` predicate still collects just the one literal from `age >= 18`.
    assert!(
        stdout.contains("activeAdultSql=((age >= ?) AND active)"),
        "expected `activeAdultSql=((age >= ?) AND active)`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("activeAdultBinds=1"),
        "expected `activeAdultBinds=1`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A camelCase field reifies to its snake_case SQL column name.
    assert!(
        stdout.contains("recentSql=(signup_year >= ?)"),
        "expected `recentSql=(signup_year >= ?)`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // An ordering key is a bare column compiled with its direction; the
    // camelCase field reifies to its snake_case SQL name and carries no bind.
    assert!(
        stdout.contains("recentFirst=signup_year DESC"),
        "expected `recentFirst=signup_year DESC`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("oldestFirst=signup_year ASC"),
        "expected `oldestFirst=signup_year ASC`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A projection compiles to a select-list. Each field's camelCase name
    // reifies to its snake_case column; a field that matches its source column
    // is emitted bare, in the order written.
    assert!(
        stdout.contains("projectCols=id, signup_year"),
        "expected `projectCols=id, signup_year`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A field renamed from its source column is emitted as `column AS alias`.
    assert!(
        stdout.contains("projectRenamed=signup_year AS year"),
        "expected `projectRenamed=signup_year AS year`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
