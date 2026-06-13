//! End-to-end proof that a **workspace** type-class method declared in one
//! module dispatches correctly when called **module-qualified** from another.
//!
//! `Lib` declares `class Tag` with `Int` and `Bool` instances that return
//! distinct strings; `Main` reaches them through the `Lib` alias (`L.tag n`).
//! This exercises the whole pipeline — the resolver accepting `L.tag` as a class
//! method (instead of R012), the type-checker dispatching on the receiver, and
//! lowering threading the cross-module instance dictionary — and the
//! per-instance output makes a wrong dispatch observably fail.
//!
//! It is also the first cross-module user-class dispatch run on the BEAM: the
//! existing typeclass e2e tests keep the class, instance, and use in one module.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

const LIB_SOURCE: &str = r#"
pub class Tag a =
    tag (x: a) -> Text

instance Tag Int =
    tag (x: Int) -> Text = "int-tag"

instance Tag Bool =
    tag (x: Bool) -> Text = "bool-tag"
"#;

const MAIN_SOURCE: &str = r"
import app.Lib as L

pub fn tagInt (n: Int) -> Text =
    L.tag n

pub fn tagBool (b: Bool) -> Text =
    L.tag b
";

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"qualified-ws-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), MAIN_SOURCE).expect("write Main");
    std::fs::write(app_src.join("Lib.ridge"), LIB_SOURCE).expect("write Lib");
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn qualified_workspace_classmethod_dispatch_survives_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping qualified_workspace_classmethod_dispatch_survives_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-qualified-ws-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-qualified-ws-e2e-cache-")
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
    // The entry module (Main) is the one that exports tagInt/tagBool. Both user
    // modules share the `ridge_module_` prefix, so pick the one that defines the
    // exported functions by probing each candidate from erl below.
    let modules: Vec<String> = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .filter(|stem| stem.starts_with("ridge_module_"))
        .map(ToOwned::to_owned)
        .collect();
    assert!(!modules.is_empty(), "expected at least one user module");

    // Try each user module; the entry exports tagInt/1 and tagBool/1.
    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut matched = false;
    for module in &modules {
        let expr = format!(
            "io:format(\"int=~s~n\",[{module}:tagInt(7)]), \
             io:format(\"bool=~s~n\",[{module}:tagBool(true)]), \
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
        if stdout.contains("int=") {
            combined_stdout = stdout.into_owned();
            combined_stderr = stderr.into_owned();
            matched = true;
            break;
        }
        combined_stderr.push_str(&stderr);
    }

    assert!(
        matched,
        "no user module exported tagInt/tagBool\nmodules: {modules:?}\nstderr:\n{combined_stderr}"
    );
    assert!(
        combined_stdout.contains("int=int-tag"),
        "expected `int=int-tag` — qualified workspace Tag Int dispatch failed\nstdout:\n{combined_stdout}\nstderr:\n{combined_stderr}"
    );
    assert!(
        combined_stdout.contains("bool=bool-tag"),
        "expected `bool=bool-tag` — qualified workspace Tag Bool dispatch failed\nstdout:\n{combined_stdout}\nstderr:\n{combined_stderr}"
    );
}
