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
import std.text as Text
import std.list as List

pub type User = { id: Int, age: Int, name: Text } deriving (Row)

pub type Code = { id: Int, label: Text } deriving (Row)

pub fn userRow (uid: Int) (uage: Int) (uname: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql uid), ("age", toSql uage), ("name", toSql uname)]

pub fn codeRow (cid: Int) (clabel: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql cid), ("label", toSql clabel)]

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

-- like: how many names contain "a"? (ada, max) -> 2
pub fn db likeContains () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> Text.contains u.name "a")
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- startsWith: how many names start with "l"? (lin) -> 1
pub fn db likeStarts () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> Text.startsWith u.name "l")
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- raw LIKE with `_`: three-character names with "a" in the middle (max) -> 1
pub fn db likeUnderscore () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> Text.like u.name "_a_")
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- IN over ages: rows whose age is 18 or 30 (ada, lin) -> 2
pub fn db inAges () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> List.contains u.age [18, 30])
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- IN over an empty set is never satisfied -> 0
pub fn db inEmpty () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> List.contains u.age [])
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- escaping: `contains "50%"` treats the `%` literally, matching only the
-- literal-percent label, not "50Xoff" -> 1
pub fn db escapeMatch () -> Int =
    let conn = memAdapter ()
    match appendRow conn "codes" (codeRow 1 "50%off")
        Err _ -> 0 - 1
        Ok _ ->
            match appendRow conn "codes" (codeRow 2 "50Xoff")
                Err _ -> 0 - 2
                Ok _ ->
                    match selectRows conn "codes" (fn (c: Code) -> Text.contains c.label "50%")
                        Ok rows -> lengthOf rows
                        Err _   -> 0 - 3

-- arithmetic `+` over two columns: age + id > 20 (lin 32, max 28) -> 2
pub fn db arithAdd () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> u.age + u.id > 20)
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- arithmetic `-` with a literal: age - 5 >= 20 (lin 25, max 20) -> 2
pub fn db arithSub () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> u.age - 5 >= 20)
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- arithmetic `*` with a literal: age * 2 > 50 (lin 60) -> 1
pub fn db arithMul () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> u.age * 2 > 50)
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- integer `/` truncates toward zero: age / 10 == 2 (max 25/10 = 2) -> 1
pub fn db arithDiv () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> u.age / 10 == 2)
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- modulo `%` (Int-only): even ages (ada 18, lin 30) -> 2
pub fn db arithMod () -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> u.age % 2 == 0)
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- F6: a quoted predicate can read a field of a captured record (`target.id`), not
-- just a bound scalar local. The field's value binds as a query parameter, so this
-- keeps the one row whose id equals the captured row's id (lin, id 2) -> 1
pub fn db captureFieldAccess () -> Int =
    let target = User { id = 2, age = 30, name = "lin" }
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> u.id == target.id)
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- F12: a captured text-match pattern (a parameter, not a literal) binds at run time
-- and matches the same rows the literal form would — startsWith "l" keeps lin -> 1
pub fn db likeStartsCaptured (prefix: Text) -> Int =
    match setup ()
        Err _ -> 0 - 1
        Ok conn ->
            match selectRows conn "users" (fn (u: User) -> Text.startsWith u.name prefix)
                Ok rows -> lengthOf rows
                Err _   -> 0 - 2

-- F12 escaping: a captured needle's LIKE wildcards are escaped at run time exactly as
-- a literal's are, so `contains "50%"` matches the literal-percent label only -> 1
pub fn db likeContainsCaptured (needle: Text) -> Int =
    let conn = memAdapter ()
    match appendRow conn "codes" (codeRow 1 "50%off")
        Err _ -> 0 - 1
        Ok _ ->
            match appendRow conn "codes" (codeRow 2 "50Xoff")
                Err _ -> 0 - 2
                Ok _ ->
                    match selectRows conn "codes" (fn (c: Code) -> Text.contains c.label needle)
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
         io:format(\"likeContains=~w~n\",[{module}:likeContains()]), \
         io:format(\"likeStarts=~w~n\",[{module}:likeStarts()]), \
         io:format(\"likeUnderscore=~w~n\",[{module}:likeUnderscore()]), \
         io:format(\"inAges=~w~n\",[{module}:inAges()]), \
         io:format(\"inEmpty=~w~n\",[{module}:inEmpty()]), \
         io:format(\"escapeMatch=~w~n\",[{module}:escapeMatch()]), \
         io:format(\"arithAdd=~w~n\",[{module}:arithAdd()]), \
         io:format(\"arithSub=~w~n\",[{module}:arithSub()]), \
         io:format(\"arithMul=~w~n\",[{module}:arithMul()]), \
         io:format(\"arithDiv=~w~n\",[{module}:arithDiv()]), \
         io:format(\"arithMod=~w~n\",[{module}:arithMod()]), \
         io:format(\"captureFieldAccess=~w~n\",[{module}:captureFieldAccess()]), \
         io:format(\"likeStartsCaptured=~w~n\",[{module}:likeStartsCaptured(<<\"l\">>)]), \
         io:format(\"likeContainsCaptured=~w~n\",[{module}:likeContainsCaptured(<<\"50%\">>)]), \
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
        ("likeContains=2", "contains \"a\" keeps ada and max"),
        ("likeStarts=1", "startsWith \"l\" keeps lin"),
        ("likeUnderscore=1", "like \"_a_\" keeps max"),
        ("inAges=2", "IN [18, 30] keeps ada and lin"),
        ("inEmpty=0", "IN [] keeps nothing"),
        (
            "escapeMatch=1",
            "contains \"50%\" matches the literal-percent label only",
        ),
        ("arithAdd=2", "age + id > 20 keeps lin and max"),
        ("arithSub=2", "age - 5 >= 20 keeps lin and max"),
        ("arithMul=1", "age * 2 > 50 keeps lin"),
        ("arithDiv=1", "age / 10 == 2 keeps max (integer truncation)"),
        ("arithMod=2", "age % 2 == 0 keeps the even ages ada and lin"),
        (
            "captureFieldAccess=1",
            "a captured record field (target.id) binds and keeps the one matching row",
        ),
        (
            "likeStartsCaptured=1",
            "a captured startsWith prefix binds and keeps lin, like the literal form",
        ),
        (
            "likeContainsCaptured=1",
            "a captured contains needle escapes its % at run time, matching 50%off only",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "expected `{probe}` ({want})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
