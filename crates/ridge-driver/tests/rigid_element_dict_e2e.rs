//! The element dictionary a parametric instance gets from a signature variable.
//!
//! `twice` promises `Weigh a` and hands its own `a` to `Weigh (Pair a)`, whose
//! context needs a `Weigh a` of its own. That sub-dictionary is not looked up —
//! it is forwarded from the caller, under the name the signature variable was
//! minted from. Get the name wrong and the compile is rejected as ambiguous;
//! get it *plausible* but wrong and the program builds and calls the wrong
//! instance, which is the failure that does not announce itself.
//!
//! So the assertions are values, not exit codes: two instances that return
//! different numbers, and a third case where the element is itself a
//! parametric instance, so the dictionary passed in is a dict-of-dicts.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// `Weigh Int` and `Weigh Text` disagree on purpose: 3 doubles to 6, "hi"
/// doubles to 200. A forwarded dictionary that resolves to the wrong instance
/// still returns an `Int`, so only the number tells them apart.
const SOURCE: &str = r#"
class Weigh a =
    weigh (x: a) -> Int

type Pair a = MkPair a a | NoPair

instance Weigh Int =
    weigh (n: Int) -> Int = n

instance Weigh Text =
    weigh (t: Text) -> Int = 100

instance Weigh (Pair a) where Weigh a =
    weigh (p: Pair a) -> Int =
        match p
            MkPair x y -> weigh x + weigh y
            NoPair -> 0

fn twice (x: a) -> Int where Weigh a =
    weigh (MkPair x x)

pub fn main_int () -> Int =
    twice 3

pub fn main_text () -> Int =
    twice "hi"

pub fn main_nested () -> Int =
    twice (MkPair 1 2)

pub fn main_direct () -> Int =
    weigh (MkPair 4 5)
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"rigid-element-dict-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn a_forwarded_element_dictionary_is_the_one_the_caller_promised() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping a_forwarded_element_dictionary_is_the_one_the_caller_promised"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-rigid-element-dict-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-rigid-element-dict-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    // The first failure this guards is a rejection: `T030` on a signature
    // variable that is not ambiguous at all.
    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile; got {:?}",
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

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~w~n\",[N,{module}:N()])end, \
         lists:foreach(F,['main_int','main_text','main_nested','main_direct']), halt()."
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

    // 3 + 3. The dictionary the caller promised is `Weigh Int`.
    assert!(
        stdout.contains("main_int=6"),
        "expected `main_int=6`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // 100 + 100. Same call, different promise — a dictionary chosen from the
    // shape of the code rather than from the caller reports 6 here.
    assert!(
        stdout.contains("main_text=200"),
        "expected `main_text=200`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // The element is itself a `Pair`, so what travels in is a dict-of-dicts:
    // `Weigh (Pair Int)` built from `Weigh Int`. (1+2) + (1+2).
    assert!(
        stdout.contains("main_nested=6"),
        "expected `main_nested=6`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // No signature variable anywhere — the path that already worked, so a fix
    // that reached too far shows up here. 4 + 5.
    assert!(
        stdout.contains("main_direct=9"),
        "expected `main_direct=9`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
