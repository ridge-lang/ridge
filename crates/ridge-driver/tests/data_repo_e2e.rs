//! End-to-end check for the std.repo typed repository — running on the BEAM,
//! with rows auto-decoded into a record through `deriving (Row)`.
//!
//! A `Repo User MemAdapter` pairs the in-memory adapter with the `users` table
//! and the `User` entity. Its read verbs run the `Adapter` primitives and decode
//! each row back into a `User`, so `find`/`getBy` answer a typed `User` whose
//! fields are read directly (`u.name`), and `findBy`/`count`/`exists`/`deleteWhere`
//! compose as a pipeline. The program seeds three users and exercises:
//! - `count` over the whole table,
//! - `findBy` filtering (a `>=` predicate keeps two of three rows),
//! - `find` + decode (a `>` predicate's first row decodes to "lin"),
//! - `getBy` key (id 2 decodes to "lin"),
//! - `exists` (a `<` predicate matches the one young row),
//! - `deleteWhere` predicate (one row goes; the table then holds two),
//! - the query builder: `orderBy` (whole-table ordering), `offset`/`limit`
//!   paging, and `filter` + `orderBy` + the `first` terminal.
//! - the inner join: `joinOn` + `toList` (decoding both entities of each matched
//!   pair) and `joinOn` + `select` (projecting columns from both sides into a
//!   named shape).
//! - the left join: `leftJoinOn` + `toList` (keeping every left row and decoding
//!   the right entity as `Option`, so an unmatched left row survives)
//!   and `leftJoinOn` + `selectLeftJoin` (projecting both sides into a named
//!   shape whose right-derived fields are `Option`, `None` for an unmatched row).
//! - the unique-row terminals: `single` (the lone match, `None` for empty, an
//!   error for more than one), `singleOrError` (the same, but an empty match is
//!   an error too), and `every` (the universal dual of `exists`, `true` over an
//!   empty selection).
//! - grouped aggregates over a second `Emp` dataset: `groupBy` + `summarize`
//!   projecting a named record per group (count, sum, average, and a min/max
//!   range), `having` narrowing by an aggregate (count and sum thresholds), and a
//!   filter-then-group case where the query's `WHERE` bounds the grouping.
//! - `distinct`: a projection that drops the repeated dept and salary columns to
//!   their distinct values, and a whole-row `distinct` that collapses exact
//!   duplicate rows.
//! - set operations: `union`/`unionAll`/`intersect`/`except` over two overlapping
//!   filters, with `orderBy`/`filter` composing on the combined result and a
//!   nested `(eng ∪ sales) ∪ ops`.
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
import std.map as Map
import std.int as Int
import std.float as Float

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

-- A second entity for the join: a post owned by a user (`author` holds the
-- owner's id). Single-word columns keep the seeded keys identical to the field
-- names, so the join's column tagging is observable without snake-case mapping.
pub type Post = { id: Int, author: Int, title: Text } deriving (Row)

-- A projected shape: the projection renames `name` -> `who` and `age` -> `years`,
-- so the decode proves the alias (`column AS alias`) and re-keying both work.
pub type Summary = { who: Text, years: Int } deriving (Row)

-- The shape a join projection decodes into: a name from the left entity and a
-- title from the right, so a `selectJoin` proves columns from both sides reach
-- one named record.
pub type Combo = { person: Text, post: Text } deriving (Row)

-- The shape a left-join projection decodes into: the right-derived `post` is
-- `Option Text`, so an unmatched left row projects it as `None`.
pub type ComboOpt = { person: Text, post: Option Text } deriving (Row)

-- The mirror shape a right-join projection decodes into: the left-derived `person`
-- is `Option Text`, so an unmatched right row projects it as `None`.
pub type ComboOptL = { person: Option Text, post: Text } deriving (Row)

-- A grouped-count shape keyed by an integer column (a post's author id), so a
-- right join can group by a right-side column and decode the integer key.
pub type AuthorCount = { author: Int, n: Int } deriving (Row)

-- A single-name projection shape for a join, so a `distinct` over a join's
-- projection collapses the repeated left entity (one person, several posts) to its
-- distinct values.
pub type Person = { person: Text } deriving (Row)

-- Join the names of a user list with commas, so a query's order is observable
-- as a single string the probe can assert on.
fn joinNames (us: List User) -> Text =
    match us
        []        -> ""
        u :: []   -> u.name
        u :: rest -> Text.concat u.name (Text.concat "," (joinNames rest))

-- The `who` field of each summary, comma-joined, so a projection's order and
-- column renaming are both observable as one string.
fn joinWho (ss: List Summary) -> Text =
    match ss
        []        -> ""
        s :: []   -> s.who
        s :: rest -> Text.concat s.who (Text.concat "," (joinWho rest))

-- Render each `(User, Post)` pair as `name:title`, comma-joined, so a join's
-- decode of both entities and its row order are observable as one string.
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

-- An optional projected title, or "-" when the column was NULL (an unmatched
-- left row).
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

-- Render each `(User, Option Post)` pair as `name:title` (or `name:-` for an
-- unmatched left row), comma-joined, so a left join's kept-but-unmatched rows
-- are observable as one string alongside the matched ones.
fn joinLeftPairs (ps: List (User, Option Post)) -> Text =
    match ps
        []              -> ""
        (u, op) :: []   -> Text.concat u.name (Text.concat ":" (optTitle op))
        (u, op) :: rest -> Text.concat u.name (Text.concat ":" (Text.concat (optTitle op) (Text.concat "," (joinLeftPairs rest))))

-- The name of an optional left user, or "-" when the right row matched none — the
-- right-join mirror of `optTitle`.
fn optName (ou: Option User) -> Text =
    match ou
        None   -> "-"
        Some u -> u.name

-- Render each `(Option User, Post)` pair as `name:title` (or `-:title` for an
-- unmatched right row), comma-joined — the right-join mirror of `joinLeftPairs`, so
-- a right join's kept-but-unmatched right rows are observable as one string.
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

-- Render the `person` field of each projected join row, comma-joined.
fn personList (ps: List Person) -> Text =
    match ps
        []        -> ""
        p :: []   -> p.person
        p :: rest -> Text.concat p.person (Text.concat "," (personList rest))

pub fn userRow (uid: Int) (uage: Int) (uname: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql uid), ("age", toSql uage), ("name", toSql uname)]

pub fn postRow (pid: Int) (pauthor: Int) (ptitle: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql pid), ("author", toSql pauthor), ("title", toSql ptitle)]

fn listLen (xs: List x) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + listLen rest

