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
//!
//! Typed errors run against the live database too: a duplicate insert into a named
//! unique constraint classifies through `dbErrorKind` as a `UniqueViolation`, and
//! `dbErrorConstraint` reads the constraint name; a NULL into a NOT NULL column
//! classifies as a `NotNullViolation`, and `dbErrorColumn`/`dbErrorTable` read the
//! column and its table — all out of the Postgres `ErrorResponse`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// The program source, with connection settings spliced in as sentinels so the
/// Ridge record braces never collide with Rust string formatting.
const SOURCE_TEMPLATE: &str = r#"
import std.data (connect, connectWith, defaultPool, withPoolSize, withQueryTimeoutMs, withCheckoutTimeoutMs, Config, Postgres, dbErrorKind, dbErrorConstraint, dbErrorColumn, dbErrorTable, DbErrorKind, UniqueViolation, ForeignKeyViolation, NotNullViolation, CheckViolation, ConnectionError, DecodeError, Unsupported, QueryError)
import std.repo as Repo
import std.migrate as Migrate
import std.migrate (MigrationOp)
import std.raw as Raw
import std.query (SortOrder, Asc, Desc)
import std.sql (toSql, SqlValue, toRow)
import std.map as Map
import std.int as Int
import std.float as Float
import std.list (length, contains)
import std.text as Text
import std.sql (DbBigInt, DbText)
import std.schema (HasSchema, schemaOf, schema, eraseSchema, EntitySchema, withColumn, mkColumn, generated, primaryKey, indexed, foreignKey, references, Identity)

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

-- A throwaway entity for the migration probes, in a table the probes themselves
-- create via `Migrate.run` rather than one the CI harness sets up.
pub type Widget = { id: Int, name: Text } deriving (Row)

-- The typed `insert` reads an entity's `HasSchema` instance to learn which columns
-- the database generates, then omits them. These Postgres tables are created with a
-- caller-supplied integer id (no serial default yet), so the instances name no
-- generated columns: every column, the explicit id included, is written as given.
instance HasSchema User =
    schemaOf (_w: Option User) -> EntitySchema User = schema "User" "ridge_pg_users"
    toInsertRow (shape: InsertShape User) -> Map Text SqlValue = toRow shape

instance HasSchema Widget =
    schemaOf (_w: Option Widget) -> EntitySchema Widget = schema "Widget" "ridge_mig_widgets"
    toInsertRow (shape: InsertShape Widget) -> Map Text SqlValue = toRow shape

-- A second entity for the join, in the `ridge_pg_posts` table; `author` holds the
-- owning user's id.
pub type Post = { id: Int, author: Int, title: Text } deriving (Row)

-- A projected shape: the projection renames `name` -> `who` and `age` -> `years`,
-- so the select-list compiles to `name AS who, age AS years` and the decode reads
-- the aliased columns back.
pub type Summary = { who: Text, years: Int } deriving (Row)

-- A computed projection shape: `label` is a CASE over `age`, `doubled` is
-- arithmetic over `age`, so the decode proves Postgres compiles a select-list
-- expression per row, not only a stored column.
pub type Tagged = { label: Text, doubled: Int } deriving (Row)

-- The shape a join projection decodes into: a name from the left entity and a
-- title from the right.
pub type Combo = { person: Text, post: Text } deriving (Row)

-- The shape a left-join projection decodes into: `post` is `Option Text`, so an
-- unmatched left row's NULL right column decodes to `None`.
pub type ComboOpt = { person: Text, post: Option Text } deriving (Row)

-- The mirror shape a right-join projection decodes into: `person` is `Option Text`,
-- so an unmatched right row's NULL left column decodes to `None`.
pub type ComboOptL = { person: Option Text, post: Text } deriving (Row)

