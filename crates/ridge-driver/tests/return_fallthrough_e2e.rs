//! Execute the return paths newly accepted by static checking.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.actor (AskError)

const seed: Int =
    let f = fn () -> Int = return 7
    f ()

actor Counter =
    state n: Int = 0
    init () =
        fn initial () -> Int = return 3
        n <- initial ()
    on step () -> Int =
        n <- n + 1
        fn local () -> Int = return 7
        let f = fn () -> Int = return 11
        n <- n + local () + f ()
        n
    on current () -> Int = n

fn read (r: Result Int AskError) -> Int =
    match r
        Ok n -> n
        Err _ -> -1000

pub fn spawn time actorBoundary () -> Int =
    let h = spawn Counter
    let first = read (h ?> step ())
    first * 100 + read (h ?> current ())
pub fn constantBoundary () -> Int = seed

class Answer a =
    answer (self: a) -> Int
instance Answer Int =
    answer (x: Int) -> Int = return x

fn sole () -> Int = return 5
fn choose (b: Bool) -> Int =
    if b then return 7 else 11
fn matchIt (b: Bool) -> Int =
    match b
        true -> return 13
        false -> return 17

pub fn direct () -> Int = sole ()
pub fn dead () -> Int =
    return 19
    "unreachable"
pub fn branches () -> Int = choose true + choose false
pub fn matches () -> Int = matchIt true + matchIt false
pub fn closure () -> Int =
    let f = fn (x: Int) -> Int = return x
    f 23 + 1
pub fn inner () -> Int =
    fn f () -> Int = return 29
    f () + 2
pub fn method () -> Int = answer 31 + 3
pub fn tried () -> Int =
    try
        return 37
pub fn shortCircuit () -> Int =
    let b = false && (return 100)
    41
pub fn evaluatedOperand () -> Int =
    let x = (return 43)
    "unreachable"
"#;

#[test]
fn return_paths_execute_the_expected_value() {
    assert!(
        which::which("erl").is_ok() && which::which("erlc").is_ok(),
        "runtime gate requires Erlang"
    );
    let workspace = common::make_workspace("Returns", SOURCE);
    let artefacts =
        compile_workspace(CompileOptions::new(workspace.path).with_emit(EmitArtefacts::Beam))
            .expect("compile return paths");
    assert!(
        artefacts.diagnostics.is_empty(),
        "{:?}",
        artefacts.diagnostics
    );
    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("beam directory");
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|name| name.starts_with("ridge_") && name.ends_with("_Returns"))
        .expect("user module");
    let names = "['direct','dead','branches','matches','closure','inner','method','tried','shortCircuit','evaluatedOperand','actorBoundary','constantBoundary']";
    let eval =
        format!("lists:foreach(fun(N)->io:format(\"~p~n\",[{module}:N()])end,{names}),halt().");
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(beam_dir)
        .args(["-eval", &eval])
        .output()
        .expect("execute return paths");
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout, "5\n19\n18\n30\n24\n31\n34\n37\n41\n43\n2222\n7\n");
}
