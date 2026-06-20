//! End-to-end check for `groupBy`/`having`/`summarize` over an in-memory `Seq` —
//! the same grouping verbs the database path exposes, run through the in-memory
//! interpreter on the BEAM, with no database or `deriving (Row)`.
//!
//! `groupBy` names a key column and returns the unified `Grouped` builder;
//! `having` narrows the groups by an aggregate condition; `summarize` projects
//! each surviving group into a named record built from the group aggregates
//! (`g.key`, `g.count`, `g.sum`/`avg`/`min`/`max` over a column accessor) and
//! decodes it. The group runs through the same `PlanGroup` node the join
//! `summarize` uses; a single-leaf `Seq` row carries no `t0$` prefix, so the
//! interpreter reads the key and each grouped aggregate off the bare columns.
//! The cases below summarise every group, read each aggregate back, and narrow
//! the groups with a `having` on the count and on a folded sum.
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

/// Lifts a `List User` into a `Seq`, groups it by `dept`, and summarises each
/// group. Neither `User` nor the summarised `DeptStat` has `deriving (Row)` —
/// each row codec is synthesised structurally. Both departments size and divide
/// cleanly, so every aggregate (including the float average) prints exactly.
const SOURCE: &str = r#"
import std.repo as Repo
import std.query (SortOrder, Asc, Desc)

pub type User = { id: Int, name: Text, dept: Text, salary: Int }
pub type DeptStat = { dept: Text, members: Int, total: Int, avg: Float, lo: Int, hi: Int }

fn sample () -> List User =
    [ User { id = 1, name = "Ana",  dept = "eng",   salary = 100 }
    , User { id = 2, name = "Beto", dept = "eng",   salary = 200 }
    , User { id = 3, name = "Cami", dept = "sales", salary = 150 }
    , User { id = 4, name = "Dan",  dept = "sales", salary = 50 }
    , User { id = 5, name = "Eva",  dept = "eng",   salary = 300 }
    ]

fn summary () -> Result (List DeptStat) Error =
    sample () |> Repo.from |> Repo.groupBy (fn (u: User) -> u.dept) |> Repo.summarize (fn g -> DeptStat { dept = g.key, members = g.count, total = g.sum (fn (u: User) -> u.salary), avg = g.avg (fn (u: User) -> u.salary), lo = g.min (fn (u: User) -> u.salary), hi = g.max (fn (u: User) -> u.salary) })

fn lengthD (xs: List DeptStat) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + lengthD rest

fn totalFor (t: Text) (xs: List DeptStat) -> Int =
    match xs
        []        -> 0 - 1
        s :: rest -> if s.dept == t then s.total else totalFor t rest

fn membersFor (t: Text) (xs: List DeptStat) -> Int =
    match xs
        []        -> 0 - 1
        s :: rest -> if s.dept == t then s.members else membersFor t rest

fn loFor (t: Text) (xs: List DeptStat) -> Int =
    match xs
        []        -> 0 - 1
        s :: rest -> if s.dept == t then s.lo else loFor t rest

fn hiFor (t: Text) (xs: List DeptStat) -> Int =
    match xs
        []        -> 0 - 1
        s :: rest -> if s.dept == t then s.hi else hiFor t rest

fn avgFor (t: Text) (xs: List DeptStat) -> Float =
    match xs
        []        -> 0.0
        s :: rest -> if s.dept == t then s.avg else avgFor t rest

-- Two departments, so two summarised groups.
pub fn groupCount () -> Int =
    match summary ()
        Err _   -> 0 - 1
        Ok rows -> lengthD rows

-- eng has three members (Ana, Beto, Eva).
pub fn engMembers () -> Int =
    match summary ()
        Err _   -> 0 - 1
        Ok rows -> membersFor "eng" rows

-- eng payroll: 100 + 200 + 300 = 600.
pub fn engTotal () -> Int =
    match summary ()
        Err _   -> 0 - 1
        Ok rows -> totalFor "eng" rows

-- sales payroll: 150 + 50 = 200.
pub fn salesTotal () -> Int =
    match summary ()
        Err _   -> 0 - 1
        Ok rows -> totalFor "sales" rows

