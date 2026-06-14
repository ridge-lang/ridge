//! End-to-end check for the std.data Postgres adapter against a real database.
//!
//! A `Repo User Postgres` pairs a live Postgres connection with the
//! `ridge_pg_users` table and the `User` entity, then runs the same repository
//! surface the in-memory adapter does — clearing the table, seeding three users,
//! and reading them back decoded through `deriving (Row)`. It proves the wire
//! client connects, authenticates, runs parameterised insert/select/delete, and
//! decodes `RowDescription`/`DataRow` into the entity. The query builder is
//! covered too — `orderBy` (ORDER BY) and `limit`/`offset` (LIMIT/OFFSET) compile
//! into real SQL. The two-table verbs run against the live database as well:
//! `joinOn` + `toList`/`select` (the `JOIN` and the `l.*, r.*` split),
//! `leftJoinOn` + `toList` (the `LEFT JOIN` and its `__ridge_matched`
//! sentinel that keeps unmatched left rows), and `leftJoinOn` + `select`
//! (a `LEFT JOIN` with a pushed-down select-list whose NULL right columns decode
//! to `None`). A final probe drives the connection pool with six concurrent reads
//! on one handle (concurrent checkout, growth, waiter reuse).
//!
//! Gated three ways: the `beam-runtime` feature, a `which` guard for `erl`/`erlc`,
//! and the `RIDGE_TEST_PG_URL` environment variable. Without a reachable database
//! the test skips rather than fails, so the default `cargo test` run is
//! unaffected. The URL is the usual libpq shape:
//!
//!   <postgres://user:password@host:5432/dbname?sslmode=require>
//!
//! `sslmode` is optional and defaults to `disable`. The target database must hold
//! a table `ridge_pg_users (id integer, name text, age integer)`, for the join
//! probes `ridge_pg_posts (id integer, author integer, title text)`, and for the
//! grouped-aggregate probes `ridge_pg_emps (id integer, dept text, salary
//! integer)`; CI provisions all three on the Postgres service, and a local run
//! expects them to exist.
//!
//! The grouped aggregates run against the live database too: `groupBy` +
//! `summarize` compile to `SELECT <aggregates> … GROUP BY <key> ORDER BY <key>`
//! (count, sum, average, and a min/max range per group), and `having` re-renders
//! the aggregate into a real `HAVING` clause, including a filter-then-group case
//! where the `WHERE` binds precede the `HAVING` bind. `distinct` projections
//! compile to `SELECT DISTINCT <cols> …`, dropping the repeated dept and salary
//! columns to their distinct values. The set operations compile to real
//! `UNION`/`UNION ALL`/`INTERSECT`/`EXCEPT` (each branch a subquery, the bind
//! placeholders threaded across the statement), with an outer filter wrapping the
//! combination in a subquery and nested unions nesting the parentheses.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// The program source, with connection settings spliced in as sentinels so the
/// Ridge record braces never collide with Rust string formatting.
const SOURCE_TEMPLATE: &str = r#"
import std.data (connect, Config, Postgres)
import std.repo as Repo
import std.query (SortOrder, Asc, Desc)
import std.sql (toSql, SqlValue)
import std.map as Map
import std.int as Int
import std.float as Float

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

-- A second entity for the join, in the `ridge_pg_posts` table; `author` holds the
-- owning user's id.
pub type Post = { id: Int, author: Int, title: Text } deriving (Row)

-- A projected shape: the projection renames `name` -> `who` and `age` -> `years`,
-- so the select-list compiles to `name AS who, age AS years` and the decode reads
-- the aliased columns back.
pub type Summary = { who: Text, years: Int } deriving (Row)

-- The shape a join projection decodes into: a name from the left entity and a
-- title from the right.
pub type Combo = { person: Text, post: Text } deriving (Row)

-- The shape a left-join projection decodes into: `post` is `Option Text`, so an
-- unmatched left row's NULL right column decodes to `None`.
pub type ComboOpt = { person: Text, post: Option Text } deriving (Row)

fn joinNames (us: List User) -> Text =
    match us
        []        -> ""
        u :: []   -> u.name
        u :: rest -> Text.concat u.name (Text.concat "," (joinNames rest))

fn joinWho (ss: List Summary) -> Text =
    match ss
        []        -> ""
        s :: []   -> s.who
        s :: rest -> Text.concat s.who (Text.concat "," (joinWho rest))

-- Render each `(User, Post)` pair as `name:title`, comma-joined.
fn joinPairs (ps: List (User, Post)) -> Text =
    match ps
        []             -> ""
        (u, p) :: []   -> Text.concat u.name (Text.concat ":" p.title)
        (u, p) :: rest -> Text.concat u.name (Text.concat ":" (Text.concat p.title (Text.concat "," (joinPairs rest))))

-- Render each projected `Combo` as `person:post`, comma-joined.
fn joinCombos (cs: List Combo) -> Text =
    match cs
        []          -> ""
        c :: []     -> Text.concat c.person (Text.concat ":" c.post)
        c :: rest   -> Text.concat c.person (Text.concat ":" (Text.concat c.post (Text.concat "," (joinCombos rest))))

-- An optional projected title, or "-" when the column was NULL.
fn optText (o: Option Text) -> Text =
    match o
        None   -> "-"
        Some s -> s

-- Render each projected `ComboOpt` as `person:post` (or `person:-`), comma-joined.
fn joinComboOpts (cs: List ComboOpt) -> Text =
    match cs
        []          -> ""
        c :: []     -> Text.concat c.person (Text.concat ":" (optText c.post))
        c :: rest   -> Text.concat c.person (Text.concat ":" (Text.concat (optText c.post) (Text.concat "," (joinComboOpts rest))))

-- The title of an optional right post, or "-" when the left row matched none.
fn optTitle (op: Option Post) -> Text =
    match op
        None   -> "-"
        Some p -> p.title

-- Render each `(User, Option Post)` pair as `name:title` (or `name:-`),
-- comma-joined, so a left join's unmatched left rows are observable.
fn joinLeftPairs (ps: List (User, Option Post)) -> Text =
    match ps
        []              -> ""
        (u, op) :: []   -> Text.concat u.name (Text.concat ":" (optTitle op))
        (u, op) :: rest -> Text.concat u.name (Text.concat ":" (Text.concat (optTitle op) (Text.concat "," (joinLeftPairs rest))))

