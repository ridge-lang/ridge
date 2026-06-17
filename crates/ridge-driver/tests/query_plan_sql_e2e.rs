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
pub type Comment = { id: Int, post: Int, body: Text } deriving (Row)
pub type Combo = { person: Text, post: Text } deriving (Row)
pub type Trio = { who: Text, what: Text, note: Text } deriving (Row)

-- A captured predicate's reified tree. `Quote` is a prelude record whose `tree`
-- field is the `QExpr` the compiler built from the lambda. A single-table filter is
-- a one-parameter quote; a join condition and a join projection are the two-entity
-- `fn e f -> r` form the join builders take, where the second entity's columns reify
-- to the right side (`QColR`).
fn pred1 (q: Quote (User -> Bool)) -> QExpr = q.tree
fn cond2 (q: Quote (fn User Post -> Bool)) -> QExpr = q.tree
fn cond3 (q: Quote (fn User Post Comment -> Bool)) -> QExpr = q.tree
fn proj2 (q: Quote (fn User Post -> Combo)) -> QExpr = q.tree
fn proj3 (q: Quote (fn User Post Comment -> Trio)) -> QExpr = q.tree

-- An always-true tree, the "keep all" filter a scan or a join's WHERE defaults to.
fn keepAll () -> QExpr = pred1 (fn (u: User) -> true)
fn keepAllJoin () -> QExpr = cond2 (fn (u: User) (p: Post) -> true)

fn usersScan () -> QueryPlan = planScan "users" (keepAll ()) [] (0 - 1) 0 false
fn postsScan () -> QueryPlan = planScan "posts" (keepAll ()) [] (0 - 1) 0 false
fn adultsScan () -> QueryPlan = planScan "users" (pred1 (fn u -> u.age >= 18)) [] (0 - 1) 0 false
fn joinCond () -> QExpr = cond2 (fn (u: User) (p: Post) -> u.id == p.author)

fn leftCols () -> List Text = ["id", "age", "name"]
fn rightCols () -> List Text = ["id", "author", "title"]
fn commentCols () -> List Text = ["id", "post", "body"]

fn commentsScan () -> QueryPlan = planScan "comments" (keepAll ()) [] (0 - 1) 0 false
fn joinCond2 () -> QExpr = cond3 (fn (u: User) (p: Post) (c: Comment) -> p.id == c.post)

