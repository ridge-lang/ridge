//! Ask failures are values, and propagation preserves handler payloads.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.actor as Actor
import std.actor (AskError, Timeout)
import std.time as Time

actor Worker =
    on value () -> Int = 7
    on echo (n: Int) -> Int = n
    on nested () -> Result Int Text = Err "handler"
    on time slow () -> Int =
        Time.sleep 30
        8
    on disappear () -> Int = 1 / 0

actor Count =
    state n: Int = 0
    on bump () -> Int =
        n <- n + 1
        n
    on value () -> Int = n

type RequestError = | RequestFailed AskError

instance ErrorFrom AskError RequestError =
    errorFrom (e: AskError) -> RequestError = RequestFailed e

fn time choose (h: Handle Worker) (c: Handle Count) -> Result (Handle Worker) AskError =
    let _ = c ?> bump () ?
    Ok h

fn time deadline (c: Handle Count) -> Result Int AskError =
    let _ = c ?> bump () ?
    Ok 1000

fn rejectDeadline () -> Result Int AskError = Err Timeout

pub fn spawn time operandError () -> Result Int AskError =
    let h = spawn Worker
    h ?> value () timeout (rejectDeadline () ?)

pub fn spawn time tryOperandError () -> Result Int AskError =
    let h = spawn Worker
    try
        let n = h ?> value () timeout (rejectDeadline () ?) ?
        n

pub fn spawn time success () -> Result Int AskError =
    let h = spawn Worker
    h ?> value ()

pub fn spawn time propagated () -> Result Int AskError =
    let h = spawn Worker
    let n = h ?> value () timeout 1000 ?
    Ok (n + 1)

pub fn spawn time nested () -> Result (Result Int Text) AskError =
    let h = spawn Worker
    h ?> nested () timeout never

pub fn spawn time timedOut () -> Result Int AskError =
    let h = spawn Worker
    h ?> slow () timeout 0

pub fn spawn time dead () -> Result Int AskError =
    let h = spawn Worker
    Actor.stop h
    h ?> value () timeout never

pub fn spawn time interrupted () -> Result Int AskError =
    let h = spawn Worker
    h ?> disappear ()

pub fn spawn time converted () -> Result Int RequestError =
    let h = spawn Worker
    Actor.stop h
    let n = h ?> value () ?
    Ok n

pub fn spawn time scoped () -> Result Int AskError =
    let h = spawn Worker
    try
        let n = h ?> value () ?
        n + 2

pub fn spawn time once () -> Result Int AskError =
    let h = spawn Worker
    let c = spawn Count
    let _ = (choose h c ?) ?> echo (c ?> bump () ?) timeout (deadline c ?) ?
    c ?> value ()
"#;

#[test]
fn ask_returns_results_and_propagates_only_transport_errors() {
    which::which("erl").expect("erl required for BEAM runtime test");
    which::which("erlc").expect("erlc required for BEAM runtime test");
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname = \"asks\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), SOURCE).unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile recoverable ask fixture");
    let user = artifacts
        .beam_files
        .iter()
        .find(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with("_Main"))
        })
        .unwrap_or_else(|| panic!("missing user module: {:?}", artifacts.diagnostics));
    let module = user.file_stem().unwrap().to_str().unwrap();
    let expression = format!(
        "logger:set_primary_config(level,none), Values=[{module}:success(),{module}:propagated(),{module}:nested(),{module}:timedOut(),{module}:dead(),{module}:interrupted(),{module}:converted(),{module}:scoped(),{module}:once(),{module}:operandError(),{module}:tryOperandError()], Expected=[{{ok,7}},{{ok,8}},{{ok,{{error,<<\"handler\">>}}}},{{error,'Timeout'}},{{error,'Noproc'}},{{error,'Noproc'}},{{error,{{'RequestFailed','Noproc'}}}},{{ok,9}},{{ok,3}},{{error,'Timeout'}},{{error,'Timeout'}}], case Values =:= Expected of true -> io:format(\"ask-values-ok~n\"),halt(0); false -> io:format(\"~p~n\",[Values]),halt(1) end."
    );
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(user.parent().unwrap())
        .args(["-eval", &expression])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        "ask-values-ok\n"
    );
}

#[test]
fn main_renders_direct_propagated_and_explicit_error_returns() {
    which::which("erl").expect("erl required for BEAM runtime test");
    which::which("erlc").expect("erlc required for BEAM runtime test");
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"main-errors\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    for (name, body) in [
        ("Direct", "Err Failure"),
        ("Propagated", "let _ = fail () ?\n    Ok ()"),
        ("Explicit", "return (Err Failure)"),
        (
            "Guarded",
            "guard false else return (Err Failure)\n    Ok ()",
        ),
        ("Success", "let _ = Ok 3 ?\n    Ok ()"),
        ("Crash", "let n = 1 / 0\n    let _ = fail () ?\n    Ok ()"),
    ] {
        let body = body.replace("Failure", &format!("{name}Failure"));
        let source = format!("type {name}Problem = {name}Failure | {name}Other\ninstance ToText {name}Problem =\n    toText (e: {name}Problem) -> Text = \"reported failure\"\nfn fail () -> Result Unit {name}Problem = Err {name}Failure\npub fn main () -> Result Unit {name}Problem =\n    {body}\n");
        std::fs::write(dir.path().join(format!("app/src/{name}.ridge")), source).unwrap();
    }
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    for name in [
        "Direct",
        "Propagated",
        "Explicit",
        "Guarded",
        "Success",
        "Crash",
    ] {
        let suffix = format!("_{name}");
        let beam = artifacts
            .beam_files
            .iter()
            .find(|p| {
                p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.ends_with(&suffix))
            })
            .unwrap();
        let module = beam.file_stem().unwrap().to_str().unwrap();
        let output = std::process::Command::new("erl")
            .args(["-noshell", "-pa"])
            .arg(beam.parent().unwrap())
            .args(["-s", "ridge_main_runner", "run", module, "main"])
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr)
            .unwrap()
            .replace("\r\n", "\n");
        if name == "Success" {
            assert!(output.status.success(), "{stderr}");
            assert!(stderr.is_empty(), "{stderr}");
        } else if name == "Crash" {
            assert_eq!(output.status.code(), Some(1));
            assert!(stderr.contains("divided by zero"), "{stderr}");
        } else {
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(stderr, "reported failure\n", "{name}");
        }
    }
}
