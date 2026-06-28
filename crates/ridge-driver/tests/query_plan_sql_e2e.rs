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
import std.query as Query (QueryPlan, planScan, planCombine, planRefine, planJoin, planProject, planAggregate, planGroup, planToSql, planExists, MutationPlan, planInsert, planUpsert, planUpdate, planDelete, mutationToSql)
import std.sql (Sql, SqlValue, sqlValue, sqlInt, sqlText)
import std.int as Int
import std.list as List
import std.text as Text
import std.map as Map

pub type User = { id: Int, age: Int, name: Text } deriving (Row)
pub type Post = { id: Int, author: Int, title: Text } deriving (Row)
pub type Comment = { id: Int, post: Int, body: Text } deriving (Row)
pub type Reaction = { id: Int, comment: Int, kind: Text } deriving (Row)
pub type Combo = { person: Text, post: Text } deriving (Row)
pub type Trio = { who: Text, what: Text, note: Text } deriving (Row)
pub type Quad = { who: Text, what: Text, note: Text, react: Text } deriving (Row)
pub type Calc = { person: Text, score: Int } deriving (Row)
pub type Caseo = { person: Text, band: Text } deriving (Row)

-- A captured predicate's reified tree. `Quote` is a prelude record whose `tree`
-- field is the `QExpr` the compiler built from the lambda. A single-table filter is
-- a one-parameter quote; a join condition and a join projection are the two-entity
-- `fn e f -> r` form the join builders take, where the second entity's columns reify
-- to the right side (`QColR`).
fn pred1 (q: Quote (User -> Bool)) -> QExpr = q.tree
fn cond2 (q: Quote (fn User Post -> Bool)) -> QExpr = q.tree
fn cond3 (q: Quote (fn User Post Comment -> Bool)) -> QExpr = q.tree
fn cond4 (q: Quote (fn User Post Comment Reaction -> Bool)) -> QExpr = q.tree
fn proj2 (q: Quote (fn User Post -> Combo)) -> QExpr = q.tree
fn proj3 (q: Quote (fn User Post Comment -> Trio)) -> QExpr = q.tree
fn proj4 (q: Quote (fn User Post Comment Reaction -> Quad)) -> QExpr = q.tree
fn projCalc (q: Quote (fn User Post -> Calc)) -> QExpr = q.tree
fn projCaseo (q: Quote (fn User Post -> Caseo)) -> QExpr = q.tree

-- A captured single-column accessor's tree, for an aggregate's folded column or a
-- join ordering key (both now carry a `QExpr`, a column or a computed expression).
-- Generic over the column type (phantom) and the join arity, so a leaf's column
-- reifies to its side (`p.x` is `QColR`, `c.x` is `QColAt 2`, `r.x` is `QColAt 3`).
fn col2 (q: Quote (fn User Post -> a)) -> QExpr = q.tree
fn col3 (q: Quote (fn User Post Comment -> a)) -> QExpr = q.tree
fn col4 (q: Quote (fn User Post Comment Reaction -> a)) -> QExpr = q.tree

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

fn reactionCols () -> List Text = ["id", "comment", "kind"]
fn reactionsScan () -> QueryPlan = planScan "reactions" (keepAll ()) [] (0 - 1) 0 false
fn joinCond3 () -> QExpr = cond4 (fn (u: User) (p: Post) (c: Comment) (r: Reaction) -> c.id == r.comment)

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

-- The write-side renderer: a `MutationPlan` to its parameterized statement and bind
-- count, the dual of `renderSql`/`renderBinds` over `mutationToSql`.
fn renderMutSql (plan: MutationPlan) -> Text =
    match mutationToSql plan
        (s, _) -> sqlValue s

fn renderMutBinds (plan: MutationPlan) -> Text =
    match mutationToSql plan
        (_, ps) -> Int.toText (List.length ps)

-- A single-row INSERT: the column list comes from the row (in column-name order),
-- each value a `$N` placeholder bound left to right.
pub fn insertSql () -> Text = renderMutSql (planInsert "users" [Map.fromList [("id", sqlInt 1), ("name", sqlText "ada")]])
pub fn insertBinds () -> Text = renderMutBinds (planInsert "users" [Map.fromList [("id", sqlInt 1), ("name", sqlText "ada")]])

-- A bulk INSERT over two rows sharing the same columns: the column list (from the
-- first row) renders once, then one parenthesised `$N` tuple per row, the binds
-- threaded id,name,id,name across both rows — one statement for the whole batch.
pub fn insertManySql () -> Text = renderMutSql (planInsert "users" [Map.fromList [("id", sqlInt 1), ("name", sqlText "ada")], Map.fromList [("id", sqlInt 2), ("name", sqlText "bob")]])
pub fn insertManyBinds () -> Text = renderMutBinds (planInsert "users" [Map.fromList [("id", sqlInt 1), ("name", sqlText "ada")], Map.fromList [("id", sqlInt 2), ("name", sqlText "bob")]])

-- An UPDATE binds its SET assignment first ($1) and then its WHERE ($2).
pub fn updateSql () -> Text = renderMutSql (planUpdate "users" (Map.fromList [("age", sqlInt 99)]) (pred1 (fn (u: User) -> u.id == 1)))
pub fn updateBinds () -> Text = renderMutBinds (planUpdate "users" (Map.fromList [("age", sqlInt 99)]) (pred1 (fn (u: User) -> u.id == 1)))

-- A DELETE binds only its WHERE.
pub fn deleteSql () -> Text = renderMutSql (planDelete "users" (pred1 (fn (u: User) -> u.age < 18)))

-- A correlated EXISTS in a DELETE predicate aliases the target table `l` so the
-- subquery names the row being deleted, exactly as the read renderer aliases a scan —
-- the write path shares `renderPred`, so no separate subquery support is needed.
pub fn existsDeleteSql () -> Text = renderMutSql (planDelete "users" (QExists "posts" (QEq (QColR "author") (QCol "id"))))
pub fn existsDeleteBinds () -> Text = renderMutBinds (planDelete "users" (QExists "posts" (QEq (QColR "author") (QCol "id"))))

-- A correlated EXISTS in an UPDATE predicate: the target is aliased `l` too, the SET
-- column stays bare ($1), and the EXISTS renders after it — locking the SET-before-WHERE
-- bind order with a subquery in the WHERE.
pub fn existsUpdateSql () -> Text = renderMutSql (planUpdate "users" (Map.fromList [("age", sqlInt 99)]) (QExists "posts" (QEq (QColR "author") (QCol "id"))))
pub fn existsUpdateBinds () -> Text = renderMutBinds (planUpdate "users" (Map.fromList [("age", sqlInt 99)]) (QExists "posts" (QEq (QColR "author") (QCol "id"))))