fn pgConfig () -> Config =
    Config { host = "__PG_HOST__", port = __PG_PORT__, database = "__PG_DATABASE__", user = "__PG_USER__", password = "__PG_PASSWORD__", sslMode = "__PG_SSLMODE__", poolSize = 4 }

pub fn userRow (uid: Int) (uage: Int) (uname: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql uid), ("age", toSql uage), ("name", toSql uname)]

pub fn postRow (pid: Int) (pauthor: Int) (ptitle: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql pid), ("author", toSql pauthor), ("title", toSql ptitle)]

fn listLen (xs: List x) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + listLen rest

-- Connect, bind a repository to the table, clear any prior rows, and seed three
-- users; return the repository so each probe queries a known, isolated state.
pub fn db setup () -> Result (Repo User Postgres) Error =
    match connect (pgConfig ())
        Err e   -> Err e
        Ok conn ->
            let r = Repo.repo conn "ridge_pg_users"
            match Repo.deleteWhere (fn (u: User) -> u.id >= 0) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insertRow (userRow 1 18 "ada") r
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insertRow (userRow 2 30 "lin") r
                                Err e -> Err e
                                Ok _  ->
                                    match Repo.insertRow (userRow 3 25 "max") r
                                        Err e -> Err e
                                        Ok _  -> Ok r

-- count: the whole table -> 3
pub fn db countAll () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match Repo.count r
                Ok n  -> n
                Err _ -> 0 - 2

-- findBy + decode: how many users are 25 or older? (lin 30, max 25) -> 2
pub fn db adultsCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.findBy (fn (u: User) -> u.age >= 25)
                Ok us -> listLen us
                Err _ -> 0 - 2

-- find + decode: the name of the first user older than 28 -> "lin"
pub fn db firstName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.find (fn (u: User) -> u.age > 28)
                Err _       -> "find-err"
                Ok None     -> "none"
                Ok (Some u) -> u.name

-- get by key + decode: the user with id 2 -> "lin"
pub fn db getName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some u) -> u.name

-- delete then count what remains: drop the under-25 row, two remain -> 2
pub fn db afterDelete () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.deleteWhere (fn (u: User) -> u.age < 25)
                Err _ -> 0 - 2
                Ok _  ->
                    match Repo.count r
                        Ok n  -> n
                        Err _ -> 0 - 3

-- builder: whole table ordered by age descending, names joined -> "lin,max,ada".
-- Proves the backend compiles ORDER BY into the query.
pub fn db orderedNames () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- builder: age-ascending, offset 1 then limit 1 -> "max". Proves LIMIT and OFFSET
-- compile into the query.
pub fn db pagedName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.age) |> Repo.offset 1 |> Repo.limit 1 |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- projection: order by age descending, project into the renamed `Summary`, and
-- join the `who` fields -> "lin,max,ada". Proves the backend compiles the
-- select-list (`name AS who, age AS years`) and decodes the aliased columns.
pub fn db summaryNames () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.select (fn (u: User) -> Summary { who = u.name, years = u.age })
                Err _ -> "list-err"
                Ok ss -> joinWho ss

-- projection: order by age descending, take the first summary, read its renamed
-- `years` column -> 30 (lin). Proves selectFirst pushes the projection + LIMIT 1.
pub fn db topYears () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.selectFirst (fn (u: User) -> Summary { who = u.name, years = u.age })
                Err _       -> 0 - 2
                Ok None     -> 0 - 3
                Ok (Some s) -> s.years

-- Connect, bind a users and a posts repository to the live tables, clear both,
-- and seed three users plus one post each for lin (id 2 -> "hello") and max
-- (id 3 -> "world"); ada (id 1) gets none. Return both repositories.
pub fn db setupJoin () -> Result (Repo User Postgres, Repo Post Postgres) Error =
    match connect (pgConfig ())
        Err e   -> Err e
        Ok conn ->
            let users: Repo User Postgres = Repo.repo conn "ridge_pg_users"
            let posts: Repo Post Postgres = Repo.repo conn "ridge_pg_posts"
            match Repo.deleteWhere (fn (u: User) -> u.id >= 0) users
                Err e -> Err e
                Ok _  ->
                    match Repo.deleteWhere (fn (p: Post) -> p.id >= 0) posts
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insertRow (userRow 1 18 "ada") users
                                Err e -> Err e
                                Ok _  ->
                                    match Repo.insertRow (userRow 2 30 "lin") users
                                        Err e -> Err e
                                        Ok _  ->
                                            match Repo.insertRow (userRow 3 25 "max") users
                                                Err e -> Err e
                                                Ok _  ->
                                                    match Repo.insertRow (postRow 10 2 "hello") posts
                                                        Err e -> Err e
                                                        Ok _  ->
                                                            match Repo.insertRow (postRow 11 3 "world") posts
                                                                Err e -> Err e
                                                                Ok _  -> Ok (users, posts)

-- join: inner-join users to their posts on `u.id == p.author`, ordered by user
-- id, rendered `name:title` per pair -> "lin:hello,max:world" (ada has no post,
-- so the inner join drops it). Proves the backend compiles the JOIN, qualifies
-- the condition columns, and splits each `l.*, r.*` row back into two entities.
pub fn db joinedNames () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.toList
                Err _  -> "join-err"
                Ok ps  -> joinPairs ps

-- join projection: the same join projected into `Combo { person, post }`
-- -> "lin:hello,max:world". Proves the backend compiles a qualified, aliased
-- select-list (`l.name AS person, r.title AS post`) and decodes it.
pub fn db joinedTitles () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.select (fn (u: User) (p: Post) -> Combo { person = u.name, post = p.title })
                Err _  -> "select-err"
                Ok cs  -> joinCombos cs

-- join ordered by a RIGHT column, descending: the same inner join ordered by post
-- title `DESC` (a right-table column) -> "max:world,lin:hello", the reverse of the
-- id order, so the ordering is driven by the right column rather than the left.
-- Proves the backend qualifies the `ORDER BY` key to the right table alias (`r`).
pub fn db joinOrderByRight () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Desc (fn (u: User) (p: Post) -> p.title) |> Repo.toList
                Err _  -> "join-order-err"
                Ok ps  -> joinPairs ps

-- left join: keep every user, pairing each with its post or with `None`, ordered
-- by user id, rendered `name:title` (or `name:-`) -> "ada:-,lin:hello,max:world".
-- ada has no post, so where the inner join dropped it the left join keeps it as
-- `ada:-`. Proves the backend compiles a `LEFT JOIN`, tells an unmatched row from
-- a matched one through the `__ridge_matched` sentinel, and decodes the right as
-- `Option`.
pub fn db leftJoinedNames () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.toList
                Err _  -> "left-join-err"
                Ok ps  -> joinLeftPairs ps

