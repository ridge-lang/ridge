//! End-to-end check for std.raw on the in-memory adapter — proves the raw-SQL
//! escape hatch reports a clear error there (the in-memory store has no SQL
//! engine) rather than silently doing nothing, on the BEAM.
//!
//! `Raw.query`/`Raw.exec` run against a SQL backend; the in-memory adapter answers
//! `Err {code = "raw.unsupported", …}` instead. This program drives both verbs on a
//! memory store and reports whether each took the error branch:
//! - `queryErrs` — a raw query on the memory store is an error (1).
//! - `execErrs` — a raw statement on the memory store is an error (1).
//!
//! The happy path (raw SQL actually running) is exercised against a real database
//! in the Postgres e2e; here the point is that the memory backend fails honestly.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

const SOURCE: &str = r#"
import std.data (memAdapter, MemAdapter)
import std.raw as Raw
import std.sql (sqlInt)

pub type Probe = { n: Int } deriving (Row)

-- A raw query on the schemaless memory store cannot run: the verb answers
-- `Err {code = "raw.unsupported", …}`, so this takes the error branch.
pub fn db queryErrs () -> Int =
    let conn = memAdapter ()
    let r: Result (List Probe) Error = Raw.query conn "SELECT 1 AS n" []
    match r
        Err _ -> 1
        Ok _  -> 0

-- A raw statement on the memory store fails the same way.
pub fn db execErrs () -> Int =
    let conn = memAdapter ()
    match Raw.exec conn "DELETE FROM nope WHERE n = $1" [sqlInt 1]
        Err _ -> 1
        Ok _  -> 0
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-raw-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"db\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn raw_sql_on_the_memory_store_errors_clearly_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping raw_sql_on_the_memory_store_errors_clearly_on_beam"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-raw-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-raw-e2e-cache-")
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
        "io:format(\"queryErrs=~w~n\",[{module}:queryErrs()]), \
         io:format(\"execErrs=~w~n\",[{module}:execErrs()]), \
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

    // A raw query on the memory store is an error, not a silent empty result.
    assert!(
        stdout.contains("queryErrs=1"),
        "expected `queryErrs=1` — a raw query on the in-memory store must error\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A raw statement on the memory store is an error too.
    assert!(
        stdout.contains("execErrs=1"),
        "expected `execErrs=1` — a raw statement on the in-memory store must error\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