-- An upsert: an INSERT with an `ON CONFLICT … DO UPDATE` tail. The values bind exactly as
-- a plain insert ($1, $2); the conflict clause names the constraint column and sets the
-- update column from `EXCLUDED`, carrying no binds of its own.
pub fn upsertSql () -> Text = renderMutSql (planUpsert "users" [Map.fromList [("id", sqlInt 1), ("name", sqlText "ada")]] ["id"] ["name"])
pub fn upsertBinds () -> Text = renderMutBinds (planUpsert "users" [Map.fromList [("id", sqlInt 1), ("name", sqlText "ada")]] ["id"] ["name"])

-- No update columns is a `DO NOTHING` over the named conflict target.
pub fn insertOrIgnoreSql () -> Text = renderMutSql (planUpsert "users" [Map.fromList [("id", sqlInt 1), ("name", sqlText "ada")]] ["id"] [])

-- No conflict target and no update columns is a bare `ON CONFLICT DO NOTHING`.
pub fn upsertBareSql () -> Text = renderMutSql (planUpsert "users" [Map.fromList [("id", sqlInt 1), ("name", sqlText "ada")]] [] [])

pub fn scanSql () -> Text = renderSql (planScan "users" (pred1 (fn u -> u.age >= 18)) [] (0 - 1) 0 false)

pub fn scanBinds () -> Text = renderBinds (planScan "users" (pred1 (fn u -> u.age >= 18)) [] (0 - 1) 0 false)

-- The optimizer folds the boolean constants out of a predicate before rendering, so a
-- filter carrying a redundant `&& true` compiles to the same SQL as the bare
-- comparison — the always-true arm drops and no clause survives for it.
pub fn foldSql () -> Text = renderSql (planScan "users" (pred1 (fn (u: User) -> u.age >= 18 && true)) [] (0 - 1) 0 false)

-- A text match: `Text.contains u.name "ann"` reifies to `QLike`, rendered as a
-- `LIKE` against a `$1`-bound pattern (`%ann%`, the needle wrapped and escaped).
pub fn likeSql () -> Text = renderSql (planScan "users" (pred1 (fn u -> Text.contains u.name "ann")) [] (0 - 1) 0 false)

pub fn likeBinds () -> Text = renderBinds (planScan "users" (pred1 (fn u -> Text.contains u.name "ann")) [] (0 - 1) 0 false)

-- An `IN` test: `List.contains u.age [18, 21]` reifies to `QIn`, rendered as
-- `IN (...)` over one `$N` placeholder per element, two binds.
pub fn inSql () -> Text = renderSql (planScan "users" (pred1 (fn u -> List.contains u.age [18, 21])) [] (0 - 1) 0 false)

pub fn inBinds () -> Text = renderBinds (planScan "users" (pred1 (fn u -> List.contains u.age [18, 21])) [] (0 - 1) 0 false)

-- The same `IN` test built from a list captured from the enclosing scope: `ages`
-- is a `let`-bound runtime list, so `List.contains u.age ages` lowers through the
-- same `QIn` path and renders to the identical `IN ($1, $2)` over two binds.
pub fn inCapturedSql () -> Text =
    let ages = [18, 21]
    renderSql (planScan "users" (pred1 (fn u -> List.contains u.age ages)) [] (0 - 1) 0 false)

pub fn inCapturedBinds () -> Text =
    let ages = [18, 21]
    renderBinds (planScan "users" (pred1 (fn u -> List.contains u.age ages)) [] (0 - 1) 0 false)

-- A correlated EXISTS: the scan aliases its table `l` so the subquery can name the
-- outer row, the inner table joins at the leaf past the base one (`x1`), and the
-- correlated predicate reads the inner column `x1."author"` against the outer `l."id"`.
-- Built from the QExpr nodes directly — the `exists posts (fn p -> …)` surface needs a
-- captured repo, which this adapter-free SQL test has none of; the surface path is
-- covered end to end by the live-Postgres and in-memory oracles.
pub fn corrExistsSql () -> Text =
    renderSql (planScan "users" (QExists "posts" (QEq (QColR "author") (QCol "id"))) [] (0 - 1) 0 false)

pub fn corrExistsBinds () -> Text =
    renderBinds (planScan "users" (QExists "posts" (QEq (QColR "author") (QCol "id"))) [] (0 - 1) 0 false)

-- The negated form `notExists` reifies to a `QNot` over the same probe, rendering
-- `(NOT EXISTS (…))`; the scan is still aliased `l` since the tree carries an EXISTS.
pub fn corrNotExistsSql () -> Text =
    renderSql (planScan "users" (QNot (QExists "posts" (QEq (QColR "author") (QCol "id")))) [] (0 - 1) 0 false)

-- A correlated EXISTS inside a binary join's post-join WHERE: the inner table joins at
-- the leaf after both join sides (`x2`), and its predicate names the right leaf as `r`
-- (`r."id"`) and the inner row as `x2` (`x2."post"`) — three correlated sources in one
-- subquery.
fn joinExistsWhere () -> QueryPlan =
    planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (QExists "comments" (QEq (QColAt 2 "post") (QColR "id"))) [] (0 - 1) 0 false (leftCols ()) (rightCols ())

pub fn joinExistsWhereSql () -> Text = renderSql (joinExistsWhere ())