-- left-join projection: the same left join projected into `ComboOpt { person,
-- post }` where `post` is `Option Text` -> "ada:-,lin:hello,max:world". ada's
-- projected `post` column is NULL (no match) and decodes to `None` (`ada:-`).
-- Proves the backend compiles a `LEFT JOIN` with a pushed-down select-list and
-- the NULL right column decodes into the shape's Option field.
pub fn db leftSelectTitles () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.select (fn (u: User) (p: Option Post) -> ComboOpt { person = u.name, post = p.title })
                Err _  -> "left-select-err"
                Ok cs  -> joinComboOpts cs

-- join + limit against Postgres: the inner join ordered by the post id (hello 10,
-- world 11), keeping the first pair -> "lin:hello". Proves the backend compiles the
-- join's own LIMIT (carried on the `Join`), bounding the joined result.
pub fn db joinLimited () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id) |> Repo.limit 1 |> Repo.toList
                Err _  -> "join-limit-err"
                Ok ps  -> joinPairs ps

-- join + offset + limit against Postgres: the same ordered join, skipping the first
-- pair and keeping one -> "max:world". Proves LIMIT and OFFSET compile on a join.
pub fn db joinOffsetLimited () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id) |> Repo.offset 1 |> Repo.limit 1 |> Repo.toList
                Err _  -> "join-page-err"
                Ok ps  -> joinPairs ps

-- join + distinct against Postgres: `distinct` over the inner join, ordered by post
-- id -> "lin:hello,max:world". The two pairs are already distinct, so the result is
-- unchanged: this proves the backend compiles `SELECT DISTINCT l.*, r.*` over the
-- join and runs it.
pub fn db joinDistinctAll () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.distinct |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id) |> Repo.toList
                Err _  -> "join-distinct-err"
                Ok ps  -> joinPairs ps

-- left join + limit against Postgres: the left join with the user-id order lifted
-- from the query (ada 1, lin 2, max 3), keeping the first two rows ->
-- "ada:-,lin:hello". Proves the backend compiles a `LEFT JOIN … LIMIT`, the
-- kept-but-unmatched ada row included in the page.
pub fn db leftJoinLimited () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.limit 2 |> Repo.toList
                Err _  -> "left-limit-err"
                Ok ps  -> joinLeftPairs ps

-- Connect, clear, and seed three users with the TYPED `insert` — the entity is
-- encoded to a row through `toRow` and the backend compiles the parameterised
-- INSERT, with no hand-built column map.
pub fn db setupInsert () -> Result (Repo User Postgres) Error =
    match connect (pgConfig ())
        Err e   -> Err e
        Ok conn ->
            let r = Repo.repo conn "ridge_pg_users"
            match Repo.deleteWhere (fn (u: User) -> u.id >= 0) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insert (User { id = 1, age = 18, name = "ada" }) r
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insert (User { id = 2, age = 30, name = "lin" }) r
                                Err e -> Err e
                                Ok _  ->
                                    match Repo.insert (User { id = 3, age = 25, name = "max" }) r
                                        Err e -> Err e
                                        Ok _  -> Ok r

-- insert round-trips through Postgres: names ascending by id -> "ada,lin,max".
-- Proves `toRow` encodes the entity and the backend's INSERT + read-back agree.
pub fn db addedNames () -> Text =
    match setupInsert ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- typed update against Postgres: overwrite ada (id 1) with a full entity (age 99)
-- and read her age back -> 99. Proves the backend compiles UPDATE … SET … WHERE
-- from `toRow` + the predicate.
pub fn db updatedAge () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.update (User { id = 1, age = 99, name = "ada" }) (fn (u: User) -> u.id == 1)
                Err _ -> 0 - 2
                Ok _  ->
                    match r |> Repo.getBy "id" (toSql 1)
                        Err _       -> 0 - 3
                        Ok None     -> 0 - 4
                        Ok (Some u) -> u.age

-- partial update against Postgres: set age = 40 on every adult and read lin's age
-- back -> 40. Proves the backend compiles a partial SET whose `$1` bind precedes
-- the WHERE clause's `$2`, so the two placeholder runs never collide.
pub fn db bumpedAge () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.updateWhere (Map.fromList [("age", toSql 40)]) (fn (u: User) -> u.age >= 25)
                Err _ -> 0 - 2
                Ok _  ->
                    match r |> Repo.getBy "id" (toSql 2)
                        Err _       -> 0 - 3
                        Ok None     -> 0 - 4
                        Ok (Some u) -> u.age

-- the column the partial update did NOT touch: lin's name is still "lin".
pub fn db bumpedName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.updateWhere (Map.fromList [("age", toSql 40)]) (fn (u: User) -> u.age >= 25)
                Err _ -> "update-err"
                Ok _  ->
                    match r |> Repo.getBy "id" (toSql 2)
                        Err _       -> "get-err"
                        Ok None     -> "none"
                        Ok (Some u) -> u.name

-- partial update changed-count: two adults (lin 30, max 25) match -> 2.
pub fn db updateWhereCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.updateWhere (Map.fromList [("age", toSql 40)]) (fn (u: User) -> u.age >= 25)
                Ok n  -> n
                Err _ -> 0 - 2

-- typed partial update against Postgres: `setWhere` sets `age = 40` on the adults
-- through a typed setter, then reads lin's age back -> 40. Proves the backend
-- compiles the same UPDATE … SET … WHERE for a typed setter as for the raw map,
-- with the SET bind preceding the WHERE bind.
pub fn db setBumpedAge () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.setWhere [ Repo.set (fn (u: User) -> u.age) 40 ] (fn (u: User) -> u.age >= 25)
                Err _ -> 0 - 2
                Ok _  ->
                    match r |> Repo.getBy "id" (toSql 2)
                        Err _       -> 0 - 3
                        Ok None     -> 0 - 4
                        Ok (Some u) -> u.age

-- typed update through the query builder against Postgres: `applySet` filters to
-- ada (id 1) and assigns her name "neo"; read it back -> "neo".
pub fn db appliedName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 1) |> Repo.applySet [ Repo.set (fn (u: User) -> u.name) "neo" ]
                Err _ -> "set-err"
                Ok _  ->
                    match r |> Repo.getBy "id" (toSql 1)
                        Err _       -> "get-err"
                        Ok None     -> "none"
                        Ok (Some u) -> u.name