-- A grouped-count shape keyed by an integer column (a post's author id).
pub type AuthorCount = { author: Int, n: Int } deriving (Row)

-- The shape a full-join projection decodes into: BOTH derived fields are `Option Text`,
-- so an unmatched row projects the missing side's field as `None`.
pub type FullCombo = { who: Option Text, title: Option Text } deriving (Row)

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

fn joinTagged (ts: List Tagged) -> Text =
    match ts
        []        -> ""
        t :: []   -> tagText t
        t :: rest -> Text.concat (tagText t) (Text.concat "," (joinTagged rest))

fn tagText (t: Tagged) -> Text = Text.concat t.label (Text.concat ":" (Int.toText t.doubled))

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

-- The name of an optional left user, or "-" when the right row matched none.
fn optName (ou: Option User) -> Text =
    match ou
        None   -> "-"
        Some u -> u.name

-- Render each `(Option User, Post)` pair as `name:title` (or `-:title`),
-- comma-joined, so a right join's unmatched right rows are observable.
fn joinRightPairs (ps: List (Option User, Post)) -> Text =
    match ps
        []              -> ""
        (ou, p) :: []   -> Text.concat (optName ou) (Text.concat ":" p.title)
        (ou, p) :: rest -> Text.concat (optName ou) (Text.concat ":" (Text.concat p.title (Text.concat "," (joinRightPairs rest))))

-- Render each projected `ComboOptL` as `person:post` (or `-:post`), comma-joined.
fn joinComboOptLs (cs: List ComboOptL) -> Text =
    match cs
        []          -> ""
        c :: []     -> Text.concat (optText c.person) (Text.concat ":" c.post)
        c :: rest   -> Text.concat (optText c.person) (Text.concat ":" (Text.concat c.post (Text.concat "," (joinComboOptLs rest))))

-- Render each `AuthorCount` as `author:n`, comma-joined.
fn authorCounts (cs: List AuthorCount) -> Text =
    match cs
        []        -> ""
        c :: []   -> Text.concat (Int.toText c.author) (Text.concat ":" (Int.toText c.n))
        c :: rest -> Text.concat (Int.toText c.author) (Text.concat ":" (Text.concat (Int.toText c.n) (Text.concat "," (authorCounts rest))))

-- Format the three full-join row categories as `both:B,left:L,right:R`.
fn fullSigFmt (b: Int) (l: Int) (r: Int) -> Text =
    Text.concat "both:" (Text.concat (Int.toText b) (Text.concat ",left:" (Text.concat (Int.toText l) (Text.concat ",right:" (Int.toText r)))))

-- Classify each `(Option User, Option Post)` pair into matched (`both`), left-only
-- (`left`), or right-only (`right`) and count them. Order-independent, so it pins the
-- full-join semantics without depending on the backend's NULL ordering.
fn fullSigGo (ps: List (Option User, Option Post)) (b: Int) (l: Int) (r: Int) -> Text =
    match ps
        []               -> fullSigFmt b l r
        (ou, op) :: rest ->
            match ou
                Some _ ->
                    match op
                        Some _ -> fullSigGo rest (b + 1) l r
                        None   -> fullSigGo rest b (l + 1) r
                None ->
                    match op
                        Some _ -> fullSigGo rest b l (r + 1)
                        None   -> fullSigGo rest b l r

fn fullSig (ps: List (Option User, Option Post)) -> Text =
    fullSigGo ps 0 0 0

-- Format a full-join projection summary as `rows:N,noWho:M,noTitle:K`.
fn fullSelFmt (n: Int) (nw: Int) (nt: Int) -> Text =
    Text.concat "rows:" (Text.concat (Int.toText n) (Text.concat ",noWho:" (Text.concat (Int.toText nw) (Text.concat ",noTitle:" (Int.toText nt)))))

-- Count projected `FullCombo` rows and how many have a `None` `who` (a right-only row)
-- or a `None` `title` (a left-only row). Order-independent.
fn fullSelGo (cs: List FullCombo) (n: Int) (nw: Int) (nt: Int) -> Text =
    match cs
        []        -> fullSelFmt n nw nt
        c :: rest ->
            match c.who
                Some _ ->
                    match c.title
                        Some _ -> fullSelGo rest (n + 1) nw nt
                        None   -> fullSelGo rest (n + 1) nw (nt + 1)
                None ->
                    match c.title
                        Some _ -> fullSelGo rest (n + 1) (nw + 1) nt
                        None   -> fullSelGo rest (n + 1) (nw + 1) (nt + 1)

fn fullSel (cs: List FullCombo) -> Text =
    fullSelGo cs 0 0 0

fn pgConfig () -> Config =
    Config { host = "__PG_HOST__", port = __PG_PORT__, database = "__PG_DATABASE__", user = "__PG_USER__", password = "__PG_PASSWORD__", sslMode = "__PG_SSLMODE__" }

-- Tag a classified error by its kind, so a probe can render the typed kind as text.
fn tag (k: DbErrorKind) -> Text =
    match k
        UniqueViolation -> "unique"
        ForeignKeyViolation -> "fk"
        NotNullViolation -> "notnull"
        CheckViolation -> "check"
        ConnectionError -> "connection"
        DecodeError -> "decode"
        Unsupported -> "unsupported"
        QueryError -> "query"

-- A real unique violation against Postgres carries its constraint name in the
-- ErrorResponse. Create a throwaway table with a named unique constraint, insert a
-- duplicate, and classify the failure -> "unique:ridge_pg_uniq_id_key". Proves
-- `dbErrorKind` reads SQLSTATE 23505 as `UniqueViolation` and `dbErrorConstraint`
-- reads the constraint the backend named.
pub fn db uniqueViolationKind () -> Text =
    match connect (pgConfig ())
        Err _ -> "connect-err"
        Ok conn ->
            match Raw.exec conn "DROP TABLE IF EXISTS ridge_pg_uniq" []
                Err _ -> "drop-err"
                Ok _ ->
                    match Raw.exec conn "CREATE TABLE ridge_pg_uniq (id integer CONSTRAINT ridge_pg_uniq_id_key UNIQUE)" []
                        Err _ -> "create-err"
                        Ok _ ->
                            match Raw.exec conn "INSERT INTO ridge_pg_uniq (id) VALUES (1)" []
                                Err _ -> "insert1-err"
                                Ok _ ->
                                    match Raw.exec conn "INSERT INTO ridge_pg_uniq (id) VALUES (1)" []
                                        Ok _ -> "unexpected-ok"
                                        Err e -> Text.concat (tag (dbErrorKind e)) (Text.concat ":" (dbErrorConstraint e))

-- A real not-null violation against Postgres carries the offending column and its
-- table in the ErrorResponse. Insert a NULL into a NOT NULL column and classify the
-- failure -> "notnull:val:ridge_pg_notnull". Proves `dbErrorKind` reads SQLSTATE
-- 23502 as `NotNullViolation`, and `dbErrorColumn`/`dbErrorTable` read the column and
-- table the backend named.
pub fn db notNullViolationDetail () -> Text =
    match connect (pgConfig ())
        Err _ -> "connect-err"
        Ok conn ->
            match Raw.exec conn "DROP TABLE IF EXISTS ridge_pg_notnull" []
                Err _ -> "drop-err"
                Ok _ ->
                    match Raw.exec conn "CREATE TABLE ridge_pg_notnull (val integer NOT NULL)" []
                        Err _ -> "create-err"
                        Ok _ ->
                            match Raw.exec conn "INSERT INTO ridge_pg_notnull (val) VALUES (NULL)" []
                                Ok _ -> "unexpected-ok"
                                Err e -> Text.join ":" [tag (dbErrorKind e), dbErrorColumn e, dbErrorTable e]

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
            match Repo.delete (fn (u: User) -> u.id >= 0) r
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

-- The body `withConnRuns` runs inside `withConnection`: count the seeded users. A
-- named fn because a multi-line lambda in call-arg position does not parse.
fn wcCountUsers (c: Postgres) -> Result Int Error =
    let r = Repo.repo c "ridge_pg_users"
    r |> Repo.query |> Repo.count

-- withConnection: seed the table, then open a connection and read its count through
-- `withConnection`, which closes the handle on the way out without a manual `close`
-- -> "rows:3". Proves the scoped combinator runs the body over the real wire and
-- releases the connection after.
pub fn db withConnRuns () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok _  ->
            match connect (pgConfig ())
                Err _ -> "connect-err"
                Ok conn ->
                    match Repo.withConnection conn wcCountUsers
                        Err _ -> "wc-err"
                        Ok n  -> Text.concat "rows:" (Int.toText n)

-- connectWith + disconnect: open with an explicit (tuned) pool, read the seeded
-- count, then release the handle with `disconnect` (the dual of `connect`). Proves
-- the tuned-pool entry point opens over the real wire and the manual disconnect
-- releases it -> "rows:3".
pub fn db connectWithRuns () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok _  ->
            match connectWith (pgConfig ()) (defaultPool () |> withPoolSize 4 |> withQueryTimeoutMs 30000 |> withCheckoutTimeoutMs 5000)
                Err _ -> "connect-err"
                Ok conn ->
                    match wcCountUsers conn
                        Err _ ->
                            match Repo.disconnect conn
                                _ -> "count-err"
                        Ok n  ->
                            match Repo.disconnect conn
                                _ -> Text.concat "rows:" (Int.toText n)

-- count: the whole table -> 3
pub fn db countAll () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.query |> Repo.count
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

-- LIKE/IN against real Postgres, folded into one probe so it opens a single pooled
-- connection rather than one per check (a fresh `setup` per check would inflate the
-- run's open-connection peak). Each findBy reifies to a QLike/QIn the backend
-- compiles into a real LIKE/IN. Against the seeded ada/lin/max: contains "a" -> 2,
-- startsWith "l" -> 1, like "_a_" -> 1, contains "%" -> 0 and contains "_" -> 0
-- (the escaped metacharacters match no name, confirming the default backslash escape
-- lines up with the renderer), IN [18, 30] -> 2, IN [] -> 0 — joined as
-- "2,1,1,0,0,2,0".
fn countOf (res: Result (List User) Error) -> Int =
    match res
        Ok us -> listLen us
        Err _ -> 0 - 1

pub fn db likeInChecks () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = countOf (r |> Repo.findBy (fn (u: User) -> Text.contains u.name "a"))
            let b = countOf (r |> Repo.findBy (fn (u: User) -> Text.startsWith u.name "l"))
            let c = countOf (r |> Repo.findBy (fn (u: User) -> Text.like u.name "_a_"))
            let d = countOf (r |> Repo.findBy (fn (u: User) -> Text.contains u.name "%"))
            let e = countOf (r |> Repo.findBy (fn (u: User) -> Text.contains u.name "_"))
            let f = countOf (r |> Repo.findBy (fn (u: User) -> contains u.age [18, 30]))
            let g = countOf (r |> Repo.findBy (fn (u: User) -> contains u.age []))
            Text.join "," [Int.toText a, Int.toText b, Int.toText c, Int.toText d, Int.toText e, Int.toText f, Int.toText g]

-- Arithmetic predicates against real Postgres, folded into one probe (one pooled
-- connection) like the LIKE/IN checks. Each findBy reifies a QAdd/QSub/QMul/QDiv/QMod
-- the backend compiles into real SQL arithmetic. Against the seeded ada(18,1)/lin(30,2)/
-- max(25,3): age * 2 > 50 -> lin = 1; age + id > 20 -> lin,max = 2; age / 10 == 2 ->
-- max = 1 (integer truncation, matching the in-memory backend); age % 2 == 0 -> ada,lin
-- = 2 — joined as "1,2,1,2".
pub fn db arithChecks () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = countOf (r |> Repo.findBy (fn (u: User) -> u.age * 2 > 50))
            let b = countOf (r |> Repo.findBy (fn (u: User) -> u.age + u.id > 20))
            let c = countOf (r |> Repo.findBy (fn (u: User) -> u.age / 10 == 2))
            let d = countOf (r |> Repo.findBy (fn (u: User) -> u.age % 2 == 0))
            Text.join "," [Int.toText a, Int.toText b, Int.toText c, Int.toText d]

-- computed projection on real Postgres: project a CASE `label` and a doubled
-- `age` per row, ordered by id -> "minor:36,adult:60,adult:50". Proves the
-- Postgres backend compiles a computed select-list (arithmetic + CASE) with its
-- literals bound as placeholders and decodes the result. Folded under one
-- connection (pooled probes share a handle).
pub fn db projChecks () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.select (fn (u: User) -> Tagged { label = if u.age >= 25 then "adult" else "minor", doubled = u.age * 2 })
                Err _ -> "list-err"
                Ok ts -> joinTagged ts

-- computed orderBy + aggregate on real Postgres, folded into one probe (one pooled
-- connection). Order by `age - id * 10`: ada(8), lin(10), max(-5) -> ascending
-- max,ada,lin; sum of `age * 2` -> 146. Proves the Postgres backend threads a
-- computed ORDER BY key's literals as `$N` placeholders after the WHERE and a
-- computed aggregate's in the SELECT, never interpolated as raw SQL.
pub fn db orderAggChecks () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.age - u.id * 10) |> Repo.toList
                Err _ -> "order-err"
                Ok us ->
                    match r |> Repo.query |> Repo.sumOf (fn (u: User) -> u.age * 2)
                        Err _       -> "sum-err"
                        Ok None     -> "none"
                        Ok (Some n) -> Text.join ":" [joinNames us, Int.toText n]

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
            match r |> Repo.delete (fn (u: User) -> u.age < 25)
                Err _ -> 0 - 2
                Ok _  ->
                    match r |> Repo.query |> Repo.count
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

-- captured runtime variable against Postgres: a `let`-bound threshold flows into
-- the predicate as a `$N` bind, so the WHERE compares against a placeholder
-- rather than an inlined literal. Adults at or above the captured `minAge` (25),
-- ascending -> "max,lin". Proves an Int captured from the enclosing scope reaches
-- the real query as a parameter.
pub fn db capturedAdults () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            let minAge = 25
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= minAge) |> Repo.orderBy Asc (fn (u: User) -> u.age) |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- captured Text variable against Postgres: the wanted name binds as a parameter,
-- so the equality compares against a placeholder -> "lin". Proves a Text capture
-- crosses to the real query, not only an Int.
pub fn db capturedByName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            let wanted = "lin"
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.name == wanted) |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- captured runtime list against Postgres: the `ages` list flows in and each element
-- binds as its own `$N`, so `List.contains u.age ages` compiles into `age IN ($1,
-- $2)`. Ascending -> "max,lin" (ada 18 drops, max 25 and lin 30 match). Proves a
-- captured `List Int` reaches the real query as an IN over bound parameters.
pub fn db capturedInList () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            let ages = [25, 30]
            match r |> Repo.query |> Repo.filter (fn (u: User) -> List.contains u.age ages) |> Repo.orderBy Asc (fn (u: User) -> u.age) |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- captured runtime list of Text against Postgres: the wanted names bind as
-- parameters, so `List.contains u.name names` compiles into `name IN ($1, $2)` ->
-- "ada,lin". Proves a captured `List Text` crosses to the real query, not only Int.
pub fn db capturedInTextList () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            let names = ["ada", "lin"]
            match r |> Repo.query |> Repo.filter (fn (u: User) -> List.contains u.name names) |> Repo.orderBy Asc (fn (u: User) -> u.age) |> Repo.toList
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
            match Repo.delete (fn (u: User) -> u.id >= 0) users
                Err e -> Err e
                Ok _  ->
                    match Repo.delete (fn (p: Post) -> p.id >= 0) posts
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

-- Seed the join tables on connection `c`: clear both and insert 3 users + 2 posts.
-- The full-join probes call this from the body they hand to `Repo.withConnection`, so
-- the connection that owns the seeded data is closed on the way out.
fn db seedJoinData (c: Postgres) -> Result Unit Error =
    let users: Repo User Postgres = Repo.repo c "ridge_pg_users"
    let posts: Repo Post Postgres = Repo.repo c "ridge_pg_posts"
    match Repo.delete (fn (u: User) -> u.id >= 0) users
        Err e -> Err e
        Ok _  ->
            match Repo.delete (fn (p: Post) -> p.id >= 0) posts
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
                                                        Ok _  -> Ok ()

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

-- correlated EXISTS on the real backend: keep the users who own at least one post by
-- probing the captured `posts` table per row. Compiles to a `SELECT … FROM users AS l
-- WHERE EXISTS (SELECT 1 FROM posts AS r WHERE r.author = l.id)`. lin (2) and max (3)
-- own posts, ada (1) owns none -> "lin,max".
pub fn db existsPosts () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> Repo.exists posts (fn (p: Post) -> p.author == u.id)) |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.toList
                Err _  -> "exists-err"
                Ok us  -> joinNames us

-- correlated NOT EXISTS: the complement — only ada (1) owns no post -> "ada".
pub fn db notExistsPosts () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> Repo.notExists posts (fn (p: Post) -> p.author == u.id)) |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.toList
                Err _  -> "nexists-err"
                Ok us  -> joinNames us

-- count over a filter carrying a correlated EXISTS, exercising the direct count path
-- (a `SELECT COUNT(*) … WHERE EXISTS (…)`) rather than the plan renderer -> 2.
pub fn db existsPostsCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> Repo.exists posts (fn (p: Post) -> p.author == u.id)) |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- correlated EXISTS inside a binary join's post-join WHERE: inner-join users to their
-- posts, then keep the pairs whose user also owns a post titled "world" — the captured
-- `posts` table is probed from inside the join filter, the inner row joining at the leaf
-- past both join sides (`SELECT 1 FROM posts AS x2 WHERE x2.author = l.id AND ...`). Only
-- max owns "world" -> "max:world".
pub fn db existsInJoinWhere () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.filter (fn (u: User) (p: Post) -> Repo.exists posts (fn (p2: Post) -> p2.author == u.id && p2.title == "world")) |> Repo.toList
                Err _  -> "exists-join-err"
                Ok ps  -> joinPairs ps

