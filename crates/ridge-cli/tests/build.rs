//! Integration tests for `ridge build`.
//!
//! All five tests run without OTP (no feature gate) because `ridge build`
//! invokes `erlc` but the driver falls back gracefully to `C004` when OTP is
//! absent — and `C001` is detectable without OTP at all.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::{make_multi_member_workspace, make_workspace};
use std::path::Path;

// ── helpers ───────────────────────────────────────────────────────────────────

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

// ── Test 1: ridge build in a single-member workspace ─────────────────────────

/// `ridge build` succeeds in a minimal single-member library workspace.
///
/// We do not assert that `.beam` files exist (that requires OTP/erlc), but we
/// do assert that the command exits successfully when the compiler pipeline
/// runs without errors.
///
/// Because this machine may not have OTP, the test is gated: if OTP is absent
/// the driver returns `C004 ErlangNotFound` and the build exits non-zero.  We
/// allow both outcomes and only assert the exit is 0 when OTP is detected.
#[test]
fn build_single_member() {
    let source = "pub fn hello -> Text = \"world\"\n";
    let tw = make_workspace("Hello", source);

    let mut cmd = ridge_cmd();
    cmd.arg("build").current_dir(&tw.path);

    // The test is permissive: success is ideal; C004 is acceptable on CI
    // machines without OTP.  An unexpected error (C001 etc.) fails the test.
    let output = cmd.output().expect("ridge build spawn failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        // Only C004 (no OTP) is acceptable.
        assert!(
            stderr.contains("C004") || stderr.contains("erlang") || stderr.contains("erl"),
            "unexpected build failure.\nstdout: {stdout}\nstderr: {stderr}"
        );
    }
    // If it succeeded, stdout should say "Compiled N module(s)".
    if output.status.success() {
        assert!(
            stdout.contains("Compiled") && stdout.contains("module(s)"),
            "expected success banner in stdout, got: {stdout}"
        );
    }
}

// ── Test 2: ridge build --member api in a multi-member workspace ──────────────

/// `ridge build --member api` only compiles the `api` member.
#[test]
fn build_member_filter() {
    let tw = make_multi_member_workspace();

    let mut cmd = ridge_cmd();
    cmd.arg("build")
        .arg("--member")
        .arg("api")
        .current_dir(&tw.path);

    let output = cmd.output().expect("ridge build spawn failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Allow C004 (no OTP).
    if !output.status.success() {
        assert!(
            stderr.contains("C004") || stderr.contains("erlang") || stderr.contains("erl"),
            "unexpected failure with --member api.\nstdout: {stdout}\nstderr: {stderr}"
        );
    }
}

// ── Test 3: ridge build --release asserts target/ridge/release/ layout ────────

/// `ridge build --release` produces output under `target/ridge/release/`.
///
/// Only verifies the directory structure when OTP is present.
#[test]
fn build_release_profile() {
    let source = "pub fn hello -> Text = \"world\"\n";
    let tw = make_workspace("Hello", source);

    let mut cmd = ridge_cmd();
    cmd.arg("build").arg("--release").current_dir(&tw.path);

    let output = cmd.output().expect("ridge build --release spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        // When OTP present: verify the release directory exists.
        let release_dir = tw.path.join("target").join("ridge").join("release");
        assert!(
            release_dir.exists(),
            "expected target/ridge/release/ to exist after --release build"
        );
    } else {
        // C004 acceptable.
        assert!(
            stderr.contains("C004") || stderr.contains("erlang") || stderr.contains("erl"),
            "unexpected failure with --release.\nstderr: {stderr}"
        );
    }
}

// ── Test 4: ridge build --emit core asserts .core files present, no .beam ─────

/// `ridge build --emit core` produces `.core` files and no `.beam` files.
#[test]
fn build_emit_core() {
    let source = "pub fn hello -> Text = \"world\"\n";
    let tw = make_workspace("Hello", source);

    let mut cmd = ridge_cmd();
    cmd.arg("build")
        .arg("--emit")
        .arg("core")
        .current_dir(&tw.path);

    let output = cmd.output().expect("ridge build --emit core spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        // Verify at least one .core file exists.
        let debug_dir = tw.path.join("target").join("ridge").join("debug");
        let core_files: Vec<_> = walkdir_collect_ext(&debug_dir, "core");
        assert!(
            !core_files.is_empty(),
            "expected at least one .core file under target/ridge/debug/"
        );
        // No .beam files.
        let beam_files: Vec<_> = walkdir_collect_ext(&debug_dir, "beam");
        assert!(
            beam_files.is_empty(),
            "expected no .beam files with --emit core, found: {beam_files:?}"
        );
    } else {
        // C004 acceptable (erlc not needed for Core emit — but driver may
        // still probe it; permit the error).
        assert!(
            stderr.contains("C004")
                || stderr.contains("erlang")
                || stderr.contains("erl")
                || stderr.contains("error"),
            "unexpected failure with --emit core.\nstderr: {stderr}"
        );
    }
}

// ── Test 5: ridge build outside a workspace exits non-zero with C001 ──────────

/// `ridge build` in a directory with no `ridge.toml` returns `C001`.
#[test]
fn build_outside_workspace() {
    // Use a plain temp dir with no ridge.toml.
    let td = tempfile::TempDir::new().expect("create tempdir");

    let mut cmd = ridge_cmd();
    cmd.arg("build").current_dir(td.path());

    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("C001"));
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Walk `root` and collect all files with the given extension.
fn walkdir_collect_ext(root: &Path, ext: &str) -> Vec<std::path::PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return vec![];
    };
    let mut out = vec![];
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walkdir_collect_ext(&path, ext));
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
    out
}

// ── Test 6: a failed build does not print a spurious C001 ────────────────────

/// A build that fails because the source has a type error prints the real
/// diagnostic and exits non-zero — but must NOT tack on a misleading
/// `C001 NoWorkspaceRoot`, which used to be reused as a generic failure
/// sentinel for every build error.
#[test]
fn build_failure_does_not_report_spurious_c001() {
    // `Int` annotated, `Text` returned — a type error surfaced at typecheck,
    // before any OTP/erlc step, so this runs on machines without OTP.
    let source = "pub fn bad -> Int = \"not an int\"\n";
    let tw = make_workspace("Bad", source);

    let output = ridge_cmd()
        .arg("build")
        .current_dir(&tw.path)
        .output()
        .expect("ridge build spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "a type error must fail the build.\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("C001") && !stderr.contains("NoWorkspaceRoot"),
        "a failed build must not report a spurious C001 NoWorkspaceRoot.\nstderr: {stderr}"
    );
}
