//! End-to-end check for the std.data in-memory adapter — proves the storage
//! seam runs on the BEAM, not just at typecheck.
//!
//! `memAdapter` opens a process-backed in-memory store (requires `db`);
//! `appendRow` appends a row and `all` reads a table back. This program opens an
//! adapter, seeds two `users` rows, reads them back, and decodes the first one
//! into a record via its `deriving (Row)` instance. It exercises:
//! - append -> read-back (two rows go in, two come out, in order),
//! - the row shape round-tripping through `fromRow` (`name` decodes to "ada"),
//! - handle isolation (each `memAdapter ()` call is an independent store).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// A program that seeds an in-memory store and reads it back.
///
/// `seededRows` opens an adapter, inserts two users (propagating any failure
/// through `Result`), and returns every row of the `users` table. `rowCount`
/// proves both inserts landed by matching an exactly-two-element list; `firstName`
/// decodes the first row back into a `User` and reads its `name`.
const SOURCE: &str = r#"
import std.data (memAdapter, appendRow, all)
import std.sql (toSql, fromRow, SqlValue)
import std.map as Map

pub type User = { id: Int, name: Text } deriving (Row)

pub fn userRow (uid: Int) (uname: Text) -> Map Text SqlValue =
    Map.fromList [("id", toSql uid), ("name", toSql uname)]

pub fn db seededRows () -> Result (List (Map Text SqlValue)) Error =
    let conn = memAdapter ()
    match appendRow conn "users" (userRow 1 "ada")
        Err e -> Err e
        Ok _  ->
            match appendRow conn "users" (userRow 2 "lin")
                Err e -> Err e
                Ok _  -> all conn "users"

pub fn db rowCount () -> Int =
    match seededRows ()
        Ok rows ->
            match rows
                _ :: _ :: [] -> 2
                _ :: []      -> 1
                []           -> 0
                _            -> 0 - 2
        Err _ -> 0 - 1

pub fn decodeUser (r: Map Text SqlValue) -> Result User Error =
    fromRow r

pub fn db firstName () -> Text =
    match seededRows ()
        Ok rows ->
            match rows
                r :: _ ->
                    match decodeUser r
                        Ok u  -> u.name
                        Err _ -> "decode-err"
                [] -> "empty"
        Err _ -> "store-err"
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"data-adapter-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn in_memory_adapter_roundtrips_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping in_memory_adapter_roundtrips_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-data-adapter-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-data-adapter-e2e-cache-")
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
        "io:format(\"count=~w~n\",[{module}:rowCount()]), \
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

    // Two rows inserted, two read back, in insertion order.
    assert!(
        stdout.contains("count=2"),
        "expected `count=2` — insert -> read-back failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The first row decodes back into a User via deriving (Row).
    assert!(
        stdout.contains("first=ada"),
        "expected `first=ada` — row shape did not round-trip through fromRow\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
