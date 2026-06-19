//! End-to-end check for implicit structural `Row` — proves a record with NO
//! `deriving (Row)` still gets a working row codec on the BEAM, not just at
//! typecheck.
//!
//! `Row` is a structural capability: any record whose fields are all `SqlType`
//! primitives can be turned into a `Map Text SqlValue` and back. The compiler
//! synthesises that instance on sight, so a plain in-memory record flows
//! through the row machinery (and, on top of it, the in-memory query verbs)
//! with no annotation — the same `fromRow`/`toRow`/`rowColumns` the database
//! path uses for entities that opt in with `deriving (Row)`.
//!
//! The decisive part is the runtime: a typecheck-only instance would pass
//! inference and then crash with a missing-dictionary error when the BEAM tries
//! to call `fromRow`. This test runs all three methods on real OTP.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// A program that decodes, encodes, and lists the columns of a `User` record
/// that never wrote `deriving (Row)`.
///
/// Every field is a `SqlType` primitive, so the `Row User` instance is
/// synthesised structurally. `roundTripId` exercises `toRow` (encode) and
/// `fromRow` (decode) together; `columns` exercises `rowColumns`; `badId`
/// exercises the decode failure path.
const SOURCE: &str = r#"
import std.sql (toSql, fromRow, toRow, rowColumns, SqlValue)
import std.map as Map

-- NOTE: no `deriving (Row)`. The `Row User` instance is synthesised because
-- every field is a SqlType primitive.
pub type User = { id: Int, name: Text, createdAt: Int }

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

pub fn badRow () -> Result User Error =
    fromRow (Map.fromList [("id", toSql "notanint"), ("name", toSql "ada"), ("created_at", toSql 1000)])

pub fn badId () -> Int =
    match badRow ()
        Ok u  -> u.id
        Err _ -> 0 - 999

-- A sample record encoded with the synthesised `toRow`, then decoded back.
fn sample () -> User =
    User { id = 42, name = "bo", createdAt = 5 }

pub fn roundTripId () -> Int =
    let back: Result User Error = fromRow (toRow (sample ()))
    match back
        Ok u  -> u.id
        Err _ -> 0 - 1

-- A phantom `Option User` witness for rowColumns: its value is ignored, its
-- type selects the synthesised `Row User` instance.
fn userWitness () -> Option User =
    let w: Option User = None
    w

pub fn columns () -> Text =
    Text.join "," (rowColumns (userWitness ()))
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"implicit-row-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn implicit_row_decodes_record_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping implicit_row_decodes_record_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-implicit-row-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-implicit-row-e2e-cache-")
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
        "io:format(\"id=~w~n\",[{module}:idOf()]), \
         io:format(\"name=~s~n\",[{module}:nameOf()]), \
         io:format(\"bad=~w~n\",[{module}:badId()]), \
         io:format(\"roundTrip=~w~n\",[{module}:roundTripId()]), \
         io:format(\"cols=~s~n\",[{module}:columns()]), \
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

    // `fromRow` of the implicit instance decodes Int and Text columns.
    assert!(
        stdout.contains("id=7"),
        "expected `id=7` — implicit fromRow Int decode failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("name=ada"),
        "expected `name=ada` — implicit fromRow Text decode failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A `Text` value in the `id` column makes the `Int` field's fromSql fail.
    assert!(
        stdout.contains("bad=-999"),
        "expected `bad=-999` — implicit fromRow should fail on a type-mismatched column\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // `toRow` then `fromRow` round-trips a record built in memory.
    assert!(
        stdout.contains("roundTrip=42"),
        "expected `roundTrip=42` — implicit toRow/fromRow round-trip failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // `rowColumns` names the columns from the type alone, in declaration order,
    // with `createdAt` mapped to its snake_cased `created_at` column.
    assert!(
        stdout.contains("cols=id,name,created_at"),
        "expected `cols=id,name,created_at` — implicit rowColumns column list wrong\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
