//! End-to-end round-trip for `std.sql` — proves `toSql`/`fromSql` work at
//! runtime on the BEAM, not just at typecheck.
//!
//! The bug: the lowering pass emitted a `SymbolRef::Local` for `$inst_SqlType_Int`
//! (the instance dictionary constant), looking for it in the user's own BEAM
//! module. The constant actually lives in the compiled `std.sql` BEAM module.
//! The fix emits `SymbolRef::Stdlib { module: "std.sql", name: "$inst_SqlType_Int" }`
//! so that codegen calls `'std.sql':'$inst_SqlType_Int'()` cross-module.
//!
//! This test compiles a program that exercises the full `toSql`/`fromSql`
//! round-trip for `Int`, `Text`, and `Bool`, runs it on the BEAM, and asserts
//! each value survives the round-trip intact.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// A program that exercises `toSql`/`fromSql` round-trips for Int, Text, and Bool.
///
/// `roundTripInt`, `roundTripText`, and `roundTripBool` each marshal a value
/// into a `SqlValue`, then unmarshal it back. On success the original value is
/// returned; on failure a sentinel is returned so the assertion catches it.
///
/// `toSql` and `fromSql` are class methods of `SqlType`. Importing them by name
/// resolves them as `StdlibSymbol` bindings; the lowering detects that they are
/// class methods (via the class table) and routes through the typeclass dictionary
/// mechanism, fetching the dictionary cross-module from `std.sql` at runtime.
const SOURCE: &str = r#"
import std.sql (toSql, fromSql, SqlValue)

pub fn roundTripInt (n: Int) -> Int =
    match fromSql (toSql n)
        Ok v  -> v
        Err _ -> 0 - 1

pub fn roundTripText (s: Text) -> Text =
    match fromSql (toSql s)
        Ok v  -> v
        Err _ -> "error"

pub fn roundTripBool (b: Bool) -> Bool =
    match fromSql (toSql b)
        Ok v  -> v
        Err _ -> false
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"sql-roundtrip-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn sql_type_roundtrip_survives_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping sql_type_roundtrip_survives_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-sql-roundtrip-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-sql-roundtrip-e2e-cache-")
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

    // All BEAM files (user module + stdlib modules including std.sql) end up in
    // the same directory; a single -pa flag covers all of them.
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

    // Drive all three round-trips in a single BEAM boot.
    let expr = format!(
        "io:format(\"int=~w~n\",[{module}:roundTripInt(42)]), \
         io:format(\"text=~s~n\",[{module}:roundTripText(<<\"hello\">>)]), \
         io:format(\"bool=~w~n\",[{module}:roundTripBool(true)]), \
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
        stdout.contains("int=42"),
        "expected `int=42` — toSql/fromSql Int round-trip failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("text=hello"),
        "expected `text=hello` — toSql/fromSql Text round-trip failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("bool=true"),
        "expected `bool=true` — toSql/fromSql Bool round-trip failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
