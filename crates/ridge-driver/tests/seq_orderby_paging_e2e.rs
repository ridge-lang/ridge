//! End-to-end check for `orderBy`/`limit`/`offset`/`distinct` over an in-memory
//! `Seq` — the same builder verbs the database path exposes, run through the
//! in-memory interpreter on the BEAM, with no database or `deriving (Row)`.
//!
//! The point this proves beyond "they run" is that an in-memory sequence pages
//! the way a database query does. `Seq` carries the same builder fields a `Query`
//! does and materialises one `planRefine` at the terminal, so `offset` then
//! `limit` mean a single offset-then-limit over the ordered rows — independent of
//! the order the verbs were chained. The two window functions build the same page
//! two ways (`offset |> limit` and `limit |> offset`) and must return the same
//! rows; a per-verb nesting would have answered different windows.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Lifts a `List User` into a `Seq` and exercises the ordering, paging, and
/// distinct verbs. `User` has no `deriving (Row)` — its row codec is synthesised
/// structurally. Every result is read back on the BEAM through the in-memory
/// interpreter.
const SOURCE: &str = r#"
import std.repo as Repo
import std.query (SortOrder, Asc, Desc)

pub type User = { id: Int, name: Text, age: Int }

-- Five distinct ages, so a `Desc` ordering is unambiguous.
fn sample () -> List User =
    [ User { id = 1, name = "Ana",  age = 34 }
    , User { id = 2, name = "Beto", age = 28 }
    , User { id = 3, name = "Cami", age = 41 }
    , User { id = 4, name = "Dan",  age = 19 }
    , User { id = 5, name = "Eva",  age = 55 }
    ]

-- A list with an exact-duplicate row, for `distinct` to collapse.
fn dupes () -> List User =
    [ User { id = 1, name = "Ana",  age = 30 }
    , User { id = 1, name = "Ana",  age = 30 }
    , User { id = 2, name = "Beto", age = 25 }
    ]

fn lenOf (xs: List User) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + lenOf rest

fn idSum (xs: List User) -> Int =
    match xs
        []        -> 0
        u :: rest -> u.id + idSum rest

-- Oldest first: `orderBy Desc age` then `first` is Eva (55).
pub fn descFirstAge () -> Int =
    match (sample () |> Repo.from |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.first)
        Err _  -> 0 - 1
        Ok opt ->
            match opt
                None   -> 0 - 2
                Some u -> u.age

-- Ordering and paging compose: oldest first, skip two, take one is the third-
-- oldest — Eva 55, Cami 41, then Ana 34.
pub fn thirdByDescAge () -> Int =
    match (sample () |> Repo.from |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.offset 2 |> Repo.limit 1 |> Repo.first)
        Err _  -> 0 - 1
        Ok opt ->
            match opt
                None   -> 0 - 2
                Some u -> u.age

-- A window of the id-ordered rows, paged offset-then-limit: skip 1, take 2 keeps
-- ids 2 and 3 (sum 5).
pub fn windowSumAB () -> Int =
    match (sample () |> Repo.from |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.offset 1 |> Repo.limit 2 |> Repo.toList)
        Err _   -> 0 - 1
        Ok rows -> idSum rows

-- The same window built in the opposite verb order. Because the page is one
-- offset-then-limit over the ordered rows, not a per-verb nesting, this keeps the
-- same ids 2 and 3 (sum 5) — the database path's semantics, not a take-then-drop.
pub fn windowSumBA () -> Int =
    match (sample () |> Repo.from |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.limit 2 |> Repo.offset 1 |> Repo.toList)
        Err _   -> 0 - 1
        Ok rows -> idSum rows

-- `filter` and `orderBy` compose: keep age >= 28 (drops Dan 19), oldest-first the
-- youngest survivor is Beto 28.
pub fn filteredAscFirstAge () -> Int =
    match (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age >= 28) |> Repo.orderBy Asc (fn (u: User) -> u.age) |> Repo.first)
        Err _  -> 0 - 1
        Ok opt ->
            match opt
                None   -> 0 - 2
                Some u -> u.age

-- `distinct` collapses the exact-duplicate row: three rows in, two out.
pub fn distinctCount () -> Int =
    match (dupes () |> Repo.from |> Repo.distinct |> Repo.toList)
        Err _   -> 0 - 1
        Ok rows -> lenOf rows

-- Without `distinct` the duplicate stays: three rows.
pub fn dupCount () -> Int =
    match (dupes () |> Repo.from |> Repo.toList)
        Err _   -> 0 - 1
        Ok rows -> lenOf rows
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"seq-orderby-paging-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn seq_orderby_and_paging_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping seq_orderby_and_paging_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-seq-orderby-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-seq-orderby-e2e-cache-")
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
        "io:format(\"descFirstAge=~w~n\",[{module}:descFirstAge()]), \
         io:format(\"thirdByDescAge=~w~n\",[{module}:thirdByDescAge()]), \
         io:format(\"windowSumAB=~w~n\",[{module}:windowSumAB()]), \
         io:format(\"windowSumBA=~w~n\",[{module}:windowSumBA()]), \
         io:format(\"filteredAscFirstAge=~w~n\",[{module}:filteredAscFirstAge()]), \
         io:format(\"distinctCount=~w~n\",[{module}:distinctCount()]), \
         io:format(\"dupCount=~w~n\",[{module}:dupCount()]), \
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

    // `orderBy Desc age |> first` is the oldest: Eva 55.
    assert!(
        stdout.contains("descFirstAge=55"),
        "expected `descFirstAge=55` — Seq orderBy Desc wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Oldest first, skip two, take one is the third-oldest: Ana 34.
    assert!(
        stdout.contains("thirdByDescAge=34"),
        "expected `thirdByDescAge=34` — orderBy + offset + limit did not compose\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Window ids 2,3 (sum 5) paged offset-then-limit.
    assert!(
        stdout.contains("windowSumAB=5"),
        "expected `windowSumAB=5` — offset/limit window wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The same window the other chain order: still ids 2,3 (sum 5). This is the
    // database path's semantics — a per-verb nesting would have answered a
    // different window (id 2 alone, sum 2).
    assert!(
        stdout.contains("windowSumBA=5"),
        "expected `windowSumBA=5` — in-memory paging diverged from the database path's offset-then-limit\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // filter then orderBy: youngest survivor of age >= 28 is Beto 28.
    assert!(
        stdout.contains("filteredAscFirstAge=28"),
        "expected `filteredAscFirstAge=28` — filter + orderBy did not compose\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // distinct collapses the exact duplicate: 3 rows in, 2 out.
    assert!(
        stdout.contains("distinctCount=2"),
        "expected `distinctCount=2` — Seq distinct did not drop the duplicate row\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Without distinct the duplicate stays.
    assert!(
        stdout.contains("dupCount=3"),
        "expected `dupCount=3` — baseline row count wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