-- nested correlated EXISTS: keep the users who own a post titled "world", expressed as
-- an outer EXISTS over the user's posts whose predicate carries an inner EXISTS
-- correlating a second `posts` row to that post (`x2.id = x1.id AND x2.title = $1`). The
-- subqueries nest, each correlating one leaf up. Only max owns "world" -> "max".
pub fn db nestedExists () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.filter (fn (u: User) -> Repo.exists posts (fn (p: Post) -> p.author == u.id && Repo.exists posts (fn (p2: Post) -> p2.id == p.id && p2.title == "world"))) |> Repo.toList
                Err _  -> "nested-exists-err"
                Ok us  -> joinNames us

-- cross join: pair every left row with every right row (the cartesian product).
-- Narrow the left query to lin (id 2), cross with both posts, order by post id
-- -> "lin:hello,lin:world". lin pairs with "world" (author 3) too — a post it
-- does not own — so the backend compiles an unconditional join (`JOIN r ON true`),
-- unlike the inner join that keeps only lin's own post.
pub fn db crossJoined () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 2) |> Repo.crossJoin posts |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id) |> Repo.toList
                Err _  -> "cross-err"
                Ok ps  -> joinPairs ps

-- cross-join count: every user crossed with every post -> 3 * 2 = 6. Proves the
-- backend's unconditional join is the full cartesian and `COUNT(*)` counts it.
pub fn db crossCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.crossJoin posts |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- right join: keep every post, pairing each with its author or with `None`. The left
-- query is narrowed to ids <= 2 (so max, id 3, drops out of the match), then a RIGHT
-- JOIN keeps every post and folds that filter into the ON — `world` (authored by max)
-- keeps its place with a `None` left side. Ordered by post id and rendered
-- `name:title` (or `-:title`) -> "lin:hello,-:world". Proves the `RIGHT JOIN` and its
-- `__ridge_matched` sentinel on the left subquery keep unmatched right rows.
pub fn db rightJoinedNames () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: Option User) (p: Post) -> p.id) |> Repo.toList
                Err _  -> "right-join-err"
                Ok ps  -> joinRightPairs ps

-- right-join projection: the same right join, projected into `ComboOptL` where
-- `person` is `Option Text` -> "lin:hello,-:world". `world` has no matching author,
-- so its projected `person` column is NULL and decodes to `None`. Proves
-- `rightJoinSelect` keeps unmatched right rows and decodes the left columns into
-- Option fields.
pub fn db rightSelectNames () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: Option User) (p: Post) -> p.id) |> Repo.select (fn (u: Option User) (p: Post) -> ComboOptL { person = u.name, post = p.title })
                Err _  -> "right-select-err"
                Ok cs  -> joinComboOptLs cs

-- right-join count: the narrowed right join keeps both posts, one matched and one
-- (`world`) unmatched, so the count is 2 -> proving `countRightJoin` keeps every
-- right row where the inner join would count only the one match.
pub fn db rightJoinCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- right-join aggregate over a LEFT column: sum the matched users' ids across the
-- narrowed right join. `hello` matches lin (id 2); `world` matches no one (its left
-- side is NULL), so the fold skips it -> 2. Proves `aggregateRightJoin` folds a left
-- column only over the matched rows.
pub fn db rightJoinSumLeftId () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> u.id)
                Err _       -> 0 - 2
                Ok None     -> 0 - 3
                Ok (Some n) -> n

-- right-join grouped summary: group every post by its author id (a right column) and
-- count each group -> author 2 owns hello, author 3 owns world, so "2:1,3:1" ordered
-- by the key. Proves `groupSummarizeRightJoin` runs the GROUP BY over the RIGHT JOIN
-- and decodes the integer key.
pub fn db rightJoinGroupAuthors () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> p.author) |> Repo.summarize (fn g -> AuthorCount { author = g.key, n = g.count })
                Err _  -> "right-group-err"
                Ok cs  -> authorCounts cs

-- full join: keep every user AND every post. The left query is narrowed to ids <= 2,
-- so ada (1) and lin (2) enter and max (3) is filtered out. The full join then yields
-- one matched row (lin owns hello), one left-only row (ada has no post), and one
-- right-only row (world, authored by the filtered-out max) -> "both:1,left:1,right:1".
-- Proves the backend compiles a `FULL JOIN` with both sentinel subqueries and decodes
-- the `(Option User, Option Post)` pair across the marker split. Each full-join probe
-- runs its body through `Repo.withConnection`, so its connection is closed on the way
-- out and the probe holds none after it returns.
fn db fullCatBody (c: Postgres) -> Result Text Error =
    match seedJoinData c
        Err e -> Err e
        Ok _  ->
            let users: Repo User Postgres = Repo.repo c "ridge_pg_users"
            let posts: Repo Post Postgres = Repo.repo c "ridge_pg_posts"
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.toList
                Err e -> Err e
                Ok ps -> Ok (fullSig ps)

pub fn db fullJoinCategories () -> Text =
    match connect (pgConfig ())
        Err _ -> "connect-err"
        Ok conn ->
            match Repo.withConnection conn fullCatBody
                Err _ -> "full-join-err"
                Ok s  -> s

-- full-join projection: the same full join, projected into `FullCombo` where both
-- fields are `Option Text`. Three rows; the right-only `world` projects `who = None`
-- and the left-only `ada` projects `title = None` -> "rows:3,noWho:1,noTitle:1".
-- Proves `fullJoinSelect` reads both sides as Option over Postgres.
fn db fullSelBody (c: Postgres) -> Result Text Error =
    match seedJoinData c
        Err e -> Err e
        Ok _  ->
            let users: Repo User Postgres = Repo.repo c "ridge_pg_users"
            let posts: Repo Post Postgres = Repo.repo c "ridge_pg_posts"
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.select (fn (u: Option User) (p: Option Post) -> FullCombo { who = u.name, title = p.title })
                Err e -> Err e
                Ok cs -> Ok (fullSel cs)

pub fn db fullSelectShape () -> Text =
    match connect (pgConfig ())
        Err _ -> "connect-err"
        Ok conn ->
            match Repo.withConnection conn fullSelBody
                Err _ -> "full-select-err"
                Ok s  -> s

-- full-join count: the same narrowed full join keeps all three rows (one matched, one
-- left-only ada, one right-only world) -> 3. Proves `countFullJoin` over a real
-- `FULL JOIN`.
fn db fullCountBody (c: Postgres) -> Result Int Error =
    match seedJoinData c
        Err e -> Err e
        Ok _  ->
            let users: Repo User Postgres = Repo.repo c "ridge_pg_users"
            let posts: Repo Post Postgres = Repo.repo c "ridge_pg_posts"
            users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.count

pub fn db fullJoinCount () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match Repo.withConnection conn fullCountBody
                Ok n  -> n
                Err _ -> 0 - 2

-- full-join aggregate over a RIGHT column: sum the post ids across the narrowed full
-- join. hello (10) and the right-only world (11) contribute; the left-only ada has no
-- post (a NULL the fold skips) -> 21. Proves `aggregateFullJoin` over Postgres folds a
-- right column over the matched and right-only rows, skipping the left-only NULL.
fn db fullSumBody (c: Postgres) -> Result Int Error =
    match seedJoinData c
        Err e -> Err e
        Ok _  ->
            let users: Repo User Postgres = Repo.repo c "ridge_pg_users"
            let posts: Repo Post Postgres = Repo.repo c "ridge_pg_posts"
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> p.id)
                Err e       -> Err e
                Ok None     -> Ok (0 - 2)
                Ok (Some n) -> Ok n

pub fn db fullJoinSumPostId () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match Repo.withConnection conn fullSumBody
                Ok n  -> n
                Err _ -> 0 - 3

-- full-join grouped summary: group every post by its author id (a right column) over a
-- full join narrowed to user ids >= 2, so both lin (2) and max (3) match their posts
-- and the group key is never NULL. lin owns hello (1), max owns world (1)
-- -> "2:1,3:1". Proves `groupSummarizeFullJoin` runs the GROUP BY over the FULL JOIN
-- and decodes the integer key.
fn db fullGroupBody (c: Postgres) -> Result Text Error =
    match seedJoinData c
        Err e -> Err e
        Ok _  ->
            let users: Repo User Postgres = Repo.repo c "ridge_pg_users"
            let posts: Repo Post Postgres = Repo.repo c "ridge_pg_posts"
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id >= 2) |> Repo.fullJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> p.author) |> Repo.summarize (fn g -> AuthorCount { author = g.key, n = g.count })
                Err e -> Err e
                Ok cs -> Ok (authorCounts cs)

pub fn db fullJoinGroupAuthors () -> Text =
    match connect (pgConfig ())
        Err _ -> "connect-err"
        Ok conn ->
            match Repo.withConnection conn fullGroupBody
                Err _ -> "full-group-err"
                Ok s  -> s

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
            match Repo.delete (fn (u: User) -> u.id >= 0) r
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

-- count over an inner join against Postgres: how many user-post pairs? (lin:hello,
-- max:world) -> 2. Proves `countJoin` compiles a `SELECT COUNT(*) FROM l JOIN r`.
pub fn db joinCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- exists over an inner join against Postgres: does any pair join? -> "true".
pub fn db joinAny () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.exists
                Err _ -> "exists-err"
                Ok b  -> boolText b

-- every over an inner join against Postgres (left column): are all joined users
-- adults? (lin 30, max 25 both >= 18) -> "true".
pub fn db joinEveryAdult () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.every (fn (u: User) (p: Post) -> u.age >= 18)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- every over an inner join against Postgres (right column): is every joined post
-- titled "hello"? (world fails) -> "false".
pub fn db joinEveryHello () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.every (fn (u: User) (p: Post) -> p.title == "hello")
                Err _ -> "every-err"
                Ok b  -> boolText b

-- count over a left join against Postgres: how many left-outer rows? ada (no post,
-- kept), lin (hello), max (world) -> 3. Proves `countLeftJoin` compiles a `SELECT
-- COUNT(*) FROM l LEFT JOIN r`, the unmatched ada counted.
pub fn db leftJoinCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- every over a left join against Postgres (right column): does every kept row have
-- a post of its own? ada is kept with a NULL post and fails -> "false". Proves a
-- right-column every drops the unmatched rows under SQL's three-valued WHERE.
pub fn db leftJoinEveryAuthored () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.every (fn (u: User) (p: Post) -> p.author == u.id)
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
            match Repo.delete (fn (em: Emp) -> em.id >= 0) r
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