-- Render an optional Int as its text, or "none" for an empty aggregate.
fn optIntText (o: Option Int) -> Text =
    match o
        None   -> "none"
        Some n -> Int.toText n

-- Render an optional Float as its text, or "none".
fn optFloatText (o: Option Float) -> Text =
    match o
        None   -> "none"
        Some f -> Float.toText f

-- Render an optional Text as itself, or "none".
fn optTextText (o: Option Text) -> Text =
    match o
        None   -> "none"
        Some s -> s

-- aggregate against Postgres: SUM(age) over the whole table (18 + 30 + 25) -> "73".
-- Proves the backend compiles the aggregate into the query and the bigint result
-- decodes back to `Int`.
pub fn db sumAllAges () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.sumOf (fn (u: User) -> u.age)
                Err _ -> "sum-err"
                Ok o  -> optIntText o

-- aggregate against Postgres: SUM(age) bounded by the filter to the adults
-- (30 + 25) -> "55". Proves the WHERE clause bounds the aggregate.
pub fn db sumAdultAges () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 25) |> Repo.sumOf (fn (u: User) -> u.age)
                Err _ -> "sum-err"
                Ok o  -> optIntText o

-- aggregate against Postgres: AVG(age) over the adults ((30 + 25) / 2) -> "27.5".
-- Proves AVG is cast to float8 so an integer column's average crosses as a float.
pub fn db avgAdultAges () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 25) |> Repo.avgOf (fn (u: User) -> u.age)
                Err _ -> "avg-err"
                Ok o  -> optFloatText o

-- aggregate against Postgres: MIN(age) over the whole table -> "18".
pub fn db minAllAges () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.minOf (fn (u: User) -> u.age)
                Err _ -> "min-err"
                Ok o  -> optIntText o

-- aggregate against Postgres: MAX(name) folds a text column lexicographically
-- (ada < lin < max) -> "max".
pub fn db maxName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.maxOf (fn (u: User) -> u.name)
                Err _ -> "max-err"
                Ok o  -> optTextText o

-- aggregate against Postgres over an empty match (no user older than 100) ->
-- "none". Proves a SQL aggregate of zero rows is NULL, decoded to `None`.
pub fn db sumNobody () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age > 100) |> Repo.sumOf (fn (u: User) -> u.age)
                Err _ -> "sum-err"
                Ok o  -> optIntText o

-- join aggregate over a RIGHT column against Postgres: SUM(r.id) over the inner
-- join (lin->hello(10), max->world(11)) -> 10+11 = "21". Proves the backend
-- compiles `SUM(r."id")` over `l JOIN r` and qualifies the column to the right
-- table alias through the `aggregateJoin` seam.
pub fn db joinSumRightId () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> p.id)
                Err _ -> "join-sum-err"
                Ok o  -> optIntText o

-- join aggregate over a LEFT column against Postgres: SUM(l.age) over the inner
-- join -> 30+25 = "55" (each of lin, max owns one post; ada is dropped). Proves the
-- backend qualifies a left-column aggregate to the `l` alias.
pub fn db joinSumLeftAge () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> u.age)
                Err _ -> "join-sum-err"
                Ok o  -> optIntText o

-- join aggregate over a RIGHT text column against Postgres: MAX(r.title) over the
-- inner join (hello < world) -> "world". Proves MAX folds a qualified right text
-- column and keeps its type.
pub fn db joinMaxRightTitle () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.maxOf (fn (u: User) (p: Post) -> p.title)
                Err _ -> "join-max-err"
                Ok o  -> optTextText o

-- join aggregate average over a RIGHT column against Postgres: AVG(r.id)::float8
-- over the inner join ((10+11)/2) -> "10.5". Proves avgOf over a join casts to
-- float8 and decodes the fractional result.
pub fn db joinAvgRightId () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.avgOf (fn (u: User) (p: Post) -> p.id)
                Err _ -> "join-avg-err"
                Ok o  -> optFloatText o

-- left-join aggregate over a LEFT column against Postgres: SUM(l.age) over the LEFT
-- join, which keeps the unmatched ada -> 18+30+25 = "73". The discriminator: the
-- inner join's same sum is "55" (ada excluded), so "73" proves the backend compiles
-- a `LEFT JOIN` whose left-column aggregate counts the kept-but-unmatched row.
pub fn db leftJoinSumLeftAge () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> u.age)
                Err _ -> "left-sum-err"
                Ok o  -> optIntText o

-- left-join aggregate over a RIGHT column against Postgres: MAX(r.title) over the
-- LEFT join -> "world". ada's right columns are NULL (skipped by the aggregate), so
-- only the matched titles fold. Proves the `LEFT JOIN` right-column aggregate needs
-- no matched sentinel — SQL's NULL handling drops the unmatched row on its own.
pub fn db leftJoinMaxRightTitle () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.maxOf (fn (u: User) (p: Post) -> p.title)
                Err _ -> "left-max-err"
                Ok o  -> optTextText o

-- The name of an optional user, or "none" for an empty match.
fn optUserName (o: Option User) -> Text =
    match o
        None   -> "none"
        Some u -> u.name

-- Render a boolean as text, so an `every` result is observable as one string.
fn boolText (b: Bool) -> Text =
    if b then "true" else "false"

-- single against Postgres: exactly one match (id 2) -> "lin". Proves the two-row
-- LIMIT fetch decodes the lone row.
pub fn db singleOne () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 2) |> Repo.single
                Err e -> e.code
                Ok o  -> optUserName o

-- single against Postgres: no match (id 99) -> "none". The empty result is `Ok None`.
pub fn db singleNone () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 99) |> Repo.single
                Err e -> e.code
                Ok o  -> optUserName o

-- single against Postgres: more than one match (the whole table) ->
-- "repo.single.many". Proves the non-unique result fails with that code.
pub fn db singleMany () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.single
                Err e -> e.code
                Ok o  -> optUserName o

-- singleOrError against Postgres: exactly one (id 1) -> "ada".
pub fn db oneOrErr () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 1) |> Repo.singleOrError
                Err e -> e.code
                Ok u  -> u.name

-- singleOrError against Postgres: no match (id 99) -> "repo.single.empty". The
-- empty result is an error here, where `single` returns None.
pub fn db noneOrErr () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 99) |> Repo.singleOrError
                Err e -> e.code
                Ok u  -> u.name