-- A three-table inner join: an inner `Join` of users and posts, joined again to
-- comments. The left child is a `PlanJoin`, so the renderer flattens the whole tree
-- into one flat multi-way join over leaf aliases `t0`/`t1`/`t2`. The third table's
-- column reifies to `QColAt 2`, qualified to `t2`. The base scan filters adults, so
-- the leaf filter qualifies to `t0` and binds `$1`.
fn inner3 () -> QueryPlan =
    planJoin "INNER"
        (planJoin "INNER" (adultsScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        []
        (0 - 1) 0 false
        []
        (commentCols ())

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
    renderSql (planAggregate "AVG" "author" 1 (wrapJoin ()))

pub fn groupSql () -> Text =
    renderSql (planGroup "author" true [("author", "KEY", "", true), ("n", "COUNT", "", false)] (keepAllJoin ()) (wrapJoin ()))

pub fn inner3Sql () -> Text = renderSql (inner3 ())

pub fn inner3Binds () -> Text = renderBinds (inner3 ())

-- A mixed-shape chain extends the inner `Join` of users and posts with a third table
-- under an outer step. The left child is the inner `PlanJoin`; the outer node's kind
-- (`LEFT`/`RIGHT`/`FULL`) sets how the new leaf joins and which leaves it null-extends.
-- A left step makes the new comments leaf optional (a `t2$__present__` marker); a right
-- or full step makes the whole `(users, posts)` composite optional as a unit (markers
-- on `t0`/`t1`). The base scan rides inside its own subquery when its leaf can be
-- null-extended, so its filter binds before the join.
fn innerLeftMix () -> QueryPlan =
    planJoin "LEFT"
        (planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        []
        (0 - 1) 0 false
        []
        (commentCols ())

fn innerRightMix () -> QueryPlan =
    planJoin "RIGHT"
        (planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        []
        (0 - 1) 0 false
        []
        (commentCols ())

fn innerFullMix () -> QueryPlan =
    planJoin "FULL"
        (planJoin "INNER" (adultsScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        []
        (0 - 1) 0 false
        []
        (commentCols ())

pub fn innerLeftMixSql () -> Text = renderSql (innerLeftMix ())

pub fn innerRightMixSql () -> Text = renderSql (innerRightMix ())

pub fn innerFullMixSql () -> Text = renderSql (innerFullMix ())

pub fn innerFullMixBinds () -> Text = renderBinds (innerFullMix ())

-- A count over the three-table inner composite: COUNT(*) over the flattened multi-way
-- join, the base adult filter qualified to t0 binding $1. A reduction selects no leaf
-- columns and reads no markers — just the count.
fn countThree () -> QueryPlan = planAggregate "COUNT" "" 0 (inner3 ())

pub fn countThreeSql () -> Text = renderSql (countThree ())

pub fn countThreeBinds () -> Text = renderBinds (countThree ())

-- A count over a mixed inner-then-left composite carrying a post-join filter on the left
-- step (`c.post >= 11`). The marker-free FROM keeps the LEFT JOIN but drops the presence
-- markers a bare terminal needs (a reduction reads the null-extended NULLs directly), and
-- the step's where2 renders in the top-level WHERE qualified to t2 — proving an outer
-- step's post-join filter reaches the clause.
fn leftMixFiltered () -> QueryPlan =
    planJoin "LEFT"
        (planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (cond3 (fn (u: User) (p: Post) (c: Comment) -> c.post >= 11))
        []
        (0 - 1) 0 false
        []
        (commentCols ())

fn countLeftMix () -> QueryPlan = planAggregate "COUNT" "" 0 (leftMixFiltered ())

pub fn countLeftMixSql () -> Text = renderSql (countLeftMix ())

pub fn countLeftMixBinds () -> Text = renderBinds (countLeftMix ())

-- A scalar SUM over the three-table inner composite, folding the deep leaf's column
-- (`post`, leaf 2): `SUM(t2."post")` over the flattened multi-way join, the base adult
-- filter qualified to t0 ($1).
fn sumThree () -> QueryPlan = planAggregate "SUM" "post" 2 (inner3 ())

pub fn sumThreeSql () -> Text = renderSql (sumThree ())

-- An AVG over the same composite leaf, carrying the `::float8` cast so an integer column
-- averages to a float, as the single-table and binary-join aggregates do.
fn avgThree () -> QueryPlan = planAggregate "AVG" "post" 2 (inner3 ())

pub fn avgThreeSql () -> Text = renderSql (avgThree ())

-- A projection over the three-table inner composite: a `PlanProject` whose `QProj` names
-- one column from each leaf (`u.name`, `p.title`, `c.body`). The deep leaf reifies to a
-- `QColAt 2` cell the renderer qualifies to `t2`, so the select-list reads
-- `t0."name" AS "who", t1."title" AS "what", t2."body" AS "note"` over the flattened
-- multi-way join, the base adult filter qualified to t0 ($1). Proves the renderer pushes
-- a leaf-spanning projection down a composite the same way it does a binary join's.
fn projectThree () -> QueryPlan =
    planProject (proj3 (fn (u: User) (p: Post) (c: Comment) -> Trio { who = u.name, what = p.title, note = c.body })) (inner3 ()) (0 - 1) 0 false

pub fn projectThreeSql () -> Text = renderSql (projectThree ())
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
         lists:foreach(F,['scanSql','scanBinds','combineSql','refineSql','innerSql','leftSql','rightSql','fullSql','fullBinds','projectSql','aggSql','groupSql','inner3Sql','inner3Binds','innerLeftMixSql','innerRightMixSql','innerFullMixSql','innerFullMixBinds','countThreeSql','countThreeBinds','countLeftMixSql','countLeftMixBinds','sumThreeSql','avgThreeSql','projectThreeSql']), halt()."
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

    // A three-table inner join flattens into one multi-way join over leaf aliases
    // t0/t1/t2: every leaf's columns prefixed by its index, both conditions qualified
    // to their leaves (the third table's column is `QColAt 2`, qualified `t2`), and the
    // base scan's adult filter qualified to `t0` binding `$1`. The always-true defaults
    // drop out of the WHERE.
    want(
        r#"inner3Sql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1)"#,
    );
    want("inner3Binds=1");

    // A mixed chain `users JOIN posts LEFT JOIN comments`: the inner pair renders flat,
    // then the left step wraps the new comments leaf in the `__present` marker subquery
    // and selects it as `t2$__present__`. Only the new leaf is optional, so only `t2`
    // carries a marker.
    want(
        r#"innerLeftMixSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body", t2."__present" AS "t2$__present__" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" LEFT JOIN (SELECT *, TRUE AS "__present" FROM "comments") AS t2 ON t1."id" = t2."post""#,
    );

    // A mixed chain `users JOIN posts RIGHT JOIN comments`: the right step keeps every
    // comments row and null-extends the whole `(users, posts)` composite as a unit, so
    // both `t0` and `t1` wrap in marker subqueries while the always-present comments
    // leaf `t2` stays bare. SQL's left-associative nesting nulls `t0` and `t1` together.
    want(
        r#"innerRightMixSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t0."__present" AS "t0$__present__", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t1."__present" AS "t1$__present__", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body" FROM (SELECT *, TRUE AS "__present" FROM "users") AS t0 JOIN (SELECT *, TRUE AS "__present" FROM "posts") AS t1 ON t0."id" = t1."author" RIGHT JOIN "comments" AS t2 ON t1."id" = t2."post""#,
    );

    // A mixed chain `adults JOIN posts FULL JOIN comments`: the full step null-extends
    // both the composite and the new leaf, so all three leaves carry markers. The base
    // `adults` filter rides inside `t0`'s subquery (bare column, `$1`), so it restricts
    // which users enter the join rather than dropping a null-extended row afterward.
    want(
        r#"innerFullMixSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t0."__present" AS "t0$__present__", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t1."__present" AS "t1$__present__", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body", t2."__present" AS "t2$__present__" FROM (SELECT *, TRUE AS "__present" FROM "users" WHERE "age" >= $1) AS t0 JOIN (SELECT *, TRUE AS "__present" FROM "posts") AS t1 ON t0."id" = t1."author" FULL JOIN (SELECT *, TRUE AS "__present" FROM "comments") AS t2 ON t1."id" = t2."post""#,
    );
    want("innerFullMixBinds=1");

    // A count over the three-table inner composite: COUNT(*) over the flattened multi-way
    // join, the base adult filter qualified to t0 ($1). No leaf select-list, no markers.
    want(
        r#"countThreeSql=SELECT COUNT(*) FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1)"#,
    );
    want("countThreeBinds=1");

    // A count over a mixed inner-then-left composite with a post-join filter on the left
    // step: the marker-free FROM keeps the LEFT JOIN but drops the presence markers, and
    // the step's where2 (`c.post >= 11`) renders in the top-level WHERE qualified to t2.
    want(
        r#"countLeftMixSql=SELECT COUNT(*) FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" LEFT JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t2."post" >= $1)"#,
    );
    want("countLeftMixBinds=1");

    // A scalar SUM over the three-table composite folds the deep leaf's column, qualified
    // to its alias t2; AVG carries the ::float8 cast. The base adult filter binds $1.
    want(
        r#"sumThreeSql=SELECT SUM(t2."post") FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1)"#,
    );
    want(
        r#"avgThreeSql=SELECT AVG(t2."post")::float8 FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1)"#,
    );

    // A projection over the three-table composite names one column per leaf, the deep
    // leaf qualified to t2 through its QColAt cell, over the flattened multi-way join with
    // the base adult filter bound to $1 — a leaf-spanning select-list pushed down a
    // composite exactly as a binary join's projection is.
    want(
        r#"projectThreeSql=SELECT t0."name" AS "who", t1."title" AS "what", t2."body" AS "note" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1)"#,
    );
}
