//! End-to-end check for `from`/`toList` over an in-memory `Seq` — proves a plain
//! `List` of records can be lifted into the query world and read back on the BEAM,
//! with no database, repository, or `deriving (Row)`.
//!
//! `from xs` snapshots a `List a` as rows (via the structurally-synthesised
//! `Row a`), producing an opaque `Seq a`. The same `toList`/`first` terminals the
//! database path exposes then decode those rows back into records — `Rows (Seq a)`
//! reduces to `a`, so `toList` answers `Result (List a) Error`. This is the
//! foundation the in-memory query verbs build on.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// A program that lifts a `List User` into a `Seq User` with `from` and reads it
/// back with `toList`/`first`. `User` has no `deriving (Row)` — its row codec is
/// synthesised structurally. The round-trip proves encode (`from`) and decode
/// (`toList`) both run on the BEAM.
const SOURCE: &str = r#"
import std.repo as Repo

pub type User = { id: Int, name: Text, age: Int }

fn sample () -> List User =
    [ User { id = 1, name = "Ana",  age = 34 }
    , User { id = 2, name = "Beto", age = 28 }
    , User { id = 3, name = "Cami", age = 41 }
    ]

fn lenOf (xs: List User) -> Int =
    match xs
        []        -> 0
        _ :: rest -> 1 + lenOf rest

fn ageSum (xs: List User) -> Int =
    match xs
        []        -> 0
        u :: rest -> u.age + ageSum rest

-- Count of round-tripped rows: from then toList, length of the result.
pub fn count () -> Int =
    match (sample () |> Repo.from |> Repo.toList)
        Err _   -> 0 - 1
        Ok back -> lenOf back

-- Sum of ages after the round-trip: proves each row decoded to the right record.
pub fn totalAge () -> Int =
    match (sample () |> Repo.from |> Repo.toList)
        Err _   -> 0 - 1
        Ok back -> ageSum back

-- The first element's name via the unified `first` terminal.
pub fn firstName () -> Text =
    match (sample () |> Repo.from |> Repo.first)
        Err _  -> "err"
        Ok opt ->
            match opt
                None   -> "(empty)"
                Some u -> u.name
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"seq-from-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn seq_from_round_trips_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping seq_from_round_trips_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-seq-from-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-seq-from-e2e-cache-")
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
        "io:format(\"count=~w~n\",[{module}:count()]), \
         io:format(\"total=~w~n\",[{module}:totalAge()]), \
         io:format(\"first=~s~n\",[{module}:firstName()]), \
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

    // Three records lifted and read back.
    assert!(
        stdout.contains("count=3"),
        "expected `count=3` — from/toList round-trip lost rows\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // 34 + 28 + 41 = 103: every row decoded to the right record.
    assert!(
        stdout.contains("total=103"),
        "expected `total=103` — decode of round-tripped rows wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The unified `first` terminal over a Seq.
    assert!(
        stdout.contains("first=Ana"),
        "expected `first=Ana` — Seq first terminal wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
