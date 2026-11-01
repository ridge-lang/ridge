//! Slice conditions survive nesting, aliases, guards and arm fallthrough.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

fn workspace(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    for (path, content) in [
        ("ridge.toml", "[workspace]\nname=\"slices\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n"),
        ("app/ridge.toml", "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"io\"]\n"),
        ("app/src/Main.ridge", source),
    ] {
        std::fs::write(dir.path().join(path), content).unwrap();
    }
    dir
}

const SOURCE: &str = r#"
import std.io as Io
import std.list as List

type Holder = { values: List Int }

fn prefix (xs: List Int) -> Int =
    match xs
        [1, .., _] -> 1
        _ -> 0
fn constructor (xs: List (Option Int)) -> Int =
    match xs
        [Some 1, .., _] -> 1
        _ -> 0
fn decimal (xs: List Decimal) -> Int =
    match xs
        [1.50m, .., _] -> 1
        _ -> 0
fn nested (xs: List (List Int)) -> Int =
    match xs
        [[.., _]] -> 1
        _ -> 0
fn wrapped (xs: List Int) -> Int =
    match xs
        ([_, .., _]) -> 1
        _ -> 0
fn aliased (xs: List Int) -> Int =
    match xs
        all @ [_, .., last] when last == 7 -> List.length all
        _ -> 0
fn ends (xs: List Int) -> Int =
    match xs
        [first, .., last] when first == last -> last
        _ -> 0
fn middle (xs: List Int) -> Int =
    match xs
        [first, mid @ .., last] when List.length mid == 1 -> first + last
        _ -> 0
fn record (holder: Holder) -> Int =
    match holder
        Holder { values = [first, .., last] } when first == last -> last
        _ -> 0
fn siblings (value: (Option (List Int), List Int)) -> Int =
    match value
        (Some [first, .., last], [.., other]) when last == other -> first
        _ -> 0
fn fallback (xs: List Int) -> Int =
    match xs
        [1, .., _] when false -> 99
        [_, .., last] -> last
        _ -> 0
fn shadow (xs: List Int) -> Int =
    match xs
        [_, .., last] ->
            let before = last
            let last = 9
            before + last
        _ -> 0
fn io observed () -> List Int =
    Io.println "once"
    [2, 3]
pub fn io exercise () -> List Int =
    let effect =
        match observed ()
            [1, .., _] -> 99
            [_, .., last] -> last
            _ -> 0
    [prefix [2, 3], prefix [1, 3], constructor [Some 2, None], constructor [Some 1, None],
     decimal [1.5m, 0.0m], decimal [1.6m, 0.0m],
     nested [[]], nested [[5]], wrapped [], wrapped [1, 2],
     aliased [], aliased [2, 7], aliased [2, 8], ends [], ends [5, 5], ends [5, 6],
     middle [1, 2, 3], middle [1, 3],
     record (Holder { values = [] }), record (Holder { values = [4, 4] }),
     siblings (Some [2, 7], [1, 7]), siblings (Some [], [1, 7]), siblings (Some [2, 7], []),
     fallback [1, 8], fallback [2, 8], shadow [1, 4], effect]
"#;

#[test]
fn slice_patterns_check_conditions_before_extracting_and_guarding() {
    let dir = workspace(SOURCE);
    let cache = tempfile::tempdir().unwrap();
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
        .find(|path| path.file_stem().and_then(|name| name.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"]).arg(module.parent().unwrap())
        .args(["-eval", "[0,1,0,1,1,0,0,1,0,1,0,2,0,0,5,0,4,0,0,4,2,0,0,8,8,13,3]=ridge_app_Main:exercise(),io:put_chars(\"slice-ok\\n\"),halt()."])
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        "once\nslice-ok\n"
    );
}

#[test]
fn refutable_suffix_elements_remain_diagnosed_when_nested_or_parenthesized() {
    for pattern in ["[.., 1]", "([.., 1])", "all @ [.., 1]"] {
        let source = format!("pub fn probe (xs: List Int) -> Int =\n    match xs\n        {pattern} -> 1\n        _ -> 0\n");
        let dir = workspace(&source);
        let cache = tempfile::tempdir().unwrap();
        let artifacts = compile_workspace(
            CompileOptions::new(dir.path().to_path_buf())
                .with_emit(EmitArtefacts::Core)
                .with_cache_root(cache.path().to_path_buf()),
        )
        .unwrap();
        assert!(
            artifacts
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "L109"),
            "{pattern}: {:?}",
            artifacts.diagnostics
        );
    }
    let dir = workspace("pub fn probe (xs: List (List Int)) -> Int =\n    match xs\n        [[.., 1]] -> 1\n        _ -> 0\n");
    let cache = tempfile::tempdir().unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Core)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "L109"),
        "{:?}",
        artifacts.diagnostics
    );
}
