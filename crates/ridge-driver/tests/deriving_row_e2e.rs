//! End-to-end check for `deriving (Row)` — proves a synthesised row decoder
//! runs on the BEAM, not just at typecheck.
//!
//! `deriving (Row)` on a record generates a `Row` instance whose `fromRow`
//! reads a database row (a `Map Text SqlValue` keyed by snake-cased column name)
//! back into the record. Each field dispatches the field type's `SqlType.fromSql`
//! cross-module to `std.sql`, threading the first failure outward.
//!
//! This test compiles a program that decodes a row into a record and exercises:
//! - the happy path (every column present and well-typed),
//! - the snake-case column mapping (`createdAt` reads column `created_at`),
//! - the failure path (a column whose `SqlValue` type does not match the field).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// A program that decodes a `Map Text SqlValue` row into a `User` record.
///
/// `User` derives `Row`. `userRow` builds a well-formed row (note `created_at`,
/// the snake_cased spelling of the `createdAt` field). `badRow` puts a `Text`
/// value in the `id` column, so the `Int` field's `fromSql` fails and `fromRow`
/// returns `Err` — the `badId` sentinel catches it.
const SOURCE: &str = r#"
import std.sql (toSql, fromRow, SqlValue)
import std.map as Map

pub type User = { id: Int, name: Text, createdAt: Int } deriving (Row)

pub fn userRow () -> Map Text SqlValue =
    Map.fromList [("id", toSql 7), ("name", toSql "ada"), ("created_at", toSql 1000)]

pub fn decoded () -> Result User Error =
    fromRow (userRow ())

pub fn idOf () -> Int =
    match decoded ()
        Ok u  -> u.id
        Err _ -> 0 - 1

pub fn nameOf () -> Text =
    match decoded ()
        Ok u  -> u.name
        Err _ -> "error"

pub fn createdOf () -> Int =
    match decoded ()
        Ok u  -> u.createdAt
        Err _ -> 0 - 1

pub fn badRow () -> Result User Error =
    fromRow (Map.fromList [("id", toSql "notanint"), ("name", toSql "ada"), ("created_at", toSql 1000)])

pub fn badId () -> Int =
    match badRow ()
        Ok u  -> u.id
        Err _ -> 0 - 999
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"deriving-row-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn deriving_row_decodes_record_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping deriving_row_decodes_record_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-deriving-row-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-deriving-row-e2e-cache-")
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

    // All BEAM files (user module + stdlib modules including std.sql/std.map) end
    // up in the same directory; a single -pa flag covers all of them.
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

    // Drive every probe in a single BEAM boot.
    let expr = format!(
        "io:format(\"id=~w~n\",[{module}:idOf()]), \
         io:format(\"name=~s~n\",[{module}:nameOf()]), \
         io:format(\"created=~w~n\",[{module}:createdOf()]), \
         io:format(\"bad=~w~n\",[{module}:badId()]), \
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

    assert!(
        stdout.contains("id=7"),
        "expected `id=7` — fromRow Int column decode failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("name=ada"),
        "expected `name=ada` — fromRow Text column decode failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // `createdAt` reads the snake_cased `created_at` column.
    assert!(
        stdout.contains("created=1000"),
        "expected `created=1000` — snake_case column mapping failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A `Text` value in the `id` column makes the `Int` field's fromSql fail, so
    // fromRow returns `Err` and `badId` yields its sentinel.
    assert!(
        stdout.contains("bad=-999"),
        "expected `bad=-999` — fromRow should fail on a type-mismatched column\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