-- Open a fresh store, bind a repository to it, and seed three users; return the
-- repository so each probe queries its own isolated data.
pub fn db setup () -> Result (Repo User MemAdapter) Error =
    let r = Repo.repo (memAdapter ()) "users"
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

-- exists: is any user younger than 20? (ada 18) -> 1
pub fn db existsYoung () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age < 20) |> Repo.exists
                Err _ -> 0 - 2
                Ok b  -> if b then 1 else 0

-- delete: how many users are under 25? (ada 18) -> 1
pub fn db deleteCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.deleteWhere (fn (u: User) -> u.age < 25)
                Ok n  -> n
                Err _ -> 0 - 2

-- delete then count what remains -> 2
pub fn db afterCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.deleteWhere (fn (u: User) -> u.age < 25)
                Err _ -> 0 - 2
                Ok _  ->
                    match r |> Repo.query |> Repo.count
                        Ok n  -> n
                        Err _ -> 0 - 3

-- builder: every user ordered by age descending, names joined -> "lin,max,ada"
-- (ages 30, 25, 18). Proves orderBy threads through the seam and the runtime
-- sorts.
pub fn db orderedNames () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- builder: ascending by age, skip 1, take 1 -> "max" (ada 18 skipped, max 25
-- taken). Proves offset and limit compose.
pub fn db pagedName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.age) |> Repo.offset 1 |> Repo.limit 1 |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- builder: filter to adults, order by age descending, take the first -> "lin".
-- Proves filter + orderBy + the `first` terminal compose.
pub fn db firstAdultName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 25) |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.first
                Err _       -> "first-err"
                Ok None     -> "none"
                Ok (Some u) -> u.name

-- projection: order by age descending, project into the renamed `Summary`, and
-- join the `who` fields -> "lin,max,ada". Proves selectList pushes the
-- select-list down and decodes the aliased columns into the named shape.
pub fn db summaryNames () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.select (fn (u: User) -> Summary { who = u.name, years = u.age })
                Err _ -> "list-err"
                Ok ss -> joinWho ss

-- projection: order by age descending, take the first summary, read its renamed
-- `years` column -> 30 (lin). Proves selectFirst + decode of an aliased column.
pub fn db topYears () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.selectFirst (fn (u: User) -> Summary { who = u.name, years = u.age })
                Err _       -> 0 - 2
                Ok None     -> 0 - 3
                Ok (Some s) -> s.years

-- Open one store, bind a users and a posts repository to it (so the join sees
-- both tables), and seed three users and three posts. Post `author` references a
-- user id: lin (id 2) owns "hello" and "again", max (id 3) owns "world", ada
-- (id 1) owns none. Return both repositories.
pub fn db setupJoin () -> Result (Repo User MemAdapter, Repo Post MemAdapter) Error =
    let conn = memAdapter ()
    let users: Repo User MemAdapter = Repo.repo conn "users"
    let posts: Repo Post MemAdapter = Repo.repo conn "posts"
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
                                        Ok _  ->
                                            match Repo.insertRow (postRow 12 2 "again") posts
                                                Err e -> Err e
                                                Ok _  -> Ok (users, posts)

-- join: inner-join users to their posts on `u.id == p.author`, order by user id,
-- and render `name:title` per pair -> "lin:hello,lin:again,max:world" (ada has
-- no posts, so the inner join drops it). Proves the unified `toList` decodes both
-- entities of a join, the condition tags left/right columns, and the order threads
-- through.
pub fn db joinedNames () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.toList
                Err _  -> "join-err"
                Ok ps  -> joinPairs ps

-- join projection: the same join, projected into `Combo { person, post }` and
-- rendered -> "lin:hello,lin:again,max:world". Proves selectJoin pushes a
-- qualified select-list down and decodes the aliased columns into the shape.
pub fn db joinedTitles () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.select (fn (u: User) (p: Post) -> Combo { person = u.name, post = p.title })
                Err _  -> "select-err"
                Ok cs  -> joinCombos cs

-- join ordered by a RIGHT column: the same inner join, ordered by the post title
-- (a right-table column) instead of the user id -> "lin:again,lin:hello,max:world"
-- (titles sort again < hello < world). Proves the unified `orderBy` takes a
-- two-row key on a `Join` and the seam orders by a column of the right table.
pub fn db joinOrderByRight () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.title) |> Repo.toList
                Err _  -> "join-order-err"
                Ok ps  -> joinPairs ps

-- cross join: pair every left row with every right row (the cartesian product).
-- Narrow the left query to lin (id 2), cross with all three posts, order by post
-- id, and render `name:title` per pair -> "lin:hello,lin:world,lin:again". lin
-- pairs with "world" too — a post it does not own — so the product is
-- unconditional, unlike the inner join that keeps only lin's own posts.
pub fn db crossJoined () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 2) |> Repo.crossJoin posts |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id) |> Repo.toList
                Err _  -> "cross-err"
                Ok ps  -> joinPairs ps

-- cross-join count: every user crossed with every post -> 3 * 3 = 9. Proves the
-- product is the full cartesian and that `count` threads through the join seam.
pub fn db crossCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.crossJoin posts |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- right join: keep every post, pairing each with its author or with `None`. The left
-- query is narrowed to ids <= 2 (so max, id 3, drops out of the match), then a right
-- join keeps every post and folds that filter into the join — `world` (authored by
-- max) keeps its place with a `None` left side rather than being dropped. Ordered by
-- post id and rendered `name:title` (or `-:title`) ->
-- "lin:hello,-:world,lin:again". The mirror of `leftJoinedNames`: where a left join
-- keeps unmatched left rows, a right join keeps unmatched right rows and decodes the
-- left entity as `Option`.
pub fn db rightJoinedNames () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: Option User) (p: Post) -> p.id) |> Repo.toList
                Err _  -> "right-join-err"
                Ok ps  -> joinRightPairs ps

-- right-join projection: the same right join, projected into `ComboOptL` where
-- `person` is `Option Text`, rendered -> "lin:hello,-:world,lin:again". `world` has
-- no matching author, so its projected `person` column is NULL and decodes to `None`
-- (`-:world`). Proves `rightJoinSelect` keeps unmatched right rows and decodes the
-- left columns into Option fields.
pub fn db rightSelectNames () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: Option User) (p: Post) -> p.id) |> Repo.select (fn (u: Option User) (p: Post) -> ComboOptL { person = u.name, post = p.title })
                Err _  -> "right-select-err"
                Ok cs  -> joinComboOptLs cs