-- A correlated EXISTS inside a three-table join's WHERE: the spine flattens to `t0`/
-- `t1`/`t2`, and the EXISTS inner table joins at the next leaf (`x3`), correlating to
-- the deepest spine leaf (`t2."id"`).
fn naryExistsWhere () -> QueryPlan =
    planJoin "INNER"
        (planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (QExists "reactions" (QEq (QColAt 3 "comment") (QColAt 2 "id")))
        []
        (0 - 1) 0 false
        []
        (commentCols ())

pub fn naryExistsWhereSql () -> Text = renderSql (naryExistsWhere ())

-- An EXISTS nested inside another EXISTS's predicate: the outer probe aliases its table
-- `x1`, and the inner probe — reached under the outer's `AND` — aliases its own table
-- `x2`, correlating to the outer probe's row (`x1."id"`). Each level climbs one leaf.
fn nestedExistsTree () -> QExpr =
    QExists "posts" (QAnd (QEq (QColR "author") (QCol "id")) (QExists "comments" (QEq (QColAt 2 "post") (QColR "id"))))

pub fn nestedExistsSql () -> Text = renderSql (planScan "users" (nestedExistsTree ()) [] (0 - 1) 0 false)

-- A single-table scan whose filter nests one EXISTS inside another, with a literal
-- in the innermost correlation and an ASC ordering on the outer leaf. This is what a
-- `filter (fn u -> exists … (fn p -> … && exists … (fn p2 -> … == "world")))` read
-- renders once it takes the plan path, so it locks the leaf aliasing (l / x1 / x2)
-- and the single `$1` bind across the two nesting levels.
fn pgNestedTree () -> QExpr =
    QExists "posts" (QAnd (QEq (QColR "author") (QCol "id")) (QExists "posts" (QAnd (QEq (QColAt 2 "id") (QColR "id")) (QEq (QColAt 2 "title") (QLitText "world")))))

pub fn pgNestedSql () -> Text = renderSql (planScan "users" (pgNestedTree ()) [(true, QCol "id")] (0 - 1) 0 false)

pub fn pgNestedBinds () -> Text = renderBinds (planScan "users" (pgNestedTree ()) [(true, QCol "id")] (0 - 1) 0 false)

-- An empty `IN` set is unsatisfiable, so it renders as the constant `FALSE` rather
-- than the syntactically invalid `IN ()`, and binds nothing.
pub fn inEmptySql () -> Text = renderSql (planScan "users" (pred1 (fn u -> List.contains u.age [])) [] (0 - 1) 0 false)

pub fn inEmptyBinds () -> Text = renderBinds (planScan "users" (pred1 (fn u -> List.contains u.age [])) [] (0 - 1) 0 false)

-- Arithmetic operands render as a parenthesised `(lhs OP rhs)` inside the
-- comparison: a column times a `$1`-bound literal, with the comparison's own
-- literal as `$2`; a column-plus-column with no bind of its own; modulo over a
-- `$1`-bound divisor. The recursive operand renderer threads placeholders left
-- to right, so a literal in either side binds in source order.
pub fn arithMulSql () -> Text = renderSql (planScan "users" (pred1 (fn (u: User) -> u.age * 2 > 50)) [] (0 - 1) 0 false)

pub fn arithMulBinds () -> Text = renderBinds (planScan "users" (pred1 (fn (u: User) -> u.age * 2 > 50)) [] (0 - 1) 0 false)

pub fn arithColSql () -> Text = renderSql (planScan "users" (pred1 (fn (u: User) -> u.age + u.id > 20)) [] (0 - 1) 0 false)

pub fn arithModSql () -> Text = renderSql (planScan "users" (pred1 (fn (u: User) -> u.age % 2 == 0)) [] (0 - 1) 0 false)

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

-- A computed join projection: a bare left column plus an arithmetic right column
-- with a literal. The select-list binds first (`$1`), ahead of the join's own
-- placeholders, proving the select-list threads the counter through the join.
pub fn projectCalcSql () -> Text =
    renderSql (planProject (projCalc (fn (u: User) (p: Post) -> Calc { person = u.name, score = p.id + 1 })) (wrapJoin ()) (0 - 1) 0 false)

pub fn projectCalcBinds () -> Text =
    renderBinds (planProject (projCalc (fn (u: User) (p: Post) -> Calc { person = u.name, score = p.id + 1 })) (wrapJoin ()) (0 - 1) 0 false)

-- A CASE join projection: a per-row label chosen by a condition over the left
-- entity, both branch values bound as placeholders.
pub fn projectCaseJoinSql () -> Text =
    renderSql (planProject (projCaseo (fn (u: User) (p: Post) -> Caseo { person = u.name, band = if u.age >= 18 then "a" else "b" })) (wrapJoin ()) (0 - 1) 0 false)

pub fn aggSql () -> Text =
    renderSql (planAggregate "AVG" (col2 (fn (u: User) (p: Post) -> p.author)) 1 (wrapJoin ()))

pub fn groupSql () -> Text =
    renderSql (planGroup "author" 1 [("author", "KEY", keepAllJoin (), 1), ("n", "COUNT", keepAllJoin (), 0)] (keepAllJoin ()) (wrapJoin ()))

pub fn inner3Sql () -> Text = renderSql (inner3 ())

pub fn inner3Binds () -> Text = renderBinds (inner3 ())

-- An existence probe over a binary join: `SELECT 1 FROM … LIMIT 1`, short-circuiting at the
-- first matching row rather than fetching every column of a row the caller never reads. No
-- presence markers — existence reads none.
pub fn existsSql () -> Text = renderSql (planExists (bareJoin "INNER" (usersScan ())))

-- The same probe over the three-table composite: the flattened spine's FROM/WHERE under
-- `SELECT 1 … LIMIT 1`, the base adult filter still bound as $1.
pub fn existsThreeSql () -> Text = renderSql (planExists (inner3 ()))

pub fn existsThreeBinds () -> Text = renderBinds (planExists (inner3 ()))

-- `every` reuses the same `SELECT 1 … LIMIT 1` probe, but over a violator predicate: a
-- row the receiver keeps whose further predicate is `IS NOT TRUE`. `IS NOT TRUE` (not a
-- plain `NOT`) so a NULL predicate — an outer join's unmatched side — counts as a
-- violation rather than slipping through, the three-valued reading that makes the
-- unmatched row fail `every`. A returned row is one that fails it, so `every` is the
-- emptiness of this probe.
fn everyJoin () -> QueryPlan =
    planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ())
        (QNotTrue (cond2 (fn (u: User) (p: Post) -> p.title == "hello")))
        [] (0 - 1) 0 false (leftCols ()) (rightCols ())

pub fn everyJoinSql () -> Text = renderSql (planExists (everyJoin ()))

pub fn everyJoinBinds () -> Text = renderBinds (planExists (everyJoin ()))

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

