//! End-to-end check for the monotonic clock (`std.time` `monotonic`/`elapsed`/`since`).
//!
//! A monotonic `Instant` measures elapsed time where a wall-clock `Timestamp` cannot:
//! the wall clock can jump backward when it is adjusted, but a monotonic reading only
//! moves forward, so a span measured from it is never negative. The exact readings
//! change on every run, so this pins the invariants rather than fixed values:
//! - `since a a` is exactly zero (no time between a reading and itself),
//! - `elapsed` since a fresh reading is never negative, and
//! - `since` two readings taken in order is never negative.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.time (monotonic, elapsed, since)

-- `since a a` is exactly zero: no time passes between a reading and itself. This is
-- deterministic, unlike the readings themselves.
pub fn time sinceZero () -> Text =
    let a = monotonic ()
    let d = since a a
    Int.toText d.ms

-- The span elapsed since a fresh reading is never negative — the monotonic clock only
-- moves forward, so `now - start` cannot be below zero.
pub fn time elapsedNonNeg () -> Text =
    let a = monotonic ()
    let d = elapsed a
    if d.ms >= 0 then "ok" else "negative"

-- The span between two readings taken in order is never negative, for the same reason.
pub fn time sinceNonNeg () -> Text =
    let a = monotonic ()
    let b = monotonic ()
    let d = since a b
    if d.ms >= 0 then "ok" else "negative"

-- Instant is a prelude type, so a caller can name it in an annotation without an
-- import, the same way `Timestamp` and `Duration` are named.
pub fn time annotated () -> Text =
    let a: Instant = monotonic ()
    let d = elapsed a
    if d.ms >= 0 then "ok" else "negative"
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"monotonic-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"time\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn monotonic_clock_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping monotonic_clock_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-monotonic-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-monotonic-e2e-cache-")
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
        "io:format(\"sinceZero=~s~n\",[{module}:sinceZero()]), \
         io:format(\"elapsedNonNeg=~s~n\",[{module}:elapsedNonNeg()]), \
         io:format(\"sinceNonNeg=~s~n\",[{module}:sinceNonNeg()]), \
         io:format(\"annotated=~s~n\",[{module}:annotated()]), \
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

    for (probe, why) in [
        (
            "sinceZero=0",
            "the span between a monotonic reading and itself is exactly zero",
        ),
        (
            "elapsedNonNeg=ok",
            "elapsed since a fresh reading is never negative (the monotonic clock only moves forward)",
        ),
        (
            "sinceNonNeg=ok",
            "the span between two readings taken in order is never negative",
        ),
        (
            "annotated=ok",
            "a caller can name the prelude type Instant in an annotation without an import",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