-- right-join count: the same narrowed right join keeps all three posts, two matched
-- and one (`world`) unmatched, so the count is 3 — proving `countRightJoin` keeps
-- every right row where the inner join would count only the two matches.
pub fn db rightJoinCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- right-join aggregate over a LEFT column: sum the matched users' ids across the
-- narrowed right join. `hello` and `again` match lin (id 2), `world` matches no one
-- (its left side is NULL), so the fold skips it -> 2 + 2 = 4. Proves
-- `aggregateRightJoin` folds a left column only over the matched rows, the unmatched
-- right rows contributing a NULL the fold drops.
pub fn db rightJoinSumLeftId () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.filter (fn (u: User) -> u.id <= 2) |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> u.id)
                Err _       -> 0 - 2
                Ok None     -> 0 - 3
                Ok (Some n) -> n

-- right-join grouped summary: group every post by its author id (a right column,
-- always present) and count each group -> author 2 owns hello and again (2), author 3
-- owns world (1), so "2:2,3:1" ordered by the key. Proves `groupSummarizeRightJoin`
-- runs the GROUP BY over the right-outer join and decodes the integer key.
pub fn db rightJoinGroupAuthors () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.rightJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> p.author) |> Repo.summarize (fn g -> AuthorCount { author = g.key, n = g.count })
                Err _  -> "right-group-err"
                Ok cs  -> authorCounts cs

-- left join: keep every user, pairing each with its posts or with `None`, order
-- by user id, and render `name:title` (or `name:-`) per pair ->
-- "ada:-,lin:hello,lin:again,max:world". ada owns no posts, so where the inner
-- join dropped it the left join keeps it as `ada:-`. Proves the unified `toList`
-- over a left join keeps unmatched left rows and decodes the right entity as
-- `Option`.
pub fn db leftJoinedNames () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.toList
                Err _  -> "left-join-err"
                Ok ps  -> joinLeftPairs ps

-- left-join projection: the same left join, projected into
-- `ComboOpt { person, post }` where `post` is `Option Text`, rendered ->
-- "ada:-,lin:hello,lin:again,max:world". ada has no post, so its projected
-- `post` column is NULL and decodes to `None` (`ada:-`). Proves selectLeftJoin
-- keeps unmatched left rows and decodes the right columns into Option fields.
pub fn db leftSelectTitles () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.select (fn (u: User) (p: Option Post) -> ComboOpt { person = u.name, post = p.title })
                Err _  -> "left-select-err"
                Ok cs  -> joinComboOpts cs

-- join + filter on a RIGHT column: the same inner join, narrowed by a two-row
-- predicate over the post title -> only `lin:hello` survives. Proves the one
-- `Repo.filter` takes a two-row predicate on a `Join` (the arity follows the
-- receiver) and the post-join WHERE folds into the join the seam runs.
pub fn db joinFilterRight () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.filter (fn (u: User) (p: Post) -> p.title == "hello") |> Repo.toList
                Err _  -> "join-filter-err"
                Ok ps  -> joinPairs ps

-- left join + filter on a RIGHT column: a predicate over the post title drops
-- both the unmatched `ada` (its right side is NULL, so the predicate is false)
-- and the non-matching posts -> only `lin:hello`. Proves a `LeftJoin` filter
-- over a right column narrows the outer join to its matches — the three-valued
-- reading SQL gives a WHERE after a LEFT JOIN.
pub fn db leftJoinFilterRight () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.filter (fn (u: User) (p: Post) -> p.title == "hello") |> Repo.toList
                Err _  -> "left-filter-err"
                Ok ps  -> joinLeftPairs ps

-- left join + filter on a LEFT column: a predicate over the user id keeps every
-- left row it admits, including the unmatched `ada` (the predicate never reads
-- the NULL right side), and drops `max` (id 3) -> "ada:-,lin:hello,lin:again".
-- Proves a `LeftJoin` filter that touches only the left row preserves the
-- kept-but-unmatched rows.
pub fn db leftJoinFilterLeft () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.filter (fn (u: User) (p: Post) -> u.id <= 2) |> Repo.toList
                Err _  -> "left-filter-err"
                Ok ps  -> joinLeftPairs ps

-- join + limit: the inner join ordered by the post id (a right column, unique:
-- hello 10, world 11, again 12), keeping the first two pairs ->
-- "lin:hello,max:world". Proves the unified `limit` bounds a join through its own
-- page (carried on the `Join`), not the left query alone.
pub fn db joinLimited () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id) |> Repo.limit 2 |> Repo.toList
                Err _  -> "join-limit-err"
                Ok ps  -> joinPairs ps

-- join + offset + limit: the same ordered join, skipping the first pair and keeping
-- one -> "max:world" (after hello comes world). Proves `offset` and `limit` compose
-- on a join.
pub fn db joinOffsetLimited () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id) |> Repo.offset 1 |> Repo.limit 1 |> Repo.toList
                Err _  -> "join-page-err"
                Ok ps  -> joinPairs ps

-- join + distinct + toList: `distinct` over the whole join, ordered by the post id
-- -> "lin:hello,max:world,lin:again". The three pairs are already distinct, so the
-- result is unchanged: this proves `distinct` threads through the `join` seam (a
-- `SELECT DISTINCT l.*, r.*`) without dropping distinct rows.
pub fn db joinDistinctAll () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.distinct |> Repo.orderBy Asc (fn (u: User) (p: Post) -> p.id) |> Repo.toList
                Err _  -> "join-distinct-err"
                Ok ps  -> joinPairs ps

-- join + distinct + projection: project the join down to just the left person, so
-- lin's two posts collapse, then `distinct` -> "lin,max". Proves `distinct` over a
-- join's projection dedups the projected rows (a `SELECT DISTINCT person`), not the
-- underlying pairs.
pub fn db joinDistinctPersons () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.distinct |> Repo.orderBy Asc (fn (u: User) (p: Post) -> u.name) |> Repo.select (fn (u: User) (p: Post) -> Person { person = u.name })
                Err _  -> "join-distinct-select-err"
                Ok ps  -> personList ps

-- left join + limit: the left join with the user-id order lifted from the query
-- (ada 1, lin 2, lin 2, max 3), keeping the first two rows -> "ada:-,lin:hello".
-- Proves the unified `limit` bounds a left join, the kept-but-unmatched ada row
-- included in the page.
pub fn db leftJoinLimited () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.limit 2 |> Repo.toList
                Err _  -> "left-limit-err"
                Ok ps  -> joinLeftPairs ps

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

