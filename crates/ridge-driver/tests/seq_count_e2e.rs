//! End-to-end check for `count`/`exists` over an in-memory `Seq` — the same
//! size-and-presence terminals the database path exposes, run through the
//! in-memory interpreter on the BEAM, with no database or `deriving (Row)`.
//!
//! `count` answers how many rows the sequence selects and `exists` whether it
//! selects any. Both reflect the accumulated filter, ordering, page, and
//! `distinct` — they measure the window the sequence's `toList` would return,
//! exactly the rule the database `count`/`exists` follow. The cases below count
//! a whole sequence, count after a filter, confirm a `limit`/`offset` ahead of
//! the terminal bounds the answer to that window, and confirm `distinct`
//! collapses duplicates before the count; `exists` is checked true over a
//! non-empty window and false once a filter or an offset empties it.
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

-- Two rows are whole-row duplicates, so a `distinct` collapses them before the
-- count reads the window — the same rows the sequence's `toList` would return.
fn banded () -> List User =
    [ User { id = 1, name = "Ana",  age = 30 }
    , User { id = 1, name = "Ana",  age = 30 }
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

-- Count honours the page: an `offset`/`limit` ahead of `count` bounds it to that
-- window, so the answer is the two-row window, not the whole matched set of five
-- — the same `Take(n).Count()` rule the scalar aggregates follow.
pub fn pagedCount () -> Int =
    intOf (sample () |> Repo.from |> Repo.offset 1 |> Repo.limit 2 |> Repo.count)

-- Count honours `distinct`: the two identical Ana rows collapse to one, so the
-- window holds two rows — the same rows the sequence's `toList` would decode.
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

-- `exists` honours the page too: an offset past every row empties the window,
-- so nothing exists — an unpaged sequence over the same filter would.
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
    // offset 1 |> limit 2 |> count: the page bounds the count to its window → two.
    assert!(
        stdout.contains("pagedCount=2"),
        "expected `pagedCount=2` — count ignored the page (should fold the window)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // distinct |> count over banded(): the duplicate Ana rows collapse → two.
    assert!(
        stdout.contains("distinctCount=2"),
        "expected `distinctCount=2` — count ignored `distinct` (should dedupe the window)\nstdout:\n{stdout}\nstderr:\n{stderr}"
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
    // An offset past every row empties the window, so nothing exists.
    assert!(
        stdout.contains("existsPastPage=0"),
        "expected `existsPastPage=0` — exists ignored the offset (should probe the window)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
