//! A return contributes no fallthrough value, but its operand still has a type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::make_workspace;
use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// Exercise the compiler's ordinary workspace path, including lowering.
fn codes(source: &str) -> Vec<String> {
    let workspace = make_workspace("Returns", source);
    compile_workspace(CompileOptions::new(workspace.path).with_emit(EmitArtefacts::Core))
        .expect("compile workspace")
        .diagnostics
        .iter()
        .map(|d| d.code.to_owned())
        .collect()
}

#[test]
fn valid_returns_do_not_require_a_fallthrough_value() {
    let cases = [
        ("sole Int", "pub fn answer () -> Int =\n    return 5\n"),
        ("sole Text", "pub fn answer () -> Text =\n    return \"ok\"\n"),
        ("sole Unit", "pub fn answer () -> Unit =\n    return\n"),
        ("generic", "pub fn identity (x: a) -> a =\n    return x\n"),
        ("dead tail", "pub fn answer () -> Int =\n    return 5\n    \"unreachable\"\n"),
        ("both if branches", "pub fn answer (b: Bool) -> Int =\n    if b then return 5 else return 6\n"),
        ("return then value", "pub fn answer (b: Bool) -> Int =\n    if b then return 5 else 6\n"),
        ("value then return", "pub fn answer (b: Bool) -> Int =\n    if b then 5 else return 6\n"),
        ("early conditional", "pub fn answer (b: Bool) -> Int =\n    if b then return 5\n    6\n"),
        ("match returns", "pub fn answer (b: Bool) -> Int =\n    match b\n        true -> return 5\n        false -> return 6\n"),
        ("match mixed", "pub fn answer (b: Bool) -> Int =\n    match b\n        true -> return 5\n        false -> 6\n"),
        ("guard exits before arm body", "pub fn answer (n: Int) -> Int =\n    match n\n        x when (return 5) -> \"unreachable\"\n        _ -> 6\n"),
        ("lambda", "pub fn answer () -> Int =\n    let f = fn (x: Int) -> Int = return x\n    f 5\n"),
        ("inferred lambda", "pub fn answer () -> Int =\n    let f = fn x -> return x\n    f 5\n"),
        ("inner function", "pub fn answer () -> Int =\n    fn inner () -> Text = return \"ok\"\n    5\n"),
        ("guard", "pub fn answer (b: Bool) -> Int =\n    guard b else return 5\n    6\n"),
        ("strict operand", "pub fn answer () -> Int =\n    let x = (return 5)\n    \"unreachable\"\n"),
        ("short circuit", "pub fn answer (b: Bool) -> Int =\n    let x = b && (return 5)\n    6\n"),
        ("explicit return in try", "pub fn answer () -> Int =\n    try\n        return 5\n"),
        ("instance method", "class Answer a =\n    answer (self: a) -> Int\n\ninstance Answer Int =\n    answer (x: Int) -> Int = return x\n\npub fn use () -> Int = answer 5\n"),
    ];
    let failures: Vec<_> = cases
        .iter()
        .filter_map(|(name, source)| {
            let actual = codes(source);
            (!actual.is_empty()).then(|| format!("{name}: {actual:?}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn invalid_return_operands_and_reachable_tails_still_fail() {
    let cases = [
        ("wrong operand", "pub fn answer () -> Int = return \"bad\"\n"),
        ("dead tail cannot repair operand", "pub fn answer () -> Int =\n    return \"bad\"\n    5\n"),
        ("wrong reachable branch", "pub fn answer (b: Bool) -> Int =\n    if b then return 5 else \"bad\"\n"),
        ("missing else", "pub fn answer (b: Bool) -> Int =\n    if b then return 5\n"),
        ("lambda does not exit outer", "pub fn answer () -> Int =\n    let f = fn (x: Int) -> Int = return x\n    \"bad\"\n"),
        ("inner function does not exit outer", "pub fn answer () -> Int =\n    fn inner () -> Int = return 5\n    \"bad\"\n"),
        ("wrong lambda operand", "pub fn answer () -> Int =\n    let f = fn (x: Int) -> Text = return x\n    5\n"),
        ("wrong guard continuation", "pub fn answer (b: Bool) -> Int =\n    guard b else return 5\n    \"bad\"\n"),
        ("short circuit can continue", "pub fn answer (b: Bool) -> Int =\n    let x = b && (return 5)\n    \"bad\"\n"),
        ("dead code still checked", "pub fn answer () -> Int =\n    return 5\n    let x: Int = \"bad\"\n    0\n"),
        ("wrong match branch", "pub fn answer (b: Bool) -> Int =\n    match b\n        true -> return 5\n        false -> \"bad\"\n"),
    ];
    for (name, source) in cases {
        let actual = codes(source);
        assert!(
            actual.iter().any(|code| code == "T001"),
            "{name}: {actual:?}"
        );
        assert!(
            !actual.iter().any(|code| code.ends_with("999")),
            "{name}: {actual:?}"
        );
    }
}

#[test]
fn an_early_return_does_not_bypass_capability_checks() {
    let actual = codes("import std.io as Io\n\npub fn answer () -> Int =\n    return 5\n    Io.println \"unreachable\"\n    0\n");
    assert!(actual.iter().any(|code| code == "T014"), "{actual:?}");
}