-- aggregate: sum every age (18 + 30 + 25) -> "73". Proves sumOf folds the column
-- over the whole table and rides the column's `SqlType` codec back to `Int`.
pub fn db sumAllAges () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.sumOf (fn (u: User) -> u.age)
                Err _ -> "sum-err"
                Ok o  -> optIntText o

-- aggregate: sum the adult ages (30 + 25) -> "55". Proves the accumulated filter
-- bounds the aggregate (ada's 18 is excluded).
pub fn db sumAdultAges () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 25) |> Repo.sumOf (fn (u: User) -> u.age)
                Err _ -> "sum-err"
                Ok o  -> optIntText o

-- aggregate: the average adult age ((30 + 25) / 2) -> "27.5". Proves avgOf is
-- fractional (an `Option Float`) even over an integer column.
pub fn db avgAdultAges () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 25) |> Repo.avgOf (fn (u: User) -> u.age)
                Err _ -> "avg-err"
                Ok o  -> optFloatText o

-- aggregate: the least age over the whole table -> "18". Proves minOf keeps the
-- column's own type.
pub fn db minAllAges () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.minOf (fn (u: User) -> u.age)
                Err _ -> "min-err"
                Ok o  -> optIntText o

-- aggregate: the greatest name lexicographically (ada < lin < max) -> "max".
-- Proves MIN/MAX fold a text column and keep its type.
pub fn db maxName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.maxOf (fn (u: User) -> u.name)
                Err _ -> "max-err"
                Ok o  -> optTextText o

-- aggregate over an empty match (no user older than 100) -> "none". Proves a SQL
-- aggregate of zero rows is NULL, decoded to `None` rather than zero.
pub fn db sumNobody () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.age > 100) |> Repo.sumOf (fn (u: User) -> u.age)
                Err _ -> "sum-err"
                Ok o  -> optIntText o

-- join aggregate over a RIGHT column: sum the post ids over the inner join (lin
-- owns hello(10) and again(12), max owns world(11)) -> 10+12+11 = "33". Proves the
-- one `Repo.sumOf` takes a two-row accessor on a `Join` and folds a right-table
-- column through the `aggregateJoin` seam.
pub fn db joinSumRightId () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> p.id)
                Err _ -> "join-sum-err"
                Ok o  -> optIntText o

-- join aggregate over a LEFT column: sum the user age over the inner join. lin
-- matches two posts so its 30 counts twice, max's 25 once, and ada (no posts) is
-- dropped by the inner join -> 30+30+25 = "85". Proves a left-column fold counts
-- once per matched pair and the inner join excludes the unmatched left row.
pub fn db joinSumLeftAge () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> u.age)
                Err _ -> "join-sum-err"
                Ok o  -> optIntText o

-- join aggregate over a RIGHT text column: the greatest post title over the inner
-- join (again < hello < world) -> "world". Proves maxOf folds a right text column
-- and keeps its type.
pub fn db joinMaxRightTitle () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.maxOf (fn (u: User) (p: Post) -> p.title)
                Err _ -> "join-max-err"
                Ok o  -> optTextText o

-- join aggregate average over a RIGHT column: the mean post id over the inner join
-- ((10+12+11)/3) -> "11.0". Proves avgOf over a join is fractional (Option Float).
pub fn db joinAvgRightId () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.avgOf (fn (u: User) (p: Post) -> p.id)
                Err _ -> "join-avg-err"
                Ok o  -> optFloatText o

-- left-join aggregate over a LEFT column: sum the user age over the LEFT join,
-- which keeps the unmatched ada. lin's 30 counts twice (two posts), max's 25 once,
-- and ada's 18 once (kept though it owns no post) -> 18+30+30+25 = "103". The
-- discriminator: the inner join's same sum is "85" (ada excluded), so "103" proves
-- the left-join aggregate counts the kept-but-unmatched left row.
pub fn db leftJoinSumLeftAge () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.sumOf (fn (u: User) (p: Post) -> u.age)
                Err _ -> "left-sum-err"
                Ok o  -> optIntText o

-- left-join aggregate over a RIGHT column: the greatest post title over the LEFT
-- join -> "world". ada's right side is absent (a NULL the fold skips), so only the
-- matched titles fold. Proves a left-join right-column aggregate ignores the
-- unmatched rows rather than faulting on the missing right value.
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

-- single: exactly one match (id 2) -> "lin". Proves single decodes the lone row.
pub fn db singleOne () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 2) |> Repo.single
                Err e -> e.code
                Ok o  -> optUserName o

-- single: no match (id 99) -> "none". Proves the empty result is `Ok None`, not an
-- error — the lenient half of the pair.
pub fn db singleNone () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 99) |> Repo.single
                Err e -> e.code
                Ok o  -> optUserName o

-- single: more than one match (the whole table) -> "repo.single.many". Proves a
-- non-unique result fails with that code and that the two-row limit catches it.
pub fn db singleMany () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.single
                Err e -> e.code
                Ok o  -> optUserName o

-- singleOrError: exactly one (id 1) -> "ada". Proves the strict reader answers the
-- bare entity, not an option.
pub fn db oneOrErr () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 1) |> Repo.singleOrError
                Err e -> e.code
                Ok u  -> u.name

-- singleOrError: no match (id 99) -> "repo.single.empty". Proves the empty result
-- is an error here, where `single` returns None.
pub fn db noneOrErr () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 99) |> Repo.singleOrError
                Err e -> e.code
                Ok u  -> u.name

-- every: are all users adults? (18, 30, 25 all >= 18) -> "true". Proves every is
-- the universal over the selected rows.
pub fn db everyAdult () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.every (fn (u: User) -> u.age >= 18)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- every: are all users at least 26? (ada 18 and max 25 fail) -> "false".
pub fn db everyHigh () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.every (fn (u: User) -> u.age >= 26)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- every over an empty selection (no user with id 99) -> "true", the vacuous reading
-- of a universal over no rows.
pub fn db everyEmpty () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (u: User) -> u.id == 99) |> Repo.every (fn (u: User) -> u.age >= 18)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- count over an inner join: how many user-post pairs join on `u.id == p.author`?
-- (lin:hello, lin:again, max:world) -> 3. Proves the unified `count` pushes a
-- `COUNT(*)` over the join down, ada (no posts) contributing none.
pub fn db joinCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- exists over an inner join: does any user-post pair join? -> "true". Proves the
-- unified `exists` probes the join with a one-row limit.
pub fn db joinAny () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.exists
                Err _ -> "exists-err"
                Ok b  -> boolText b

