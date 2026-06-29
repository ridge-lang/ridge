//! End-to-end guard for the param-shadows-module-fn miscompile, on the BEAM.
//!
//! A function parameter (or `let`/`var`) may share its name with a same-module
//! top-level function. The parameter must win: a use of the name inside the
//! function body is the bound value, not the function. Before the fix this
//! miscompiled — the Erlang codegen turned the shadowing reference into a curried
//! `#Fun<...>` value because the name was present in the module's fn-arity table.
//! The bug typechecked clean and only bit at runtime, so this oracle runs the
//! compiled module and reads the value back.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
-- A top-level fn whose name a later parameter deliberately shadows.
fn label (x: Text) -> Text = x

-- `label` here is the PARAMETER, not the fn above. The body must return the
-- bound argument, not a reference to `label/1`.
fn tag (label: Text) -> Text = label

-- A `let` that shadows the same top-level fn, exercising the non-param path.
fn viaLet () -> Text =
    let label = "let"
    label

pub fn shadowParam () -> Text = tag "param"
pub fn shadowLet () -> Text = viaLet ()
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"param-shadow-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn param_shadowing_top_level_fn_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping param_shadowing_top_level_fn_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-param-shadow-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-param-shadow-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
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

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['shadowParam','shadowLet']), halt()."
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

    let want = |needle: &str| {
        assert!(
            stdout.contains(needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    };

    want("shadowParam=param");
    want("shadowLet=let");
}
