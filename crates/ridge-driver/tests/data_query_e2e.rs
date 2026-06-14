//! End-to-end check for the std.data query surface — `select`/`get`/`delete`
//! running on the BEAM, with predicates captured inline.
//!
//! A predicate written `fn (u: User) -> u.age >= 25` is captured as a `QExpr`
//! tree (the annotation pins the row type), threaded through the `Adapter`
//! `select`/`delete` methods, and walked against each stored row by the runtime
//! interpreter. `get` looks a row up by an exact column match. The program seeds
//! three users and exercises:
//! - `select` filtering (a `>=` predicate keeps two of three rows),
//! - `select` + `deriving (Row)` decode (a `>` predicate's first row is "lin"),
//! - `get` by key (id 2 decodes to "lin"; a missing id is `None`),
//! - `delete` by predicate (a `<` predicate removes one row and reports the
//!   count; the table then holds two).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (memAdapter, appendRow, selectRows, get, delete)
import std.sql (toSql, fromRow, SqlValue)
import std.map as Map

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub fn userRow (uid: Int) (uage: Int) (uname: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql uid), ("age", toSql uage), ("name", toSql uname)]

-- Open a fresh store and seed three users; return the handle so each probe
-- queries its own isolated data.
pub fn db setup () -> Result MemAdapter Error =
    let conn = memAdapter ()
    match appendRow conn "users" (userRow 1 18 "ada")
        Err e -> Err e
        Ok _  ->
            match appendRow conn "users" (userRow 2 30 "lin")
                Err e -> Err e
                Ok _  ->
                    match appendRow conn "users" (userRow 3 25 "max")
                        Err e -> Err e
                        Ok _  -> Ok conn

fn lengthOf (rows: List (Map Text SqlValue)) -> Int =
    match rows
        []        -> 0
        _ :: rest -> 1 + lengthOf rest

pub fn decodeUser (r: Map Text SqlValue) -> Result User Error =
    fromRow r

-- select: how many users are 25 or older? (lin 30, max 25) -> 2
pub fn db selectCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> u.age >= 25)
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- select + decode: the name of the first user older than 28 -> "lin"
pub fn db selectName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> u.age > 28)
                Err _   -> "select-err"
                Ok rows ->
                    match rows
                        []     -> "empty"
                        r :: _ ->
                            match decodeUser r
                                Ok u  -> u.name
                                Err _ -> "decode-err"

-- get by key: the user with id 2 -> "lin"
pub fn db getName () -> Text =
    match setup ()
        Err _ -> "setup-err"
        Ok conn ->
            match get conn "users" "id" (toSql 2)
                Err _       -> "get-err"
                Ok None     -> "none"
                Ok (Some r) ->
                    match decodeUser r
                        Ok u  -> u.name
                        Err _ -> "decode-err"

-- get a missing key -> None -> 1
pub fn db getMissing () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match get conn "users" "id" (toSql 99)
                Err _       -> 0 - 2
                Ok None     -> 1
                Ok (Some _) -> 0

-- delete: how many users are under 25? (ada 18) -> 1
pub fn db deleteCount () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match delete conn "users" (fn (u: User) -> u.age < 25)
                Ok n  -> n
                Err _ -> 0 - 2

-- delete then count what remains -> 2
pub fn db afterDelete () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match delete conn "users" (fn (u: User) -> u.age < 25)
                Err _ -> 0 - 2
                Ok _  ->
                    match selectRows conn "users" (fn (u: User) -> u.age >= 0)
                        Ok rows -> lengthOf rows
                        Err _   -> 0 - 3
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-query-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn query_surface_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping query_surface_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-query-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-query-e2e-cache-")
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
        "io:format(\"selectCount=~w~n\",[{module}:selectCount()]), \
         io:format(\"selectName=~s~n\",[{module}:selectName()]), \
         io:format(\"getName=~s~n\",[{module}:getName()]), \
         io:format(\"getMissing=~w~n\",[{module}:getMissing()]), \
         io:format(\"deleteCount=~w~n\",[{module}:deleteCount()]), \
         io:format(\"afterDelete=~w~n\",[{module}:afterDelete()]), \
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
        ("selectCount=2", "select keeps the two rows with age >= 25"),
        (
            "selectName=lin",
            "select + fromRow decodes the first row older than 28",
        ),
        ("getName=lin", "get by id 2 decodes to lin"),
        ("getMissing=1", "get of a missing id is None"),
        ("deleteCount=1", "delete removes the one row under 25"),
        ("afterDelete=2", "two rows remain after the delete"),
    ] {
        assert!(
            stdout.contains(probe),
            "expected `{probe}` ({want})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
