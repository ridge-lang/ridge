//! End-to-end check that a union type declared in the standard library can be
//! imported, constructed, and matched from user code, and round-trips on the
//! BEAM.
//!
//! `std.query` declares `pub type SortOrder = Asc | Desc` as ordinary Ridge.
//! A user module imports the type and its constructors, builds `Asc`/`Desc`, and
//! matches on them. This proves the whole chain works without the type being a
//! hand-written compiler builtin: the imported name resolves in an annotation,
//! the constructors type-check and lower to the right tags, and a match over them
//! dispatches correctly at runtime.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.query (SortOrder, Asc, Desc)

fn dirText (d: SortOrder) -> Text =
    match d
        Asc  -> "ASC"
        Desc -> "DESC"

pub fn ascText () -> Text = dirText Asc

pub fn descText () -> Text = dirText Desc
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"union-import-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn imported_stdlib_union_constructs_and_matches_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping imported_stdlib_union_constructs_and_matches_on_beam"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-union-import-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-union-import-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

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

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['ascText','descText']), halt()."
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

    // `Asc`/`Desc` were imported from std.query, constructed, and matched: the
    // match dispatches to the right branch at runtime.
    assert!(
        stdout.contains("ascText=ASC"),
        "expected `ascText=ASC`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("descText=DESC"),
        "expected `descText=DESC`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
