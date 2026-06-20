//! End-to-end check for `union`/`unionAll`/`intersect`/`except` over an in-memory
//! `Seq` — the same set-operation verbs the database path exposes, run through the
//! in-memory interpreter on the BEAM, with no database or `deriving (Row)`.
//!
//! Each verb combines two sequences of the same row shape into one through the same
//! `PlanCombine` node the query path captures; the interpreter evaluates both sides
//! and applies the set operation over the two row lists, comparing rows by their full
//! contents. `union`/`intersect`/`except` drop duplicate rows (set semantics) and
//! `unionAll` keeps every one — the in-memory dual of SQL's `UNION`/`UNION ALL`/
//! `INTERSECT`/`EXCEPT`. The combined sequence is itself a `Seq`, so it keeps
//! composing: the cases below combine, then filter, order, page, and decode on top of
//! the combination, and read back a count, a presence bit, or a decoded row's field.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Two overlapping lists of `User` lifted into sequences and combined with each set
/// operation. `User` has no `deriving (Row)` — its row codec is synthesised
/// structurally. `active` is {1,2,3} and `admins` is {2,3,4} (by whole row), so the
/// combinations have known, exact sizes the cases below assert.
const SOURCE: &str = r#"
import std.repo as Repo
import std.query (SortOrder, Asc)

pub type User = { id: Int, name: Text }

fn active () -> List User =
    [ User { id = 1, name = "Ana" }
    , User { id = 2, name = "Beto" }
    , User { id = 3, name = "Cami" }
    ]

fn admins () -> List User =
    [ User { id = 2, name = "Beto" }
    , User { id = 3, name = "Cami" }
    , User { id = 4, name = "Dan" }
    ]

fn countOr (r: Result Int Error) -> Int =
    match r
        Err _ -> 0 - 1
        Ok n  -> n

fn existsBit (r: Result Bool Error) -> Int =
    match r
        Err _ -> 0 - 1
        Ok b  -> if b then 1 else 0

fn firstId (r: Result (Option User) Error) -> Int =
    match r
        Err _       -> 0 - 1
        Ok None     -> 0 - 1
        Ok (Some u) -> u.id

-- union de-dupes: {1,2,3} ∪ {2,3,4} = {1,2,3,4} → 4.
pub fn unionCount () -> Int =
    countOr (active () |> Repo.from |> Repo.union (admins () |> Repo.from) |> Repo.count)

-- unionAll keeps duplicates: 3 + 3 = 6.
pub fn unionAllCount () -> Int =
    countOr (active () |> Repo.from |> Repo.unionAll (admins () |> Repo.from) |> Repo.count)

-- intersect keeps the shared rows: {2,3} → 2.
pub fn intersectCount () -> Int =
    countOr (active () |> Repo.from |> Repo.intersect (admins () |> Repo.from) |> Repo.count)

-- except keeps the left rows absent from the right: active − admins = {1} → 1.
pub fn exceptCount () -> Int =
    countOr (active () |> Repo.from |> Repo.except (admins () |> Repo.from) |> Repo.count)

-- except is order-sensitive: admins − active = {4} → 1, a different singleton.
pub fn exceptReverseCount () -> Int =
    countOr (admins () |> Repo.from |> Repo.except (active () |> Repo.from) |> Repo.count)

-- The combined sequence keeps composing: filter the union down to id >= 3 → {3,4} → 2.
pub fn composedCount () -> Int =
    countOr (active () |> Repo.from |> Repo.union (admins () |> Repo.from) |> Repo.filter (fn (u: User) -> u.id >= 3) |> Repo.count)

-- A presence probe past a combine: anyone in the intersection with id == 2 → true.
pub fn intersectHasBeto () -> Int =
    existsBit (active () |> Repo.from |> Repo.intersect (admins () |> Repo.from) |> Repo.filter (fn (u: User) -> u.id == 2) |> Repo.exists)

-- Order, page, and decode on top of the union: the smallest id is 1.
pub fn unionMinId () -> Int =
    firstId (active () |> Repo.from |> Repo.union (admins () |> Repo.from) |> Repo.orderBy Asc (fn (u: User) -> u.id) |> Repo.limit 1 |> Repo.first)
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"seq-setops-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn seq_set_operations_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping seq_set_operations_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-seq-setops-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-seq-setops-e2e-cache-")
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
        "io:format(\"unionCount=~w~n\",[{module}:unionCount()]), \
         io:format(\"unionAllCount=~w~n\",[{module}:unionAllCount()]), \
         io:format(\"intersectCount=~w~n\",[{module}:intersectCount()]), \
         io:format(\"exceptCount=~w~n\",[{module}:exceptCount()]), \
         io:format(\"exceptReverseCount=~w~n\",[{module}:exceptReverseCount()]), \
         io:format(\"composedCount=~w~n\",[{module}:composedCount()]), \
         io:format(\"intersectHasBeto=~w~n\",[{module}:intersectHasBeto()]), \
         io:format(\"unionMinId=~w~n\",[{module}:unionMinId()]), \
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

    // union de-dupes the shared rows: {1,2,3,4}.
    assert!(
        stdout.contains("unionCount=4"),
        "expected `unionCount=4` — Seq union did not de-duplicate the combined rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // unionAll keeps every row: 3 + 3.
    assert!(
        stdout.contains("unionAllCount=6"),
        "expected `unionAllCount=6` — Seq unionAll dropped duplicates it should keep\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // intersect keeps the shared rows: {2,3}.
    assert!(
        stdout.contains("intersectCount=2"),
        "expected `intersectCount=2` — Seq intersect wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // except keeps the left rows not in the right: active − admins = {1}.
    assert!(
        stdout.contains("exceptCount=1"),
        "expected `exceptCount=1` — Seq except wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // except is order-sensitive: admins − active = {4}.
    assert!(
        stdout.contains("exceptReverseCount=1"),
        "expected `exceptReverseCount=1` — Seq except is not honouring branch order\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The combined sequence keeps composing: union then filter id >= 3 → {3,4}.
    assert!(
        stdout.contains("composedCount=2"),
        "expected `composedCount=2` — a filter did not compose on top of the combine\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A presence probe past a combine.
    assert!(
        stdout.contains("intersectHasBeto=1"),
        "expected `intersectHasBeto=1` — exists did not compose on top of the combine\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Order + project on top of the union: smallest id is 1.
    assert!(
        stdout.contains("unionMinId=1"),
        "expected `unionMinId=1` — orderBy/selectFirst did not compose on top of the combine\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
