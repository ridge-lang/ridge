//! Auto-promotion (spec §5.6.6) is pure sugar for an explicit `instance
//! ToText T`: a qualifying `pub fn toText (x: T) -> Text` lowers to a private
//! `ToText__T__toText` fn plus a public `$inst_ToText_T` dictionary constant,
//! and the bare name `toText` is never a module symbol. So a use-site
//! `toText` always dispatches through the polymorphic `ToText` class method,
//! argument-directed, exactly like any explicit or derived instance.
//!
//! Two behaviours this locks in on the real BEAM runtime:
//!
//! - An auto-promoted `toText` for one type no longer shadows a `toText` call
//!   on a *different* type in the same module (previously a `T001` type
//!   mismatch, since the auto-promoted fn's own scheme bound the bare name).
//! - Two auto-promoted `pub fn toText` declarations for different types can
//!   coexist in one module (previously `R005`, a name collision at the
//!   module-index level).
//!
//! Each entry point is exposed as a zero-argument wrapper that both builds
//! its argument and calls `toText` from *inside* Ridge, so the test never has
//! to hand-construct a Ridge record's BEAM representation from `erl -eval`.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileArtefacts, CompileOptions, EmitArtefacts};

/// Compile `source` as a one-member `app` workspace and return the compiled
/// artefacts.
fn compile_source(
    dir: &std::path::Path,
    cache: &std::path::Path,
    source: &str,
) -> CompileArtefacts {
    let app_src = dir.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        dir.join("ridge.toml"),
        "[workspace]\nname = \"totext-autopromote-dispatch-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        dir.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");

    compile_workspace(
        CompileOptions::new(dir.to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.to_path_buf()),
    )
    .expect("compile to BEAM")
}

/// Run `expr` against the compiled module's BEAM files via `erl -eval` and
/// return its captured stdout.
fn run_erl(beam_dir: &std::path::Path, expr: &str) -> String {
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(expr)
        .output()
        .expect("run erl");
    assert!(
        output.status.success(),
        "erl exited non-zero\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The compiled user module's BEAM output directory and its `ridge_module_*`
/// atom name.
fn user_module(artefacts: &CompileArtefacts) -> (std::path::PathBuf, String) {
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
    (beam_dir, module)
}

/// An auto-promoted `toText` for `Widget` must not shadow a `toText` call on
/// `Coin`, whose `ToText` instance is explicit — the bug this change fixes.
#[test]
fn autopromoted_totext_does_not_shadow_other_types_dispatch() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping autopromoted_totext_does_not_shadow_other_types_dispatch"
        );
        return;
    }

    const SOURCE: &str = r##"
pub type Widget = { tag: Text }
pub type Coin = { n: Int }

pub fn toText (w: Widget) -> Text = Text.concat "W:" w.tag

instance ToText Coin =
    toText (c: Coin) -> Text = "C"

pub fn describe () -> Text = toText (Coin { n = 1 })
pub fn showW () -> Text = toText (Widget { tag = "x" })
"##;

    let dir = tempfile::Builder::new()
        .prefix("ridge-totext-ap-dispatch-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-totext-ap-dispatch-e2e-cache-")
        .tempdir()
        .expect("cache dir");

    let artefacts = compile_source(dir.path(), cache.path(), SOURCE);
    assert!(
        artefacts.diagnostics.is_empty(),
        "a bare toText on one type must not shadow ToText dispatch for another; got {:?}",
        artefacts.diagnostics
    );

    let (beam_dir, module) = user_module(&artefacts);
    let expr = format!(
        "io:format(\"describe=~s showW=~s~n\",[{module}:describe(),{module}:showW()]), halt()."
    );
    let stdout = run_erl(&beam_dir, &expr);
    assert!(
        stdout.contains("describe=C showW=W:x"),
        "expected `describe=C showW=W:x` (Coin dispatches to its explicit instance, Widget to its auto-promoted one)\nstdout:\n{stdout}"
    );
}

/// Two auto-promoted `pub fn toText` declarations for different types, with
/// no explicit `instance` at all, must coexist and each dispatch correctly.
#[test]
fn two_autopromoted_totext_declarations_coexist() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping two_autopromoted_totext_declarations_coexist");
        return;
    }

    const SOURCE: &str = r##"
pub type Widget = { tag: Text }
pub type Coin = { label: Text }

pub fn toText (w: Widget) -> Text = Text.concat "W:" w.tag
pub fn toText (c: Coin) -> Text = Text.concat "C:" c.label

pub fn showW () -> Text = toText (Widget { tag = "x" })
pub fn showC () -> Text = toText (Coin { label = "7" })
"##;

    let dir = tempfile::Builder::new()
        .prefix("ridge-totext-ap-dispatch-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-totext-ap-dispatch-e2e-cache-")
        .tempdir()
        .expect("cache dir");

    let artefacts = compile_source(dir.path(), cache.path(), SOURCE);
    assert!(
        artefacts.diagnostics.is_empty(),
        "two auto-promoted toText declarations for different types must compile cleanly; got {:?}",
        artefacts.diagnostics
    );

    let (beam_dir, module) = user_module(&artefacts);
    let expr = format!("io:format(\"w=~s c=~s~n\",[{module}:showW(),{module}:showC()]), halt().");
    let stdout = run_erl(&beam_dir, &expr);
    assert!(
        stdout.contains("w=W:x c=C:7"),
        "expected `w=W:x c=C:7` (each auto-promoted toText dispatches to its own type)\nstdout:\n{stdout}"
    );
}