-- group + summarize over a COMPUTED aggregate against Postgres: SUM(salary * 2) per
-- dept -> "eng:600,ops:100,sales:1200". The fold compiles to `SUM(("salary" * $1))`,
-- the literal bound as a parameter rather than spliced into the statement.
pub fn db groupComputedSum () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (e: Emp) -> e.salary * 2) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- group + having on a COMPUTED aggregate against Postgres: depts whose doubled
-- payroll is >= 1200 -> "sales:1200". Proves a computed expression folds inside a
-- HAVING aggregate, its literal bound alongside the SELECT's.
pub fn db groupComputedHaving () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.having (fn g -> g.sum (fn (e: Emp) -> e.salary * 2) >= 1200) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (e: Emp) -> e.salary * 2) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

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

-- group a join by the left key (user name) against Postgres, counting the pairs ->
-- "lin:1,max:1" (each authors one of the two posts; ada joins nothing). Proves the
-- GROUP BY compiles over a real JOIN.
pub fn db joinGroupCounts () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group a join by the left key, summing a RIGHT column (post id) -> "lin:10,max:11".
-- Proves a grouped aggregate qualifies the fold to the right table.
pub fn db joinGroupRightIds () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (u: User) (p: Post) -> p.id) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- group a join by the left key with HAVING on a right-side sum -> "max:11" (only
-- max's post id clears 11; lin's 10 drops). Proves HAVING re-renders a side-qualified
-- aggregate.
pub fn db joinGroupHaving () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.having (fn g -> g.sum (fn (u: User) (p: Post) -> p.id) >= 11) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (u: User) (p: Post) -> p.id) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- group a join by a RIGHT key (post title) -> "hello:1,world:1" (each title its own
-- group). Proves the group key qualifies to the right table.
pub fn db joinGroupByTitle () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> p.title) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group a LEFT join by the left key -> "ada:1,lin:1,max:1" (ada, matching no post,
-- still forms a one-row group). Proves a real LEFT JOIN keeps every left row.
pub fn db leftJoinGroupCounts () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
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

-- A deliberate failure with no SQL fault: a single-row query filtered to match
-- nothing answers `Err` ("matched no rows"), which a transaction body returns to
-- roll back. It is a plain SELECT, so it never aborts the session.
fn pgForceFail (conn: Postgres) -> Result Unit Error =
    let r = Repo.repo conn "ridge_pg_users"
    match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 999999) |> Repo.singleOrError
        Err e -> Err e
        Ok _  -> Ok ()

fn pgCountUsers (conn: Postgres) -> Int =
    let r = Repo.repo conn "ridge_pg_users"
    match r |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 9

fn pgClearUsers (conn: Postgres) -> Result Int Error =
    let r = Repo.repo conn "ridge_pg_users"
    Repo.delete (fn (u: User) -> u.id >= 0) r

-- A transaction body that inserts two rows and succeeds.
fn pgInsertTwo (tx: Postgres) -> Result Unit Error =
    let r = Repo.repo tx "ridge_pg_users"
    match Repo.insert (User { id = 1, age = 18, name = "ada" }) r
        Err e -> Err e
        Ok _  -> Repo.insert (User { id = 2, age = 30, name = "lin" }) r

-- A transaction body that inserts a row and then fails, so it rolls back.
fn pgInsertThenFail (tx: Postgres) -> Result Unit Error =
    let r = Repo.repo tx "ridge_pg_users"
    match Repo.insert (User { id = 2, age = 30, name = "lin" }) r
        Err e -> Err e
        Ok _  -> pgForceFail tx

-- A transaction body whose nested transaction inserts a row and fails (rewinding
-- to its savepoint); this body commits its own row.
fn pgOuterKeepsInnerRollsBack (tx: Postgres) -> Result Unit Error =
    let r = Repo.repo tx "ridge_pg_users"
    match Repo.insert (User { id = 1, age = 18, name = "ada" }) r
        Err e -> Err e
        Ok _  ->
            let _inner = Repo.transaction tx pgInsertThenFail
            Ok ()

-- A committed transaction makes both inserts durable: COMMIT, then count -> 2.
pub fn db txCommittedCount () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match pgClearUsers conn
                Err _ -> 0 - 2
                Ok _  ->
                    match Repo.transaction conn pgInsertTwo
                        Err _ -> 0 - 3
                        Ok _  -> pgCountUsers conn

-- A failing transaction rolls back its insert over a committed baseline: ROLLBACK
-- leaves only the baseline row -> 1.
pub fn db txRolledBackCount () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match pgClearUsers conn
                Err _ -> 0 - 2
                Ok _  ->
                    let r = Repo.repo conn "ridge_pg_users"
                    match Repo.insert (User { id = 1, age = 18, name = "ada" }) r
                        Err _ -> 0 - 3
                        Ok _  ->
                            match Repo.transaction conn pgInsertThenFail
                                Ok _  -> 0 - 4
                                Err _ -> pgCountUsers conn

-- A nested transaction opens a SAVEPOINT: the inner fails (ROLLBACK TO SAVEPOINT,
-- undoing its insert), the outer commits its own row -> 1.
pub fn db txSavepointCount () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match pgClearUsers conn
                Err _ -> 0 - 2
                Ok _  ->
                    match Repo.transaction conn pgOuterKeepsInnerRollsBack
                        Err _ -> 0 - 3
                        Ok _  -> pgCountUsers conn

-- The migration probes create their own table on the live database through the
-- schema DSL: `CREATE TABLE ridge_mig_widgets (id bigint PRIMARY KEY, name text)`,
-- recorded in the `_ridge_migrations` tracking table. Applying the same schema again
-- is a no-op, so the probes stay deterministic against the persistent test database
-- (the tracking table outlives any one probe).
fn widgetsTable () -> MigrationOp =
    Migrate.createTable "ridge_mig_widgets"
        [ Migrate.intCol  "id"   |> Migrate.primaryKey
        , Migrate.textCol "name" ]

fn runWidgets (conn: Postgres) -> Result (List Text) Error =
    Migrate.run conn [ Migrate.migration "0001_widgets" [ widgetsTable () ] ]

fn pgClearWidgets (conn: Postgres) -> Result Int Error =
    let r = Repo.repo conn "ridge_mig_widgets"
    Repo.delete (fn (w: Widget) -> w.id >= 0) r

fn pgAddWidget (conn: Postgres) (wid: Int) (wname: Text) -> Result Unit Error =
    let r = Repo.repo conn "ridge_mig_widgets"
    Repo.insert (Widget { id = wid, name = wname }) r

fn pgCountWidgets (conn: Postgres) -> Int =
    let r = Repo.repo conn "ridge_mig_widgets"
    match r |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 9

-- Applying a recorded migration again applies nothing: the second run answers an
-- empty list -> 0. Holds whatever the database's prior state, since the first run
-- here records the migration if it was not already.
pub fn db pgMigrateIdempotent () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match runWidgets conn
                Err _ -> 0 - 2
                Ok _  ->
                    match runWidgets conn
                        Ok names -> length names
                        Err _    -> 0 - 3

-- The migrated table is real and typed: after the CREATE TABLE lands, two rows
-- insert and count back -> 2. Clears the table first so the count is deterministic
-- across runs.
pub fn db pgMigratedUsable () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match runWidgets conn
                Err _ -> 0 - 2
                Ok _  ->
                    match pgClearWidgets conn
                        Err _ -> 0 - 3
                        Ok _  ->
                            match pgAddWidget conn 1 "left"
                                Err _ -> 0 - 4
                                Ok _  ->
                                    match pgAddWidget conn 2 "right"
                                        Err _ -> 0 - 5
                                        Ok _  -> pgCountWidgets conn

-- The entity-driven create against the live database: the migration builds the table
-- from `deriving (Schema)` alone — no hand-written column list — and the descriptor's
-- convention names the type's snake-plural table (`ridge_mig_gadgets`) and marks `id`
-- an identity. So `createSchema` renders `CREATE TABLE ridge_mig_gadgets (id bigserial
-- PRIMARY KEY, name text NOT NULL)`, the omitted id is assigned by the sequence on
-- insert, and two rows count back.
pub type RidgeMigGadget = { id: Int, name: Text } deriving (Row, Schema)

fn gadgetWitness () -> Option RidgeMigGadget = None

fn gadgetsSchema () -> MigrationOp =
    Migrate.createSchema (schemaOf (gadgetWitness ()))

fn runGadgets (conn: Postgres) -> Result (List Text) Error =
    Migrate.run conn [ Migrate.migration "0001_gadgets" [ gadgetsSchema () ] ]

fn pgClearGadgets (conn: Postgres) -> Result Int Error =
    let r = Repo.repo conn "ridge_mig_gadgets"
    Repo.delete (fn (g: RidgeMigGadget) -> g.id >= 0) r

fn pgAddGadget (conn: Postgres) (gname: Text) -> Result Unit Error =
    let r = Repo.repo conn "ridge_mig_gadgets"
    Repo.insert (RidgeMigGadgetInsert { name = gname }) r

fn pgCountGadgets (conn: Postgres) -> Int =
    let r = Repo.repo conn "ridge_mig_gadgets"
    match r |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 9

-- The entity-driven create lands a real table with a `serial` identity column: the
-- descriptor's CREATE TABLE runs, the omitted id is assigned on insert, and two rows
-- count back -> 2. Clears the table first so the count is deterministic across runs.
pub fn db pgEntityCreated () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match runGadgets conn
                Err _ -> 0 - 2
                Ok _  ->
                    match pgClearGadgets conn
                        Err _ -> 0 - 3
                        Ok _  ->
                            match pgAddGadget conn "alpha"
                                Err _ -> 0 - 4
                                Ok _  ->
                                    match pgAddGadget conn "beta"
                                        Err _ -> 0 - 5
                                        Ok _  -> pgCountGadgets conn

-- The auto-diff against the live database: `diffSchemas` compares an empty snapshot
-- against a one-entity model and returns a create step, which lands a real table the
-- same way a hand-written `createSchema` does. Uses its own table so the create never
-- collides with the gadget migration, and its own migration name so the tracking table
-- skips it on re-runs (the create runs once; later runs clear, insert, and count).
pub type RidgeMigCog = { id: Int, label: Text } deriving (Row, Schema)

fn cogWitness () -> Option RidgeMigCog = None

fn cogErased () -> EntitySchema Unit =
    eraseSchema (schemaOf (cogWitness ()))

fn runCogsDiff (conn: Postgres) -> Result (List Text) Error =
    Migrate.run conn [ Migrate.migration "0002_cogs" (Migrate.diffSchemas [] [ cogErased () ]) ]

fn pgClearCogs (conn: Postgres) -> Result Int Error =
    let r = Repo.repo conn "ridge_mig_cogs"
    Repo.delete (fn (c: RidgeMigCog) -> c.id >= 0) r

