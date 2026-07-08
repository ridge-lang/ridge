//! Regression: a genuinely polymorphic `where ToText a` call dispatches
//! correctly when the argument's `ToText` instance is AUTO-PROMOTED — a bare
//! `pub fn toText (x: T) -> Text`, with no `instance` and no `deriving`.
//!
//! Auto-promoted instances emit no `$inst_ToText_T` dictionary constant: the
//! method IS the public module function. The monomorphic interpolation path
//! calls that function directly, but a polymorphic caller threads a dictionary
//! VALUE, which the solver resolves through `dict_plan_to_expr`. That path used
//! to emit a reference to the missing `$inst_ToText_T` const and fail to compile
//! (`E001: malformed IR: Local symbol '$inst_ToText_Widget' not found in
//! fn-arity table (T8)`). The dictionary is now synthesised inline for
//! auto-promoted instances, closing over the public function.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r##"
-- `Widget` has ToText ONLY via a bare `pub fn toText` (auto-promoted): no
-- `instance`, no `deriving`. So there is no `$inst_ToText_Widget` dictionary.
pub type Widget = { tag: Text }

pub fn toText (w: Widget) -> Text = Text.concat "W:" w.tag

-- Genuinely polymorphic: inside `label`, `x` is a type variable, so the hole
-- dispatches through the dict PARAMETER and the caller must supply the Widget
-- dictionary. That dictionary must be synthesised inline from the public fn.
pub fn label (x: a) -> Text where ToText a = $"[${x}]"

pub fn probe () -> Text = label (Widget { tag = "ok" })
"##;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"totext-autopromote-poly-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn polymorphic_totext_on_autopromoted_type_dispatches() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping polymorphic_totext_on_autopromoted_type_dispatches");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-totext-ap-poly-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-totext-ap-poly-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    // The bug was a COMPILE error (E001), so first assert a clean compile.
    assert!(
        artefacts.diagnostics.is_empty(),
        "polymorphic ToText on an auto-promoted type must compile cleanly; got {:?}",
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

    let expr = format!("io:format(\"probe=~s~n\",[{module}:probe()]), halt().");
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

    assert!(
        stdout.contains("probe=[W:ok]"),
        "expected `probe=[W:ok]` (label wraps the auto-promoted toText through the synthesised dict)\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
