//! End-to-end check for the std.data Postgres adapter against a real database.
//!
//! A `Repo User Postgres` pairs a live Postgres connection with the
//! `ridge_pg_users` table and the `User` entity, then runs the same repository
//! surface the in-memory adapter does — clearing the table, seeding three users,
//! and reading them back decoded through `deriving (Row)`. It proves the wire
//! client connects, authenticates, runs parameterised insert/select/delete, and
//! decodes `RowDescription`/`DataRow` into the entity. The query builder is
//! covered too — `orderBy` (ORDER BY) and `limit`/`offset` (LIMIT/OFFSET) compile
//! into real SQL — and a final probe drives the connection pool with six
//! concurrent reads on one handle (concurrent checkout, growth, waiter reuse).
//!
//! Gated three ways: the `beam-runtime` feature, a `which` guard for `erl`/`erlc`,
//! and the `RIDGE_TEST_PG_URL` environment variable. Without a reachable database
//! the test skips rather than fails, so the default `cargo test` run is
//! unaffected. The URL is the usual libpq shape:
//!
//!   <postgres://user:password@host:5432/dbname?sslmode=require>
//!
//! `sslmode` is optional and defaults to `disable`. The target database must hold
//! a table `ridge_pg_users (id integer, name text, age integer)`; CI provisions
//! it on the Postgres service, and a local run expects it to exist.

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
            match r |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.selectList (fn (u: User) -> Summary { who = u.name, years = u.age })
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
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.toPairs
                Err _  -> "join-err"
                Ok ps  -> joinPairs ps

-- join projection: the same join projected into `Combo { person, post }`
-- -> "lin:hello,max:world". Proves the backend compiles a qualified, aliased
-- select-list (`l.name AS person, r.title AS post`) and decodes it.
pub fn db joinedTitles () -> Text =
    match setupJoin ()
        Err _ -> "setup-err"
        Ok (users, posts) ->
            match users |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.author) |> Repo.selectJoin (fn (u: User) (p: Post) -> Combo { person = u.name, post = p.title })
                Err _  -> "select-err"
                Ok cs  -> joinCombos cs
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
