//! Regression: a nullary stdlib function (`fn f ()`, zero params) called as `f ()`
//! from user code must compile to a 0-arity BEAM call and run.
//!
//! `sqlNull () -> SqlValue` is parsed as zero params and compiled to `'std.sql':sqlNull/0`.
//! A cross-module call `sqlNull ()` carries a single Unit arg in the IR that the codegen's
//! Unit-paren shim drops — but only when the callee's recorded arity is 0. The generated
//! `ffi_targets` arity was counting the empty `()` parameter list as one param group, so the
//! shim saw arity 1, kept the `()`, and emitted an arity-1 call that was `undef` against
//! `sqlNull/0`. This exercises the whole path end to end: `sqlValueSource (sqlNull ())` calls
//! the nullary factory and renders the result, which crashes at runtime if the `()` is not
//! dropped.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SRC: &str = r#"
import std.sql (sqlNull, sqlValueSource)

-- `sqlNull ()` is a 0-param stdlib factory; calling it from a user module used to emit an
-- arity-1 call that was `undef`. Rendering the result proves the call ran.
pub fn nullSrc () -> Text = sqlValueSource (sqlNull ())
"#;

fn write_workspace(root: &Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"nullary-call-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
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
        .find(|stem| stem.starts_with("ridge_module_"))
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
fn nullary_stdlib_fn_called_from_user_code_runs() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping nullary_stdlib_fn_called_from_user_code_runs");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-nullary-call-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-nullary-call-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), SRC);
    let (beam_dir, module) = compile(dir.path(), cache.path());

    // `sqlNull ()` runs and renders to its own factory-call source.
    let out = run_fun(&beam_dir, &module, "nullSrc");
    assert_eq!(out, "(sqlNull ())", "nullSrc rendered {out:?}");
}
