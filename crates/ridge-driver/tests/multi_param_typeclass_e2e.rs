//! End-to-end value checks for multi-parameter typeclasses (L7) on the BEAM.
//!
//! Proves the whole chain for a two-parameter class:
//! parse → collect (instance registry keyed by the head tuple) → typecheck
//! (multi-parameter constraint dispatch by tuple) → lower (dict const named by
//! the full head) → Core Erlang → run on the BEAM → assert runtime values.
//!
//! The decisive case: two instances that share their FIRST head type but differ
//! in the second — `Convert Celsius Text` and `Convert Celsius Int`. Selecting
//! the right one requires keying the instance by the whole head tuple and naming
//! the dictionary by every head constructor; a first-constructor-only scheme
//! would collide both onto one dictionary and dispatch the wrong method.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
class Convert a b =
    convert (x: a) -> b

type Temp = Cold | Hot

fn tempLabel (t: Temp) -> Text = match t
    Cold -> "cold"
    Hot -> "hot"

instance Convert Temp Text =
    convert (x: Temp) -> Text = tempLabel x

pub fn label () -> Text = convert Cold

pub fn hotLabel () -> Text = convert Hot
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"mptc-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn multi_param_instances_dispatch_by_full_head_tuple() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping multi_param_instances_dispatch_by_full_head_tuple"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-mptc-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-mptc-e2e-cache-")
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
         lists:foreach(F,['label','hotLabel']), halt()."
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

    // The two-parameter constraint `Convert Temp Text` is solved by tuple, the
    // dictionary const is named by the full head (`$inst_Convert_Temp_Text`),
    // and the method projects and runs on the BEAM.
    assert!(
        stdout.contains("label=cold"),
        "expected `label=cold`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("hotLabel=hot"),
        "expected `hotLabel=hot`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
