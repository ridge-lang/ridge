//! A constructor reached through its module alias builds the same value as the
//! bare spelling.
//!
//! `Alias.Ctor` used to take the qualified-*name* path, which knows about
//! functions and nothing about constructors. What came out depended on the
//! syntax around it and none of it was right: a union variant with a payload
//! compiled to a remote call to a function nobody generated (`undef` at run
//! time), a nullary one with a record body compiled to an empty map, and one
//! without a body reached the backend as a symbol with no bridge. The type
//! error that #515 reported was the mildest of the outcomes.
//!
//! Every assertion here compares the two spellings against each other rather
//! than against a literal. A fix that gets both wrong the same way is the shape
//! this bug already had once — the empty map type-checked as the union — so the
//! bare spelling is the control and the module also states what the bare
//! spelling produces, which pins the pair to reality.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const LIB: &str = r"
pub type Colour = Red | Green | Blue Int
pub type Point = { x: Int, y: Int }
";

const MAIN: &str = r#"
import app.Lib as L
import app.Lib (Colour, Point, Red, Blue)
import std.actor as Actor
import std.actor (Timeout)

-- Expression position: nullary, with a payload, and a record type.
pub fn qualifiedNullary () -> Colour = L.Red
pub fn bareNullary () -> Colour = Red
pub fn qualifiedPayload () -> Colour = L.Blue 3
pub fn barePayload () -> Colour = Blue 3
pub fn qualifiedRecord () -> Int = (L.Point { x = 1, y = 2 }).x

-- A stdlib union's variant through its alias, with no item list involved.
pub fn qualifiedStdlib () -> AskError = Actor.Timeout
pub fn bareStdlib () -> AskError = Timeout

-- Pattern position. The qualified spelling did not parse here at all.
fn name (c: Colour) -> Text =
    match c
        L.Red -> "red"
        L.Blue n -> Int.toText n
        _ -> "other"

pub fn matchedNullary () -> Text = name Red
pub fn matchedPayload () -> Text = name (Blue 7)

-- A record body in a pattern, reached through the alias.
pub fn matchedRecord () -> Int =
    match (L.Point { x = 4, y = 5 })
        L.Point { x, .. } -> x
"#;

fn write_workspace(root: &Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"qualified-ctor-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Lib.ridge"), LIB).expect("write lib");
    std::fs::write(app_src.join("Main.ridge"), MAIN).expect("write main");
}

fn compile(dir: &Path, cache: &Path) -> (PathBuf, String) {
    let artefacts = compile_workspace(
        CompileOptions::new(dir.to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.to_path_buf()),
    )
    .expect("compile to BEAM");
    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile, got diagnostics: {:?}",
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
        .find(|stem| stem.ends_with("_Main"))
        .expect("the Main module")
        .to_owned();
    (beam_dir, module)
}

/// Evaluate `module:fun()` and return the Erlang term as `~p` renders it, so a
/// tuple, an atom and a map are all distinguishable in the assertion.
fn run_fun(beam_dir: &Path, module: &str, fun: &str) -> String {
    let expr = format!("io:format(\"~p\", [{module}:{fun}()]), halt().");
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.is_empty(),
        "`{fun}` produced no output; stderr:\n{stderr}"
    );
    stdout
}

#[test]
fn a_module_qualified_constructor_builds_what_the_bare_one_builds() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping \
             a_module_qualified_constructor_builds_what_the_bare_one_builds"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-qualified-ctor-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-qualified-ctor-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());
    let (beam_dir, module) = compile(dir.path(), cache.path());
    let run = |f: &str| run_fun(&beam_dir, &module, f);

    // Each pair is the same constructor written two ways. Comparing them is
    // what catches a fix that is wrong about both.
    for (qualified, bare) in [
        ("qualifiedNullary", "bareNullary"),
        ("qualifiedPayload", "barePayload"),
        ("qualifiedStdlib", "bareStdlib"),
    ] {
        let q = run(qualified);
        let b = run(bare);
        assert_eq!(
            q, b,
            "`{qualified}` and `{bare}` name one constructor and must build one value"
        );
    }

    // And what that value is, so a fix that breaks both spellings the same way
    // does not read as agreement. `Red` is variant 0 of its union, which is the
    // case a record-vs-union mix-up gets wrong.
    assert_eq!(run("bareNullary"), "'Red'", "a nullary variant is its tag");
    assert_eq!(
        run("barePayload"),
        "{'Blue',3}",
        "a variant with a payload is a tagged tuple"
    );
    assert_eq!(
        run("bareStdlib"),
        "'Timeout'",
        "a stdlib union's variant is its tag too"
    );

    // A record type through the alias keeps working — it always did, because
    // the fallback a union variant needed was already right for a record.
    assert_eq!(run("qualifiedRecord"), "1");

    // Pattern position, which did not parse before this.
    assert_eq!(run("matchedNullary"), "<<\"red\">>");
    assert_eq!(run("matchedPayload"), "<<\"7\">>");
    assert_eq!(run("matchedRecord"), "4");
}