-- every against Postgres: are all users adults? (18, 30, 25 all >= 18) -> "true".
pub fn db everyAdult () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.every (fn (u: User) -> u.age >= 18)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- every against Postgres: are all users at least 26? (ada 18 and max 25 fail) -> "false".
pub fn db everyHigh () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.every (fn (u: User) -> u.age >= 26)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- every against Postgres over an empty selection (no user with id 99) -> "true"
-- (vacuous truth).
pub fn db everyEmpty () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 99) |> Repo.every (fn (u: User) -> u.age >= 18)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- A grouping dataset in the `ridge_pg_emps` table: employees with a repeated
-- `dept` key and a salary, so a real GROUP BY partitions several rows per group.
pub type Emp = { id: Int, dept: Text, salary: Int } deriving (Row)

-- The summarised shapes a `groupBy` projects into, decoded back from the
-- aggregate columns the backend pushes down.
pub type DeptCount = { dept: Text, n: Int } deriving (Row)
pub type DeptSum   = { dept: Text, total: Int } deriving (Row)
pub type DeptAvg   = { dept: Text, mean: Float } deriving (Row)
pub type DeptRange = { dept: Text, lo: Int, hi: Int } deriving (Row)

-- Single-column shapes the distinct projections decode into.
pub type DeptName = { dept: Text } deriving (Row)
pub type SalAmt   = { salary: Int } deriving (Row)

pub fn empRow (eid: Int) (edept: Text) (esalary: Int) -> Map Text SqlValue =
    Map.fromList [("id", toSql eid), ("dept", toSql edept), ("salary", toSql esalary)]

-- Connect, clear the emps table, and seed six employees across three departments:
-- eng {100, 200}, sales {150, 150, 300}, ops {50}.
pub fn db setupEmps () -> Result (Repo Emp Postgres) Error =
    match connect (pgConfig ())
        Err e   -> Err e
        Ok conn ->
            let r = Repo.repo conn "ridge_pg_emps"
            match Repo.deleteWhere (fn (em: Emp) -> em.id >= 0) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insertRow (empRow 1 "eng" 100) r
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insertRow (empRow 2 "eng" 200) r
                                Err e -> Err e
                                Ok _  ->
                                    match Repo.insertRow (empRow 3 "sales" 150) r
                                        Err e -> Err e
                                        Ok _  ->
                                            match Repo.insertRow (empRow 4 "sales" 150) r
                                                Err e -> Err e
                                                Ok _  ->
                                                    match Repo.insertRow (empRow 5 "sales" 300) r
                                                        Err e -> Err e
                                                        Ok _  ->
                                                            match Repo.insertRow (empRow 6 "ops" 50) r
                                                                Err e -> Err e
                                                                Ok _  -> Ok r

-- Render the grouped result rows as `key:value` cells. Postgres orders the groups
-- by the key (the backend appends ORDER BY <key>), so the string is deterministic.
fn countCells (rows: List DeptCount) -> Text =
    match rows
        []        -> ""
        r :: []   -> Text.concat r.dept (Text.concat ":" (Int.toText r.n))
        r :: rest -> Text.concat r.dept (Text.concat ":" (Text.concat (Int.toText r.n) (Text.concat "," (countCells rest))))

fn sumCells (rows: List DeptSum) -> Text =
    match rows
        []        -> ""
        r :: []   -> Text.concat r.dept (Text.concat ":" (Int.toText r.total))
        r :: rest -> Text.concat r.dept (Text.concat ":" (Text.concat (Int.toText r.total) (Text.concat "," (sumCells rest))))

fn avgCells (rows: List DeptAvg) -> Text =
    match rows
        []        -> ""
        r :: []   -> Text.concat r.dept (Text.concat ":" (Float.toText r.mean))
        r :: rest -> Text.concat r.dept (Text.concat ":" (Text.concat (Float.toText r.mean) (Text.concat "," (avgCells rest))))

fn rangeCell (r: DeptRange) -> Text =
    Text.concat r.dept (Text.concat ":" (Text.concat (Int.toText r.lo) (Text.concat "-" (Int.toText r.hi))))

fn rangeCells (rows: List DeptRange) -> Text =
    match rows
        []        -> ""
        r :: []   -> rangeCell r
        r :: rest -> Text.concat (rangeCell r) (Text.concat "," (rangeCells rest))

fn deptList (rows: List DeptName) -> Text =
    match rows
        []        -> ""
        r :: []   -> r.dept
        r :: rest -> Text.concat r.dept (Text.concat "," (deptList rest))

fn salList (rows: List SalAmt) -> Text =
    match rows
        []        -> ""
        r :: []   -> Int.toText r.salary
        r :: rest -> Text.concat (Int.toText r.salary) (Text.concat "," (salList rest))

-- Render the ids of a row list, comma-joined. The set-op probes order by id, so
-- the rendered string is deterministic.
fn idList (rows: List Emp) -> Text =
    match rows
        []        -> ""
        e :: []   -> Int.toText e.id
        e :: rest -> Text.concat (Int.toText e.id) (Text.concat "," (idList rest))

-- group + summarize against Postgres: COUNT(*) per dept -> "eng:2,ops:1,sales:3".
pub fn db groupCounts () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group + summarize against Postgres: SUM(salary) per dept -> "eng:300,ops:50,sales:600".
pub fn db groupSums () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (e: Emp) -> e.salary) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- group + summarize against Postgres: AVG(salary)::float8 per dept ->
-- "eng:150.0,ops:50.0,sales:200.0".
pub fn db groupAvgs () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptAvg { dept = g.key, mean = g.avg (fn (e: Emp) -> e.salary) })
                Err _   -> "group-err"
                Ok rows -> avgCells rows

-- group + summarize against Postgres: MIN/MAX(salary) per dept ->
-- "eng:100-200,ops:50-50,sales:150-300".
pub fn db groupRanges () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptRange { dept = g.key, lo = g.min (fn (e: Emp) -> e.salary), hi = g.max (fn (e: Emp) -> e.salary) })
                Err _   -> "group-err"
                Ok rows -> rangeCells rows