-- every over an inner join (left column): are all joined users adults? (lin 30,
-- lin 30, max 25 all >= 18) -> "true". Proves `every` folds a two-row predicate
-- into the join's count comparison.
pub fn db joinEveryAdult () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.every (fn (u: User) (p: Post) -> u.age >= 18)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- every over an inner join (right column): is every joined post titled "hello"?
-- (world and again fail) -> "false". Proves a two-row predicate over the right side
-- narrows the matching count below the total.
pub fn db joinEveryHello () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.every (fn (u: User) (p: Post) -> p.title == "hello")
                Err _ -> "every-err"
                Ok b  -> boolText b

-- count over a left join: how many left-outer rows? ada (no post, kept), lin
-- (hello), lin (again), max (world) -> 4. Proves `countLeftJoin` counts every left
-- row, the unmatched one included.
pub fn db leftJoinCount () -> Int =
    match setupJoin ()
        Err _ -> 0 - 1
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.count
                Ok n  -> n
                Err _ -> 0 - 2

-- exists over a left join: a left join keeps every left row, so it is non-empty
-- whenever any user exists -> "true". Proves the unified `exists` probes a left join
-- too.
pub fn db leftJoinAny () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.exists
                Err _ -> "exists-err"
                Ok b  -> boolText b

-- every over a left join (right column): does every kept row have a post of its
-- own? ada is kept with no post, so its right side is NULL and fails the predicate
-- -> "false". Proves a right-column `every` drops the unmatched rows, as SQL's
-- three-valued reading gives.
pub fn db leftJoinEveryAuthored () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.every (fn (u: User) (p: Post) -> p.author == u.id)
                Err _ -> "every-err"
                Ok b  -> boolText b

-- A grouping dataset: employees with a repeated `dept` key and a salary, so a
-- GROUP BY partitions several rows per group (eng has 2, sales 3, ops 1).
pub type Emp = { id: Int, dept: Text, salary: Int } deriving (Row)

-- The summarised shapes a `groupBy` projects into. Each names the group key
-- alongside the aggregates a probe reads (count, sum, average, or min/max range).
pub type DeptCount = { dept: Text, n: Int } deriving (Row)
pub type DeptSum   = { dept: Text, total: Int } deriving (Row)
pub type DeptAvg   = { dept: Text, mean: Float } deriving (Row)
pub type DeptRange = { dept: Text, lo: Int, hi: Int } deriving (Row)

-- Single-column shapes the distinct projections decode into: a list of dept names
-- and a list of salaries, each deduplicated by `distinct`.
pub type DeptName = { dept: Text } deriving (Row)
pub type SalAmt   = { salary: Int } deriving (Row)

pub fn empRow (eid: Int) (edept: Text) (esalary: Int) -> Map Text SqlValue =
    Map.fromList [("id", toSql eid), ("dept", toSql edept), ("salary", toSql esalary)]

-- Seed six employees across three departments so each grouped aggregate folds a
-- different group size: eng {100, 200}, sales {150, 150, 300}, ops {50}.
pub fn db setupEmps () -> Result (Repo Emp MemAdapter) Error =
    let r = Repo.repo (memAdapter ()) "emps"
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

-- Render the grouped result rows as `key:value` cells joined by commas. Each
-- backend returns the groups ordered by the key, so the rendered string is
-- deterministic without sorting here.
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
-- the rendered string is deterministic across backends.
fn idList (rows: List Emp) -> Text =
    match rows
        []        -> ""
        e :: []   -> Int.toText e.id
        e :: rest -> Text.concat (Int.toText e.id) (Text.concat "," (idList rest))

-- group + summarize: COUNT(*) per dept, key-ordered -> "eng:2,ops:1,sales:3".
-- Proves GROUP BY partitions the rows and the result is ordered by the key.
pub fn db groupCounts () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group + summarize: SUM(salary) per dept -> "eng:300,ops:50,sales:600".
pub fn db groupSums () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (e: Emp) -> e.salary) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- group + summarize: AVG(salary) per dept -> "eng:150.0,ops:50.0,sales:200.0".
-- Proves the per-group average is fractional even over an integer column.
pub fn db groupAvgs () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptAvg { dept = g.key, mean = g.avg (fn (e: Emp) -> e.salary) })
                Err _   -> "group-err"
                Ok rows -> avgCells rows

-- group + summarize: MIN/MAX(salary) per dept -> "eng:100-200,ops:50-50,sales:150-300".
-- Proves two aggregates over one column compose in a single projection.
pub fn db groupRanges () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.summarize (fn g -> DeptRange { dept = g.key, lo = g.min (fn (e: Emp) -> e.salary), hi = g.max (fn (e: Emp) -> e.salary) })
                Err _   -> "group-err"
                Ok rows -> rangeCells rows

-- group + having on the count: only depts with more than one member -> "eng:2,sales:3".
-- Proves HAVING filters groups by an aggregate (ops, a single member, drops out).
pub fn db groupHavingCount () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.having (fn g -> g.count > 1) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group + having on a summed aggregate: only depts whose payroll is >= 600 ->
-- "sales:600". Proves HAVING can threshold a different aggregate than COUNT.
pub fn db groupHavingSum () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.having (fn g -> g.sum (fn (e: Emp) -> e.salary) >= 600) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (e: Emp) -> e.salary) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- filter + group + having: the row filter runs first (salary >= 100 drops ops's
-- lone 50), then the surviving rows group and keep depts with more than one member
-- -> "eng:2,sales:3". Proves the query's WHERE bounds the grouping.
pub fn db groupFilteredHaving () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 100) |> Repo.groupBy (fn (e: Emp) -> e.dept) |> Repo.having (fn g -> g.count > 1) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group a join by the left key (user name), counting the joined pairs ->
-- "lin:2,max:1" (lin authored two posts, max one; ada joins nothing). Proves
-- GROUP BY over a join partitions the pairs and orders by the key.
pub fn db joinGroupCounts () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group a join by the left key, summing a RIGHT column (post id) -> "lin:22,max:11"
-- (lin folds 10+12, max 11). Proves a grouped aggregate folds the right table.
pub fn db joinGroupRightIds () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (u: User) (p: Post) -> p.id) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- group a join by the left key, summing a LEFT column (user age) -> "lin:60,max:25"
-- (lin appears in two pairs, each age 30; max once at 25). Proves a left-column fold
-- counts each joined pair.
pub fn db joinGroupLeftAges () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (u: User) -> u.age) })
                Err _   -> "group-err"
                Ok rows -> sumCells rows