-- eng min/max salary: 100 and 300.
pub fn engLo () -> Int =
    match summary ()
        Err _   -> 0 - 1
        Ok rows -> loFor "eng" rows

pub fn engHi () -> Int =
    match summary ()
        Err _   -> 0 - 1
        Ok rows -> hiFor "eng" rows

-- eng average salary: 600 / 3 = 200.0.
pub fn engAvg () -> Float =
    match summary ()
        Err _   -> 0.0
        Ok rows -> avgFor "eng" rows

-- `having` on the group count keeps only eng (3 > 2); sales (2) drops.
pub fn havingCount () -> Int =
    match (sample () |> Repo.from |> Repo.groupBy (fn (u: User) -> u.dept) |> Repo.having (fn g -> g.count > 2) |> Repo.summarize (fn g -> DeptStat { dept = g.key, members = g.count, total = g.sum (fn (u: User) -> u.salary), avg = g.avg (fn (u: User) -> u.salary), lo = g.min (fn (u: User) -> u.salary), hi = g.max (fn (u: User) -> u.salary) }))
        Err _   -> 0 - 1
        Ok rows -> lengthD rows

-- `having` on a folded sum keeps only eng (600 >= 500); sales (200) drops.
pub fn havingSum () -> Int =
    match (sample () |> Repo.from |> Repo.groupBy (fn (u: User) -> u.dept) |> Repo.having (fn g -> g.sum (fn (u: User) -> u.salary) >= 500) |> Repo.summarize (fn g -> DeptStat { dept = g.key, members = g.count, total = g.sum (fn (u: User) -> u.salary), avg = g.avg (fn (u: User) -> u.salary), lo = g.min (fn (u: User) -> u.salary), hi = g.max (fn (u: User) -> u.salary) }))
        Err _   -> 0 - 1
        Ok rows -> lengthD rows
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"seq-groupby-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn seq_groupby_summarizes_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping seq_groupby_summarizes_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-seq-groupby-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-seq-groupby-e2e-cache-")
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
        "io:format(\"groupCount=~w~n\",[{module}:groupCount()]), \
         io:format(\"engMembers=~w~n\",[{module}:engMembers()]), \
         io:format(\"engTotal=~w~n\",[{module}:engTotal()]), \
         io:format(\"salesTotal=~w~n\",[{module}:salesTotal()]), \
         io:format(\"engLo=~w~n\",[{module}:engLo()]), \
         io:format(\"engHi=~w~n\",[{module}:engHi()]), \
         io:format(\"engAvg=~w~n\",[{module}:engAvg()]), \
         io:format(\"havingCount=~w~n\",[{module}:havingCount()]), \
         io:format(\"havingSum=~w~n\",[{module}:havingSum()]), \
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

    // Two departments → two groups.
    assert!(
        stdout.contains("groupCount=2"),
        "expected `groupCount=2` — Seq groupBy did not partition by key\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // eng has three members.
    assert!(
        stdout.contains("engMembers=3"),
        "expected `engMembers=3` — group count wrong (bare-key fallback?)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // eng payroll: 100+200+300 = 600.
    assert!(
        stdout.contains("engTotal=600"),
        "expected `engTotal=600` — grouped sum folded the wrong column\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // sales payroll: 150+50 = 200.
    assert!(
        stdout.contains("salesTotal=200"),
        "expected `salesTotal=200` — grouped sum wrong for the second group\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // eng min/max salary.
    assert!(
        stdout.contains("engLo=100"),
        "expected `engLo=100` — grouped min wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("engHi=300"),
        "expected `engHi=300` — grouped max wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // eng average salary: 600/3 = 200.0.
    assert!(
        stdout.contains("engAvg=200.0"),
        "expected `engAvg=200.0` — grouped avg wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // having count > 2 keeps only eng.
    assert!(
        stdout.contains("havingCount=1"),
        "expected `havingCount=1` — having on the group count did not narrow\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // having sum(salary) >= 500 keeps only eng (600).
    assert!(
        stdout.contains("havingSum=1"),
        "expected `havingSum=1` — having on a folded sum did not narrow\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
