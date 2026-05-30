//! End-to-end value checks for record `with` updates across shapes.
//!
//! Regression coverage for the closure-`with` miscompile: a `with` update on a
//! value whose concrete record type is not statically known (an unannotated
//! closure parameter) used to lower to `unit` and crash at run time. These tests
//! compile real Ridge to BEAM and assert the *computed values*, so a regression
//! that drops the update (or any touched/untouched field) is caught.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// Each `pub fn` returns an `Int` so the harness can assert exact values.
const SOURCE: &str = r"
type Cell = { v: Int, n: Int }

pub fn closure_single () -> Int =
    let g = fn acc -> acc with { v = acc.v + 5 }
    let r = g (Cell { v = 10, n = 0 })
    r.v

pub fn closure_multi () -> Int =
    let g = fn acc -> acc with { v = acc.v + 1, n = acc.n + 100 }
    let r = g (Cell { v = 10, n = 2 })
    r.v + r.n

pub fn closure_untouched_survives () -> Int =
    let g = fn acc -> acc with { v = 99 }
    let r = g (Cell { v = 0, n = 7 })
    r.n

pub fn shorthand_pulls_local () -> Int =
    let v = 42
    let r = Cell { v = 0, n = 7 } with { v }
    r.v + r.n

pub fn chained_update () -> Int =
    let r = Cell { v = 0, n = 0 } with { v = 1 } with { n = 2 }
    r.v + r.n
";

/// Build a single-member workspace whose entry module holds `SOURCE`.
fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"with-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn record_with_shapes_compute_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping record_with_shapes_compute_correct_values");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-with-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-with-e2e-cache-")
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

    // Drive every shape in one BEAM boot; each prints `name=value`.
    let expr = format!(
        "F=fun(N)->io:format(\"~s=~p~n\",[N,{module}:N()])end, \
         lists:foreach(F,['closure_single','closure_multi','closure_untouched_survives',\
         'shorthand_pulls_local','chained_update']), halt()."
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
        ("closure_single", 15),            // 10 + 5
        ("closure_multi", 113),            // (10+1) + (2+100)
        ("closure_untouched_survives", 7), // n preserved through the update
        ("shorthand_pulls_local", 49),     // local v=42, n=7 preserved
        ("chained_update", 3),             // v=1, n=2
    ] {
        let needle = format!("{name}={want}");
        assert!(
            stdout.contains(&needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