-- group a join by the left key with HAVING count > 1 -> "lin:2" (max, a single pair,
-- drops out). Proves HAVING filters join groups.
pub fn db joinGroupHaving () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.having (fn g -> g.count > 1) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group a join by a RIGHT key (post title), counting -> "again:1,hello:1,world:1"
-- (each title once, key-ordered). Proves the group key qualifies to the right table.
pub fn db joinGroupByTitle () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> p.title) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- group a LEFT join by the left key, counting -> "ada:1,lin:2,max:1" (ada, matching
-- no post, still forms a one-row group). Proves a left join keeps every left row in
-- the grouping.
pub fn db leftJoinGroupCounts () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.leftJoinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.groupBy (fn (u: User) (p: Post) -> u.name) |> Repo.summarize (fn g -> DeptCount { dept = g.key, n = g.count })
                Err _   -> "group-err"
                Ok rows -> countCells rows

-- selectList without distinct: every dept, ordered by dept -> all six rows
-- "eng,eng,ops,sales,sales,sales". The baseline the distinct probe contrasts with.
pub fn db deptsAll () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (e: Emp) -> e.dept) |> Repo.select (fn (e: Emp) -> DeptName { dept = e.dept })
                Err _   -> "err"
                Ok rows -> deptList rows

-- distinct + selectList: the distinct dept values, ordered -> "eng,ops,sales".
-- Proves DISTINCT collapses the repeated dept column (six rows -> three).
pub fn db deptsDistinct () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.distinct |> Repo.orderBy Asc (fn (e: Emp) -> e.dept) |> Repo.select (fn (e: Emp) -> DeptName { dept = e.dept })
                Err _   -> "err"
                Ok rows -> deptList rows

-- distinct over a numeric column, ordered ascending -> "50,100,150,200,300".
-- The two sales rows at 150 collapse to one (six salaries -> five distinct).
pub fn db salariesDistinct () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.distinct |> Repo.orderBy Asc (fn (e: Emp) -> e.salary) |> Repo.select (fn (e: Emp) -> SalAmt { salary = e.salary })
                Err _   -> "err"
                Ok rows -> salList rows

-- Seed a fresh store with three identical rows and one different one, so a
-- whole-row distinct has exact duplicates to collapse.
pub fn db setupDups () -> Result (Repo Emp MemAdapter) Error =
    let r = Repo.repo (memAdapter ()) "dups"
    match Repo.insertRow (empRow 1 "x" 10) r
        Err e -> Err e
        Ok _  ->
            match Repo.insertRow (empRow 1 "x" 10) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insertRow (empRow 1 "x" 10) r
                        Err e -> Err e
                        Ok _  ->
                            match Repo.insertRow (empRow 2 "y" 20) r
                                Err e -> Err e
                                Ok _  -> Ok r

-- whole-row distinct: `distinct` over the whole row collapses the three identical
-- rows, so the count is 2. Proves a `SELECT DISTINCT *` dedups exact-duplicate rows.
pub fn db distinctRows () -> Int =
    match setupDups ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.query |> Repo.distinct |> Repo.toList
                Err _   -> 0 - 1
                Ok rows -> listLen rows

-- Set operations over two overlapping filters. A = salary >= 150 (ids 2,3,4,5),
-- B = salary <= 150 (ids 1,3,4,6); ids 3 and 4 (salary 150) are in both. Each
-- probe orders the combined result by id so the rendered ids are deterministic.

-- union: every row in either, duplicates removed, ordered -> "1,2,3,4,5,6".
-- Proves UNION dedups the shared rows and that orderBy composes on the result.
pub fn db unionIds () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.union b |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows

-- intersect: the rows in both, ordered -> "3,4" (the two salary-150 rows).
pub fn db intersectIds () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.intersect b |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows

-- except: the rows in A but not B, ordered -> "2,5" (salary 200 and 300). Order
-- matters: the piped-in query is the left side.
pub fn db exceptIds () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.except b |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows

-- unionAll: every row in either, keeping duplicates -> 8 rows (4 + 4, with 3 and
-- 4 counted twice). Proves UNION ALL keeps the shared rows.
pub fn db unionAllCount () -> Int =
    match setupEmps ()
        Err _ -> 0 - 1
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.unionAll b |> Repo.toList
                Err _   -> 0 - 1
                Ok rows -> listLen rows

-- filter after a union: the combined result is filtered again (salary >= 200) ->
-- "2,5". Proves an outer filter applies on top of the combination.
pub fn db unionFiltered () -> Text =
    match setupEmps ()
        Err _ -> "setup-err"
        Ok r  ->
            let a = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary >= 150)
            let b = r |> Repo.query |> Repo.filter (fn (e: Emp) -> e.salary <= 150)
            match a |> Repo.union b |> Repo.filter (fn (e: Emp) -> e.salary >= 200) |> Repo.orderBy Asc (fn (e: Emp) -> e.id) |> Repo.toList
                Err _   -> "err"
                Ok rows -> idList rows