-- A mixed chain whose base leaf carries a scan filter under an inner-then-left spine
-- (`adults JOIN posts LEFT JOIN comments`). Only the new comments leaf is optional, so the
-- base `adults` leaf is never null-extended; the renderer lifts its filter to the flat
-- top-level WHERE (qualified to t0) and reads the table directly, rather than wrapping the
-- base in a `(SELECT * FROM "users" WHERE …)` subquery. A subquery survives only where it
-- helps — on the genuinely-optional comments leaf, which still rides in its marker subquery.
fn adultLeftMix () -> QueryPlan =
    planJoin "LEFT"
        (planJoin "INNER" (adultsScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        []
        (0 - 1) 0 false
        []
        (commentCols ())

pub fn adultLeftMixSql () -> Text = renderSql (adultLeftMix ())

pub fn adultLeftMixBinds () -> Text = renderBinds (adultLeftMix ())

-- A count over the same filtered-base inner-then-left composite. The reduction path emits no
-- presence markers, and the base `adults` leaf is never null-extended, so its filter lifts to
-- the flat top-level WHERE rather than a `(SELECT * FROM "users" WHERE …)` subquery — the same
-- "subquery only where it helps" lift the bare terminal makes, now through the aggregate path.
fn countAdultLeftMix () -> QueryPlan = planAggregate "COUNT" (keepAll ()) 0 (adultLeftMix ())

pub fn countAdultLeftMixSql () -> Text = renderSql (countAdultLeftMix ())

-- A count over the three-table inner composite: COUNT(*) over the flattened multi-way
-- join, the base adult filter qualified to t0 binding $1. A reduction selects no leaf
-- columns and reads no markers — just the count.
fn countThree () -> QueryPlan = planAggregate "COUNT" (keepAll ()) 0 (inner3 ())

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

fn countLeftMix () -> QueryPlan = planAggregate "COUNT" (keepAll ()) 0 (leftMixFiltered ())

pub fn countLeftMixSql () -> Text = renderSql (countLeftMix ())

pub fn countLeftMixBinds () -> Text = renderBinds (countLeftMix ())

-- A scalar SUM over the three-table inner composite, folding the deep leaf's column
-- (`post`, leaf 2): `SUM(t2."post")` over the flattened multi-way join, the base adult
-- filter qualified to t0 ($1).
fn sumThree () -> QueryPlan = planAggregate "SUM" (col3 (fn (u: User) (p: Post) (c: Comment) -> c.post)) 2 (inner3 ())

pub fn sumThreeSql () -> Text = renderSql (sumThree ())

-- An AVG over the same composite leaf, carrying the `::float8` cast so an integer column
-- averages to a float, as the single-table and binary-join aggregates do.
fn avgThree () -> QueryPlan = planAggregate "AVG" (col3 (fn (u: User) (p: Post) (c: Comment) -> c.post)) 2 (inner3 ())

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

-- A projection over an outer (mixed-shape) composite: the same leaf-spanning select-list
-- over a chain that joins the third table under a LEFT/RIGHT/FULL step. The renderer reads
-- each null-extended leaf's column directly (NULL, decoded as the shape's Option field), so
-- it needs no presence markers — the marker-free FROM the aggregates also render. Proves a
-- projection pushes down a mixed composite as it does an all-inner one.
fn projectLeftMix () -> QueryPlan =
    planProject (proj3 (fn (u: User) (p: Post) (c: Comment) -> Trio { who = u.name, what = p.title, note = c.body })) (innerLeftMix ()) (0 - 1) 0 false

fn projectRightMix () -> QueryPlan =
    planProject (proj3 (fn (u: User) (p: Post) (c: Comment) -> Trio { who = u.name, what = p.title, note = c.body })) (innerRightMix ()) (0 - 1) 0 false

fn projectFullMix () -> QueryPlan =
    planProject (proj3 (fn (u: User) (p: Post) (c: Comment) -> Trio { who = u.name, what = p.title, note = c.body })) (innerFullMix ()) (0 - 1) 0 false

pub fn projectLeftMixSql () -> Text = renderSql (projectLeftMix ())

pub fn projectRightMixSql () -> Text = renderSql (projectRightMix ())

pub fn projectFullMixSql () -> Text = renderSql (projectFullMix ())

-- A grouped summary over the three-table inner composite: a `PlanGroup` keyed on a
-- middle leaf (post title, t1) summarising a COUNT and a SUM that folds the first
-- leaf's column (user age, t0). Proves the renderer leaf-qualifies the group key and
-- each aggregate independently over the flattened multi-way join, GROUP BY and ORDER BY
-- carrying the same key expression, the base adult filter qualified to t0 ($1).
fn groupThree () -> QueryPlan =
    planGroup "title" 1 [("label", "KEY", keepAllJoin (), 1), ("n", "COUNT", keepAllJoin (), 0), ("ages", "SUM", col3 (fn (u: User) (p: Post) (c: Comment) -> u.age), 0)] (keepAllJoin ()) (inner3 ())

pub fn groupThreeSql () -> Text = renderSql (groupThree ())

-- The same grouped composite, but the SUM folds a COMPUTED expression (user age
-- doubled) rather than a bare column. The SELECT list renders first, so the literal
-- 2 binds as $1 and the base adult filter shifts to $2 — proving a computed grouped
-- aggregate parameterises its literals and threads them ahead of the WHERE.
fn groupComputedThree () -> QueryPlan =
    planGroup "title" 1 [("label", "KEY", keepAllJoin (), 1), ("n", "COUNT", keepAllJoin (), 0), ("ages", "SUM", col3 (fn (u: User) (p: Post) (c: Comment) -> u.age * 2), 0)] (keepAllJoin ()) (inner3 ())

pub fn groupComputedThreeSql () -> Text = renderSql (groupComputedThree ())

pub fn groupComputedThreeBinds () -> Text = renderBinds (groupComputedThree ())

-- A grouped summary over an outer (mixed-shape) composite: the same leaf-qualified
-- GROUP BY over a chain that joins the third table under a LEFT/RIGHT/FULL step. The
-- group reads the null-extended leaves directly (no presence markers, the marker-free
-- FROM the aggregates render). The LEFT case keys on a left leaf (t0), the RIGHT on the
-- new leaf (t2), the FULL on a left leaf with the base scan riding inside its subquery.
fn groupLeftMix () -> QueryPlan =
    planGroup "name" 0 [("label", "KEY", keepAllJoin (), 0), ("n", "COUNT", keepAllJoin (), 0)] (keepAllJoin ()) (innerLeftMix ())

fn groupRightMix () -> QueryPlan =
    planGroup "body" 2 [("label", "KEY", keepAllJoin (), 2), ("n", "COUNT", keepAllJoin (), 0)] (keepAllJoin ()) (innerRightMix ())

fn groupFullMix () -> QueryPlan =
    planGroup "name" 0 [("label", "KEY", keepAllJoin (), 0), ("n", "COUNT", keepAllJoin (), 0)] (keepAllJoin ()) (innerFullMix ())

pub fn groupLeftMixSql () -> Text = renderSql (groupLeftMix ())

pub fn groupRightMixSql () -> Text = renderSql (groupRightMix ())

pub fn groupFullMixSql () -> Text = renderSql (groupFullMix ())

-- A bare three-table inner composite carrying an `orderBy` on a deeper leaf: a one-key
-- ordering over the comment body (leaf 2), ascending. The renderer qualifies the key to
-- its leaf alias t2 and emits ORDER BY after the flattened multi-way join, the dual of a
-- binary join's leaf-tagged ORDER BY generalised to an unbounded leaf.
fn orderThree () -> QueryPlan =
    planJoin "INNER"
        (planJoin "INNER" (adultsScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        [(true, 2, col3 (fn (u: User) (p: Post) (c: Comment) -> c.body))]
        (0 - 1) 0 false
        []
        (commentCols ())

pub fn orderThreeSql () -> Text = renderSql (orderThree ())

-- The same ordering over the three mixed-shape steps: a LEFT chain ordered by the base
-- user name (leaf 0) descending, a RIGHT chain by the always-present comment body (leaf
-- 2) ascending, a FULL chain by the post title (leaf 1) descending. The key qualifies to
-- its leaf alias regardless of which leaf the step null-extends.
fn orderLeftMix () -> QueryPlan =
    planJoin "LEFT"
        (planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        [(false, 0, col2 (fn (u: User) (p: Post) -> u.name))]
        (0 - 1) 0 false
        []
        (commentCols ())

fn orderRightMix () -> QueryPlan =
    planJoin "RIGHT"
        (planJoin "INNER" (usersScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        [(true, 2, col3 (fn (u: User) (p: Post) (c: Comment) -> c.body))]
        (0 - 1) 0 false
        []
        (commentCols ())

fn orderFullMix () -> QueryPlan =
    planJoin "FULL"
        (planJoin "INNER" (adultsScan ()) (postsScan ()) (joinCond ()) (keepAllJoin ()) [] (0 - 1) 0 false (leftCols ()) (rightCols ()))
        (commentsScan ())
        (joinCond2 ())
        (keepAllJoin ())
        [(false, 1, col2 (fn (u: User) (p: Post) -> p.title))]
        (0 - 1) 0 false
        []
        (commentCols ())

pub fn orderLeftMixSql () -> Text = renderSql (orderLeftMix ())

pub fn orderRightMixSql () -> Text = renderSql (orderRightMix ())

pub fn orderFullMixSql () -> Text = renderSql (orderFullMix ())

-- A four-table inner join: the three-table composite joined again to reactions. The left
-- child is itself a composite `PlanJoin`, so the renderer flattens the whole tree into one
-- flat multi-way join over leaf aliases t0/t1/t2/t3. The fourth table's join condition
-- names its column via `r`, reifying to `QColAt 3`, qualified to t3; the base scan filters
-- adults, so the leaf filter qualifies to t0 and binds $1. Proves the spine scales past
-- three leaves with no per-depth machinery.
fn inner4 () -> QueryPlan =
    planJoin "INNER"
        (inner3 ())
        (reactionsScan ())
        (joinCond3 ())
        (keepAllJoin ())
        []
        (0 - 1) 0 false
        []
        (reactionCols ())

pub fn inner4Sql () -> Text = renderSql (inner4 ())

-- A scalar SUM over the four-table inner composite, folding the deepest leaf's column
-- (`comment`, leaf 3): `SUM(t3."comment")` over the flattened four-way join, the base adult
-- filter qualified to t0 ($1).
fn sumFour () -> QueryPlan = planAggregate "SUM" (col4 (fn (u: User) (p: Post) (c: Comment) (r: Reaction) -> r.comment)) 3 (inner4 ())

pub fn sumFourSql () -> Text = renderSql (sumFour ())

-- A projection over the four-table inner composite naming one column from each leaf into
-- `Quad`. The deepest leaf reifies to a `QColAt 3` cell the renderer qualifies to t3, so the
-- select-list reads `t0."name" AS "who", t1."title" AS "what", t2."body" AS "note",
-- t3."kind" AS "react"` over the flattened four-way join.
fn projectFour () -> QueryPlan =
    planProject (proj4 (fn (u: User) (p: Post) (c: Comment) (r: Reaction) -> Quad { who = u.name, what = p.title, note = c.body, react = r.kind })) (inner4 ()) (0 - 1) 0 false

pub fn projectFourSql () -> Text = renderSql (projectFour ())

-- A bare four-table inner composite carrying an `orderBy` on the deepest leaf: a one-key
-- ordering over the reaction kind (leaf 3), ascending. The renderer qualifies the key to
-- its leaf alias t3 and emits ORDER BY after the flattened four-way join.
fn orderFour () -> QueryPlan =
    planJoin "INNER"
        (inner3 ())
        (reactionsScan ())
        (joinCond3 ())
        (keepAllJoin ())
        [(true, 3, col4 (fn (u: User) (p: Post) (c: Comment) (r: Reaction) -> r.kind))]
        (0 - 1) 0 false
        []
        (reactionCols ())

pub fn orderFourSql () -> Text = renderSql (orderFour ())
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
         lists:foreach(F,['scanSql','scanBinds','foldSql','likeSql','likeBinds','inSql','inBinds','inCapturedSql','inCapturedBinds','corrExistsSql','corrExistsBinds','corrNotExistsSql','joinExistsWhereSql','naryExistsWhereSql','nestedExistsSql','pgNestedSql','pgNestedBinds','inEmptySql','inEmptyBinds','arithMulSql','arithMulBinds','arithColSql','arithModSql','combineSql','refineSql','innerSql','leftSql','rightSql','fullSql','fullBinds','projectSql','projectCalcSql','projectCalcBinds','projectCaseJoinSql','aggSql','groupSql','inner3Sql','inner3Binds','existsSql','existsThreeSql','existsThreeBinds','everyJoinSql','everyJoinBinds','innerLeftMixSql','innerRightMixSql','innerFullMixSql','innerFullMixBinds','adultLeftMixSql','adultLeftMixBinds','countAdultLeftMixSql','countThreeSql','countThreeBinds','countLeftMixSql','countLeftMixBinds','sumThreeSql','avgThreeSql','projectThreeSql','projectLeftMixSql','projectRightMixSql','projectFullMixSql','groupThreeSql','groupComputedThreeSql','groupComputedThreeBinds','groupLeftMixSql','groupRightMixSql','groupFullMixSql','orderThreeSql','orderLeftMixSql','orderRightMixSql','orderFullMixSql','inner4Sql','sumFourSql','projectFourSql','orderFourSql','insertSql','insertBinds','insertManySql','insertManyBinds','updateSql','updateBinds','deleteSql','existsDeleteSql','existsDeleteBinds','existsUpdateSql','existsUpdateBinds','upsertSql','upsertBinds','insertOrIgnoreSql','upsertBareSql']), halt()."
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

    // A text match renders to `LIKE` over a `$1`-bound pattern; an `IN` test to
    // `IN (...)` over one placeholder per element; an empty `IN` to `FALSE`.
    want(r#"likeSql=SELECT * FROM "users" WHERE "name" LIKE $1"#);
    want("likeBinds=1");
    want(r#"inSql=SELECT * FROM "users" WHERE "age" IN ($1, $2)"#);
    want("inBinds=2");
    want(r#"inCapturedSql=SELECT * FROM "users" WHERE "age" IN ($1, $2)"#);
    want("inCapturedBinds=2");

    // A correlated EXISTS aliases the scanned table `l` and the subquery's table `x1`
    // (the leaf past the single base), qualifying the inner column to `x1` and the
    // correlated outer column to `l`; no binds. `notExists` wraps it in `(NOT EXISTS (…))`.
    want(
        r#"corrExistsSql=SELECT * FROM "users" AS l WHERE EXISTS (SELECT 1 FROM "posts" AS x1 WHERE x1."author" = l."id")"#,
    );
    want("corrExistsBinds=0");

    // The typed write renderer (`mutationToSql`): an INSERT lists its columns and binds
    // each value; an UPDATE binds SET before WHERE; a DELETE binds only WHERE. A
    // correlated EXISTS in an update or delete predicate aliases the target `l` and
    // renders the subquery exactly as a read does — the write path shares `renderPred`,
    // so EXISTS works in a mutation with no separate subquery support (the cw text
    // builder it replaces could not nest a correlated subquery here).
    want(r#"insertSql=INSERT INTO "users" ("id", "name") VALUES ($1, $2)"#);
    want("insertBinds=2");
    // A bulk INSERT folds many same-shape rows into one statement: the shared column
    // list once, then a parenthesised `$N` tuple per row, the binds threaded across rows.
    want(r#"insertManySql=INSERT INTO "users" ("id", "name") VALUES ($1, $2), ($3, $4)"#);
    want("insertManyBinds=4");
    want(r#"updateSql=UPDATE "users" SET "age" = $1 WHERE "id" = $2"#);
    want("updateBinds=2");
    want(r#"deleteSql=DELETE FROM "users" WHERE "age" < $1"#);
    want(
        r#"existsDeleteSql=DELETE FROM "users" AS l WHERE EXISTS (SELECT 1 FROM "posts" AS x1 WHERE x1."author" = l."id")"#,
    );
    want("existsDeleteBinds=0");
    want(
        r#"existsUpdateSql=UPDATE "users" AS l SET "age" = $1 WHERE EXISTS (SELECT 1 FROM "posts" AS x1 WHERE x1."author" = l."id")"#,
    );
    want("existsUpdateBinds=1");

    // An upsert renders the insert's columns and `$N` values, then an `ON CONFLICT` tail.
    // With update columns it is a `DO UPDATE` setting each from `EXCLUDED` (no extra
    // binds); with none it is a `DO NOTHING`, whose conflict target is optional — a bare
    // `ON CONFLICT DO NOTHING` when no columns are named.
    want(
        r#"upsertSql=INSERT INTO "users" ("id", "name") VALUES ($1, $2) ON CONFLICT ("id") DO UPDATE SET "name" = EXCLUDED."name""#,
    );
    want("upsertBinds=2");
    want(
        r#"insertOrIgnoreSql=INSERT INTO "users" ("id", "name") VALUES ($1, $2) ON CONFLICT ("id") DO NOTHING"#,
    );
    want(
        r#"upsertBareSql=INSERT INTO "users" ("id", "name") VALUES ($1, $2) ON CONFLICT DO NOTHING"#,
    );

    want(
        r#"corrNotExistsSql=SELECT * FROM "users" AS l WHERE (NOT EXISTS (SELECT 1 FROM "posts" AS x1 WHERE x1."author" = l."id"))"#,
    );

    // An EXISTS inside a binary join's WHERE: the inner table joins at `x2` (past both
    // join leaves), correlating to the right leaf `r` and its own row `x2`.
    want(
        r#"joinExistsWhereSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author" WHERE (EXISTS (SELECT 1 FROM "comments" AS x2 WHERE x2."post" = r."id"))"#,
    );
    // An EXISTS inside a three-table join's WHERE: the spine reads `t0`/`t1`/`t2`, and
    // the inner table joins at `x3`, correlating to the deepest spine leaf `t2`.
    want(
        r#"naryExistsWhereSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (EXISTS (SELECT 1 FROM "reactions" AS x3 WHERE x3."comment" = t2."id"))"#,
    );
    // An EXISTS nested in another EXISTS: the outer probe aliases `x1`, the inner `x2`,
    // each correlating one leaf up (`x2."post" = x1."id"`, `x1."author" = l."id"`).
    want(
        r#"nestedExistsSql=SELECT * FROM "users" AS l WHERE EXISTS (SELECT 1 FROM "posts" AS x1 WHERE (x1."author" = l."id" AND EXISTS (SELECT 1 FROM "comments" AS x2 WHERE x2."post" = x1."id")))"#,
    );
    // A single-table read whose filter nests one EXISTS inside another: the base leaf
    // aliases `l`, the outer probe `x1`, the inner `x2`, and the lone literal binds `$1`.
    want(
        r#"pgNestedSql=SELECT * FROM "users" AS l WHERE EXISTS (SELECT 1 FROM "posts" AS x1 WHERE (x1."author" = l."id" AND EXISTS (SELECT 1 FROM "posts" AS x2 WHERE (x2."id" = x1."id" AND x2."title" = $1)))) ORDER BY "id" ASC"#,
    );
    want("pgNestedBinds=1");
    want(r#"inEmptySql=SELECT * FROM "users" WHERE FALSE"#);
    want("inEmptyBinds=0");
    want("scanBinds=1");

    // Arithmetic operands render as a parenthesised `(lhs OP rhs)` inside the
    // comparison; a literal in either side binds left to right.
    want(r#"arithMulSql=SELECT * FROM "users" WHERE ("age" * $1) > $2"#);
    want("arithMulBinds=2");
    want(r#"arithColSql=SELECT * FROM "users" WHERE ("age" + "id") > $1"#);
    want(r#"arithModSql=SELECT * FROM "users" WHERE ("age" % $1) = $2"#);

    // The optimizer folds the `&& true` away before rendering, so the filter compiles to
    // the same SQL as the bare comparison — no redundant `AND (TRUE)` survives.
    want(r#"foldSql=SELECT * FROM "users" WHERE "age" >= $1"#);

    // A set-operation combine wraps each branch in parens around the keyword; a
    // refine wraps the combination in a subquery and re-applies the outer WHERE. The
    // unfiltered branch carries no WHERE — the optimizer drops its always-true default.
    want(r#"combineSql=(SELECT * FROM "users" WHERE "age" >= $1) UNION (SELECT * FROM "users")"#);
    // The `$N` counter threads across the whole plan: the inner combine's filter
    // binds `$1`, so the outer refine's filter binds `$2`.
    want(
        r#"refineSql=SELECT * FROM ((SELECT * FROM "users" WHERE "age" >= $1) UNION (SELECT * FROM "users")) AS ridge_sub WHERE "age" >= $2"#,
    );

    // An inner join: each source's columns prefixed (`t0$`/`t1$`), the condition
    // qualified to its side, no marker. With no post-join filters the WHERE drops out.
    want(
        r#"innerSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author""#,
    );

    // A left join wraps the right table in the `__present` marker subquery and
    // selects the marker as `t1$__present__`; its trivial post-join WHERE drops out.
    want(
        r#"leftSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title", r."__present" AS "t1$__present__" FROM "users" AS l LEFT JOIN (SELECT *, TRUE AS "__present" FROM "posts") AS r ON l."id" = r."author""#,
    );

    // A right join wraps the left table and folds the left filter into the ON; with no
    // left filter and no post-join WHERE the ON carries the bare condition alone.
    want(
        r#"rightSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", l."__present" AS "t0$__present__", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title" FROM (SELECT *, TRUE AS "__present" FROM "users") AS l RIGHT JOIN "posts" AS r ON (l."id" = r."author")"#,
    );

    // A full join wraps both sides; the left filter goes inside the left subquery
    // and compiles with bare column names (so `$1`, one bind). The trivial top-level
    // WHERE drops out, leaving the bare join condition.
    want(
        r#"fullSql=SELECT l."id" AS "t0$id", l."age" AS "t0$age", l."name" AS "t0$name", l."__present" AS "t0$__present__", r."id" AS "t1$id", r."author" AS "t1$author", r."title" AS "t1$title", r."__present" AS "t1$__present__" FROM (SELECT *, TRUE AS "__present" FROM "users" WHERE ("age" >= $1)) AS l FULL JOIN (SELECT *, TRUE AS "__present" FROM "posts") AS r ON (l."id" = r."author")"#,
    );
    want("fullBinds=1");

    // A projected join: the projection's own aliased select-list, no prefixing.
    want(
        r#"projectSql=SELECT l."name" AS "person", r."title" AS "post" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author""#,
    );

    // A computed join projection: the arithmetic column renders side-qualified and
    // binds its literal as `$1` in the select-list, ahead of the join's clauses.
    want(
        r#"projectCalcSql=SELECT l."name" AS "person", (r."id" + $1) AS "score" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author""#,
    );
    want("projectCalcBinds=1");

    // A CASE join projection: the condition reads side-qualified, both branch values
    // bind as placeholders, and the whole `CASE … END` is aliased.
    want(
        r#"projectCaseJoinSql=SELECT l."name" AS "person", CASE WHEN l."age" >= $1 THEN $2 ELSE $3 END AS "band" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author""#,
    );

    // A scalar aggregate over a join: the side-qualified column, AVG cast to float8.
    want(
        r#"aggSql=SELECT AVG(r."author")::float8 FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author""#,
    );

    // A grouped join: the side-qualified key, COUNT(*), GROUP BY and ORDER BY the key.
    want(
        r#"groupSql=SELECT r."author" AS "author", COUNT(*) AS "n" FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author" GROUP BY r."author" ORDER BY r."author""#,
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

    // An existence probe renders `SELECT 1 FROM … LIMIT 1`: it short-circuits at the first
    // matching row and transfers one trivial column instead of every column of a row the
    // caller discards. The binary join keeps its ON; no presence markers, no select-list.
    want(
        r#"existsSql=SELECT 1 FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author" LIMIT 1"#,
    );
    // The same probe over the three-table composite reuses the flattened spine's FROM and
    // WHERE (the base adult filter still bound as $1) under `SELECT 1 … LIMIT 1`.
    want(
        r#"existsThreeSql=SELECT 1 FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1) LIMIT 1"#,
    );
    want("existsThreeBinds=1");

    // `every` runs the same probe over a violator predicate: a kept row whose further
    // predicate is `IS NOT TRUE`. The renderer emits `(<pred> IS NOT TRUE)`, the
    // three-valued test that flags a row where the predicate is false or NULL — so an
    // outer join's unmatched side fails `every` rather than slipping through a plain
    // `NOT`. `every` is true exactly when this probe returns no row.
    want(
        r#"everyJoinSql=SELECT 1 FROM "users" AS l JOIN "posts" AS r ON l."id" = r."author" WHERE ((r."title" = $1 IS NOT TRUE)) LIMIT 1"#,
    );
    want("everyJoinBinds=1");

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

    // The same inner-then-left chain with a filtered base: the base `adults` leaf is never
    // null-extended, so its filter lifts to the flat top-level WHERE (`t0."age" >= $1`) and
    // `t0` reads `"users"` directly — no `(SELECT * FROM "users" WHERE …)` wrapper. Only the
    // optional comments leaf keeps its marker subquery, so a subquery survives only where it
    // helps. Contrast `innerFullMixSql`, whose FULL step makes the base nullable and keeps it.
    want(
        r#"adultLeftMixSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body", t2."__present" AS "t2$__present__" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" LEFT JOIN (SELECT *, TRUE AS "__present" FROM "comments") AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1)"#,
    );
    want("adultLeftMixBinds=1");

    // The same filtered-base lift through the marker-free aggregate path: COUNT(*) over the
    // composite, the base `adults` leaf bare with its filter in the flat top-level WHERE.
    want(
        r#"countAdultLeftMixSql=SELECT COUNT(*) FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" LEFT JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1)"#,
    );

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

    // The same projection over a mixed-shape composite renders the third leaf under its
    // own join kind and reads each null-extendable leaf's column directly, no presence
    // markers — the marker-free FROM the aggregates render. The select-list is identical
    // regardless of the step kind; only the FROM changes.
    want(
        r#"projectLeftMixSql=SELECT t0."name" AS "who", t1."title" AS "what", t2."body" AS "note" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" LEFT JOIN "comments" AS t2 ON t1."id" = t2."post""#,
    );
    want(
        r#"projectRightMixSql=SELECT t0."name" AS "who", t1."title" AS "what", t2."body" AS "note" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" RIGHT JOIN "comments" AS t2 ON t1."id" = t2."post""#,
    );
    // A full step's base scan rides inside its own subquery so its adult filter restricts
    // which base rows enter the join (correct under a FULL JOIN that would otherwise keep a
    // null-extended row the filter rejects), binding $1 — the same base-scan handling the
    // mixed bare terminals and aggregates use.
    want(
        r#"projectFullMixSql=SELECT t0."name" AS "who", t1."title" AS "what", t2."body" AS "note" FROM (SELECT * FROM "users" WHERE "age" >= $1) AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" FULL JOIN "comments" AS t2 ON t1."id" = t2."post""#,
    );

    // A grouped summary over the three-table composite leaf-qualifies the group key (t1)
    // and each aggregate independently — COUNT(*) over the rows, SUM folding the first
    // leaf's column (t0) — with GROUP BY and ORDER BY carrying the key expression and the
    // base adult filter bound to $1, the grouped dual of the composite projection.
    want(
        r#"groupThreeSql=SELECT t1."title" AS "label", COUNT(*) AS "n", SUM(t0."age") AS "ages" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1) GROUP BY t1."title" ORDER BY t1."title""#,
    );

    // The computed grouped fold: SUM over `t0."age" * 2`. The SELECT renders first, so
    // the literal 2 binds as $1 and the base adult filter shifts to $2 — the literal is
    // parameterised, never spliced into the statement, and the two placeholder runs
    // stay in textual order.
    want(
        r#"groupComputedThreeSql=SELECT t1."title" AS "label", COUNT(*) AS "n", SUM((t0."age" * $1)) AS "ages" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $2) GROUP BY t1."title" ORDER BY t1."title""#,
    );
    want("groupComputedThreeBinds=2");

    // The same grouped summary over a mixed-shape composite renders the third leaf under
    // its own join kind and qualifies the key to whichever leaf names it — a left leaf (t0)
    // for the LEFT/FULL cases, the new leaf (t2) for the RIGHT case — reading the
    // null-extended leaves directly with no presence markers, the FULL base scan riding
    // inside its subquery.
    want(
        r#"groupLeftMixSql=SELECT t0."name" AS "label", COUNT(*) AS "n" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" LEFT JOIN "comments" AS t2 ON t1."id" = t2."post" GROUP BY t0."name" ORDER BY t0."name""#,
    );
    want(
        r#"groupRightMixSql=SELECT t2."body" AS "label", COUNT(*) AS "n" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" RIGHT JOIN "comments" AS t2 ON t1."id" = t2."post" GROUP BY t2."body" ORDER BY t2."body""#,
    );
    want(
        r#"groupFullMixSql=SELECT t0."name" AS "label", COUNT(*) AS "n" FROM (SELECT * FROM "users" WHERE "age" >= $1) AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" FULL JOIN "comments" AS t2 ON t1."id" = t2."post" GROUP BY t0."name" ORDER BY t0."name""#,
    );

    // An `orderBy` over the three-table inner composite emits ORDER BY after the
    // flattened multi-way join, the key qualified to its leaf alias t2 with its direction.
    want(
        r#"orderThreeSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" WHERE (t0."age" >= $1) ORDER BY t2."body" ASC"#,
    );

    // The same ORDER BY over the mixed-shape composites: the key qualifies to whichever
    // leaf names it regardless of which leaf the step null-extends — t0 (left) for LEFT,
    // t2 (new leaf) for RIGHT, t1 (middle) for FULL with the base scan in its subquery.
    want(
        r#"orderLeftMixSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body", t2."__present" AS "t2$__present__" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" LEFT JOIN (SELECT *, TRUE AS "__present" FROM "comments") AS t2 ON t1."id" = t2."post" ORDER BY t0."name" DESC"#,
    );
    want(
        r#"orderRightMixSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t0."__present" AS "t0$__present__", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t1."__present" AS "t1$__present__", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body" FROM (SELECT *, TRUE AS "__present" FROM "users") AS t0 JOIN (SELECT *, TRUE AS "__present" FROM "posts") AS t1 ON t0."id" = t1."author" RIGHT JOIN "comments" AS t2 ON t1."id" = t2."post" ORDER BY t2."body" ASC"#,
    );
    want(
        r#"orderFullMixSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t0."__present" AS "t0$__present__", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t1."__present" AS "t1$__present__", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body", t2."__present" AS "t2$__present__" FROM (SELECT *, TRUE AS "__present" FROM "users" WHERE "age" >= $1) AS t0 JOIN (SELECT *, TRUE AS "__present" FROM "posts") AS t1 ON t0."id" = t1."author" FULL JOIN (SELECT *, TRUE AS "__present" FROM "comments") AS t2 ON t1."id" = t2."post" ORDER BY t1."title" DESC"#,
    );

    // Depth 4: the four-table inner composite flattens into one multi-way join over leaf
    // aliases t0/t1/t2/t3 — the fourth table joined on `t2."id" = t3."comment"` (its
    // condition naming the deep leaf via `QColAt 3`), the base adult filter qualified to t0
    // ($1). The aggregate, projection, and ORDER BY each name the deepest leaf and qualify
    // it to t3, proving the spine scales past three leaves with no per-depth machinery.
    want(
        r#"inner4Sql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body", t3."id" AS "t3$id", t3."comment" AS "t3$comment", t3."kind" AS "t3$kind" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" JOIN "reactions" AS t3 ON t2."id" = t3."comment" WHERE (t0."age" >= $1)"#,
    );
    want(
        r#"sumFourSql=SELECT SUM(t3."comment") FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" JOIN "reactions" AS t3 ON t2."id" = t3."comment" WHERE (t0."age" >= $1)"#,
    );
    want(
        r#"projectFourSql=SELECT t0."name" AS "who", t1."title" AS "what", t2."body" AS "note", t3."kind" AS "react" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" JOIN "reactions" AS t3 ON t2."id" = t3."comment" WHERE (t0."age" >= $1)"#,
    );
    want(
        r#"orderFourSql=SELECT t0."id" AS "t0$id", t0."age" AS "t0$age", t0."name" AS "t0$name", t1."id" AS "t1$id", t1."author" AS "t1$author", t1."title" AS "t1$title", t2."id" AS "t2$id", t2."post" AS "t2$post", t2."body" AS "t2$body", t3."id" AS "t3$id", t3."comment" AS "t3$comment", t3."kind" AS "t3$kind" FROM "users" AS t0 JOIN "posts" AS t1 ON t0."id" = t1."author" JOIN "comments" AS t2 ON t1."id" = t2."post" JOIN "reactions" AS t3 ON t2."id" = t3."comment" WHERE (t0."age" >= $1) ORDER BY t3."kind" ASC"#,
    );
}
