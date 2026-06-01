//! End-to-end runtime checks for the prelude `JsonValue` union.
//!
//! Exercises the full pipeline for first-class JSON values built and matched
//! from a user module (no `import std.json`): parse → resolve (prelude
//! constructors) → typecheck (prelude union schema) → lower (`SymbolRef::Prelude`)
//! → Core Erlang (lowercase-snake `json_*` atoms) → run on the BEAM.
//!
//! The encode side calls `Json.encode` (the prelude `std.json` accessor) so
//! the emitted wire format is checked against the runtime walker; the match
//! side builds and destructures values entirely within the user module.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// A user module that builds `JsonValue` trees with the prelude constructors,
/// encodes them through `Json.encode`, and matches on the variants.
/// `JsonValue` and its constructors are in scope without importing `std.json`.
const SOURCE: &str = r#"
pub fn main_encode_int () -> Text =
    Json.encode (JInt 42)

pub fn main_encode_null () -> Text =
    Json.encode JNull

pub fn main_encode_bool () -> Text =
    Json.encode (JBool true)

pub fn main_encode_text () -> Text =
    Json.encode (JText "hi")

pub fn main_encode_list () -> Text =
    Json.encode (JList [JInt 1, JInt 2])

-- Build + match in the same module: returns the payload of a JInt.
pub fn main_match_int () -> Int =
    match JInt 7
        JInt n -> n
        _ -> 0

-- Match the nullary variant.
pub fn main_match_null () -> Bool =
    match JNull
        JNull -> true
        _ -> false

-- Match a nested list variant (wildcard payload).
pub fn main_match_list () -> Bool =
    match JList [JInt 1, JInt 2]
        JList _ -> true
        _ -> false
"#;

fn write_workspace_source(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"json-value-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

#[test]
fn json_value_construct_encode_and_match() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping json_value_construct_encode_and_match");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-json-value-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-json-value-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace_source(dir.path(), SOURCE);

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    if !artefacts.diagnostics.is_empty() {
        eprintln!("COMPILE DIAGNOSTICS:");
        for d in &artefacts.diagnostics {
            eprintln!("  {d:?}");
        }
    }
    assert!(
        artefacts.diagnostics.is_empty(),
        "no compile errors expected; got {:?}",
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
        .find(|stem| stem.starts_with("ridge_module_"))
        .expect("a user module")
        .to_owned();

    // `~s` for Text (binary); integers via integer_to_list; bools via atom_to_list.
    let expr = format!(
        "io:format(\"main_encode_int=~s~n\",[{module}:main_encode_int()]), \
         io:format(\"main_encode_null=~s~n\",[{module}:main_encode_null()]), \
         io:format(\"main_encode_bool=~s~n\",[{module}:main_encode_bool()]), \
         io:format(\"main_encode_text=~s~n\",[{module}:main_encode_text()]), \
         io:format(\"main_encode_list=~s~n\",[{module}:main_encode_list()]), \
         io:format(\"main_match_int=~s~n\",[integer_to_list({module}:main_match_int())]), \
         io:format(\"main_match_null=~s~n\",[atom_to_list({module}:main_match_null())]), \
         io:format(\"main_match_list=~s~n\",[atom_to_list({module}:main_match_list())]), \
         halt()."
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

    let want = [
        ("main_encode_int", "42"),
        ("main_encode_null", "null"),
        ("main_encode_bool", "true"),
        ("main_encode_text", "\"hi\""),
        ("main_encode_list", "[1,2]"),
        ("main_match_int", "7"),
        ("main_match_null", "true"),
        ("main_match_list", "true"),
    ];
    for (name, value) in want {
        let line = format!("{name}={value}");
        assert!(
            stdout.contains(&line),
            "expected `{line}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