-- group + having (COUNT) against Postgres: depts with more than one member ->
-- "eng:2,sales:3". The HAVING re-renders COUNT(*) rather than an output alias.
pub fn db groupHavingCount () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.having (fn g -> g.count > 1) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group + having (SUM) against Postgres: depts whose payroll is >= 600 ->
-- "sales:600". Proves HAVING re-renders SUM(salary) with its own bind parameter.
pub fn db groupHavingSum () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.having (fn g -> g.sum (fn (e: Emp) -> e.salary) >= 600) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (e: Emp) -> e.salary) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- filter + group + having against Postgres: the WHERE drops ops's lone 50, then
-- the surviving rows group and keep depts with > 1 member -> "eng:2,sales:3".
-- Proves the WHERE binds precede the HAVING bind in the compiled statement.
pub fn db groupFilteredHaving () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 100) |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.having (fn g -> g.count > 1) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- selectList without distinct against Postgres: every dept, ordered by dept ->
-- "eng,eng,ops,sales,sales,sales". The baseline the distinct probe contrasts with.
pub fn db deptsAll () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (e: Emp) -> e.dept) |> Repo.select (fn (e: Emp) -> DeptName { dept = e.dept })
                Err _   -> "err"
                Ok rows -> deptList rows

-- distinct + selectList against Postgres: `SELECT DISTINCT dept` ordered ->
-- "eng,ops,sales". Proves Postgres dedups the repeated dept column.
pub fn db deptsDistinct () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.distinct |> Repo.orderBy Asc (fn (e: Emp) -> e.dept) |> Repo.select (fn (e: Emp) -> DeptName { dept = e.dept })
                Err _   -> "err"
                Ok rows -> deptList rows

-- distinct over a numeric column against Postgres, ordered ascending ->
-- "50,100,150,200,300". The two sales rows at 150 collapse to one.
pub fn db salariesDistinct () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.distinct |> Repo.orderBy Asc (fn (e: Emp) -> e.salary) |> Repo.select (fn (e: Emp) -> SalAmt { salary = e.salary })
                Err _   -> "err"
                Ok rows -> salList rows

-- Set operations compiled to SQL on Postgres. A = salary >= 150 (ids 2,3,4,5),
-- B = salary <= 150 (ids 1,3,4,6); ids 3 and 4 are in both. Each orders the
-- combined result by id so the rendered ids are deterministic.

-- union -> "1,2,3,4,5,6". Compiles to `(SELECT … WHERE …) UNION (SELECT … WHERE …)`
-- wrapped by the outer ORDER BY, with the WHERE binds of each branch threaded so
-- the `$N` placeholders never collide.
pub fn db unionIds () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.union b |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows

-- intersect -> "3,4" (a SQL `INTERSECT`).
pub fn db intersectIds () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.intersect b |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows

-- except -> "2,5" (a SQL `EXCEPT`; the piped-in query is the left side).
pub fn db exceptIds () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.except b |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows

-- unionAll -> 8 rows (a SQL `UNION ALL`, keeping the shared rows).
pub fn db unionAllCount () -> Int =
    match setupEmps ()
        Err _ -> 0 - 1
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.unionAll b |> Repo.toList
                Err _   -> 0 - 1
                Ok rows -> listLen rows

-- filter after a union -> "2,5". The outer filter compiles to a wrapping subquery
-- `SELECT * FROM (… UNION …) AS sub WHERE …`.
pub fn db unionFiltered () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.union b |> Repo.filter (fn (e: Emp) -> e.salary >= 200) |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows

-- nested unions -> "1,2,3,4,5,6". Compiles to nested parenthesised `UNION`s.
pub fn db nestedUnionIds () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let eng = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.dept == "eng")
            let sales = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.dept == "sales")
            let ops = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.dept == "ops")
            match eng |> Repo.union sales |> Repo.union ops |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows
"#;

/// Connection settings parsed out of `RIDGE_TEST_PG_URL`.
struct PgParts<'a> {
    host: &'a str,
    port: u16,
    user: &'a str,
    password: &'a str,
    database: &'a str,
    sslmode: &'a str,
}

