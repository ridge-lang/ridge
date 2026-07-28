//! Integration tests for `reload --check` over real compiler output.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ridge_driver::reload::{reload_check, snapshot_path_for, ReloadCheckError};
use ridge_driver::{compile_workspace, CheckOptions, CompileOptions, EmitArtefacts};
use ridge_reload::check::Verdict;

/// Compiles the workspace in Core-only mode (no external toolchain needed)
/// and returns the diagnostics.
fn compile(tw: &common::TempWorkspace) -> Vec<ridge_diagnostics::Diagnostic> {
    let options =
        CompileOptions::new(tw.path.clone()).with_emit(EmitArtefacts::Core);
    compile_workspace(options).expect("compile").diagnostics
}

#[test]
fn snapshot_written_by_compile_and_reload_check_runs() {
    let tw = common::make_workspace("main", "pub fn answer () -> Int = 42\n");
    let diags = compile(&tw);
    assert!(
        !diags.iter().any(|d| matches!(d.severity, ridge_diagnostics::Severity::Error)),
        "workspace should compile clean: {diags:?}"
    );

    let snapshot = snapshot_path_for(&tw.path, "debug");
    assert!(snapshot.exists(), "snapshot written at {}", snapshot.display());

    // Add a second public fn — a compatible change.
    common::write_file(
        &tw.path,
        "apps/demo/src/main.ridge",
        "pub fn answer () -> Int = 42\npub fn answer2 () -> Int = 43\n",
    );
    let report = reload_check(CheckOptions::new(tw.path), &snapshot).expect("reload check");
    assert_eq!(report.verdicts.len(), 1, "one change expected: {report:?}");
    assert_eq!(report.verdicts[0].symbol, "answer2");
    assert_eq!(report.verdicts[0].verdict, Verdict::Compatible);
    assert!(report.is_reloadable());
}

#[test]
fn missing_snapshot_is_a_clear_error() {
    let tw = common::make_workspace("main", "pub fn answer () -> Int = 42\n");
    let snapshot = snapshot_path_for(&tw.path, "debug");
    match reload_check(CheckOptions::new(tw.path), &snapshot) {
        Err(ReloadCheckError::MissingSnapshot(path)) => {
            assert_eq!(path, snapshot);
            assert!(
                ReloadCheckError::MissingSnapshot(path).to_string().contains("ridge build"),
                "error should hint at `ridge build`"
            );
        }
        other => panic!("expected MissingSnapshot, got {other:?}"),
    }
}

#[test]
fn incompatible_change_reported() {
    let tw = common::make_workspace("main", "pub fn f (x: Int) -> Int = x\n");
    compile(&tw);
    let snapshot = snapshot_path_for(&tw.path, "debug");
    assert!(snapshot.exists());

    common::write_file(&tw.path, "apps/demo/src/main.ridge", "pub fn f (x: Text) -> Int = 0\n");
    let report = reload_check(CheckOptions::new(tw.path), &snapshot).expect("reload check");
    assert!(!report.is_reloadable(), "{report:?}");
    let bad = report
        .verdicts
        .iter()
        .find(|v| matches!(v.verdict, Verdict::Incompatible { .. }))
        .expect("one incompatible verdict");
    assert_eq!(bad.symbol, "f");
    assert!(bad.module.ends_with("main"), "module names the file: {}", bad.module);
}
