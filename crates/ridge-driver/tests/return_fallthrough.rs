//! A return contributes no fallthrough value, but its operand still has a type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::make_workspace;
use ridge_driver::{
    check_workspace, compile_workspace, CheckOptions, CompileOptions, EmitArtefacts,
};

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

#[test]
fn returns_without_a_function_target_are_rejected_before_emission() {
    let cases = [
        ("handler", "actor Counter =\n    on value () -> Unit = return ()\n"),
        ("handler with tail", "actor Counter =\n    on value () -> Int =\n        return \"wrong\"\n        7\n"),
        ("branch", "actor Counter =\n    on value () -> Unit =\n        if true then return () else ()\n"),
        ("guard", "actor Counter =\n    on value () -> Int =\n        guard true else return 7\n        8\n"),
        ("try", "actor Counter =\n    on value () -> Result Unit Text =\n        try\n            return ()\n"),
        ("init", "actor Counter =\n    state n: Int = 0\n    init () =\n        return ()\n    on value () -> Int = n\n"),
        ("terminate", "import std.actor (ExitReason)\nactor Counter =\n    on value () -> Unit = ()\n    terminate (reason: ExitReason) =\n        return ()\n"),
        ("onDown", "import std.actor (ExitReason)\nactor Counter =\n    on value () -> Unit = ()\n    onDown (m: Monitor) (reason: ExitReason) =\n        return ()\n"),
        ("state default", "actor Counter =\n    state n: Unit = return 7\n    on value () -> Unit = n\n"),
        ("constant", "const answer: Unit = return 7\npub fn value () -> Unit = answer\n"),
        ("restored after lambda", "actor Counter =\n    on value () -> Unit =\n        let f = fn () -> Int = return 7\n        return ()\n"),
        ("restored after local fn", "actor Counter =\n    on value () -> Unit =\n        fn f () -> Int = return 7\n        return ()\n"),
    ];
    for (name, source) in cases {
        let workspace = make_workspace("Returns", source);
        let checked = check_workspace(CheckOptions::new(workspace.path.clone())).unwrap();
        let compiled =
            compile_workspace(CompileOptions::new(workspace.path).with_emit(EmitArtefacts::Core))
                .unwrap();
        for diagnostics in [&checked.diagnostics, &compiled.diagnostics] {
            let errors: Vec<_> = diagnostics.iter().filter(|d| d.code == "T062").collect();
            assert_eq!(errors.len(), 1, "{name}: {diagnostics:?}");
            assert!(errors[0]
                .primary_message
                .contains("enclosing function or lambda"));
            assert!(
                !diagnostics.iter().any(|d| d.code.ends_with("999")),
                "{name}: {diagnostics:?}"
            );
        }
        assert!(
            compiled.core_files.is_empty(),
            "{name}: emitted invalid source"
        );
        assert!(
            compiled.beam_files.is_empty(),
            "{name}: emitted invalid source"
        );
    }
}

#[test]
fn nested_function_targets_in_actor_and_constant_contexts_remain_valid() {
    for source in [
        "actor Counter =\n    on value () -> Int =\n        let f = fn () -> Int = return 7\n        f ()\n",
        "actor Counter =\n    on value () -> Int =\n        fn f () -> Int = return 7\n        f ()\n",
        "actor Counter =\n    state n: Int = 0\n    init () =\n        fn f () -> Int = return 7\n        n <- f ()\n    on value () -> Int = n\n",
        "const answer: Int =\n    let f = fn () -> Int = return 7\n    f ()\npub fn value () -> Int = answer\n",
        "actor Counter =\n    state n: Int =\n        let f = fn () -> Int = return 7\n        f ()\n    on value () -> Int = n\n",
        "actor Counter =\n    on value () -> Result Int Text =\n        try\n            let f = fn () -> Int = return 7\n            f ()\n",
        "import std.actor (ExitReason)\nactor Counter =\n    on value () -> Unit = ()\n    terminate (reason: ExitReason) =\n        let f = fn () -> Unit = return ()\n        f ()\n    onDown (m: Monitor) (reason: ExitReason) =\n        fn f () -> Unit = return ()\n        f ()\n",
    ] {
        assert!(codes(source).is_empty(), "{source}");
    }
}

#[test]
fn a_missing_return_target_does_not_hide_other_source_errors() {
    for (source, other) in [
        ("actor Counter =\n    on value () -> Unit = return (1 + \"wrong\")\n", "T001"),
        ("actor Counter =\n    on value () -> Unit =\n        return ()\n        let n: Int = \"wrong\"\n        ()\n", "T001"),
        ("import std.io as Io\nactor Counter =\n    on value () -> Unit = return (Io.println \"effect\")\n", "T014"),
        ("actor Counter =\n    on value () -> Unit =\n        return ()\n        let n = 0\n        n <- 1\n        ()\n", "R031"),
    ] {
        let actual = codes(source);
        if other != "R031" {
            assert!(actual.iter().any(|c| c == "T062"), "{actual:?}");
        }
        assert!(actual.iter().any(|c| c == other), "{actual:?}");
    }
}
