//! End-to-end check that a quoted predicate round-trips to a reified tree on
//! the BEAM.
//!
//! Proves the whole chain: a native lambda `fn u -> u.age >= 18` passed where a
//! `Quote (User -> Bool)` is expected is captured (not lowered to a closure),
//! reified into a `QExpr` tree via `std.query`'s smart constructors, and the
//! tree survives to runtime where `Query.debugShow` walks it back into a
//! SQL-shaped string.
//!
//! The decisive details: `u.age` reifies to the column's *SQL* name (so a
//! camelCase field like `signupYear` shows as `signup_year`), and a boolean
//! column is usable directly as a predicate (`... && u.active`).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.query (debugShow)

pub type User = { id: Int, age: Int, active: Bool, signupYear: Int } deriving (Table)

fn showUserPred (q: Quote (User -> Bool)) -> Text = debugShow q

pub fn adultPred () -> Text = showUserPred (fn u -> u.age >= 18)

pub fn activeAdultPred () -> Text = showUserPred (fn u -> u.age >= 18 && u.active)

pub fn recentSignup () -> Text = showUserPred (fn u -> u.signupYear >= 2020)
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"quote-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn quoted_predicate_round_trips_to_a_reified_tree() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping quoted_predicate_round_trips_to_a_reified_tree");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-quote-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-quote-e2e-cache-")
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
         lists:foreach(F,['adultPred','activeAdultPred','recentSignup']), halt()."
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

    // The native lambda was captured and reified: the column reads back by its
    // SQL name and the literal as a `?` placeholder.
    assert!(
        stdout.contains("adultPred=(age >= ?)"),
        "expected `adultPred=(age >= ?)`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A boolean column is a predicate on its own; `&&` nests two predicates.
    assert!(
        stdout.contains("activeAdultPred=((age >= ?) AND active)"),
        "expected `activeAdultPred=((age >= ?) AND active)`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // A camelCase field reifies to its snake_case SQL column name.
    assert!(
        stdout.contains("recentSignup=(signup_year >= ?)"),
        "expected `recentSignup=(signup_year >= ?)`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
