//! End-to-end check for the std.repo typed write path — running on the BEAM.
//!
//! Where `data_repo_e2e` builds each row by hand (`Map.fromList [("col", toSql …)]`)
//! and feeds it to `insertRow`, this exercises the typed verbs that derive the row
//! from the entity:
//! - `insert` encodes a `User` through `deriving (Row)`'s `toRow` and appends it,
//!   so a record goes in the way one comes out — no hand-built column map.
//! - `update` overwrites every column of the rows matching a predicate with a typed
//!   entity (again through `toRow`).
//! - `updateWhere` sets only the columns named in a partial map, leaving the rest.
//!
//! The `User` entity carries a nullable `nick: Option Text`, so the write path's
//! NULL encoding is covered too: `None` is written as SQL NULL by `toRow` and reads
//! back as `None`, while `Some s` round-trips to `s`.
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

-- An entity with a nullable column, so the typed write path's NULL encoding
-- (`None` -> SQL NULL via `toRow`) is exercised alongside the base types.
pub type User = { id: Int, age: Int, name: Text, nick: Option Text } deriving (Row)

-- Comma-join the names of a user list, so a query's order is observable as one
-- string the probe can assert on.
fn joinNames (us: List User) -> Text =
    match us
        []        -> ""
        u :: []   -> u.name
        u :: rest -> Text.concat u.name (Text.concat "," (joinNames rest))

-- A present nick renders as its text; a NULL column (`None`) renders as "-".
fn nickOf (o: Option Text) -> Text =
    match o
        None   -> "-"
        Some s -> s

-- Open a fresh store, bind a repository, and seed three users with the TYPED
-- `insert` — no by-hand row map. lin's nick is `None` (a NULL column), the
-- others carry a value. Each probe seeds its own isolated store.
pub fn db setup () -> Result (Repo User MemAdapter) Error =
    let r: Repo User MemAdapter = Repo.repo (memAdapter ()) "users"
    match Repo.insert (User { id = 1, age = 18, name = "ada", nick = Some "ace" }) r
        Err e -> Err e
        Ok _  ->
            match Repo.insert (User { id = 2, age = 30, name = "lin", nick = None }) r
                Err e -> Err e
                Ok _  ->
                    match Repo.insert (User { id = 3, age = 25, name = "max", nick = Some "mad" }) r
                        Err e -> Err e
                        Ok _  -> Ok r

-- insert round-trips: every name, ascending by id -> "ada,lin,max". Proves
-- `toRow` encodes a typed entity and `fromRow` reads it back.
pub fn db addedNames () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.query |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.toList
                Err _ -> "list-err"
                Ok us -> joinNames us

-- present Option column: ada's nick is `Some "ace"` -> "ace".
pub fn db adaNick () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 1)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some u) -> nickOf u.nick

-- nullable round-trip: lin's nick is `None`, written as SQL NULL by `toRow` and
-- read back as `None` -> "-".
pub fn db linNick () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok r  ->
            match r |> Repo.getBy "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some u) -> nickOf u.nick

-- typed update: overwrite ada (id 1) with a new full entity (age 99), then read
-- her age back -> 99. Proves `update` encodes the whole entity through `toRow`.
pub fn db updatedAge () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.update (User { id = 1, age = 99, name = "ada", nick = None }) (fn (u: User) -> u.id == 1)
                Err _ -> 0 - 2
                Ok _  ->
                    match r |> Repo.getBy "id" (toSql 1)
                        Err _       -> 0 - 3
                        Ok None     -> 0 - 4
                        Ok (Some u) -> u.age

-- typed update changed-count: only ada is under 25, so one row changes -> 1.
pub fn db updateCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok r  ->
            match r |> Repo.update (User { id = 1, age = 20, name = "ada", nick = None }) (fn (u: User) -> u.age < 25)
                Ok n  -> n
                Err _ -> 0 - 2

-- partial update: set `age = 40` on every adult (age >= 25), leaving name and
-- nick untouched, then read lin's (id 2) age back -> 40. Proves `updateWhere`
-- writes only the named columns.
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

-- the column the partial update did NOT touch: lin's name is still "lin" after
-- the `age`-only `updateWhere`. Proves a partial map leaves other columns alone.
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
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-write-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn write_path_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping write_path_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-write-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-write-e2e-cache-")
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
        "io:format(\"addedNames=~s~n\",[{module}:addedNames()]), \
         io:format(\"adaNick=~s~n\",[{module}:adaNick()]), \
         io:format(\"linNick=~s~n\",[{module}:linNick()]), \
         io:format(\"updatedAge=~w~n\",[{module}:updatedAge()]), \
         io:format(\"updateCount=~w~n\",[{module}:updateCount()]), \
         io:format(\"bumpedAge=~w~n\",[{module}:bumpedAge()]), \
         io:format(\"bumpedName=~s~n\",[{module}:bumpedName()]), \
         io:format(\"updateWhereCount=~w~n\",[{module}:updateWhereCount()]), \
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

    for (probe, why) in [
        (
            "addedNames=ada,lin,max",
            "insert encodes each entity through toRow and the rows read back in id order",
        ),
        ("adaNick=ace", "a present Option column round-trips through the write path"),
        (
            "linNick=-",
            "None is written as SQL NULL by toRow and decodes back to None",
        ),
        (
            "updatedAge=99",
            "update overwrites the whole entity, so ada's age becomes 99",
        ),
        ("updateCount=1", "only the one row under 25 is updated"),
        (
            "bumpedAge=40",
            "updateWhere sets the age column on lin (an adult)",
        ),
        (
            "bumpedName=lin",
            "updateWhere leaves the untouched name column alone",
        ),
        ("updateWhereCount=2", "two adults match the partial update"),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
