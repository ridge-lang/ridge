//! End-to-end check for `every` over an in-memory `Seq` — the same
//! universal-predicate terminal the database path exposes, run through the
//! in-memory interpreter on the BEAM, with no database or `deriving (Row)`.
//!
//! `every` answers whether all the rows the sequence selects satisfy a further
//! predicate. It probes the window for one row that violates the predicate
//! (applied as `IS NOT TRUE` after the page) and is true exactly when none
//! does, so an empty window is vacuously true. Like the database `every`, it
//! reflects the accumulated filter, ordering, and page — it tests the rows the
//! window holds, the way `Take(n).All(...)` does. The cases below check a true
//! and a false universal, compose with a filter on both sides, confirm the
//! vacuous truth over an emptied selection, and confirm a `limit` ahead of
//! `every` narrows the tested rows to the window in both directions.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Lifts a `List User` into a `Seq` and tests universals over it with `every`.
/// `User` has no `deriving (Row)` — the row codec is synthesised structurally.
/// `every` returns `Result Bool Error`, decoded to 1/0 (with -1 on the
/// unreachable error branch) so the BEAM can print it.
const SOURCE: &str = r#"
import std.repo as Repo
import std.query (SortOrder, Asc, Desc)

pub type User = { id: Int, name: Text, age: Int }

fn sample () -> List User =
    [ User { id = 1, name = "Ana",  age = 34 }
    , User { id = 2, name = "Beto", age = 28 }
    , User { id = 3, name = "Cami", age = 41 }
    , User { id = 4, name = "Dan",  age = 19 }
    , User { id = 5, name = "Eva",  age = 55 }
    ]

fn boolOf (r: Result Bool Error) -> Int =
    match r
        Err _ -> 0 - 1
        Ok b  -> if b then 1 else 0

-- Every age is at least 18 (the youngest is Dan, 19), so the universal holds.
pub fn allOver18 () -> Int =
    boolOf (sample () |> Repo.from |> Repo.every (fn (u: User) -> u.age >= 18))

-- Not every age is at least 20: Dan (19) violates it, so the universal fails.
pub fn allOver20 () -> Int =
    boolOf (sample () |> Repo.from |> Repo.every (fn (u: User) -> u.age >= 20))

-- `every` reflects a filter: once age >= 30 keeps Ana/Cami/Eva, all of them are
-- >= 30, so the universal holds over the filtered selection.
pub fn filteredAllOver30 () -> Int =
    boolOf (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age >= 30) |> Repo.every (fn (u: User) -> u.age >= 30))

-- The same filtered selection is not all >= 40: Ana (34) violates it.
pub fn filteredNotAllOver40 () -> Int =
    boolOf (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age >= 30) |> Repo.every (fn (u: User) -> u.age >= 40))

-- A filter that matches nothing empties the selection, and a universal over an
-- empty selection is vacuously true — even with a predicate nothing could satisfy.
pub fn vacuousEmpty () -> Int =
    boolOf (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age > 100) |> Repo.every (fn (u: User) -> u.age < 0))

-- `every` honours the page: the window holds only the oldest row (Eva 55), which
-- is over 50, so the universal holds over it — unpaged, Ana (34) would violate it.
pub fn everyPagedWindow () -> Int =
    boolOf (sample () |> Repo.from |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.limit 1 |> Repo.every (fn (u: User) -> u.age >= 50))

-- The dual: the youngest-first window holds only Dan (19), who violates
-- age >= 20, so the universal fails over the window even though the older rows
-- beyond the page would pass it.
pub fn everyPagedViolator () -> Int =
    boolOf (sample () |> Repo.from |> Repo.orderBy Asc (fn (u: User) -> u.age) |> Repo.limit 1 |> Repo.every (fn (u: User) -> u.age >= 20))
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"seq-every-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn seq_every_tests_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping seq_every_tests_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-seq-every-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-seq-every-e2e-cache-")
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
        "io:format(\"allOver18=~w~n\",[{module}:allOver18()]), \
         io:format(\"allOver20=~w~n\",[{module}:allOver20()]), \
         io:format(\"filteredAllOver30=~w~n\",[{module}:filteredAllOver30()]), \
         io:format(\"filteredNotAllOver40=~w~n\",[{module}:filteredNotAllOver40()]), \
         io:format(\"vacuousEmpty=~w~n\",[{module}:vacuousEmpty()]), \
         io:format(\"everyPagedWindow=~w~n\",[{module}:everyPagedWindow()]), \
         io:format(\"everyPagedViolator=~w~n\",[{module}:everyPagedViolator()]), \
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

    // Every age >= 18 (youngest is 19): true.
    assert!(
        stdout.contains("allOver18=1"),
        "expected `allOver18=1` — every missed a holding universal\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Dan (19) violates age >= 20: false.
    assert!(
        stdout.contains("allOver20=0"),
        "expected `allOver20=0` — every did not find the violator\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // filter age >= 30 then every age >= 30: true over the filtered selection.
    assert!(
        stdout.contains("filteredAllOver30=1"),
        "expected `filteredAllOver30=1` — every did not reflect the filter\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The filtered selection is not all >= 40 (Ana 34): false.
    assert!(
        stdout.contains("filteredNotAllOver40=0"),
        "expected `filteredNotAllOver40=0` — every missed the filtered violator\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A universal over an empty selection is vacuously true.
    assert!(
        stdout.contains("vacuousEmpty=1"),
        "expected `vacuousEmpty=1` — every was not vacuously true over an emptied Seq\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // orderBy Desc age |> limit 1 |> every age >= 50: the window is [Eva 55] → true.
    assert!(
        stdout.contains("everyPagedWindow=1"),
        "expected `everyPagedWindow=1` — every tested rows beyond the page\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // orderBy Asc age |> limit 1 |> every age >= 20: the window is [Dan 19] → false.
    assert!(
        stdout.contains("everyPagedViolator=0"),
        "expected `everyPagedViolator=0` — every missed the violator inside the window\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
