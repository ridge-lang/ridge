//! End-to-end check for `count`/`exists` over an in-memory `Seq` — the same
//! size-and-presence terminals the database path exposes, run through the
//! in-memory interpreter on the BEAM, with no database or `deriving (Row)`.
//!
//! `count` answers how many rows the sequence selects and `exists` whether it
//! selects any. Both reflect the accumulated filter but ignore ordering, the
//! page, and `distinct` — they answer the size of the matched row set, not a
//! paged window of it — exactly the rule the database `count`/`exists` follow.
//! The cases below count a whole sequence, count after a filter, and confirm
//! that a `limit`/`offset`/`distinct` ahead of the terminal does not change the
//! answer; `exists` is checked true over a non-empty selection and false once a
//! filter empties it, and likewise unmoved by an offset past the rows.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Lifts a `List User` into a `Seq` and reduces it with `count`/`exists`. `User`
/// has no `deriving (Row)` — the row codec is synthesised structurally. Each
/// terminal returns a `Result`, decoded to a plain `Int` (a count, or 1/0 for a
/// boolean, with -1 on the unreachable error branch) so the BEAM can print it.
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

-- Two rows share an age, so a `distinct` would collapse them — but a `count`
-- ignores `distinct`, the same way the database count does.
fn banded () -> List User =
    [ User { id = 1, name = "Ana",  age = 30 }
    , User { id = 2, name = "Beto", age = 30 }
    , User { id = 3, name = "Cami", age = 25 }
    ]

fn intOf (r: Result Int Error) -> Int =
    match r
        Err _ -> 0 - 1
        Ok n  -> n

fn boolOf (r: Result Bool Error) -> Int =
    match r
        Err _ -> 0 - 1
        Ok b  -> if b then 1 else 0

-- Whole-sequence count: five rows.
pub fn totalCount () -> Int =
    intOf (sample () |> Repo.from |> Repo.count)

-- Count reflects the filter: keep age >= 30 (Ana 34, Cami 41, Eva 55), so three.
pub fn filteredCount () -> Int =
    intOf (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age >= 30) |> Repo.count)

-- Count ignores the page: an `offset`/`limit` ahead of `count` does not bound it,
-- so the answer is still the whole matched set of five, not the two-row window.
pub fn pagedCount () -> Int =
    intOf (sample () |> Repo.from |> Repo.offset 1 |> Repo.limit 2 |> Repo.count)

-- Count ignores `distinct`: the two age-30 rows are not collapsed, so three rows
-- in still count as three — the same rule the database count follows.
pub fn distinctCount () -> Int =
    intOf (banded () |> Repo.from |> Repo.distinct |> Repo.count)

-- A non-empty sequence exists.
pub fn anyAll () -> Int =
    boolOf (sample () |> Repo.from |> Repo.exists)

-- A filter that matches nothing empties the selection, so it does not exist.
pub fn anyFiltered () -> Int =
    boolOf (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age > 100) |> Repo.exists)

-- A filter that keeps a row (Eva 55) exists.
pub fn existsFilteredTrue () -> Int =
    boolOf (sample () |> Repo.from |> Repo.filter (fn (u: User) -> u.age >= 50) |> Repo.exists)

-- `exists` ignores the page too: an offset past every row does not empty the
-- matched set, so the sequence still exists.
pub fn existsPastPage () -> Int =
    boolOf (sample () |> Repo.from |> Repo.offset 100 |> Repo.exists)
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"seq-count-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn seq_count_reduces_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping seq_count_reduces_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-seq-count-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-seq-count-e2e-cache-")
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
        "io:format(\"totalCount=~w~n\",[{module}:totalCount()]), \
         io:format(\"filteredCount=~w~n\",[{module}:filteredCount()]), \
         io:format(\"pagedCount=~w~n\",[{module}:pagedCount()]), \
         io:format(\"distinctCount=~w~n\",[{module}:distinctCount()]), \
         io:format(\"anyAll=~w~n\",[{module}:anyAll()]), \
         io:format(\"anyFiltered=~w~n\",[{module}:anyFiltered()]), \
         io:format(\"existsFilteredTrue=~w~n\",[{module}:existsFilteredTrue()]), \
         io:format(\"existsPastPage=~w~n\",[{module}:existsPastPage()]), \
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

    // Whole-sequence count: five rows.
    assert!(
        stdout.contains("totalCount=5"),
        "expected `totalCount=5` — Seq count miscounted\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // filter age >= 30 then count: Ana 34, Cami 41, Eva 55 → three.
    assert!(
        stdout.contains("filteredCount=3"),
        "expected `filteredCount=3` — count did not reflect the filter\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // offset 1 |> limit 2 |> count: the page does not bound the count → still five.
    assert!(
        stdout.contains("pagedCount=5"),
        "expected `pagedCount=5` — count was bounded by the page (should ignore it)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // distinct |> count over banded(): distinct is ignored → three rows, not two.
    assert!(
        stdout.contains("distinctCount=3"),
        "expected `distinctCount=3` — count honoured `distinct` (should ignore it)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A non-empty sequence exists.
    assert!(
        stdout.contains("anyAll=1"),
        "expected `anyAll=1` — exists was false over a non-empty Seq\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A filter that matches nothing empties the selection.
    assert!(
        stdout.contains("anyFiltered=0"),
        "expected `anyFiltered=0` — exists was true over an emptied Seq\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A filter that keeps Eva (55) still exists.
    assert!(
        stdout.contains("existsFilteredTrue=1"),
        "expected `existsFilteredTrue=1` — exists did not reflect the kept row\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // An offset past every row does not empty the matched set.
    assert!(
        stdout.contains("existsPastPage=1"),
        "expected `existsPastPage=1` — exists was bounded by the offset (should ignore it)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
