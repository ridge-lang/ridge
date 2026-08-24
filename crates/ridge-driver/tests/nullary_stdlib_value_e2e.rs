//! Regression: a nullary stdlib function passed as a *value* must be referenced,
//! and a stdlib value constant used as a value must still be evaluated.
//!
//! `std.time.epoch` is `() -> Timestamp` and `std.map.empty` is `Map k v`. Both
//! take nothing, so both reach the BEAM as arity 0 — and codegen used to decide
//! between "reference it" and "evaluate it" from exactly that arity. It chose
//! "evaluate", so `clockText Time.epoch` handed `clockText` a `Timestamp` and
//! the runtime reported `badfun`; the specification's own dependency-injection
//! example is that shape. Deciding the other way instead breaks `Map.empty`,
//! which is why the two live in one file: a fix for either one that is wrong
//! about the other fails here.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SRC: &str = r"
import std.map as Map
import std.time as Time

-- The injection shape: the clock is named, not called, and the callee calls it.
fn clockText (clock: fn () -> Timestamp) -> Text = Time.toIso (clock ())

-- `Time.epoch` is a function of no arguments. Naming it is a reference.
pub fn injectedClock () -> Text = clockText Time.epoch

-- `Map.empty` is a value. Naming it is that value, and `Map.length` of a fun
-- reference is `badmap`, not 0.
pub fn constantIsAValue () -> Text = Int.toText (Map.length Map.empty)
";

fn write_workspace(root: &Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"nullary-value-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

fn compile(dir: &Path, cache: &Path) -> (PathBuf, String) {
    let artefacts = compile_workspace(
        CompileOptions::new(dir.to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.to_path_buf()),
    )
    .expect("compile to BEAM");
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
        .find(|stem| {
            stem.starts_with("ridge_")
                && !matches!(
                    *stem,
                    "ridge_rt"
                        | "ridge_main_runner"
                        | "ridge_test_runner"
                        | "ridge_pg"
                        | "ridge_sup"
                        | "ridge_sqlite"
                        | "ridge_bench_runner"
                )
        })
        .expect("a user module")
        .to_owned();
    (beam_dir, module)
}

fn run_fun(beam_dir: &Path, module: &str, fun: &str) -> String {
    let expr = format!("io:format(\"~s\", [{module}:{fun}()]), halt().");
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.is_empty(),
        "`{fun}` produced no output; stderr:\n{stderr}"
    );
    stdout
}

#[test]
fn a_nullary_stdlib_fn_passed_as_a_value_is_referenced_not_called() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping \
             a_nullary_stdlib_fn_passed_as_a_value_is_referenced_not_called"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-nullary-value-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-nullary-value-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), SRC);
    let (beam_dir, module) = compile(dir.path(), cache.path());

    // The injected clock is applied by the callee, so the Unix epoch comes back
    // rendered. Evaluating `Time.epoch` at the reference instead would pass a
    // `Timestamp` where a function was expected and crash with `badfun`.
    let injected = run_fun(&beam_dir, &module, "injectedClock");
    assert!(
        injected.starts_with("1970-01-01T00:00:00"),
        "injectedClock rendered {injected:?}; a rendered epoch means the callee called it"
    );

    // The control, and the reason the fix cannot simply reference everything of
    // arity 0: a declared value has to arrive as the value.
    let constant = run_fun(&beam_dir, &module, "constantIsAValue");
    assert_eq!(
        constant, "0",
        "constantIsAValue rendered {constant:?} — a fun reference in value \
         position would have failed with badmap"
    );
}
