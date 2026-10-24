//! A try expression preserves its Result value and its local propagation boundary.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
fn fail () -> Result Int Text = Err "fail"

pub fn value () -> Result Int Text =
    try
        7

pub fn failure () -> Result Int Text =
    try
        let n = fail () ?
        n

pub fn nested () -> Result Int Text =
    try
        let inner: Result Int Text = try
            let n = fail () ?
            n
        match inner
            Err _ -> 9
            Ok n -> n

pub fn explicit () -> Result Int Text =
    let ignored: Result Int Text = try
        return Ok 77
    Err "wrong"

pub fn tailExplicit () -> Result Int Text =
    let result: Result Int Text = try
        return Ok 88
    return result

fn invoke (callback: fn Unit -> Result Int Text) -> Result Int Text = callback ()

pub fn callback () -> Result Int Text =
    try
        let result = invoke (fn _ ->
            let n = fail () ?
            Ok n)
        match result
            Err _ -> 11
            Ok n -> n

pub fn crash (n: Int) -> Result Int Text =
    try
        7 / n

pub fn localBoundary () -> Result Int Text =
    try
        fn inner () -> Option Int =
            let n = None ?
            Some n
        let result = inner ()
        match result
            None -> 13
            Some n -> n

pub fn mutation () -> Result Int Text =
    var n = 0
    let result: Result Int Text = try
        n <- 3
        7
    match result
        Ok _ -> Ok n
        Err e -> Err e

pub fn mutationBeforeError () -> Result Int Text =
    var n = 0
    let result: Result Int Text = try
        n <- 4
        let x = fail () ?
        n <- 99
        x
    match result
        Err _ -> Ok n
        Ok _ -> Err "expected failure"

pub fn mutationAfterError () -> Result Int Text =
    var n = 0
    let result: Result Int Text = try
        let x = fail () ?
        n <- 99
        x
    match result
        Err _ -> Ok n
        Ok _ -> Err "expected failure"

pub fn nestedMutation () -> Result Int Text =
    var n = 0
    let result: Result Int Text = try
        let inner: Result Int Text = try
            n <- 5
            let x = fail () ?
            n <- 99
            x
        match inner
            Err _ -> n
            Ok x -> x
    match result
        Ok x -> Ok (x + n)
        Err e -> Err e

pub fn shadowMutation () -> Result Int Text =
    var n = 0
    let result: Result Int Text = try
        n <- 6
        var n = 20
        n <- 21
        fn get (input: Int) -> Int = n + input
        get 0
    match result
        Ok x -> Ok (x + n)
        Err e -> Err e

pub fn blockShadow () -> Int =
    let (n, text) = (42, "hello")
    let chosen =
        let n = "inner"
        n
    n

pub fn blockMutableShadow () -> Int =
    var n = 7
    let chosen =
        var n = "inner"
        n <- "changed"
        n
    n
"#;

#[test]
fn try_wraps_values_and_catches_only_its_own_propagation() {
    which::which("erl").expect("erl required");
    which::which("erlc").expect("erlc required");
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"try-values\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), SOURCE).unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    let user = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap_or_else(|| {
            panic!(
                "missing module; diagnostics: {:?}; core: {}",
                artifacts.diagnostics,
                std::fs::read_to_string(
                    dir.path()
                        .join("target/ridge/debug/core/ridge_app_Main.core")
                )
                .unwrap_or_default()
            )
        });
    let expression = "M=ridge_app_Main, Crash=try M:crash(0) of _ -> wrong catch error:badarith -> raised; _:_ -> wrong end, Actual={M:value(),M:failure(),M:nested(),M:explicit(),M:tailExplicit(),M:callback(),M:localBoundary(),M:mutation(),M:mutationBeforeError(),M:mutationAfterError(),M:nestedMutation(),M:shadowMutation(),M:blockShadow(),M:blockMutableShadow(),Crash}, case Actual of {{ok,7},{error,<<\"fail\">>},{ok,9},{ok,77},{ok,88},{ok,11},{ok,13},{ok,3},{ok,4},{ok,0},{ok,10},{ok,27},42,7,raised} -> io:put_chars(\"try-ok\\n\"); _ -> io:format(\"unexpected: ~p~n\",[Actual]),halt(5) end,halt().";
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(user.parent().unwrap())
        .args(["-eval", expression])
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
        "try-ok\n"
    );
}
