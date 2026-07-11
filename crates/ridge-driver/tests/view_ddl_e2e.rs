//! End-to-end check that a query plan renders to a `CREATE VIEW` on the BEAM.
//!
//! A view is a named query: `createViewDdl name plan` renders `CREATE VIEW <name> AS
//! <select>`, where the `<select>` is the plan rendered with every captured value spelled
//! in place rather than collected as a `$N` bind — a view body takes no parameters.
//! `createViewDdlFor` threads the dialect into that render, and `dropViewDdl` removes the
//! view by name.
//!
//! The inline render reuses the bound plan renderer for the whole statement, then splices
//! each bind back through `sqlLiteral`, so this pins the pieces that splicing has to get
//! right: an integer literal spelled bare, a text literal single-quoted with an embedded
//! quote doubled (`O'Brien` → `'O''Brien'`), several literals filled left to right, and —
//! the subtle one — a text value that itself contains a `$1` never being re-read as a
//! placeholder. A grouped aggregate view pins that the dialect still threads through (the
//! Postgres `AVG(...)::float8` vs the SQLite bare `AVG(...)`), and a join view pins that the
//! `"t0$col"` column aliases, whose `$` sits inside a quoted identifier, pass through
//! untouched.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.query as Query (QueryPlan, planScan, planJoin, planAggregate, createViewDdl, createViewDdlFor, dropViewDdl)
import std.sql (SqliteDialect)

-- The plans are built directly from prelude `QExpr` constructors, so no entity type or
-- reified `Quote` is needed: a column is `QCol`/`QColR`, a literal a `QLit*`, a comparison
-- a `QEq`/`QGe`, a conjunction a `QAnd`. The "keep all" filter a scan defaults to is a
-- literal `true`, which the renderer folds away (no WHERE clause).
fn keepAll () -> QExpr = QLitBool true

-- A single-table filter with an integer literal: the view body inlines `18`, not `$1`.
fn adultsPred () -> QExpr = QGe (QCol "age") (QLitInt 18)
fn adultsPlan () -> QueryPlan = planScan "users" (adultsPred ()) [] (0 - 1) 0 false
pub fn adultViewPg () -> Text = createViewDdl "active_users" (adultsPlan ())
pub fn adultViewLite () -> Text = createViewDdlFor SqliteDialect "active_users" (adultsPlan ())

-- A text literal carrying an apostrophe: the inline form single-quotes it and doubles the
-- embedded quote, the same escape a column DEFAULT uses.
fn brienPred () -> QExpr = QEq (QCol "name") (QLitText "O'Brien")
pub fn brienView () -> Text = createViewDdl "vips" (planScan "users" (brienPred ()) [] (0 - 1) 0 false)

-- Two literals, plus a text value that spells `$1`: the first proves the binds fill left to
-- right, the second proves an inlined value is never re-scanned for placeholders.
fn comboPred () -> QExpr = QAnd (QGe (QCol "age") (QLitInt 18)) (QEq (QCol "code") (QLitText "$1off"))
pub fn comboView () -> Text = createViewDdl "promo" (planScan "users" (comboPred ()) [] (0 - 1) 0 false)

pub fn dropIt () -> Text = dropViewDdl "active_users"

-- A grouped aggregate, to prove the dialect threads through the inline path: Postgres casts
-- the average, SQLite does not.
fn avgPlan () -> QueryPlan = planAggregate "AVG" (QCol "age") 0 (adultsPlan ())
pub fn avgViewPg () -> Text = createViewDdl "avg_age" (avgPlan ())
pub fn avgViewLite () -> Text = createViewDdlFor SqliteDialect "avg_age" (avgPlan ())

-- A join, to prove the `"t0$col"`/`"t1$col"` aliases (a `$` inside a quoted identifier) are
-- copied through rather than mistaken for a placeholder.
fn usersScan () -> QueryPlan = planScan "users" (keepAll ()) [] (0 - 1) 0 false
fn postsScan () -> QueryPlan = planScan "posts" (keepAll ()) [] (0 - 1) 0 false
fn joinCond () -> QExpr = QEq (QCol "id") (QColR "author")
fn feedJoin () -> QueryPlan =
    planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (keepAll ()) [] (0 - 1) 0 false ["id", "age", "name"] ["id", "author", "title"]
pub fn joinView () -> Text = createViewDdl "feed" (feedJoin ())
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"view-ddl-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn query_plan_compiles_to_create_view() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping query_plan_compiles_to_create_view");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-view-ddl-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-view-ddl-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

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
         lists:foreach(F,['adultViewPg','adultViewLite','brienView','comboView','dropIt','avgViewPg','avgViewLite','joinView']), halt()."
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

    let want = |needle: &str| {
        assert!(
            stdout.contains(needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    };

    // A filtered scan: the `18` is inlined into the view body, not left as `$1`. Both
    // dialects spell a plain scan the same way.
    want(r#"adultViewPg=CREATE VIEW "active_users" AS SELECT * FROM "users" WHERE "age" >= 18"#);
    want(r#"adultViewLite=CREATE VIEW "active_users" AS SELECT * FROM "users" WHERE "age" >= 18"#);

    // A text literal single-quoted, the embedded apostrophe doubled.
    want(r#"brienView=CREATE VIEW "vips" AS SELECT * FROM "users" WHERE "name" = 'O''Brien'"#);

    // Two literals filled left to right; the second, `$1off`, proves an inlined value is not
    // re-read as a placeholder.
    want(
        r#"comboView=CREATE VIEW "promo" AS SELECT * FROM "users" WHERE ("age" >= 18 AND "code" = '$1off')"#,
    );

    want(r#"dropIt=DROP VIEW "active_users""#);

    // The dialect still threads through: Postgres casts the average, SQLite does not.
    want(
        r#"avgViewPg=CREATE VIEW "avg_age" AS SELECT AVG("age")::float8 FROM "users" WHERE "age" >= 18"#,
    );
    want(
        r#"avgViewLite=CREATE VIEW "avg_age" AS SELECT AVG("age") FROM "users" WHERE "age" >= 18"#,
    );

    // The join's `"t0$col"`/`"t1$col"` aliases pass through — the `$` inside a quoted
    // identifier is not a placeholder.
    want(
        r#"joinView=CREATE VIEW "feed" AS SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author""#,
    );
}