/// Parse `postgres://user:password@host:port/database?sslmode=mode`. The scheme,
/// userinfo, host, and database are required; the port defaults to 5432 and
/// `sslmode` to `disable`.
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
        "[workspace]\nname = \"data-pg-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn postgres_adapter_reads_a_real_table() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping postgres_adapter_reads_a_real_table");
        return;
    }
    let url = match std::env::var("RIDGE_TEST_PG_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("RIDGE_TEST_PG_URL not set — skipping postgres_adapter_reads_a_real_table");
            return;
        }
    };
    let parts = parse_pg_url(&url)
        .unwrap_or_else(|| panic!("RIDGE_TEST_PG_URL is not a postgres:// URL: {url}"));
    let source = render_source(&parts);

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-pg-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-pg-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), &source);

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

    // Drive the connection pool directly: open one handle with room for four
    // connections, fire six reads at once, and confirm they all come back. This
    // exercises concurrent checkout, the pool growing under load, and waiters
    // reusing a connection once it frees — all against the live database.
    let pool_probe = format!(
        "{{ok, ProbeConn}} = ridge_pg:pg_connect(<<\"{host}\">>, {port}, <<\"{db}\">>, <<\"{user}\">>, <<\"{pass}\">>, <<\"{ssl}\">>, 4), \
         ProbeId = maps:get(id, ProbeConn), \
         ProbeSelf = self(), \
         [spawn(fun() -> ProbeSelf ! {{probe, ridge_pg:pg_all(ProbeId, <<\"ridge_pg_users\">>)}} end) || _ <- lists:seq(1, 6)], \
         ProbeRs = [receive {{probe, ProbeX}} -> ProbeX after 15000 -> timeout end || _ <- lists:seq(1, 6)], \
         ProbeOk = lists:all(fun(ProbeR) -> case ProbeR of {{ok, _}} -> true; _ -> false end end, ProbeRs), \
         io:format(\"concurrent=~p~n\", [ProbeOk]), \
         ridge_pg:pg_close(ProbeId), ",
        host = parts.host,
        port = parts.port,
        db = parts.database,
        user = parts.user,
        pass = parts.password,
        ssl = parts.sslmode,
    );
    let expr = format!(
        "io:format(\"countAll=~w~n\",[{module}:countAll()]), \
         io:format(\"adultsCount=~w~n\",[{module}:adultsCount()]), \
         io:format(\"firstName=~s~n\",[{module}:firstName()]), \
         io:format(\"getName=~s~n\",[{module}:getName()]), \
         io:format(\"afterDelete=~w~n\",[{module}:afterDelete()]), \
         io:format(\"orderedNames=~s~n\",[{module}:orderedNames()]), \
         io:format(\"pagedName=~s~n\",[{module}:pagedName()]), \
         io:format(\"summaryNames=~s~n\",[{module}:summaryNames()]), \
         io:format(\"topYears=~w~n\",[{module}:topYears()]), \
         io:format(\"joinedNames=~s~n\",[{module}:joinedNames()]), \
         io:format(\"joinedTitles=~s~n\",[{module}:joinedTitles()]), \
         io:format(\"joinOrderByRight=~s~n\",[{module}:joinOrderByRight()]), \
         io:format(\"leftJoinedNames=~s~n\",[{module}:leftJoinedNames()]), \
         io:format(\"leftSelectTitles=~s~n\",[{module}:leftSelectTitles()]), \
         io:format(\"joinLimited=~s~n\",[{module}:joinLimited()]), \
         io:format(\"joinOffsetLimited=~s~n\",[{module}:joinOffsetLimited()]), \
         io:format(\"joinDistinctAll=~s~n\",[{module}:joinDistinctAll()]), \
         io:format(\"leftJoinLimited=~s~n\",[{module}:leftJoinLimited()]), \
         io:format(\"addedNames=~s~n\",[{module}:addedNames()]), \
         io:format(\"updatedAge=~w~n\",[{module}:updatedAge()]), \
         io:format(\"bumpedAge=~w~n\",[{module}:bumpedAge()]), \
         io:format(\"bumpedName=~s~n\",[{module}:bumpedName()]), \
         io:format(\"updateWhereCount=~w~n\",[{module}:updateWhereCount()]), \
         io:format(\"setBumpedAge=~w~n\",[{module}:setBumpedAge()]), \
         io:format(\"appliedName=~s~n\",[{module}:appliedName()]), \
         io:format(\"sumAllAges=~s~n\",[{module}:sumAllAges()]), \
         io:format(\"sumAdultAges=~s~n\",[{module}:sumAdultAges()]), \
         io:format(\"avgAdultAges=~s~n\",[{module}:avgAdultAges()]), \
         io:format(\"minAllAges=~s~n\",[{module}:minAllAges()]), \
         io:format(\"maxName=~s~n\",[{module}:maxName()]), \
         io:format(\"sumNobody=~s~n\",[{module}:sumNobody()]), \
         io:format(\"joinSumRightId=~s~n\",[{module}:joinSumRightId()]), \
         io:format(\"joinSumLeftAge=~s~n\",[{module}:joinSumLeftAge()]), \
         io:format(\"joinMaxRightTitle=~s~n\",[{module}:joinMaxRightTitle()]), \
         io:format(\"joinAvgRightId=~s~n\",[{module}:joinAvgRightId()]), \
         io:format(\"leftJoinSumLeftAge=~s~n\",[{module}:leftJoinSumLeftAge()]), \
         io:format(\"leftJoinMaxRightTitle=~s~n\",[{module}:leftJoinMaxRightTitle()]), \
         io:format(\"singleOne=~s~n\",[{module}:singleOne()]), \
         io:format(\"singleNone=~s~n\",[{module}:singleNone()]), \
         io:format(\"singleMany=~s~n\",[{module}:singleMany()]), \
         io:format(\"oneOrErr=~s~n\",[{module}:oneOrErr()]), \
         io:format(\"noneOrErr=~s~n\",[{module}:noneOrErr()]), \
         io:format(\"everyAdult=~s~n\",[{module}:everyAdult()]), \
         io:format(\"everyHigh=~s~n\",[{module}:everyHigh()]), \
         io:format(\"everyEmpty=~s~n\",[{module}:everyEmpty()]), \
         io:format(\"groupCounts=~s~n\",[{module}:groupCounts()]), \
         io:format(\"groupSums=~s~n\",[{module}:groupSums()]), \
         io:format(\"groupAvgs=~s~n\",[{module}:groupAvgs()]), \
         io:format(\"groupRanges=~s~n\",[{module}:groupRanges()]), \
         io:format(\"groupHavingCount=~s~n\",[{module}:groupHavingCount()]), \
         io:format(\"groupHavingSum=~s~n\",[{module}:groupHavingSum()]), \
         io:format(\"groupFilteredHaving=~s~n\",[{module}:groupFilteredHaving()]), \
         io:format(\"deptsAll=~s~n\",[{module}:deptsAll()]), \
         io:format(\"deptsDistinct=~s~n\",[{module}:deptsDistinct()]), \
         io:format(\"salariesDistinct=~s~n\",[{module}:salariesDistinct()]), \
         io:format(\"unionIds=~s~n\",[{module}:unionIds()]), \
         io:format(\"intersectIds=~s~n\",[{module}:intersectIds()]), \
         io:format(\"exceptIds=~s~n\",[{module}:exceptIds()]), \
         io:format(\"unionAllCount=~w~n\",[{module}:unionAllCount()]), \
         io:format(\"unionFiltered=~s~n\",[{module}:unionFiltered()]), \
         io:format(\"nestedUnionIds=~s~n\",[{module}:nestedUnionIds()]), \
         {pool_probe} \
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

    for (probe, want) in [
        ("countAll=3", "count answers the whole seeded table"),
        ("adultsCount=2", "findBy keeps the two rows with age >= 25"),
        (
            "firstName=lin",
            "find + deriving (Row) decodes the first row older than 28",
        ),
        ("getName=lin", "get by id 2 decodes to lin"),
        (
            "afterDelete=2",
            "two rows remain after deleting the under-25 row",
        ),
        (
            "orderedNames=lin,max,ada",
            "the builder compiles ORDER BY age DESC into the query",
        ),
        (
            "pagedName=max",
            "the builder compiles LIMIT and OFFSET into the query",
        ),
        (
            "summaryNames=lin,max,ada",
            "selectList compiles the renamed select-list and decodes it in age order",
        ),
        (
            "topYears=30",
            "selectFirst pushes the projection with LIMIT 1 and decodes `years`",
        ),
        (
            "joinedNames=lin:hello,max:world",
            "pg_join compiles the JOIN and splits each l.*, r.* row back into two entities",
        ),
        (
            "joinedTitles=lin:hello,max:world",
            "pg_join_select compiles a qualified, aliased select-list and decodes the Combo shape",
        ),
        (
            "joinOrderByRight=max:world,lin:hello",
            "the unified orderBy qualifies the ORDER BY key to the right table (r.title DESC), reversing the id order",
        ),
        (
            "leftJoinedNames=ada:-,lin:hello,max:world",
            "pg_left_join keeps the unmatched ada row via the __ridge_matched sentinel and decodes the right as Option",
        ),
        (
            "leftSelectTitles=ada:-,lin:hello,max:world",
            "selectLeftJoin keeps the unmatched ada row and decodes its NULL right column into None",
        ),
        (
            "joinLimited=lin:hello",
            "the backend compiles the join's own LIMIT, keeping the first post-id-ordered pair",
        ),
        (
            "joinOffsetLimited=max:world",
            "LIMIT and OFFSET compile on a join (skip hello, keep world)",
        ),
        (
            "joinDistinctAll=lin:hello,max:world",
            "the backend compiles SELECT DISTINCT l.*, r.* over the join and keeps the two distinct pairs",
        ),
        (
            "leftJoinLimited=ada:-,lin:hello",
            "the backend compiles a LEFT JOIN with LIMIT, the unmatched ada row included in the page",
        ),
        (
            "addedNames=ada,lin,max",
            "insert encodes each entity through toRow and the backend's INSERT round-trips",
        ),
        (
            "updatedAge=99",
            "update compiles UPDATE … SET … WHERE from the whole entity, so ada's age becomes 99",
        ),
        (
            "bumpedAge=40",
            "updateWhere compiles a partial SET whose $1 bind precedes the WHERE clause's $2",
        ),
        (
            "bumpedName=lin",
            "updateWhere leaves the untouched name column alone",
        ),
        (
            "updateWhereCount=2",
            "two adults match the partial update",
        ),
        (
            "setBumpedAge=40",
            "a typed setWhere compiles the same partial UPDATE on Postgres as the raw map",
        ),
        (
            "appliedName=neo",
            "applySet is the query-builder write terminal on Postgres: the filter picks the row, the setter assigns it",
        ),
        (
            "sumAllAges=73",
            "sumOf compiles SUM(age) into the query and decodes the bigint result",
        ),
        (
            "sumAdultAges=55",
            "the filter compiles a WHERE that bounds the aggregate to the adults",
        ),
        (
            "avgAdultAges=27.5",
            "avgOf casts AVG to float8 so an integer column's average crosses as a float",
        ),
        (
            "minAllAges=18",
            "minOf compiles MIN(age) and keeps the column's integer type",
        ),
        (
            "maxName=max",
            "maxOf compiles MAX(name) over a text column (ada < lin < max)",
        ),
        (
            "sumNobody=none",
            "an aggregate over an empty match is NULL, decoded to None",
        ),
        (
            "joinSumRightId=21",
            "pg compiles SUM(r.id) over l JOIN r, qualifying the column to the right alias (10+11)",
        ),
        (
            "joinSumLeftAge=55",
            "pg qualifies a left-column aggregate to the l alias over the inner join (30+25)",
        ),
        (
            "joinMaxRightTitle=world",
            "pg folds MAX(r.title), a right text column, over the join",
        ),
        (
            "joinAvgRightId=10.5",
            "pg casts AVG(r.id) to float8 over the join ((10+11)/2)",
        ),
        (
            "leftJoinSumLeftAge=73",
            "pg compiles a LEFT JOIN whose left-column aggregate counts the unmatched ada (18+30+25), unlike the inner join's 55",
        ),
        (
            "leftJoinMaxRightTitle=world",
            "pg's LEFT JOIN right-column aggregate skips the unmatched ada's NULL with no sentinel",
        ),
        ("singleOne=lin", "single decodes the lone matching row (id 2)"),
        (
            "singleNone=none",
            "single answers None for an empty match rather than failing",
        ),
        (
            "singleMany=repo.single.many",
            "single fails when more than one row matches",
        ),
        (
            "oneOrErr=ada",
            "singleOrError answers the bare entity for an exact single match (id 1)",
        ),
        (
            "noneOrErr=repo.single.empty",
            "singleOrError fails on an empty match where single returns None",
        ),
        ("everyAdult=true", "every is true when all selected rows match"),
        (
            "everyHigh=false",
            "every is false when a selected row fails the predicate",
        ),
        (
            "everyEmpty=true",
            "every over an empty selection is vacuously true",
        ),
        (
            "groupCounts=eng:2,ops:1,sales:3",
            "GROUP BY partitions the emps and COUNT(*) folds each dept, key-ordered",
        ),
        (
            "groupSums=eng:300,ops:50,sales:600",
            "SUM(salary) is grouped per dept and pushed down",
        ),
        (
            "groupAvgs=eng:150.0,ops:50.0,sales:200.0",
            "AVG(salary)::float8 crosses the wire as a float per group",
        ),
        (
            "groupRanges=eng:100-200,ops:50-50,sales:150-300",
            "MIN and MAX over one column compose in a single grouped select-list",
        ),
        (
            "groupHavingCount=eng:2,sales:3",
            "HAVING COUNT(*) > 1 drops the single-member ops group",
        ),
        (
            "groupHavingSum=sales:600",
            "HAVING SUM(salary) >= 600 keeps only the sales group",
        ),
        (
            "groupFilteredHaving=eng:2,sales:3",
            "the WHERE bind precedes the HAVING bind in the compiled statement",
        ),
        (
            "deptsAll=eng,eng,ops,sales,sales,sales",
            "selectList without distinct returns the dept column for all six rows",
        ),
        (
            "deptsDistinct=eng,ops,sales",
            "SELECT DISTINCT collapses the repeated dept column to three values",
        ),
        (
            "salariesDistinct=50,100,150,200,300",
            "SELECT DISTINCT over the salary column drops the duplicate 150",
        ),
        (
            "unionIds=1,2,3,4,5,6",
            "a SQL UNION dedups the shared rows and the outer ORDER BY composes",
        ),
        (
            "intersectIds=3,4",
            "a SQL INTERSECT keeps the rows present in both branches",
        ),
        (
            "exceptIds=2,5",
            "a SQL EXCEPT keeps the left rows not in the right",
        ),
        (
            "unionAllCount=8",
            "a SQL UNION ALL keeps the duplicate rows the branches share",
        ),
        (
            "unionFiltered=2,5",
            "an outer filter compiles to a wrapping subquery over the union",
        ),
        (
            "nestedUnionIds=1,2,3,4,5,6",
            "nested unions compile to nested parenthesised UNIONs with threaded binds",
        ),
        (
            "concurrent=true",
            "the pool serves six concurrent reads on one handle",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "expected `{probe}` ({want})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
