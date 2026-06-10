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

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

-- A projected shape: the projection renames `name` -> `who` and `age` -> `years`,
-- so the decode proves the alias (`column AS alias`) and re-keying both work.
pub type Summary = { who: Text, years: Int } deriving (Row)

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

pub fn userRow (uid: Int) (uage: Int) (uname: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql uid), ("age", toSql uage), ("name", toSql uname)]

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

-- exists: is any user younger than 20? (ada 18) -> 1
pub fn db existsYoung () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.exists (fn (u: User) -> u.age < 20)
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
                    match Repo.count r
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
            match r |> Repo.query |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.selectList (fn (u: User) -> Summary { who = u.name, years = u.age })
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
    ] {
        assert!(
            stdout.contains(probe),
            "expected `{probe}` ({want})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