fn pgAddCog (conn: Postgres) (clabel: Text) -> Result Unit Error =
    let r = Repo.repo conn "ridge_mig_cogs"
    Repo.insert (RidgeMigCogInsert { label = clabel }) r

fn pgCountCogs (conn: Postgres) -> Int =
    let r = Repo.repo conn "ridge_mig_cogs"
    match r |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 9

-- A diff-driven create lands a real table: `diffSchemas` yields the create step, the
-- migration runs it, the omitted identity id is assigned on insert, and two rows count
-- back -> 2. Clears the table first so the count is deterministic across runs.
pub fn db pgDiffCreated () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match runCogsDiff conn
                Err _ -> 0 - 2
                Ok _  ->
                    match pgClearCogs conn
                        Err _ -> 0 - 3
                        Ok _  ->
                            match pgAddCog conn "alpha"
                                Err _ -> 0 - 4
                                Ok _  ->
                                    match pgAddCog conn "beta"
                                        Err _ -> 0 - 5
                                        Ok _  -> pgCountCogs conn

-- The column-level auto-diff against the live database: the model gains a `note` field,
-- and `diffSchemas` turns that into an `ALTER TABLE … ADD COLUMN` step that lands on the
-- real table. Its own entity and table so the migrations never collide with the others.
pub type RidgeMigBolt = { id: Int, code: Text, note: Text } deriving (Row, Schema)

fn boltWitness () -> Option RidgeMigBolt = None

-- The bolts table before the `note` column existed — id and code only, hand-built so it
-- stands in as the previous snapshot the diff compares the current model against.
fn boltV1 () -> EntitySchema Unit =
    eraseSchema (schema "RidgeMigBolt" "ridge_mig_bolts"
        |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
        |> withColumn (mkColumn "code" "code" DbText false))

fn boltFull () -> EntitySchema Unit =
    eraseSchema (schemaOf (boltWitness ()))

-- Create the table at v1 (id, code), then diff v1 -> the full model to add `note`. The
-- add runs while the table is still empty, so the NOT NULL column lands cleanly.
fn runBoltsColumnDiff (conn: Postgres) -> Result (List Text) Error =
    let create  = Migrate.migration "0003_bolts" (Migrate.diffSchemas [] [ boltV1 () ])
    let addNote = Migrate.migration "0004_bolts_note" (Migrate.diffSchemas [ boltV1 () ] [ boltFull () ])
    Migrate.run conn [ create, addNote ]

fn pgClearBolts (conn: Postgres) -> Result Int Error =
    let r = Repo.repo conn "ridge_mig_bolts"
    Repo.delete (fn (b: RidgeMigBolt) -> b.id >= 0) r

fn pgAddBolt (conn: Postgres) (bcode: Text) (bnote: Text) -> Result Unit Error =
    let r = Repo.repo conn "ridge_mig_bolts"
    Repo.insert (RidgeMigBoltInsert { code = bcode, note = bnote }) r

fn pgCountBolts (conn: Postgres) -> Int =
    let r = Repo.repo conn "ridge_mig_bolts"
    match r |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 9

-- The diffed ALTER TABLE lands the `note` column on the real table: after the two
-- migrations run, a row written through the full entity (code + note) inserts and counts
-- back -> 2. Clears first so the count is deterministic across runs.
pub fn db pgDiffAddedColumn () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match runBoltsColumnDiff conn
                Err _ -> 0 - 2
                Ok _  ->
                    match pgClearBolts conn
                        Err _ -> 0 - 3
                        Ok _  ->
                            match pgAddBolt conn "a" "first"
                                Err _ -> 0 - 4
                                Ok _  ->
                                    match pgAddBolt conn "b" "second"
                                        Err _ -> 0 - 5
                                        Ok _  -> pgCountBolts conn

-- The column-alter auto-diff against the live database: the `code` column starts NOT NULL
-- and the model relaxes it to nullable, and `diffSchemas` turns that into an
-- `ALTER TABLE … ALTER COLUMN … DROP NOT NULL` step that lands on the real table. Its own
-- entity and table so the migrations never collide with the others.
pub type RidgeMigGear = { id: Int, code: Text } deriving (Row, Schema)

-- The gears table with `code` NOT NULL — the previous snapshot the diff compares against.
-- Hand-built so it shares the table name with the relaxed version below, which is what makes
-- the diff descend into the column rather than treat them as two tables.
fn gearV1 () -> EntitySchema Unit =
    eraseSchema (schema "RidgeMigGear" "ridge_mig_gears"
        |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
        |> withColumn (mkColumn "code" "code" DbText false))

-- The same table with `code` relaxed to nullable — one facet changed, so the diff emits a
-- single `ALTER COLUMN … DROP NOT NULL`.
fn gearV2 () -> EntitySchema Unit =
    eraseSchema (schema "RidgeMigGear" "ridge_mig_gears"
        |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
        |> withColumn (mkColumn "code" "code" DbText true))

-- Create the table at v1 (code NOT NULL), then diff v1 -> v2 to relax `code`. The alter runs
-- against the real table; if the rendered statement were invalid PG the migration would fail.
fn runGearsAlterDiff (conn: Postgres) -> Result (List Text) Error =
    let create = Migrate.migration "0005_gears" (Migrate.diffSchemas [] [ gearV1 () ])
    let relax  = Migrate.migration "0006_gears_relax" (Migrate.diffSchemas [ gearV1 () ] [ gearV2 () ])
    Migrate.run conn [ create, relax ]

fn pgClearGears (conn: Postgres) -> Result Int Error =
    let r = Repo.repo conn "ridge_mig_gears"
    Repo.delete (fn (g: RidgeMigGear) -> g.id >= 0) r

fn pgAddGear (conn: Postgres) (gcode: Text) -> Result Unit Error =
    let r = Repo.repo conn "ridge_mig_gears"
    Repo.insert (RidgeMigGearInsert { code = gcode }) r

fn pgCountGears (conn: Postgres) -> Int =
    let r = Repo.repo conn "ridge_mig_gears"
    match r |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 9

-- The diffed ALTER TABLE lands on the real table: after the create-then-relax migrations
-- run, the table stays usable — two rows insert through the entity and count back -> 2.
-- Clears first so the count is deterministic across runs.
pub fn db pgDiffAlteredColumn () -> Int =
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            match runGearsAlterDiff conn
                Err _ -> 0 - 2
                Ok _  ->
                    match pgClearGears conn
                        Err _ -> 0 - 3
                        Ok _  ->
                            match pgAddGear conn "a"
                                Err _ -> 0 - 4
                                Ok _  ->
                                    match pgAddGear conn "b"
                                        Err _ -> 0 - 5
                                        Ok _  -> pgCountGears conn

-- A seed step against the live database. The migration creates the table and seeds two
-- rows through a real `INSERT ... ON CONFLICT ("id") DO UPDATE`, keyed on the primary key;
-- rolling the seed migration back runs a real keyed `DELETE ... WHERE "id" IN ($1, $2)`
-- and untracks it. The probe applies, checks the two rows landed (a wrong count short-
-- circuits to -4), rolls the seed back, and counts zero rows back -> 0.
--
-- `_ridge_migrations` is shared across every e2e binary pointed at this database, and
-- rollback picks what to reverse from it ordered by name. Another binary can leave a
-- migration whose name sorts after ours but whose steps we do not hold — rollback would
-- select it and fail to resolve. Drop our own migration state first so this apply/rollback
-- cycle stays self-contained no matter what else shares the database (or lingers from a
-- prior local run against a reused container).
pub type RidgeMigLabel = { id: Int, name: Text } deriving (Row, Schema)

fn labelWitness () -> Option RidgeMigLabel = None

fn labelSchema () -> MigrationOp =
    Migrate.createSchema (schemaOf (labelWitness ()))

fn seedLabels () -> MigrationOp =
    Migrate.seed [ RidgeMigLabel { id = 1, name = "one" }, RidgeMigLabel { id = 2, name = "two" } ]

fn pgCountLabels (conn: Postgres) -> Int =
    let r = Repo.repo conn "ridge_mig_labels"
    match r |> Repo.query |> Repo.count
        Ok n  -> n
        Err _ -> 0 - 9

pub fn db pgSeedRollback () -> Int =
    let migs = [ Migrate.migration "0007_labels" [ labelSchema () ], Migrate.migration "0008_labels_seed" [ seedLabels () ] ]
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            let _ = Raw.exec conn "DROP TABLE IF EXISTS _ridge_migrations" []
            let _ = Raw.exec conn "DROP TABLE IF EXISTS ridge_mig_labels" []
            match Migrate.run conn migs
                Err _ -> 0 - 2
                Ok _  ->
                    if pgCountLabels conn == 2 then
                        match Migrate.rollback conn migs 1
                            Err _ -> 0 - 3
                            Ok _  -> pgCountLabels conn
                    else 0 - 4

-- ── runSql migration op against the live database ─────────────────────────────
--
-- The raw-SQL escape hatch as a migration step. On the schemaless in-memory store a
-- `runSql` step reports `raw.unsupported` (proven in the mem migrate e2e); here it runs
-- for real, so these probes exercise the half that only a SQL backend can: a `runSql`
-- CREATE lands a usable table, a plain `runSql` migration is irreversible (raw SQL has no
-- derivable inverse), and a `reversibleMigration` whose `down` is a `runSql` DROP reverses
-- cleanly. Each probe drops its own migration state first, for the shared-`_ridge_migrations`
-- reason spelled out on `pgSeedRollback` above.

-- Count the tables matching a name in the catalog — 0 once a table has been dropped.
fn pgTableCount (conn: Postgres) (name: Text) -> Int =
    let q: Result (List RawCount) Error = Raw.query conn "SELECT count(*) AS n FROM information_schema.tables WHERE table_name = $1" [toSql name]
    match q
        Err _ -> 0 - 9
        Ok rows ->
            match rows
                []     -> 0 - 8
                c :: _ -> c.n

-- Count the indexes matching a name in the catalog — 0 once an index has been dropped.
fn pgIndexCount (conn: Postgres) (name: Text) -> Int =
    let q: Result (List RawCount) Error = Raw.query conn "SELECT count(*) AS n FROM pg_indexes WHERE indexname = $1" [toSql name]
    match q
        Err _ -> 0 - 9
        Ok rows ->
            match rows
                []     -> 0 - 8
                c :: _ -> c.n

-- A `runSql` CREATE runs verbatim and leaves a usable table: applying the migration then
-- inserting a row succeeds (affected 1), which it could not if the CREATE had not run.
pub fn db pgRunSqlApply () -> Int =
    let migs = [ Migrate.migration "0010_gadget" [ Migrate.runSql "CREATE TABLE ridge_mig_gadget (id bigint PRIMARY KEY, label text)" ] ]
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            let _ = Raw.exec conn "DROP TABLE IF EXISTS _ridge_migrations" []
            let _ = Raw.exec conn "DROP TABLE IF EXISTS ridge_mig_gadget" []
            match Migrate.run conn migs
                Err _ -> 0 - 2
                Ok _  ->
                    match Raw.exec conn "INSERT INTO ridge_mig_gadget (id, label) VALUES (1, 'alpha')" []
                        Err _ -> 0 - 3
                        Ok n  -> n

