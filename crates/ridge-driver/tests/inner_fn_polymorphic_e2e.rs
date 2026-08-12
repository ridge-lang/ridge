//! An inner `fn` at run time, once it is polymorphic.
//!
//! Type-checking it as a top-level declaration is half the story: the lowering
//! has its own path for an inner `fn`, and two things that were impossible
//! before now reach it — a body used at two types, and a signature carrying a
//! `where` clause, which means a dictionary parameter where none was ever
//! passed.
//!
//! The constrained case is the one worth running. Two instances that return
//! different numbers make a dictionary that never arrives, or arrives wrong,
//! show up as a wrong integer rather than as a program that happens to run.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
class Weigh a =
    weigh (x: a) -> Int

instance Weigh Int =
    weigh (n: Int) -> Int = n

instance Weigh Text =
    weigh (t: Text) -> Int = 100

pub fn main_two_types () -> Text =
    fn keep (x: a) -> a = x
    let t: Text = keep "ok"
    let n: Int = keep 7
    $"${t}${n}"

pub fn main_constrained () -> Int =
    fn double (x: a) -> Int where Weigh a =
        weigh x + weigh x
    double 3 + double "hi"

pub fn main_recursive () -> Int =
    fn countDown (n: Int) -> Int =
        if n <= 0 then 0 else 1 + countDown (n - 1)
    countDown 4

pub fn main_constrained_recursive () -> Int =
    fn tally (xs: List a) -> Int where Weigh a =
        match xs
            [] -> 0
            x :: rest -> weigh x + tally rest
    tally [ 1, 2 ] + tally [ "a" ]

fn viaOuter (x: a) -> Int where Weigh a =
    fn twice (y: b) -> Int where Weigh b = weigh y * 2
    twice x + weigh x

pub fn main_nested_dicts () -> Int =
    viaOuter 4 + viaOuter "z"

pub fn main_shadowed () -> Int =
    fn scale (x: a) -> Int where Weigh a = weigh x
    fn nested (k: Int) -> Int =
        fn scale (x: a) -> Int where Weigh a = weigh x * 10
        scale k
    scale 5 + nested 5
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"inner-fn-poly-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn an_inner_fn_runs_at_two_types_and_carries_its_dictionary() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping an_inner_fn_runs_at_two_types_and_carries_its_dictionary"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-inner-fn-poly-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-inner-fn-poly-cache-")
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
        "F=fun(N)->io:format(\"~s=~p~n\",[N,{module}:N()])end, \
         lists:foreach(F,['main_two_types','main_constrained','main_recursive',\
         'main_constrained_recursive','main_nested_dicts','main_shadowed']), halt()."
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

    // One body, two types, one run.
    assert!(
        stdout.contains(r#"main_two_types=<<"ok7">>"#),
        "expected `main_two_types=<<\"ok7\">>`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // 3+3 then 100+100. A dictionary that never arrives crashes here; one that
    // arrives from the wrong instance returns 12.
    assert!(
        stdout.contains("main_constrained=206"),
        "expected `main_constrained=206`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Recursion through the new binding, which is now a declared scheme rather
    // than a monomorphic one.
    assert!(
        stdout.contains("main_recursive=4"),
        "expected `main_recursive=4`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // A helper that both promises a class and calls itself: the dictionary has
    // to be in scope inside its own body, not only after the declaration.
    // 1+2 then 100.
    assert!(
        stdout.contains("main_constrained_recursive=103"),
        "expected `main_constrained_recursive=103`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // A promising helper inside a promising function. The inner call is met
    // from the outer function's own incoming dictionary, so the two sets have
    // to coexist rather than one replacing the other.
    // (4*2 + 4) then (100*2 + 100).
    assert!(
        stdout.contains("main_nested_dicts=312"),
        "expected `main_nested_dicts=312`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Two helpers of one name, the inner one declared in a nested body. The
    // nested call must reach the nested helper; had the lookup taken the first
    // match rather than the innermost, this would read 10.
    assert!(
        stdout.contains("main_shadowed=55"),
        "expected `main_shadowed=55`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
