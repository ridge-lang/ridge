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

/// The other half of the same feature: a function *constrained* by the
/// two-parameter class rather than calling it at a concrete type. The caller's
/// dictionary is forwarded, so the parameter carrying it has to be named and
/// found by the whole constraint. Lowering used to abort here outright — the
/// name and the lookup were both derived from the constraint's first variable,
/// and asking a two-variable constraint for its only one asserts.
const FORWARDED: &str = r#"
class Pairable a b | a -> b =
    wrap (x: a) -> b

instance Pairable Int Text =
    wrap (n: Int) -> Text = "from Int"

instance Pairable Bool Text =
    wrap (b: Bool) -> Text = "from Bool"

fn go (x: a) -> b where Pairable a b = wrap x

pub fn fromInt () -> Text = go 1

pub fn fromBool () -> Text = go true
"#;

fn write_workspace(root: &std::path::Path, source: &str) {
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
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
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
    write_workspace(dir.path(), SOURCE);

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

#[test]
fn a_where_constrained_fn_forwards_a_multi_param_dictionary() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping a_where_constrained_fn_forwards_a_multi_param_dictionary"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-mptc-fwd-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-mptc-fwd-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), FORWARDED);

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

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['fromInt','fromBool']), halt()."
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

    // Two instances that share their second head type, reached through one
    // constrained function: each call picks the instance its own argument
    // pins, and that choice travels in the forwarded dictionary. Naming or
    // finding that dictionary from the constraint's first variable alone is
    // what used to abort lowering here.
    assert!(
        stdout.contains("fromInt=from Int"),
        "expected `fromInt=from Int`
stdout:
{stdout}
stderr:
{stderr}"
    );
    assert!(
        stdout.contains("fromBool=from Bool"),
        "expected `fromBool=from Bool`
stdout:
{stdout}
stderr:
{stderr}"
    );
}
