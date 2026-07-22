//! End-to-end check for the scalar aggregates (`sumOf`/`avgOf`/`minOf`/`maxOf`)
//! over an in-memory `Seq` — the same column-folding verbs the database path
//! exposes, run through the in-memory interpreter on the BEAM, with no database
//! or `deriving (Row)`.
//!
//! Each aggregate folds the column a one-row accessor names over the rows the
//! sequence's filter selects, and answers that column's own type wrapped in
//! `Option` (`avgOf` is always `Option Float`), with `None` over an empty fold.
//! Like the database aggregates, the filter narrows the folded rows and a page
//! bounds them: `limit n` ahead of a fold folds that window, not the whole
//! selection. The cases below fold a whole sequence, compose with a filter,
//! confirm a `limit` bounds the fold, and confirm an emptied selection folds to
//! `None`. This is also the verb that
//! exercises the interpreter's bare-column aggregate fallback: a single-leaf
//! `Seq` row carries no `t0$` prefix, so `PlanAggregate` reads the bare column.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Lifts a `List User` into a `Seq` and folds columns over it with the scalar
/// aggregates. `User` has no `deriving (Row)` — the row codec is synthesised
/// structurally. `sumOf`/`minOf`/`maxOf` return `Result (Option Int) Error`,
/// decoded to a plain `Int` (-1 on the error branch, -2 on `None`); `avgOf`
/// returns `Result (Option Float) Error`, decoded to a `Float`.
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

-- Ages average cleanly to 30.0, so the float prints exactly.
fn triple () -> List User =
    [ User { id = 1, name = "Ana",  age = 20 }
    , User { id = 2, name = "Beto", age = 30 }
    , User { id = 3, name = "Cami", age = 40 }
    ]

fn optIntOf (r: Result (Option Int) Error) -> Int =
    match r
        Err _ -> 0 - 1
        Ok o  ->
            match o
                None   -> 0 - 2
                Some n -> n

fn optFloatOf (r: Result (Option Float) Error) -> Float =
    match r
        Err _ -> 0.0
        Ok o  ->
            match o
                None   -> 0.0
                Some f -> f

-- Sum every age: 34 + 28 + 41 + 19 + 55 = 177.
pub fn sumAges () -> Int =
    optIntOf (sample () |> Repo.from |> Repo.sumOf (fn (u: User) -> u.age))

-- The smallest age is Dan's 19.
pub fn minAge () -> Int =
    optIntOf (sample () |> Repo.from |> Repo.minOf (fn (u: User) -> u.age))

-- The largest age is Eva's 55.
pub fn maxAge () -> Int =
    optIntOf (sample () |> Repo.from |> Repo.maxOf (fn (u: User) -> u.age))

-- An aggregate folds only the filtered rows: age >= 30 keeps Ana 34, Cami 41,
-- Eva 55, summing to 130.
pub fn filteredSum () -> Int =
    optIntOf (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age >= 30) |> Repo.sumOf (fn (u: User) -> u.age))

-- An aggregate folds only the page: `limit 2` keeps Ana 34 and Beto 28, so the
-- sum is 62 rather than all five rows — the `Take(2).Sum()` it reads like.
pub fn pagedSum () -> Int =
    optIntOf (sample () |> Repo.from |> Repo.limit 2 |> Repo.sumOf (fn (u: User) -> u.age))

-- An aggregate over an empty selection is NULL, decoded as `None` (sentinel -2):
-- no row survives age > 100.
pub fn emptyMax () -> Int =
    optIntOf (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age > 100) |> Repo.maxOf (fn (u: User) -> u.age))

-- Average the three ages (20 + 30 + 40) / 3 = 30.0.
pub fn avgTriple () -> Float =
    optFloatOf (triple () |> Repo.from |> Repo.avgOf (fn (u: User) -> u.age))
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"seq-aggregate-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn seq_aggregate_folds_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping seq_aggregate_folds_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-seq-aggregate-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-seq-aggregate-e2e-cache-")
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
        "io:format(\"sumAges=~w~n\",[{module}:sumAges()]), \
         io:format(\"minAge=~w~n\",[{module}:minAge()]), \
         io:format(\"maxAge=~w~n\",[{module}:maxAge()]), \
         io:format(\"filteredSum=~w~n\",[{module}:filteredSum()]), \
         io:format(\"pagedSum=~w~n\",[{module}:pagedSum()]), \
         io:format(\"emptyMax=~w~n\",[{module}:emptyMax()]), \
         io:format(\"avgTriple=~w~n\",[{module}:avgTriple()]), \
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

    // Sum of all ages: 34+28+41+19+55 = 177.
    assert!(
        stdout.contains("sumAges=177"),
        "expected `sumAges=177` — Seq sumOf folded the wrong column (bare-name fallback?)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Smallest age: 19.
    assert!(
        stdout.contains("minAge=19"),
        "expected `minAge=19` — Seq minOf wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Largest age: 55.
    assert!(
        stdout.contains("maxAge=55"),
        "expected `maxAge=55` — Seq maxOf wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // filter age >= 30 then sum: 34+41+55 = 130.
    assert!(
        stdout.contains("filteredSum=130"),
        "expected `filteredSum=130` — aggregate did not reflect the filter\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // limit 2 then sum: the fold reads only that window → 34 + 28 = 62.
    assert!(
        stdout.contains("pagedSum=62"),
        "expected `pagedSum=62` — the fold ignored the page and summed every row\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // An aggregate over an empty selection is None (sentinel -2).
    assert!(
        stdout.contains("emptyMax=-2"),
        "expected `emptyMax=-2` — empty fold was not None\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Average of (20+30+40)/3 = 30.0.
    assert!(
        stdout.contains("avgTriple=30.0"),
        "expected `avgTriple=30.0` — Seq avgOf wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
