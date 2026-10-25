//! Newly generalized immutable helpers execute at independent concrete types.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
pub fn direct () -> (Text, Int) =
    let identity = fn x -> x
    let text: Text = identity "hello"
    (text, identity 7)

pub fn alias () -> (Text, Int) =
    fn same (x: a) -> a = x
    let identity = same
    (identity "alias", identity 9)

pub fn destructured () -> (Text, Int) =
    let ((identity, count), other) = ((fn x -> x, 0), 1)
    (identity "tuple", identity 11)

pub fn shadow () -> (Text, Int, Text) =
    let identity = fn x -> x
    let first = identity "outer"
    let inside =
        let identity = fn x -> x + 1
        identity 20
    (first, inside, identity "after")

pub fn rows () -> (Int, Text) =
    let getValue = fn (item: { value: a | r }) -> item.value
    (getValue ({ value = 13 }), getValue ({ value = "row", extra = true }))

pub fn higherOrder () -> (Text, Int) =
    let apply = fn callback x -> callback x
    (apply (fn y -> y) "higher", apply (fn y -> y) 17)

"#;

#[test]
fn let_helpers_run_at_distinct_types_and_keep_lexical_identity() {
    which::which("erl").expect("erl required");
    which::which("erlc").expect("erlc required");
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"let-values\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
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
    let expression = "M=ridge_app_Main, Actual={M:direct(),M:alias(),M:destructured(),M:shadow(),M:rows(),M:higherOrder()}, Expected={{<<\"hello\">>,7},{<<\"alias\">>,9},{<<\"tuple\">>,11},{<<\"outer\">>,21,<<\"after\">>},{13,<<\"row\">>},{<<\"higher\">>,17}}, case Actual of Expected -> io:put_chars(\"let-schemes-ok\\n\"); _ -> io:format(\"unexpected: ~p~n\",[Actual]),halt(5) end,halt().";
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
        "let-schemes-ok\n"
    );
}