-- A plain `migration` whose step is a `runSql` has no derivable reverse, so rolling it
-- back fails with `migrate.irreversible` — the same contract a lossy `dropTable` reports.
pub fn db pgRunSqlIrreversible () -> Int =
    let migs = [ Migrate.migration "0011_gadget2" [ Migrate.runSql "CREATE TABLE ridge_mig_gadget2 (id bigint PRIMARY KEY)" ] ]
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            let _ = Raw.exec conn "DROP TABLE IF EXISTS _ridge_migrations" []
            let _ = Raw.exec conn "DROP TABLE IF EXISTS ridge_mig_gadget2" []
            match Migrate.run conn migs
                Err _ -> 0 - 2
                Ok _  ->
                    match Migrate.rollback conn migs 1
                        Err e -> if e.code == "migrate.irreversible" then 1 else 0 - 3
                        Ok _  -> 0 - 4

-- A `reversibleMigration` whose `down` is a `runSql` DROP reverses cleanly: after rollback
-- the table is gone, so the catalog count for it is 0 — the `Up`/`Down` `Sql` pattern.
pub fn db pgRunSqlReversible () -> Int =
    let migs = [ Migrate.reversibleMigration "0012_gadget3" [ Migrate.runSql "CREATE TABLE ridge_mig_gadget3 (id bigint PRIMARY KEY)" ] [ Migrate.runSql "DROP TABLE ridge_mig_gadget3" ] ]
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            let _ = Raw.exec conn "DROP TABLE IF EXISTS _ridge_migrations" []
            let _ = Raw.exec conn "DROP TABLE IF EXISTS ridge_mig_gadget3" []
            match Migrate.run conn migs
                Err _ -> 0 - 2
                Ok _  ->
                    match Migrate.rollback conn migs 1
                        Err _ -> 0 - 3
                        Ok _  -> pgTableCount conn "ridge_mig_gadget3"

-- The snapshot diff orders `CREATE TABLE`s so a referenced table precedes its referrer.
-- `child` is declared before `parent` yet carries a foreign key to it, so the migration
-- applies only because the create for `parent` is emitted first; a wrong order would make
-- Postgres reject the child's inline `REFERENCES` (the target would not yet exist). The probe
-- returns the catalog count for `child`, 1 once the create has run.
fn fkTopoParent () -> EntitySchema Unit =
    schema "Parent" "ridge_mig_parent"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> primaryKey)

fn fkTopoChild () -> EntitySchema Unit =
    schema "Child" "ridge_mig_child"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> primaryKey)
      |> withColumn (mkColumn "parent_id" "parent_id" DbBigInt false |> foreignKey (references "ridge_mig_parent" "id"))

fn fkTopoEmpty () -> List (EntitySchema Unit) = []
fn fkTopoModel () -> List (EntitySchema Unit) = [ fkTopoChild (), fkTopoParent () ]

pub fn db pgFkTopoApply () -> Int =
    let migs = [ Migrate.migration "0013_fk_topo" (Migrate.diffSchemas (fkTopoEmpty ()) (fkTopoModel ())) ]
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            let _ = Raw.exec conn "DROP TABLE IF EXISTS ridge_mig_child" []
            let _ = Raw.exec conn "DROP TABLE IF EXISTS ridge_mig_parent" []
            let _ = Raw.exec conn "DROP TABLE IF EXISTS _ridge_migrations" []
            match Migrate.run conn migs
                Err _ -> 0 - 2
                Ok _  -> pgTableCount conn "ridge_mig_child"

-- The column-level auto-diff reconciles non-unique indexes: a column whose `indexed` flag turns
-- on gets a CREATE INDEX, one whose flag turns off a DROP INDEX. `v1` gives `kind` an index;
-- the `v1 -> v2` diff adds an indexed `owner`, turns `sku`'s index on and `kind`'s off. After
-- both migrations `ridge_mig_idx_sku_idx` exists and `ridge_mig_idx_kind_idx` is gone, so the
-- created count minus the dropped count is 1.
fn idxEmpty () -> List (EntitySchema Unit) = []

fn idxV1 () -> EntitySchema Unit =
    schema "Idx" "ridge_mig_idx"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> primaryKey)
      |> withColumn (mkColumn "sku" "sku" DbText false)
      |> withColumn (mkColumn "kind" "kind" DbText false |> indexed)

fn idxV2 () -> EntitySchema Unit =
    schema "Idx" "ridge_mig_idx"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> primaryKey)
      |> withColumn (mkColumn "sku" "sku" DbText false |> indexed)
      |> withColumn (mkColumn "kind" "kind" DbText false)
      |> withColumn (mkColumn "owner" "owner" DbText false |> indexed)

fn idxModelV1 () -> List (EntitySchema Unit) = [ idxV1 () ]
fn idxModelV2 () -> List (EntitySchema Unit) = [ idxV2 () ]

pub fn db pgIndexDiff () -> Int =
    let migs = [ Migrate.migration "0014_idx_v1" (Migrate.diffSchemas (idxEmpty ()) (idxModelV1 ()))
               , Migrate.migration "0015_idx_v2" (Migrate.diffSchemas (idxModelV1 ()) (idxModelV2 ())) ]
    match connect (pgConfig ())
        Err _ -> 0 - 1
        Ok conn ->
            let _ = Raw.exec conn "DROP TABLE IF EXISTS ridge_mig_idx" []
            let _ = Raw.exec conn "DROP TABLE IF EXISTS _ridge_migrations" []
            match Migrate.run conn migs
                Err _ -> 0 - 2
                Ok _  ->
                    let created = pgIndexCount conn "ridge_mig_idx_sku_idx"
                    let dropped = pgIndexCount conn "ridge_mig_idx_kind_idx"
                    created - dropped

-- Raw-SQL escape hatch against the live database. Each probe seeds the users table
-- through `setup` (clearing and inserting ada/lin/max), then opens a fresh
-- connection and runs raw SQL over `ridge_pg_users`: a parameterised SELECT decoded
-- into `User`, its first-row form, a row-less UPDATE for its affected count, and a
-- scalar aliased into a one-field record — the patterns std.raw documents.
pub type RawCount = { n: Int } deriving (Row)

-- raw query + decode: adults (age >= 25) ordered by id, names joined -> "lin,max".
-- Proves the backend binds `$1` and decodes each row into `User` through Row.
pub fn db rawAdults () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok _  ->
            match connect (pgConfig ())
                Err _ -> "conn-err"
                Ok conn ->
                    let q: Result (List User) Error = Raw.query conn "SELECT id, age, name FROM ridge_pg_users WHERE age >= $1 ORDER BY id" [toSql 25]
                    match q
                        Err _ -> "raw-err"
                        Ok us -> joinNames us

-- raw queryFirst: the oldest user by age descending -> "lin" (30). Proves the
-- first-row form decodes a single row, with no bind parameters.
pub fn db rawFirstName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok _  ->
            match connect (pgConfig ())
                Err _ -> "conn-err"
                Ok conn ->
                    let q: Result (Option User) Error = Raw.queryFirst conn "SELECT id, age, name FROM ridge_pg_users ORDER BY age DESC" []
                    match q
                        Err _       -> "raw-err"
                        Ok None     -> "none"
                        Ok (Some u) -> u.name

-- raw exec: a parameterised UPDATE over the two adults -> affected 2. Proves a
-- row-less statement binds its parameters and answers the affected row count.
pub fn db rawBumpCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok _  ->
            match connect (pgConfig ())
                Err _ -> 0 - 2
                Ok conn ->
                    match Raw.exec conn "UPDATE ridge_pg_users SET age = $1 WHERE age >= $2" [toSql 40, toSql 25]
                        Ok n  -> n
                        Err _ -> 0 - 3