-- nested unions: (eng ∪ sales) ∪ ops, ordered -> "1,2,3,4,5,6". Proves a combined
-- query is itself composable — unioning it again nests the plans.
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

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-repo-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn repo_surface_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping repo_surface_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-repo-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-repo-e2e-cache-")
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
        "io:format(\"countAll=~w~n\",[{module}:countAll()]), \
         io:format(\"adultsCount=~w~n\",[{module}:adultsCount()]), \
         io:format(\"firstName=~s~n\",[{module}:firstName()]), \
         io:format(\"getName=~s~n\",[{module}:getName()]), \
         io:format(\"existsYoung=~w~n\",[{module}:existsYoung()]), \
         io:format(\"deleteCount=~w~n\",[{module}:deleteCount()]), \
         io:format(\"afterCount=~w~n\",[{module}:afterCount()]), \
         io:format(\"orderedNames=~s~n\",[{module}:orderedNames()]), \
         io:format(\"pagedName=~s~n\",[{module}:pagedName()]), \
         io:format(\"firstAdultName=~s~n\",[{module}:firstAdultName()]), \
         io:format(\"summaryNames=~s~n\",[{module}:summaryNames()]), \
         io:format(\"topYears=~w~n\",[{module}:topYears()]), \
         io:format(\"joinedNames=~s~n\",[{module}:joinedNames()]), \
         io:format(\"joinedTitles=~s~n\",[{module}:joinedTitles()]), \
         io:format(\"joinOrderByRight=~s~n\",[{module}:joinOrderByRight()]), \
         io:format(\"crossJoined=~s~n\",[{module}:crossJoined()]), \
         io:format(\"crossCount=~w~n\",[{module}:crossCount()]), \
         io:format(\"rightJoinedNames=~s~n\",[{module}:rightJoinedNames()]), \
         io:format(\"rightSelectNames=~s~n\",[{module}:rightSelectNames()]), \
         io:format(\"rightJoinCount=~w~n\",[{module}:rightJoinCount()]), \
         io:format(\"rightJoinSumLeftId=~w~n\",[{module}:rightJoinSumLeftId()]), \
         io:format(\"rightJoinGroupAuthors=~s~n\",[{module}:rightJoinGroupAuthors()]), \
         io:format(\"leftJoinedNames=~s~n\",[{module}:leftJoinedNames()]), \
         io:format(\"leftSelectTitles=~s~n\",[{module}:leftSelectTitles()]), \
         io:format(\"joinFilterRight=~s~n\",[{module}:joinFilterRight()]), \
         io:format(\"leftJoinFilterRight=~s~n\",[{module}:leftJoinFilterRight()]), \
         io:format(\"leftJoinFilterLeft=~s~n\",[{module}:leftJoinFilterLeft()]), \
         io:format(\"joinLimited=~s~n\",[{module}:joinLimited()]), \
         io:format(\"joinOffsetLimited=~s~n\",[{module}:joinOffsetLimited()]), \
         io:format(\"joinDistinctAll=~s~n\",[{module}:joinDistinctAll()]), \
         io:format(\"joinDistinctPersons=~s~n\",[{module}:joinDistinctPersons()]), \
         io:format(\"leftJoinLimited=~s~n\",[{module}:leftJoinLimited()]), \
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
         io:format(\"leftJoinAny=~s~n\",[{module}:leftJoinAny()]), \
         io:format(\"leftJoinEveryAuthored=~s~n\",[{module}:leftJoinEveryAuthored()]), \
         io:format(\"groupCounts=~s~n\",[{module}:groupCounts()]), \
         io:format(\"groupSums=~s~n\",[{module}:groupSums()]), \
         io:format(\"groupAvgs=~s~n\",[{module}:groupAvgs()]), \
         io:format(\"groupRanges=~s~n\",[{module}:groupRanges()]), \
         io:format(\"groupHavingCount=~s~n\",[{module}:groupHavingCount()]), \
         io:format(\"groupHavingSum=~s~n\",[{module}:groupHavingSum()]), \
         io:format(\"groupFilteredHaving=~s~n\",[{module}:groupFilteredHaving()]), \
         io:format(\"joinGroupCounts=~s~n\",[{module}:joinGroupCounts()]), \
         io:format(\"joinGroupRightIds=~s~n\",[{module}:joinGroupRightIds()]), \
         io:format(\"joinGroupLeftAges=~s~n\",[{module}:joinGroupLeftAges()]), \
         io:format(\"joinGroupHaving=~s~n\",[{module}:joinGroupHaving()]), \
         io:format(\"joinGroupByTitle=~s~n\",[{module}:joinGroupByTitle()]), \
         io:format(\"leftJoinGroupCounts=~s~n\",[{module}:leftJoinGroupCounts()]), \
         io:format(\"deptsAll=~s~n\",[{module}:deptsAll()]), \
         io:format(\"deptsDistinct=~s~n\",[{module}:deptsDistinct()]), \
         io:format(\"salariesDistinct=~s~n\",[{module}:salariesDistinct()]), \
         io:format(\"distinctRows=~w~n\",[{module}:distinctRows()]), \
         io:format(\"unionIds=~s~n\",[{module}:unionIds()]), \
         io:format(\"intersectIds=~s~n\",[{module}:intersectIds()]), \
         io:format(\"exceptIds=~s~n\",[{module}:exceptIds()]), \
         io:format(\"unionAllCount=~w~n\",[{module}:unionAllCount()]), \
         io:format(\"unionFiltered=~s~n\",[{module}:unionFiltered()]), \
         io:format(\"nestedUnionIds=~s~n\",[{module}:nestedUnionIds()]), \
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
        ("countAll=3", "count answers the whole table"),
        ("adultsCount=2", "findBy keeps the two rows with age >= 25"),
        (
            "firstName=lin",
            "find + deriving (Row) decodes the first row older than 28",
        ),
        ("getName=lin", "get by id 2 decodes to lin"),
        ("existsYoung=1", "exists finds the one row under 20"),
        ("deleteCount=1", "delete removes the one row under 25"),
        ("afterCount=2", "two rows remain after the delete"),
        (
            "orderedNames=lin,max,ada",
            "the builder orders the whole table by age descending",
        ),
        (
            "pagedName=max",
            "offset 1 + limit 1 over the age-ascending order yields the second row",
        ),
        (
            "firstAdultName=lin",
            "filter + orderBy + first yields the oldest adult",
        ),
        (
            "summaryNames=lin,max,ada",
            "selectList projects the renamed columns and decodes them in age order",
        ),
        (
            "topYears=30",
            "selectFirst decodes the aliased `years` column of the oldest row",
        ),
        (
            "joinedNames=lin:hello,lin:again,max:world",
            "toList inner-joins users to posts and decodes both entities in id order",
        ),
        (
            "joinedTitles=lin:hello,lin:again,max:world",
            "selectJoin projects columns from both entities into the named Combo shape",
        ),
        (
            "joinOrderByRight=lin:again,lin:hello,max:world",
            "the unified orderBy sorts the join by a right-table column (post title), so the pairs come back title-ordered",
        ),
        (
            "crossJoined=lin:hello,lin:world,lin:again",
            "a cross join pairs lin with every post, including world (author 3) that lin does not own, so the cartesian product spans all three posts",
        ),
        (
            "crossCount=9",
            "count over the full cross join is 3 users * 3 posts = 9 pairs",
        ),
        (
            "rightJoinedNames=lin:hello,-:world,lin:again",
            "toList over a right join keeps every post and folds the left filter into the match, so world (authored by the filtered-out max) keeps a None left side as `-:world`",
        ),
        (
            "rightSelectNames=lin:hello,-:world,lin:again",
            "rightJoinSelect keeps the unmatched world row and decodes its NULL left column into an Option field as None",
        ),
        (
            "rightJoinCount=3",
            "countRightJoin keeps all three posts (two matched, world unmatched) where an inner join would count only two",
        ),
        (
            "rightJoinSumLeftId=4",
            "aggregateRightJoin folds the left id only over the matched rows (lin twice = 4), skipping the unmatched world",
        ),
        (
            "rightJoinGroupAuthors=2:2,3:1",
            "groupSummarizeRightJoin groups every post by author id: author 2 owns two posts, author 3 one",
        ),
        (
            "leftJoinedNames=ada:-,lin:hello,lin:again,max:world",
            "toList over a left join keeps the unmatched ada row as `ada:-` and decodes the right entity as Option",
        ),
        (
            "leftSelectTitles=ada:-,lin:hello,lin:again,max:world",
            "selectLeftJoin keeps the unmatched ada row and decodes its NULL right column into an Option field as None",
        ),
        (
            "joinFilterRight=lin:hello",
            "the unified filter narrows an inner join by a two-row predicate over a right column",
        ),
        (
            "leftJoinFilterRight=lin:hello",
            "a left-join filter over a right column drops the unmatched and non-matching rows (NULL right reads false)",
        ),
        (
            "leftJoinFilterLeft=ada:-,lin:hello,lin:again",
            "a left-join filter over a left column keeps the unmatched ada row and drops max",
        ),
        (
            "joinLimited=lin:hello,max:world",
            "limit bounds the join's own page, keeping the first two post-id-ordered pairs",
        ),
        (
            "joinOffsetLimited=max:world",
            "offset and limit compose on a join (skip hello, keep world)",
        ),
        (
            "joinDistinctAll=lin:hello,max:world,lin:again",
            "distinct threads through the join seam and keeps the three already-distinct pairs",
        ),
        (
            "joinDistinctPersons=lin,max",
            "distinct over a join's projection dedups the repeated person (lin's two posts collapse)",
        ),
        (
            "leftJoinLimited=ada:-,lin:hello",
            "limit bounds a left join, the kept-but-unmatched ada row included in the page",
        ),
        (
            "joinCount=3",
            "count pushes a COUNT(*) over the inner join (three user-post pairs)",
        ),
        ("joinAny=true", "exists probes the inner join for any pair"),
        (
            "joinEveryAdult=true",
            "every folds a two-row left-column predicate into the join's count comparison",
        ),
        (
            "joinEveryHello=false",
            "a right-column every narrows the matching count below the join total",
        ),
        (
            "leftJoinCount=4",
            "countLeftJoin counts every left-outer row, the unmatched ada included",
        ),
        ("leftJoinAny=true", "exists probes a left join, always non-empty here"),
        (
            "leftJoinEveryAuthored=false",
            "a right-column every over a left join fails the unmatched ada row (its post is NULL)",
        ),
        ("sumAllAges=73", "sumOf folds every age (18 + 30 + 25)"),
        (
            "sumAdultAges=55",
            "the filter bounds sumOf to the adult ages (30 + 25)",
        ),
        (
            "avgAdultAges=27.5",
            "avgOf is fractional even over an integer column ((30 + 25) / 2)",
        ),
        ("minAllAges=18", "minOf keeps the column type and finds the least age"),
        (
            "maxName=max",
            "maxOf folds a text column and keeps its type (ada < lin < max)",
        ),
        (
            "sumNobody=none",
            "an aggregate over an empty match is NULL, decoded to None",
        ),
        (
            "joinSumRightId=33",
            "sumOf folds a right-table column over an inner join (10+12+11)",
        ),
        (
            "joinSumLeftAge=85",
            "a left-column join fold counts once per matched pair (30+30+25)",
        ),
        (
            "joinMaxRightTitle=world",
            "maxOf folds a right text column over a join (again < hello < world)",
        ),
        (
            "joinAvgRightId=11.0",
            "avgOf over a join is fractional Option Float ((10+12+11)/3)",
        ),
        (
            "leftJoinSumLeftAge=103",
            "a left join's left-column fold counts the unmatched ada (18+30+30+25), unlike the inner join's 85",
        ),
        (
            "leftJoinMaxRightTitle=world",
            "a left join's right-column fold skips the unmatched ada's NULL",
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
            "summarize counts the rows of each dept group, ordered by the key",
        ),
        (
            "groupSums=eng:300,ops:50,sales:600",
            "summarize sums the salary column within each dept group",
        ),
        (
            "groupAvgs=eng:150.0,ops:50.0,sales:200.0",
            "the per-group average is fractional even over an integer column",
        ),
        (
            "groupRanges=eng:100-200,ops:50-50,sales:150-300",
            "min and max over one column compose in a single grouped projection",
        ),
        (
            "groupHavingCount=eng:2,sales:3",
            "having drops the single-member ops group (count > 1)",
        ),
        (
            "groupHavingSum=sales:600",
            "having thresholds a summed aggregate, keeping only the >= 600 payroll",
        ),
        (
            "groupFilteredHaving=eng:2,sales:3",
            "the query filter bounds the grouping before having runs",
        ),
        (
            "joinGroupCounts=lin:2,max:1",
            "group a join by the left key: lin joins two posts, max one, ada none",
        ),
        (
            "joinGroupRightIds=lin:22,max:11",
            "a grouped aggregate folds a right column: lin sums post ids 10+12, max 11",
        ),
        (
            "joinGroupLeftAges=lin:60,max:25",
            "a grouped aggregate folds a left column once per joined pair (lin twice at 30)",
        ),
        (
            "joinGroupHaving=lin:2",
            "having filters join groups: max's single pair drops on count > 1",
        ),
        (
            "joinGroupByTitle=again:1,hello:1,world:1",
            "group a join by a right key: each post title forms its own group",
        ),
        (
            "leftJoinGroupCounts=ada:1,lin:2,max:1",
            "a left join keeps ada as a one-row group though it matches no post",
        ),
        (
            "deptsAll=eng,eng,ops,sales,sales,sales",
            "selectList without distinct returns the dept column for all six rows",
        ),
        (
            "deptsDistinct=eng,ops,sales",
            "distinct collapses the repeated dept column to its three distinct values",
        ),
        (
            "salariesDistinct=50,100,150,200,300",
            "distinct over the salary column drops the duplicate 150 (six rows -> five)",
        ),
        (
            "distinctRows=2",
            "distinct over whole rows collapses three identical rows, leaving two",
        ),
        (
            "unionIds=1,2,3,4,5,6",
            "union dedups the rows the two filters share and orderBy composes on the result",
        ),
        (
            "intersectIds=3,4",
            "intersect keeps the rows present in both filters (the salary-150 rows)",
        ),
        (
            "exceptIds=2,5",
            "except keeps the left rows not in the right (salary 200 and 300)",
        ),
        (
            "unionAllCount=8",
            "unionAll keeps the duplicate rows the two branches share (4 + 4)",
        ),
        (
            "unionFiltered=2,5",
            "an outer filter applies on top of the union result",
        ),
        (
            "nestedUnionIds=1,2,3,4,5,6",
            "a combined query unions again, nesting the plans",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "expected `{probe}` ({want})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
