//! Value initializers carry executed updates to their consumers without capture.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
pub fn minimal () -> Int =
    var n = 1
    let chosen =
        n <- 7
        n
    n

pub fn shadow () -> (Int, Int) =
    var n = 1
    let chosen =
        n <- 7
        var n = 20
        n <- 21
        n
    (chosen, n)

pub fn localOnly () -> (Int, Int) =
    var n = 1
    let chosen =
        var n = 20
        n <- 21
        n
    (chosen, n)

pub fn localLifetime () -> (Int, Int) =
    let chosen =
        var n = 0
        n <- 21
        n
    let n = 11
    (chosen, n)

pub fn mutableInitializer () -> (Int, Int) =
    var n = 1
    var chosen =
        n <- 7
        n
    (chosen, n)

pub fn destructured () -> (Int, Int, Int) =
    var n = 1
    let (left, right) =
        n <- 7
        (n, 3)
    (left, right, n)

pub fn nested () -> (Int, Int, Int) =
    var n = 1
    let first =
        let second =
            n <- 7
            n
        (second, n)
    let (left, right) = first
    (left, right, n)

pub fn sequential () -> (Int, Int, Int) =
    var n = 1
    let first =
        n <- 7
        n
    let second =
        n <- 9
        n
    (first, second, n)

pub fn hygiene () -> (Int, Int, Int) =
    let x = 11
    var n = 1
    let chosen =
        let x = 20
        n <- 7
        x
    (chosen, x, n)

pub fn once () -> (Int, Int) =
    var n = 0
    let chosen =
        n <- n + 1
        n
    (chosen, n)

pub fn multipleTypes () -> ((Int, Text), Int, Text) =
    var n = 1
    var text = "old"
    let chosen =
        n <- 7
        text <- "new"
        (n, text)
    (chosen, n, text)

pub fn capturedMutation () -> (Int, Int, Int) =
    var n = 1
    let callback = fn _ ->
        n <- 9
        n
    let chosen =
        n <- 7
        n
    let inside = callback ()
    (chosen, inside, n)

pub fn conditional (enabled: Bool) -> (Int, Int) =
    var n = 1
    let chosen =
        match enabled
            true ->
                n <- 7
                n
            false -> n
    (chosen, n)

pub fn explicitReturn () -> Int =
    var n = 1
    let chosen =
        n <- 7
        return 42
    n

fn fail () -> Result Int Text = Err "stop"

pub fn propagation () -> Int =
    var n = 1
    let result: Result Int Text = try
        let chosen =
            n <- 7
            let item = fail () ?
            item
        chosen
    match result
        Err _ -> n
        Ok _ -> 0
"#;

#[test]
fn initializers_preserve_values_state_order_and_binding_identity() {
    which::which("erl").expect("erl required");
    which::which("erlc").expect("erlc required");
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"initializer-values\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
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
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let expression = "M=ridge_app_Main, Actual={M:minimal(),M:shadow(),M:localOnly(),M:localLifetime(),M:mutableInitializer(),M:destructured(),M:nested(),M:sequential(),M:hygiene(),M:once(),M:multipleTypes(),M:capturedMutation(),M:conditional(true),M:conditional(false),M:explicitReturn(),M:propagation()}, Expected={7,{21,7},{21,1},{21,11},{7,7},{7,3,7},{7,7,7},{7,9,9},{20,11,7},{1,1},{{7,<<\"new\">>},7,<<\"new\">>},{7,9,7},{7,7},{1,1},42,7}, case Actual of Expected -> io:put_chars(\"initializer-ok\\n\"); _ -> io:format(\"unexpected: ~p~n\",[Actual]),halt(5) end,halt().";
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
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
        "initializer-ok\n"
    );
}
