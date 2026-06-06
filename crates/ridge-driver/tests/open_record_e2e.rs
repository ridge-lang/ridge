//! End-to-end value checks for open (row-polymorphic) records on the BEAM.
//!
//! Where `inline_record_e2e` covers closed inline records, this file proves the
//! row-polymorphism path: a function with an open record parameter
//! `{ x: Int | a }` compiles to a `maps:get`/`maps:merge` over a real BEAM map
//! and computes the right value even when the argument carries extra keys, and
//! the same function applies at differently-shaped records within one program.
//!
//! Each `pub fn` returns an `Int` so the harness can assert exact values.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

const SOURCE: &str = r"
-- An open record parameter: only `x` is fixed; the tail `a` absorbs any extras.
fn getX (r: { x: Int | a }) -> Int = r.x

-- Case 1: projection through an open row.
-- The argument carries an extra `y` key the parameter never names; reading `.x`
-- must still pick the right value out of the BEAM map `#{x => 11, y => 22}`.
pub fn open_project () -> Int =
    let rec = { x = 11, y = 22 }
    getX rec

-- Case 2: the same function applied at two different shapes in one program.
-- Quantifying the row variable makes each call independent — `{x, y}` and
-- `{x, z}` both type and both run.
pub fn open_multishape () -> Int =
    let withY = { x = 5, y = 9 }
    let withZ = { x = 3, z = 4 }
    getX withY + getX withZ

-- An open record that is updated and returned. The row threads from the
-- parameter to the result, so the caller still sees the extra field.
fn bumpX (r: { x: Int | a }) -> { x: Int | a } =
    r with { x = r.x + 100 }

-- Case 3: with-update over an open base preserves the extra key at runtime.
-- `bumpX` updates `x` via `maps:merge`; the untouched `y` survives the merge,
-- so the caller reads r2.x = 110 and r2.y = 5.
pub fn open_preserve () -> Int =
    let rec = { x = 10, y = 5 }
    let r2 = bumpX rec
    r2.x + r2.y
";

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"open-record-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn open_record_types_compute_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping open_record_types_compute_correct_values");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-open-rec-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-open-rec-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

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
        "F=fun(N)->io:format(\"~s=~p~n\",[N,{module}:N()])end, \
         lists:foreach(F,['open_project','open_multishape','open_preserve']), halt()."
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

    for (name, want) in [
        ("open_project", 11),   // .x of { x = 11, y = 22 }
        ("open_multishape", 8), // 5 + 3 — one fn, two shapes
        ("open_preserve", 115), // 110 + 5 — y survives the with-update merge
    ] {
        let needle = format!("{name}={want}");
        assert!(
            stdout.contains(&needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
