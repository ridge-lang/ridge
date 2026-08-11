//! A `ToText` dictionary at a built-in type, as a value.
//!
//! Interpolating a built-in lowers to a direct `std.<x>.toText` call and never
//! asks for a dictionary, so none is emitted. Nothing needed one until a call
//! inside a constrained function could resolve `ToText` at a concrete built-in
//! type — then the reference is to `$inst_ToText_Int`, a constant that was
//! never generated, and the module fails to build with `E001`.
//!
//! No recursion and no generic container: one `where ToText a` function
//! calling another at `Int` is the whole shape.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// `label` is constrained and calls `describe` at `Int`, which pins the
/// built-in instance. `main_text` does the same at `Text`, whose conversion is
/// the identity — the arm most likely to be written as a no-op and never run.
const SOURCE: &str = r#"
fn describe (x: a) -> Text where ToText a =
    $"got:${x}"

fn label (x: a) (pick: Bool) -> Text where ToText a =
    if pick then describe x else describe 7

fn labelText (x: a) (pick: Bool) -> Text where ToText a =
    if pick then describe x else describe "seven"

type Color = Red | Green
    deriving (ToText)

pub fn main_int () -> Text =
    label Red false

pub fn main_text () -> Text =
    labelText Red false

pub fn main_forwarded () -> Text =
    label Green true
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"builtin-totext-dict-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn a_builtin_totext_dictionary_exists_as_a_value() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping a_builtin_totext_dictionary_exists_as_a_value");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-builtin-totext-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-builtin-totext-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    // The failure this guards is a build failure, so the diagnostics are the
    // first assertion rather than an afterthought: `E001: Local symbol
    // '$inst_ToText_Int' not found in fn-arity table`.
    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile; got {:?}",
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
        .find(|stem| {
            stem.starts_with("ridge_")
                && !matches!(
                    *stem,
                    "ridge_rt"
                        | "ridge_main_runner"
                        | "ridge_test_runner"
                        | "ridge_pg"
                        | "ridge_sup"
                        | "ridge_sqlite"
                        | "ridge_bench_runner"
                )
        })
        .expect("a user module")
        .to_owned();

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['main_int','main_text','main_forwarded']), halt()."
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

    // Rendering through the dictionary must read the same as rendering inline.
    assert!(
        stdout.contains("main_int=got:7"),
        "expected `main_int=got:7`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // `Text`'s conversion is the identity, and a dictionary that returns the
    // wrong thing here is easy to miss: the value is already a string.
    assert!(
        stdout.contains("main_text=got:seven"),
        "expected `main_text=got:seven`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // The forwarding branch, unaffected, so a fix that reached too far shows up.
    assert!(
        stdout.contains("main_forwarded=got:Green"),
        "expected `main_forwarded=got:Green`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
