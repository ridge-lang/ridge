//! End-to-end check that a destructuring function parameter binds and runs on
//! the BEAM.
//!
//! A top-level parameter may carry an irrefutable pattern instead of a bare
//! name: `(Point { x, y }: Point)` unwraps a record in the binder, and
//! `((a, b): (Int, Int))` unwraps a tuple. Both lower to a synthetic parameter
//! plus a `match` that destructures the pattern around the body, so the bound
//! variables (`x`, `y`, `a`, `b`) are in scope and read at runtime.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.int as Int

pub type Point = { x: Int, y: Int }

fn addCoords (Point { x, y }: Point) -> Int = x + y

fn diff ((a, b): (Int, Int)) -> Int = a - b

pub fn sumPoint () -> Text = Int.toText (addCoords (Point { x = 3, y = 4 }))

pub fn diffed () -> Text = Int.toText (diff (10, 3))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"pattern-param-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn destructuring_params_bind_and_run() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping destructuring_params_bind_and_run");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-pattern-param-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-pattern-param-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    // A destructuring param must compile clean: the pattern is irrefutable, so
    // no T043 fires and the body sees the bound variables.
    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile, got diagnostics: {:?}",
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
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['sumPoint','diffed']), halt()."
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

    // The record param binds `x`/`y` and the body reads both fields.
    assert!(
        stdout.contains("sumPoint=7"),
        "expected `sumPoint=7`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The tuple param binds `a`/`b` positionally.
    assert!(
        stdout.contains("diffed=7"),
        "expected `diffed=7`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
