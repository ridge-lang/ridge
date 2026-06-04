//! End-to-end check that `std.crypto.constantTimeEq` bridges to BEAM.
//!
//! `crypto.ridge` declares `@ffi("crypto", "hash_equals", 2)`, but the module
//! was missing from `STDLIB_MODULES` in `crates/ridge-stdlib/build.rs`, so its
//! target never made it into the generated `ffi_targets` table. A call to
//! `constantTimeEq` parsed and typechecked but failed codegen with
//! `E002 StdlibBridgeMissing`. This test compiles a call site through the full
//! pipeline and runs it on the BEAM, so a regression in the bridge surfaces as
//! either a compile diagnostic or a wrong runtime answer.
//!
//! `crypto:hash_equals/2` requires both inputs to be the same length, so the
//! mismatch case compares two equal-length values that differ in one byte.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// Two equal-length secrets: identical pair returns `true`, one-byte-different
/// pair returns `false`. Both comparisons feed `crypto:hash_equals/2`.
const SOURCE: &str = r#"
import std.crypto as Crypto

pub fn equal_secrets () -> Bool =
    Crypto.constantTimeEq "s3cr3t-token" "s3cr3t-token"

pub fn different_secrets () -> Bool =
    Crypto.constantTimeEq "s3cr3t-token" "s3cr3t-tXken"
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace_source(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"crypto-bridge-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

fn compile_and_find_module() -> Option<(
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    String,
)> {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        return None;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-crypto-bridge-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-crypto-bridge-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace_source(dir.path(), SOURCE);

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

    Some((dir, cache, beam_dir, module))
}

fn run_erl(beam_dir: &std::path::Path, expr: &str) -> (String, String) {
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(expr)
        .output()
        .expect("run erl");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn constant_time_eq_bridges_to_beam() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module() else {
        eprintln!("erl/erlc not on PATH — skipping constant_time_eq_bridges_to_beam");
        return;
    };

    let expr = format!(
        "io:format(\"equal=~w~n\",[{module}:equal_secrets()]), \
         io:format(\"different=~w~n\",[{module}:different_secrets()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("equal=true"),
        "expected `equal=true`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("different=false"),
        "expected `different=false`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
