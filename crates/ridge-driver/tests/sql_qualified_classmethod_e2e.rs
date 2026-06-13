//! End-to-end round-trip for `std.sql` class methods called **module-qualified**
//! (`Sql.toSql` / `Sql.fromSql`), proving qualified type-class method calls
//! dispatch to the right instance dictionary at runtime on the BEAM.
//!
//! A qualified class-method call resolves to a `StdlibSymbol` binding (the name
//! is in the module's export manifest) and typechecks via the last-segment
//! fallback, but lowering used to emit a plain stdlib symbol that missed the
//! bridge map and failed at codegen (E002). The fix routes a qualified callee
//! through the same dictionary dispatch as the bare form.
//!
//! Each round-trip's output depends on the dispatched instance (Int/Text/Bool),
//! so a wrong dispatch produces a wrong value and fails the assertion.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// The same Int/Text/Bool round-trips as `sql_type_roundtrip_e2e`, but every
/// `toSql`/`fromSql` is reached through the `Sql` module alias rather than an
/// unqualified import.
const SOURCE: &str = r#"
import std.sql as Sql

pub fn roundTripInt (n: Int) -> Int =
    match Sql.fromSql (Sql.toSql n)
        Ok v  -> v
        Err _ -> 0 - 1

pub fn roundTripText (s: Text) -> Text =
    match Sql.fromSql (Sql.toSql s)
        Ok v  -> v
        Err _ -> "error"

pub fn roundTripBool (b: Bool) -> Bool =
    match Sql.fromSql (Sql.toSql b)
        Ok v  -> v
        Err _ -> false
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"sql-qualified-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn qualified_sql_classmethod_dispatch_survives_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping qualified_sql_classmethod_dispatch_survives_beam"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-sql-qualified-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-sql-qualified-e2e-cache-")
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
        "expected `int=42` — qualified Sql.toSql/Sql.fromSql Int round-trip failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("text=hello"),
        "expected `text=hello` — qualified Sql.toSql/Sql.fromSql Text round-trip failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("bool=true"),
        "expected `bool=true` — qualified Sql.toSql/Sql.fromSql Bool round-trip failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
