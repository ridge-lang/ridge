//! End-to-end check for `select`/`selectFirst` over an in-memory `Seq` — the
//! same projection verb the database path exposes, run through the in-memory
//! interpreter on the BEAM, with no database or `deriving (Row)`.
//!
//! `select` reshapes each row into a chosen record and decodes it, so it is the
//! one in-memory verb that changes the element type (`Seq User` projected to a
//! `Summary`). The projection quote reifies to a `QProj` of unprefixed
//! `(alias, column)` pairs, read straight off the inline rows; the projected
//! shape needs only `Row` (auto-synthesised), no adapter. The cases below project
//! whole-table, compose projection with `filter`/`orderBy`, and check that a
//! `distinct` ahead of a `select` dedupes the projected columns.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Lifts a `List User` into a `Seq` and projects it into smaller shapes. Neither
/// `User` nor the projected `Summary`/`Band` has `deriving (Row)` — each row codec
/// is synthesised structurally. Every result is read back on the BEAM.
const SOURCE: &str = r#"
import std.repo as Repo
import std.query (SortOrder, Asc, Desc)

pub type User = { id: Int, name: Text, age: Int }
pub type Summary = { who: Text, years: Int }
pub type Band = { years: Int }

fn sample () -> List User =
    [ User { id = 1, name = "Ana",  age = 34 }
    , User { id = 2, name = "Beto", age = 28 }
    , User { id = 3, name = "Cami", age = 41 }
    , User { id = 4, name = "Dan",  age = 19 }
    , User { id = 5, name = "Eva",  age = 55 }
    ]

-- Two rows share an age, so a `distinct` over the projected `years` collapses them.
fn banded () -> List User =
    [ User { id = 1, name = "Ana",  age = 30 }
    , User { id = 2, name = "Beto", age = 30 }
    , User { id = 3, name = "Cami", age = 25 }
    ]

fn countS (xs: List Summary) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + countS rest

fn sumYearsS (xs: List Summary) -> Int =
    match xs
        []        -> 0
        s :: rest -> s.years + sumYearsS rest

fn countB (xs: List Band) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + countB rest

-- Project every row into a Summary: five rows out.
pub fn projCount () -> Int =
    match (sample () |> Repo.from |> Repo.select (fn (u: User) -> Summary { who = u.name, years = u.age }))
        Err _   -> 0 - 1
        Ok rows -> countS rows

-- The projected `years` carry the source ages: 34 + 28 + 41 + 19 + 55 = 177.
pub fn projYearsSum () -> Int =
    match (sample () |> Repo.from |> Repo.select (fn (u: User) -> Summary { who = u.name, years = u.age }))
        Err _   -> 0 - 1
        Ok rows -> sumYearsS rows

-- Projection composes with ordering: oldest-first, the first projected `who` is Eva.
pub fn projFirstWho () -> Text =
    match (sample () |> Repo.from |> Repo.orderBy Desc (fn (u: User) -> u.age) |> Repo.selectFirst (fn (u: User) -> Summary { who = u.name, years = u.age }))
        Err _  -> "err"
        Ok opt ->
            match opt
                None   -> "(empty)"
                Some s -> s.who

-- Projection composes with a filter: keep age >= 30 (Ana 34, Cami 41, Eva 55),
-- then project; the projected years sum to 130.
pub fn filteredProjYearsSum () -> Int =
    match (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age >= 30) |> Repo.select (fn (u: User) -> Summary { who = u.name, years = u.age }))
        Err _   -> 0 - 1
        Ok rows -> sumYearsS rows

-- `distinct` ahead of a `select` dedupes the PROJECTED columns: the two age-30 rows
-- project to the same `years`, so three rows in become two out.
pub fn distinctProjCount () -> Int =
    match (banded () |> Repo.from |> Repo.distinct |> Repo.select (fn (u: User) -> Band { years = u.age }))
        Err _   -> 0 - 1
        Ok rows -> countB rows

-- Without `distinct` the projected duplicate stays: three rows.
pub fn dupProjCount () -> Int =
    match (banded () |> Repo.from |> Repo.select (fn (u: User) -> Band { years = u.age }))
        Err _   -> 0 - 1
        Ok rows -> countB rows
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"seq-select-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn seq_select_projects_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping seq_select_projects_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-seq-select-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-seq-select-e2e-cache-")
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
        "io:format(\"projCount=~w~n\",[{module}:projCount()]), \
         io:format(\"projYearsSum=~w~n\",[{module}:projYearsSum()]), \
         io:format(\"projFirstWho=~s~n\",[{module}:projFirstWho()]), \
         io:format(\"filteredProjYearsSum=~w~n\",[{module}:filteredProjYearsSum()]), \
         io:format(\"distinctProjCount=~w~n\",[{module}:distinctProjCount()]), \
         io:format(\"dupProjCount=~w~n\",[{module}:dupProjCount()]), \
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

    // Five rows projected into Summaries.
    assert!(
        stdout.contains("projCount=5"),
        "expected `projCount=5` — Seq select dropped rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The projected years carry the source ages: 34+28+41+19+55 = 177.
    assert!(
        stdout.contains("projYearsSum=177"),
        "expected `projYearsSum=177` — projected column wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // orderBy Desc age then selectFirst: the oldest projected `who` is Eva.
    assert!(
        stdout.contains("projFirstWho=Eva"),
        "expected `projFirstWho=Eva` — select did not compose with orderBy/selectFirst\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // filter age >= 30 then project: 34+41+55 = 130.
    assert!(
        stdout.contains("filteredProjYearsSum=130"),
        "expected `filteredProjYearsSum=130` — filter did not compose with select\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // distinct over the projected `years`: two age-30 rows collapse, 3 in -> 2 out.
    assert!(
        stdout.contains("distinctProjCount=2"),
        "expected `distinctProjCount=2` — distinct did not dedupe the projected columns\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Without distinct the projected duplicate stays: three rows.
    assert!(
        stdout.contains("dupProjCount=3"),
        "expected `dupProjCount=3` — baseline projected count wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
