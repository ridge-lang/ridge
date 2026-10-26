//! Checked replies preserve state and reach consumers with their declared types.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

fn workspace(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"replies\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/src/Replies.ridge"), source).unwrap();
    dir
}

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.actor (AskError)
type Book = { value: Int }

actor Source =
    on value () -> Int = 7

fn time readSource (h: Handle Source) -> Result Int AskError =
    let n = h ?> value () ?
    Ok n

fn readInt (r: Result Int AskError) -> Int =
    match r
        Ok n -> n
        Err _ -> -1000

actor Counter =
    state n: Int = 0
    on add (delta: Int) -> Int =
        n <- n + delta
        n
    on choose (b: Bool) -> Result Int Text =
        if b then
            n <- n + 10
            Ok n
        else
            n <- n + 100
            Err "declined"
    on book () -> Book = Book { value = n }
    on viaLambda () -> Int =
        let f = fn (x: Int) -> Int = return x + 1
        f n
    on time viaHelper (h: Handle Source) -> Result Int AskError = readSource h
    on value () -> Int = n

pub fn spawn time exercise () -> Text =
    let h = spawn Counter
    h ! add 1
    let first = readInt (h ?> value ())
    let second = readInt (h ?> add 2)
    let chosen = match h ?> choose true
        Ok (Ok n) -> n
        Ok (Err _) -> -1000
        Err _ -> -2000
    let _ = h ?> choose false
    let after = readInt (h ?> value ())
    let book = match h ?> book ()
        Ok b -> b
        Err _ -> Book { value = -1000 }
    let fromLambda = readInt (h ?> viaLambda ())
    let source = spawn Source
    let fromHelper = match h ?> viaHelper source
        Ok (Ok n) -> n
        Ok (Err _) -> -1000
        Err _ -> -2000
    $"${first},${second},${chosen},${after},${book.value},${fromLambda},${fromHelper}"
"#;

#[test]
fn declared_replies_run_with_state_result_and_nominal_values() {
    assert!(
        which::which("erl").is_ok() && which::which("erlc").is_ok(),
        "runtime gate requires Erlang"
    );
    let workspace = workspace(SOURCE);
    let cache = tempfile::tempdir().unwrap();
    let artefacts = compile_workspace(
        CompileOptions::new(workspace.path().to_owned())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_owned()),
    )
    .unwrap();
    assert!(
        artefacts.diagnostics.is_empty(),
        "{:?}",
        artefacts.diagnostics
    );
    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .unwrap();
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|name| name.to_ascii_lowercase().ends_with("_replies"))
        .unwrap();
    let eval = format!("io:format(\"~ts~n\",[{module}:exercise()]),halt().");
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(beam_dir)
        .args(["-eval", &eval])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout, "1,3,13,113,113,114,7\n");
}

#[test]
fn incompatible_reply_is_rejected_before_artifact_emission() {
    let source = "actor Counter =\n    on value () -> Int = \"wrong\"\n";
    let workspace = workspace(source);
    let cache = tempfile::tempdir().unwrap();
    let artefacts = compile_workspace(
        CompileOptions::new(workspace.path().to_owned())
            .with_emit(EmitArtefacts::Both)
            .with_cache_root(cache.path().to_owned()),
    )
    .unwrap();
    assert!(
        artefacts.diagnostics.iter().any(|d| d.code == "T001"),
        "{:?}",
        artefacts.diagnostics
    );
    assert!(
        artefacts.beam_files.is_empty() && artefacts.core_files.is_empty(),
        "invalid reply reached code generation"
    );
}
