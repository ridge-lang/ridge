//! End-to-end check that a whole query plan compiles to one parameterized SQL
//! statement on the BEAM.
//!
//! `Query.planToSql` is the Postgres renderer: it lowers a `QueryPlan` tree to a
//! `(Sql, List SqlValue)` pair — the statement with positional `$N` placeholders
//! and the bind values in order. This exercises every node shape: a single-table
//! scan, a set-operation combine and refine, the four join kinds (with the
//! source-prefixed select list and the outer-join presence markers), a projected
//! join, a scalar aggregate, and a grouped join.
//!
//! The plans are built directly through the public `plan*` builders, with each
//! captured predicate's reified tree read off a `Quote`'s `tree` field. The SQL is
//! asserted against what the proven backend verbs emit (`l."col"`/`r."col"`
//! qualifiers, `$N` placeholders, `TRUE AS "__present"` markers, `AVG(...)::float8`).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.query as Query (QueryPlan, planScan, planCombine, planRefine, planJoin, planProject, planAggregate, planGroup, planToSql)
import std.sql (Sql, SqlValue, sqlValue)
import std.int as Int
import std.list as List

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, author: Int, title: Text } deriving (Row)
pub type Combo = { person: Text, post: Text } deriving (Row)

-- A captured predicate's reified tree. `Quote` is a prelude record whose `tree`
-- field is the `QExpr` the compiler built from the lambda. A single-table filter is
-- a one-parameter quote; a join condition and a join projection are the two-entity
-- `fn e f -> r` form the join builders take, where the second entity's columns reify
-- to the right side (`QColR`).
fn pred1 (q: Quote (User -> Bool)) -> QExpr = q.tree
fn cond2 (q: Quote (fn User Post -> Bool)) -> QExpr = q.tree
fn proj2 (q: Quote (fn User Post -> Combo)) -> QExpr = q.tree

-- An always-true tree, the "keep all" filter a scan or a join's WHERE defaults to.
fn keepAll () -> QExpr = pred1 (fn (u: User) -> true)
fn keepAllJoin () -> QExpr = cond2 (fn (u: User) (p: Post) -> true)

fn usersScan () -> QueryPlan = planScan "users" (keepAll ()) [] (0 - 1) 0 false
fn postsScan () -> QueryPlan = planScan "posts" (keepAll ()) [] (0 - 1) 0 false
fn adultsScan () -> QueryPlan = planScan "users" (pred1 (fn u -> u.age >= 18)) [] (0 - 1) 0 false
fn joinCond () -> QExpr = cond2 (fn (u: User) (p: Post) -> u.id == p.author)

fn leftCols () -> List Text = ["id", "age", "name"]
fn rightCols () -> List Text = ["id", "author", "title"]

fn bareJoin (kind: Text) (left: QueryPlan) -> QueryPlan =
    planJoin kind left (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ())

fn wrapJoin () -> QueryPlan =
    planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false [] []

fn renderSql (plan: QueryPlan) -> Text =
    match planToSql plan
        (s, _) -> sqlValue s

fn renderBinds (plan: QueryPlan) -> Text =
    match planToSql plan
        (_, ps) -> Int.toText (List.length ps)

pub fn scanSql () -> Text = renderSql (planScan "users" (pred1 (fn u -> u.age >= 18)) [] (0 - 1) 0 false)

pub fn scanBinds () -> Text = renderBinds (planScan "users" (pred1 (fn u -> u.age >= 18)) [] (0 - 1) 0 false)

pub fn combineSql () -> Text =
    renderSql (planCombine "UNION" (adultsScan ()) (usersScan ()))

pub fn refineSql () -> Text =
    renderSql (planRefine (planCombine "UNION" (adultsScan ()) (usersScan ())) (pred1 (fn u -> u.age >= 18)) [] (0 - 1) 0 false)

pub fn innerSql () -> Text = renderSql (bareJoin "INNER" (usersScan ()))

pub fn leftSql () -> Text = renderSql (bareJoin "LEFT" (usersScan ()))

pub fn rightSql () -> Text = renderSql (bareJoin "RIGHT" (usersScan ()))

pub fn fullSql () -> Text = renderSql (bareJoin "FULL" (adultsScan ()))

