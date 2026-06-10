//! End-to-end check that a union constructor imported from a sibling workspace
//! module resolves as a constructor in pattern position — not as a catch-all
//! variable.
//!
//! `shapes.Shapes` declares `pub type Color = Red | Green | Blue`; `app.App`
//! imports the constructors and matches a `Color` parameter against them. A
//! constructor imported from a workspace module resolves to an `ImportedSymbol`,
//! and if pattern lowering does not rewrite that to a real constructor match the
//! first arm becomes a wildcard that matches every colour — so `greenText` and
//! `blueText` would both answer "red". This guards that regression, which also
//! reaches the standard library's own build (every `std.*` module is a workspace
//! module there, so cross-module constructor imports take the same path).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SHAPES_SRC: &str = r#"
pub type Color = Red | Green | Blue
"#;

const APP_SRC: &str = r#"
import shapes.Shapes (Color, Red, Green, Blue)

-- Match a `Color` parameter against constructors imported from another module.
-- If those patterns bind as variables, the first arm catches every colour.
fn colorText (c: Color) -> Text =
    match c
        Red   -> "red"
        Green -> "green"
        Blue  -> "blue"

pub fn redText () -> Text = colorText Red
pub fn greenText () -> Text = colorText Green
pub fn blueText () -> Text = colorText Blue
"#;

fn write_workspace(root: &std::path::Path) {
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"union-xmod-e2e\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    )
    .expect("write workspace manifest");

    let shapes_src = root.join("apps").join("shapes").join("src");
    std::fs::create_dir_all(&shapes_src).expect("create shapes dirs");
    std::fs::write(
        root.join("apps").join("shapes").join("ridge.toml"),
        "[project]\nname = \"shapes\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"**\"]\n",
    )
    .expect("write shapes manifest");
    std::fs::write(shapes_src.join("Shapes.ridge"), SHAPES_SRC).expect("write shapes source");

    let app_src = root.join("apps").join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create app dirs");
    std::fs::write(
        root.join("apps").join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"**\"]\n",
    )
    .expect("write app manifest");
    std::fs::write(app_src.join("App.ridge"), APP_SRC).expect("write app source");
}

#[test]
fn imported_workspace_union_ctor_matches_in_patterns_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping cross-module union e2e");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-union-xmod-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-union-xmod-e2e-cache-")
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

    // Two user modules are emitted; only the app one exports the probes. Call
    // each probe on every module under `catch` so the producer module is a no-op.
    let modules: Vec<String> = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .filter(|stem| stem.starts_with("ridge_module_"))
        .map(str::to_owned)
        .collect();
    assert!(
        modules.len() >= 2,
        "expected at least two user modules (producer + consumer), got {modules:?}"
    );

    let module_list = modules
        .iter()
        .map(|m| format!("'{m}'"))
        .collect::<Vec<_>>()
        .join(",");
    let expr = format!(
        "lists:foreach(fun(M) -> \
           catch io:format(\"red=~s~n\", [M:redText()]), \
           catch io:format(\"green=~s~n\", [M:greenText()]), \
           catch io:format(\"blue=~s~n\", [M:blueText()]) \
         end, [{module_list}]), halt()."
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

    // Each imported constructor pattern dispatches to its own arm. Before the
    // fix, all three answered "red" because the first arm was a wildcard.
    for (probe, want) in [
        ("red=red", "Red matches its own arm"),
        ("green=green", "Green is not absorbed by the Red arm"),
        ("blue=blue", "Blue is not absorbed by the Red arm"),
    ] {
        assert!(
            stdout.contains(probe),
            "expected `{probe}` ({want})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
