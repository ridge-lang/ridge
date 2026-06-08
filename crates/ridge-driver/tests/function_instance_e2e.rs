//! End-to-end value checks for typeclass instances over function types (L8 / P1).
//!
//! A class whose instance head is a function type (`instance Run (Int -> Int)`)
//! lets a **bare function** satisfy the class constraint. This exercises the new
//! per-arity `Fn/N` dispatch through the whole pipeline:
//! parse → collect (the instance registers under `(Run, Fn/1)`) → typecheck
//! (`dispatch_constraint`'s `Type::Fn` arm resolves the constraint) → lower (the
//! `$inst_Run_Fn1` static dict + call-site threading) → Core Erlang → run on the
//! BEAM → assert runtime values.
//!
//! Two call sites, mirroring `typeclass_dict_e2e`: a static one (`run` applied to
//! a bare lambda, the instance dict threaded directly) and a polymorphic-forward
//! one (a constrained consumer `useRun` forwarding its own dict parameter).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Source exercising a function-type instance through both dispatch paths.
///
/// - `class Run f` declares a single method `run` whose receiver is the class
///   parameter `f` — here a function type.
/// - `instance Run (Int -> Int)` provides the dictionary: `run g x = g x` simply
///   applies the captured function. It registers under `(Run, Fn/1)`.
/// - `main_static` calls `run` on a bare lambda directly — the constraint
///   resolves to a concrete `DictPlan::Static`.
/// - `useRun` is constrained (`where Run a`) and forwards its dict parameter;
///   `main_forward` pins `a` to a function type at the call site.
const SOURCE: &str = r#"
class Run f =
    run (self: f) (x: Int) -> Int

instance Run (Int -> Int) =
    run (g: Int -> Int) (x: Int) -> Int = g x

fn useRun (f: a) (n: Int) -> Int where Run a =
    run f n

pub fn main_static () -> Int =
    run (fn (x: Int) -> Int = x * 2) 21

pub fn main_forward () -> Int =
    useRun (fn (x: Int) -> Int = x + 1) 41
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"function-instance-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn function_instance_dispatch_computes_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping function_instance_dispatch_computes_correct_values"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-function-instance-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-function-instance-e2e-cache-")
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

    // Compilation must succeed with no fatal diagnostics.
    let fatal_count = artefacts
        .diagnostics
        .iter()
        .filter(|d| format!("{d:?}").contains("Error"))
        .count();
    assert_eq!(
        fatal_count, 0,
        "expected no fatal diagnostics; got: {:?}",
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

    // Drive both call sites in one BEAM boot; each prints `name=value`.
    let expr = format!(
        "F=fun(N)->io:format(\"~s=~w~n\",[N,{module}:N()])end, \
         lists:foreach(F,['main_static','main_forward']), halt()."
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

    // Static call site: `run (fn x -> x * 2) 21` resolves the `Fn/1` instance
    // directly; the dict is the instance literal, peephole folds the lookup.
    assert!(
        stdout.contains("main_static=42"),
        "expected `main_static=42`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Polymorphic-forward: `useRun (fn x -> x + 1) 41` forwards its own dict
    // parameter, pinning `a` to the function type at the call site.
    assert!(
        stdout.contains("main_forward=42"),
        "expected `main_forward=42`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