pub fn fullBinds () -> Text = renderBinds (bareJoin "FULL" (adultsScan ()))

pub fn projectSql () -> Text =
    renderSql (planProject (proj2 (fn (u: User) (p: Post) -> Combo { person = u.name, post = p.title })) (wrapJoin ()) (0 - 1) 0 false)

pub fn aggSql () -> Text =
    renderSql (planAggregate "AVG" "author" true (wrapJoin ()))

pub fn groupSql () -> Text =
    renderSql (planGroup "author" true [("author", "KEY", "", true), ("n", "COUNT", "", false)] (keepAllJoin ()) (wrapJoin ()))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"query-plan-sql-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn query_plan_compiles_to_parameterized_sql() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping query_plan_compiles_to_parameterized_sql");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-query-plan-sql-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-query-plan-sql-e2e-cache-")
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
         lists:foreach(F,['scanSql','scanBinds','combineSql','refineSql','innerSql','leftSql','rightSql','fullSql','fullBinds','projectSql','aggSql','groupSql']), halt()."
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

    // A single-table scan: a bare-quoted column, the literal as `$1`, one bind.
    want(r#"scanSql=SELECT * FROM "users" WHERE "age" >= $1"#);
    want("scanBinds=1");

    // A set-operation combine wraps each branch in parens around the keyword; a
    // refine wraps the combination in a subquery and re-applies the outer WHERE.
    want(
        r#"combineSql=(SELECT * FROM "users" WHERE "age" >= $1) UNION (SELECT * FROM "users" WHERE TRUE)"#,
    );
    // The `$N` counter threads across the whole plan: the inner combine's filter
    // binds `$1`, so the outer refine's filter binds `$2`.
    want(
        r#"refineSql=SELECT * FROM ((SELECT * FROM "users" WHERE "age" >= $1) UNION (SELECT * FROM "users" WHERE TRUE)) AS ridge_sub WHERE "age" >= $2"#,
    );

    // An inner join: each source's columns prefixed (`t0$`/`t1$`), the condition
    // qualified to its side, no marker.
    want(
        r#"innerSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author" WHERE (TRUE) AND (TRUE)"#,
    );

    // A left join wraps the right table in the `__present` marker subquery and
    // selects the marker as `t1$__present__`.
    want(
        r#"leftSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title", r."__present" AS "t1$__present__" FROM "users" AS l LEFT JOIN (SELECT *, TRUE AS "__present" FROM "posts") AS r ON l."id" = r."author" WHERE (TRUE) AND (TRUE)"#,
    );

    // A right join wraps the left table and folds the left filter into the ON.
    want(
        r#"rightSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", l."__present" AS "t0$__present__", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title" FROM (SELECT *, TRUE AS "__present" FROM "users") AS l RIGHT JOIN "posts" AS r ON (l."id" = r."author") AND (TRUE) WHERE (TRUE)"#,
    );

    // A full join wraps both sides; the left filter goes inside the left subquery
    // and compiles with bare column names (so `$1`, one bind).
    want(
        r#"fullSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", l."__present" AS "t0$__present__", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title", r."__present" AS "t1$__present__" FROM (SELECT *, TRUE AS "__present" FROM "users" WHERE ("age" >= $1)) AS l FULL JOIN (SELECT *, TRUE AS "__present" FROM "posts") AS r ON (l."id" = r."author") WHERE (TRUE)"#,
    );
    want("fullBinds=1");

    // A projected join: the projection's own aliased select-list, no prefixing.
    want(
        r#"projectSql=SELECT l."name" AS "person", r."title" AS "post" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author" WHERE (TRUE) AND (TRUE)"#,
    );

    // A scalar aggregate over a join: the side-qualified column, AVG cast to float8.
    want(
        r#"aggSql=SELECT AVG(r."author")::float8 FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author" WHERE (TRUE) AND (TRUE)"#,
    );

    // A grouped join: the side-qualified key, COUNT(*), GROUP BY and ORDER BY the key.
    want(
        r#"groupSql=SELECT r."author" AS "author", COUNT(*) AS "n" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author" WHERE (TRUE) AND (TRUE) GROUP BY r."author" ORDER BY r."author""#,
    );
}