-- raw scalar via the alias-into-record pattern: SELECT count(*) AS n -> 3. Proves a
-- computed column aliased to a field decodes through Row like any other row.
pub fn db rawUserCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok _  ->
            match connect (pgConfig ())
                Err _ -> 0 - 2
                Ok conn ->
                    let q: Result (List RawCount) Error = Raw.query conn "SELECT count(*) AS n FROM ridge_pg_users" []
                    match q
                        Err _ -> 0 - 3
                        Ok rows ->
                            match rows
                                []     -> 0 - 4
                                c :: _ -> c.n
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
#[allow(
    clippy::too_many_lines,
    reason = "one probe-and-assertion block per verb reads best in one place"
)]
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

    // Drive the connection pool directly. First, with maintenance off, open one
    // handle with room for four connections, fire six reads at once, and confirm
    // they all come back — exercising concurrent checkout, the pool growing under
    // load, and waiters reusing a connection once it frees. Then two maintenance
    // probes: one with a short idle-timeout, max-lifetime, and health-check that
    // confirms the pool evicts and recycles in the background and reopens on
    // demand; one with only the health-check on that confirms idle connections
    // are pinged and keep serving. Finally three resilience probes: retries and a
    // bounded queue enabled against the live database still connect and serve; a
    // refused connect on a dead port is retried then surfaces db.connect.refused;
    // and a non-existent database fails with a permanent (non-refused) error even
    // with retries on, proving only a refused connect is retried. All against the
    // live database.
    let pool_probe = format!(
        "{{ok, ProbeConn}} = ridge_pg:pg_connect(<<\"{host}\">>, {port}, <<\"{db}\">>, <<\"{user}\">>, <<\"{pass}\">>, <<\"{ssl}\">>, 4, 10000, 30000, 5000, 0, 0, 0, 0, 0, 0), \
         ProbeId = maps:get(id, ProbeConn), \
         ProbeSelf = self(), \
         [spawn(fun() -> ProbeSelf ! {{probe, ridge_pg:pg_all(ProbeId, <<\"ridge_pg_users\">>)}} end) || _ <- lists:seq(1, 6)], \
         ProbeRs = [receive {{probe, ProbeX}} -> ProbeX after 15000 -> timeout end || _ <- lists:seq(1, 6)], \
         ProbeOk = lists:all(fun(ProbeR) -> case ProbeR of {{ok, _}} -> true; _ -> false end end, ProbeRs), \
         io:format(\"concurrent=~p~n\", [ProbeOk]), \
         ridge_pg:pg_close(ProbeId), \
         {{ok, MaintConn}} = ridge_pg:pg_connect(<<\"{host}\">>, {port}, <<\"{db}\">>, <<\"{user}\">>, <<\"{pass}\">>, <<\"{ssl}\">>, 2, 10000, 30000, 5000, 300, 600, 200, 0, 0, 0), \
         MaintId = maps:get(id, MaintConn), \
         MaintBefore = ridge_pg:pg_all(MaintId, <<\"ridge_pg_users\">>), \
         timer:sleep(1200), \
         MaintAfter = ridge_pg:pg_all(MaintId, <<\"ridge_pg_users\">>), \
         MaintOk = case {{MaintBefore, MaintAfter}} of {{{{ok, _}}, {{ok, _}}}} -> true; _ -> false end, \
         io:format(\"maintHeals=~p~n\", [MaintOk]), \
         ridge_pg:pg_close(MaintId), \
         {{ok, PingConn}} = ridge_pg:pg_connect(<<\"{host}\">>, {port}, <<\"{db}\">>, <<\"{user}\">>, <<\"{pass}\">>, <<\"{ssl}\">>, 2, 10000, 30000, 5000, 0, 0, 200, 0, 0, 0), \
         PingId = maps:get(id, PingConn), \
         _ = ridge_pg:pg_all(PingId, <<\"ridge_pg_users\">>), \
         timer:sleep(800), \
         PingAfter = ridge_pg:pg_all(PingId, <<\"ridge_pg_users\">>), \
         io:format(\"pingHealthy=~p~n\", [case PingAfter of {{ok, _}} -> true; _ -> false end]), \
         ridge_pg:pg_close(PingId), \
         RetryConn = ridge_pg:pg_connect(<<\"{host}\">>, {port}, <<\"{db}\">>, <<\"{user}\">>, <<\"{pass}\">>, <<\"{ssl}\">>, 2, 10000, 30000, 5000, 0, 0, 0, 3, 100, 8), \
         RetryOk = case RetryConn of {{ok, RConn}} -> RId = maps:get(id, RConn), RR = ridge_pg:pg_all(RId, <<\"ridge_pg_users\">>), ridge_pg:pg_close(RId), case RR of {{ok, _}} -> true; _ -> false end; _ -> false end, \
         io:format(\"retryHappy=~p~n\", [RetryOk]), \
         DeadConn = ridge_pg:pg_connect(<<\"{host}\">>, 59999, <<\"{db}\">>, <<\"{user}\">>, <<\"{pass}\">>, <<\"{ssl}\">>, 1, 2000, 30000, 5000, 0, 0, 0, 2, 50, 0), \
         RetryExhausts = case DeadConn of {{error, #{{code := <<\"db.connect.refused\">>}}}} -> true; _ -> false end, \
         io:format(\"retryExhausts=~p~n\", [RetryExhausts]), \
         BadDbConn = ridge_pg:pg_connect(<<\"{host}\">>, {port}, <<\"ridge_no_such_db_xyz\">>, <<\"{user}\">>, <<\"{pass}\">>, <<\"{ssl}\">>, 1, 10000, 30000, 5000, 0, 0, 0, 5, 50, 0), \
         PermNoRetry = case BadDbConn of {{error, #{{code := PermCode}}}} -> PermCode =/= <<\"db.connect.refused\">>; _ -> false end, \
         io:format(\"permanentNoRetry=~p~n\", [PermNoRetry]), ",
        host = parts.host,
        port = parts.port,
        db = parts.database,
        user = parts.user,
        pass = parts.password,
        ssl = parts.sslmode,
    );
    let expr = format!(
        "io:format(\"countAll=~w~n\",[{module}:countAll()]), \
         io:format(\"withConnRuns=~s~n\",[{module}:withConnRuns()]), \
         io:format(\"connectWithRuns=~s~n\",[{module}:connectWithRuns()]), \
         io:format(\"adultsCount=~w~n\",[{module}:adultsCount()]), \
         io:format(\"likeInChecks=~s~n\",[{module}:likeInChecks()]), \
         io:format(\"arithChecks=~s~n\",[{module}:arithChecks()]), \
         io:format(\"projChecks=~s~n\",[{module}:projChecks()]), \
         io:format(\"orderAggChecks=~s~n\",[{module}:orderAggChecks()]), \
         io:format(\"firstName=~s~n\",[{module}:firstName()]), \
         io:format(\"getName=~s~n\",[{module}:getName()]), \
         io:format(\"afterDelete=~w~n\",[{module}:afterDelete()]), \
         io:format(\"orderedNames=~s~n\",[{module}:orderedNames()]), \
         io:format(\"pagedName=~s~n\",[{module}:pagedName()]), \
         io:format(\"capturedAdults=~s~n\",[{module}:capturedAdults()]), \
         io:format(\"capturedByName=~s~n\",[{module}:capturedByName()]), \
         io:format(\"capturedInList=~s~n\",[{module}:capturedInList()]), \
         io:format(\"capturedInTextList=~s~n\",[{module}:capturedInTextList()]), \
         io:format(\"summaryNames=~s~n\",[{module}:summaryNames()]), \
         io:format(\"topYears=~w~n\",[{module}:topYears()]), \
         io:format(\"joinedNames=~s~n\",[{module}:joinedNames()]), \
         io:format(\"joinedTitles=~s~n\",[{module}:joinedTitles()]), \
         io:format(\"joinOrderByRight=~s~n\",[{module}:joinOrderByRight()]), \
         io:format(\"existsPosts=~s~n\",[{module}:existsPosts()]), \
         io:format(\"notExistsPosts=~s~n\",[{module}:notExistsPosts()]), \
         io:format(\"existsPostsCount=~w~n\",[{module}:existsPostsCount()]), \
         io:format(\"existsInJoinWhere=~s~n\",[{module}:existsInJoinWhere()]), \
         io:format(\"nestedExists=~s~n\",[{module}:nestedExists()]), \
         io:format(\"crossJoined=~s~n\",[{module}:crossJoined()]), \
         io:format(\"crossCount=~w~n\",[{module}:crossCount()]), \
         io:format(\"rightJoinedNames=~s~n\",[{module}:rightJoinedNames()]), \
         io:format(\"rightSelectNames=~s~n\",[{module}:rightSelectNames()]), \
         io:format(\"rightJoinCount=~w~n\",[{module}:rightJoinCount()]), \
         io:format(\"rightJoinSumLeftId=~w~n\",[{module}:rightJoinSumLeftId()]), \
         io:format(\"rightJoinGroupAuthors=~s~n\",[{module}:rightJoinGroupAuthors()]), \
         io:format(\"fullJoinCategories=~s~n\",[{module}:fullJoinCategories()]), \
         io:format(\"fullSelectShape=~s~n\",[{module}:fullSelectShape()]), \
         io:format(\"fullJoinCount=~w~n\",[{module}:fullJoinCount()]), \
         io:format(\"fullJoinSumPostId=~w~n\",[{module}:fullJoinSumPostId()]), \
         io:format(\"fullJoinGroupAuthors=~s~n\",[{module}:fullJoinGroupAuthors()]), \
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
         io:format(\"joinCount=~w~n\",[{module}:joinCount()]), \
         io:format(\"joinAny=~s~n\",[{module}:joinAny()]), \
         io:format(\"joinEveryAdult=~s~n\",[{module}:joinEveryAdult()]), \
         io:format(\"joinEveryHello=~s~n\",[{module}:joinEveryHello()]), \
         io:format(\"leftJoinCount=~w~n\",[{module}:leftJoinCount()]), \
         io:format(\"leftJoinEveryAuthored=~s~n\",[{module}:leftJoinEveryAuthored()]), \
         io:format(\"groupCounts=~s~n\",[{module}:groupCounts()]), \
         io:format(\"groupSums=~s~n\",[{module}:groupSums()]), \
         io:format(\"groupAvgs=~s~n\",[{module}:groupAvgs()]), \
         io:format(\"groupRanges=~s~n\",[{module}:groupRanges()]), \
         io:format(\"groupComputedSum=~s~n\",[{module}:groupComputedSum()]), \
         io:format(\"groupComputedHaving=~s~n\",[{module}:groupComputedHaving()]), \
         io:format(\"groupHavingCount=~s~n\",[{module}:groupHavingCount()]), \
         io:format(\"groupHavingSum=~s~n\",[{module}:groupHavingSum()]), \
         io:format(\"groupFilteredHaving=~s~n\",[{module}:groupFilteredHaving()]), \
         io:format(\"joinGroupCounts=~s~n\",[{module}:joinGroupCounts()]), \
         io:format(\"joinGroupRightIds=~s~n\",[{module}:joinGroupRightIds()]), \
         io:format(\"joinGroupHaving=~s~n\",[{module}:joinGroupHaving()]), \
         io:format(\"joinGroupByTitle=~s~n\",[{module}:joinGroupByTitle()]), \
         io:format(\"leftJoinGroupCounts=~s~n\",[{module}:leftJoinGroupCounts()]), \
         io:format(\"deptsAll=~s~n\",[{module}:deptsAll()]), \
         io:format(\"deptsDistinct=~s~n\",[{module}:deptsDistinct()]), \
         io:format(\"salariesDistinct=~s~n\",[{module}:salariesDistinct()]), \
         io:format(\"unionIds=~s~n\",[{module}:unionIds()]), \
         io:format(\"intersectIds=~s~n\",[{module}:intersectIds()]), \
         io:format(\"exceptIds=~s~n\",[{module}:exceptIds()]), \
         io:format(\"unionAllCount=~w~n\",[{module}:unionAllCount()]), \
         io:format(\"unionFiltered=~s~n\",[{module}:unionFiltered()]), \
         io:format(\"nestedUnionIds=~s~n\",[{module}:nestedUnionIds()]), \
         io:format(\"txCommittedCount=~w~n\",[{module}:txCommittedCount()]), \
         io:format(\"txRolledBackCount=~w~n\",[{module}:txRolledBackCount()]), \
         io:format(\"txSavepointCount=~w~n\",[{module}:txSavepointCount()]), \
         io:format(\"pgMigrateIdempotent=~w~n\",[{module}:pgMigrateIdempotent()]), \
         io:format(\"pgMigratedUsable=~w~n\",[{module}:pgMigratedUsable()]), \
         io:format(\"pgEntityCreated=~w~n\",[{module}:pgEntityCreated()]), \
         io:format(\"pgDiffCreated=~w~n\",[{module}:pgDiffCreated()]), \
         io:format(\"pgDiffAddedColumn=~w~n\",[{module}:pgDiffAddedColumn()]), \
         io:format(\"pgDiffAlteredColumn=~w~n\",[{module}:pgDiffAlteredColumn()]), \
         io:format(\"pgSeedRollback=~w~n\",[{module}:pgSeedRollback()]), \
         io:format(\"pgRunSqlApply=~w~n\",[{module}:pgRunSqlApply()]), \
         io:format(\"pgRunSqlIrreversible=~w~n\",[{module}:pgRunSqlIrreversible()]), \
         io:format(\"pgRunSqlReversible=~w~n\",[{module}:pgRunSqlReversible()]), \
         io:format(\"pgFkTopoApply=~w~n\",[{module}:pgFkTopoApply()]), \
         io:format(\"pgIndexDiff=~w~n\",[{module}:pgIndexDiff()]), \
         io:format(\"rawAdults=~s~n\",[{module}:rawAdults()]), \
         io:format(\"rawFirstName=~s~n\",[{module}:rawFirstName()]), \
         io:format(\"rawBumpCount=~w~n\",[{module}:rawBumpCount()]), \
         io:format(\"rawUserCount=~w~n\",[{module}:rawUserCount()]), \
         io:format(\"uniqueViolationKind=~s~n\",[{module}:uniqueViolationKind()]), \
         io:format(\"notNullViolationDetail=~s~n\",[{module}:notNullViolationDetail()]), \
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
        (
            "withConnRuns=rows:3",
            "withConnection runs the body over the real wire (counting the seeded rows) and closes the handle on the way out",
        ),
        (
            "connectWithRuns=rows:3",
            "connectWith opens with an explicit tuned pool over the real wire and disconnect releases the handle",
        ),
        ("adultsCount=2", "findBy keeps the two rows with age >= 25"),
        (
            "likeInChecks=2,1,1,0,0,2,0",
            "LIKE/IN compile to real SQL: contains/startsWith/raw-LIKE, escaped %/_ \
             matching nothing, IN over a set and the empty set",
        ),
        (
            "arithChecks=1,2,1,2",
            "arithmetic compiles to real SQL: age*2>50, age+id>20, integer age/10==2, \
             age%2==0",
        ),
        (
            "projChecks=minor:36,adult:60,adult:50",
            "a computed projection compiles to real SQL: a CASE label and a doubled \
             age, decoded per row",
        ),
        (
            "orderAggChecks=max,ada,lin:146",
            "a computed ORDER BY key and a computed aggregate compile to real SQL with \
             their literals bound as placeholders",
        ),
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
            "capturedAdults=max,lin",
            "an Int captured from the enclosing scope reaches the real query as a bound parameter",
        ),
        (
            "capturedByName=lin",
            "a captured Text value drives the equality as a placeholder against Postgres",
        ),
        (
            "capturedInList=max,lin",
            "a captured List Int compiles into a real IN over bound parameters",
        ),
        (
            "capturedInTextList=ada,lin",
            "a captured List Text compiles into a real IN over the name column",
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
            "existsPosts=lin,max",
            "a correlated EXISTS compiles to a SELECT ... WHERE EXISTS (SELECT 1 FROM posts AS r WHERE r.author = l.id) and keeps the users who own a post",
        ),
        (
            "notExistsPosts=ada",
            "a correlated NOT EXISTS keeps the complement — the users who own no post",
        ),
        (
            "existsPostsCount=2",
            "count over a filter with a correlated EXISTS renders the subquery on the COUNT(*) path too",
        ),
        (
            "existsInJoinWhere=max:world",
            "a correlated EXISTS inside a binary join's WHERE runs on Postgres, the inner table aliased x2 past both join sides and correlating to the left leaf",
        ),
        (
            "nestedExists=max",
            "an EXISTS nested inside another EXISTS runs on Postgres, the inner probe (x2) correlating to the outer probe's row (x1)",
        ),
        (
            "crossJoined=lin:hello,lin:world",
            "a cross join pairs lin with every post, including world (author 3) that lin does not own, so the backend's unconditional join spans both posts",
        ),
        (
            "crossCount=6",
            "COUNT(*) over the full cross join is 3 users * 2 posts = 6 pairs",
        ),
        (
            "rightJoinedNames=lin:hello,-:world",
            "pg_right_join keeps every post and folds the left filter into the ON, so world (authored by the filtered-out max) keeps a None left side as `-:world`",
        ),
        (
            "rightSelectNames=lin:hello,-:world",
            "pg_right_join_select keeps the unmatched world row and decodes its NULL left column into an Option field as None",
        ),
        (
            "rightJoinCount=2",
            "pg_count_right_join keeps both posts (one matched, world unmatched) where an inner join would count only one",
        ),
        (
            "rightJoinSumLeftId=2",
            "pg_aggregate_right_join folds the left id only over the matched row (lin = 2), skipping the unmatched world",
        ),
        (
            "rightJoinGroupAuthors=2:1,3:1",
            "pg_group_summarize_right_join groups every post by author id over the RIGHT JOIN",
        ),
        (
            "fullJoinCategories=both:1,left:1,right:1",
            "pg_full_join keeps the matched lin:hello, the left-only ada, and the right-only world across the marker split",
        ),
        (
            "fullSelectShape=rows:3,noWho:1,noTitle:1",
            "pg_full_join_select reads both sides as Option: world projects who=None, ada projects title=None",
        ),
        (
            "fullJoinCount=3",
            "pg_count_full_join counts every row of both tables (one matched, one left-only, one right-only)",
        ),
        (
            "fullJoinSumPostId=21",
            "pg_aggregate_full_join folds the post id over the matched and right-only rows (10+11), skipping the left-only ada's NULL",
        ),
        (
            "fullJoinGroupAuthors=2:1,3:1",
            "pg_group_summarize_full_join groups every post by author id over the FULL JOIN (key total here, ids >= 2)",
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
            "joinCount=2",
            "count compiles a COUNT(*) over the inner join (two user-post pairs)",
        ),
        ("joinAny=true", "exists probes the inner join for any pair"),
        (
            "joinEveryAdult=true",
            "every folds a two-row left-column predicate into the join's count comparison",
        ),
        (
            "joinEveryHello=false",
            "a right-column every narrows the matching count below the join total (world fails)",
        ),
        (
            "leftJoinCount=3",
            "countLeftJoin counts every left-outer row, the unmatched ada included",
        ),
        (
            "leftJoinEveryAuthored=false",
            "a right-column every over a left join fails the unmatched ada row (its post is NULL)",
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
            "groupComputedSum=eng:600,ops:100,sales:1200",
            "SUM(salary * 2) folds a computed expression per group, the literal bound",
        ),
        (
            "groupComputedHaving=sales:1200",
            "HAVING SUM(salary * 2) >= 1200 keeps only sales, a computed aggregate threshold",
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
            "joinGroupCounts=lin:1,max:1",
            "group a real join by the left key: lin and max each author one post",
        ),
        (
            "joinGroupRightIds=lin:10,max:11",
            "a grouped join aggregate folds the right table's post id per group",
        ),
        (
            "joinGroupHaving=max:11",
            "having re-renders a right-side sum: only max's post id clears 11",
        ),
        (
            "joinGroupByTitle=hello:1,world:1",
            "the group key qualifies to the right table, one group per post title",
        ),
        (
            "leftJoinGroupCounts=ada:1,lin:1,max:1",
            "a real LEFT JOIN keeps ada as a one-row group though it matches no post",
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
            "txCommittedCount=2",
            "a committed transaction persists both inserts on the live database",
        ),
        (
            "txRolledBackCount=1",
            "a failed transaction rolls back its insert, leaving the committed baseline",
        ),
        (
            "txSavepointCount=1",
            "a nested transaction's failure rewinds to its savepoint while the outer commits",
        ),
        (
            "pgMigrateIdempotent=0",
            "re-running a recorded migration applies nothing on the live database",
        ),
        (
            "pgMigratedUsable=2",
            "a CREATE TABLE migration lands and the typed table accepts two inserts",
        ),
        (
            "pgEntityCreated=2",
            "an entity-driven createSchema renders the descriptor's CREATE TABLE with a serial id and the table accepts two inserts",
        ),
        (
            "pgDiffCreated=2",
            "the auto-diff turns a new entity into a create step that lands a real table accepting two inserts",
        ),
        (
            "pgDiffAddedColumn=2",
            "the column-level auto-diff turns a new field into an ALTER TABLE ADD COLUMN that lands on the real table",
        ),
        (
            "pgDiffAlteredColumn=2",
            "the column-level auto-diff turns a relaxed NOT NULL into an ALTER TABLE ALTER COLUMN DROP NOT NULL that lands on the real table",
        ),
        (
            "pgSeedRollback=0",
            "a seed step runs a real INSERT ... ON CONFLICT DO UPDATE (two rows), and rolling it back runs a real keyed DELETE that removes exactly them",
        ),
        (
            "pgRunSqlApply=1",
            "a runSql CREATE runs verbatim against the real database, leaving a table a follow-up INSERT lands one row into",
        ),
        (
            "pgRunSqlIrreversible=1",
            "a plain runSql migration has no derivable reverse, so rolling it back fails with migrate.irreversible",
        ),
        (
            "pgRunSqlReversible=0",
            "a reversibleMigration whose down is a runSql DROP reverses cleanly, so the table is gone from the catalog afterwards",
        ),
        (
            "pgFkTopoApply=1",
            "the snapshot diff orders creates topologically, so a child table declared before its parent still applies because the parent's create is emitted first",
        ),
        (
            "pgIndexDiff=1",
            "the column-level auto-diff turns an indexed flag on into a real CREATE INDEX and off into a real DROP INDEX",
        ),
        (
            "rawAdults=lin,max",
            "Raw.query binds $1 and decodes the adult rows into User through Row",
        ),
        (
            "rawFirstName=lin",
            "Raw.queryFirst decodes the single oldest row with no bind parameters",
        ),
        (
            "rawBumpCount=2",
            "Raw.exec binds an UPDATE's parameters and answers the affected row count",
        ),
        (
            "rawUserCount=3",
            "a raw scalar aliased to a record field decodes through Row (count(*) AS n)",
        ),
        (
            "uniqueViolationKind=unique:ridge_pg_uniq_id_key",
            "a real unique violation classifies to UniqueViolation (SQLSTATE 23505) and dbErrorConstraint reads the named constraint out of the ErrorResponse",
        ),
        (
            "notNullViolationDetail=notnull:val:ridge_pg_notnull",
            "a real not-null violation classifies to NotNullViolation (SQLSTATE 23502); dbErrorColumn reads the offending column and dbErrorTable its table out of the ErrorResponse",
        ),
        (
            "concurrent=true",
            "the pool serves six concurrent reads on one handle",
        ),
        (
            "maintHeals=true",
            "the maintenance sweep evicts idle and recycles aged connections in the background, and the pool reopens a fresh one on demand",
        ),
        (
            "pingHealthy=true",
            "the active health-check pings idle connections and the pool keeps serving",
        ),
        (
            "retryHappy=true",
            "a pool with connection retries and a bounded wait queue still connects and serves",
        ),
        (
            "retryExhausts=true",
            "a refused connect is retried and then surfaces db.connect.refused",
        ),
        (
            "permanentNoRetry=true",
            "a permanent connect error (a non-existent database) fails fast and is not retried",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "expected `{probe}` ({want})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
