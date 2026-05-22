//! Integration tests for `ridge new`.
//!
//! 4 tests:
//! 1. Happy path — files created with substituted content; `ridge build` exits 0.
//! 2. `C201 InvalidProjectName` — bad name rejected.
//! 3. `C202 DirectoryExists` — pre-existing directory rejected.
//! 4. `C203 ReservedName` — reserved name rejected.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use std::fs;

// ── helper ────────────────────────────────────────────────────────────────────

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

// ── Test 1: happy path ────────────────────────────────────────────────────────

/// `ridge new my-app` creates the canonical layout with substituted content.
///
/// Asserts that `my-app/ridge.toml`, `my-app/src/Main.ridge`, and
/// `my-app/README.md` exist and contain the project name in their content.
/// Then runs `ridge build` inside the generated project and asserts exit 0
/// (or C004 when OTP is absent — same permissive gate as existing build tests).
#[test]
fn new_happy_path() {
    let td = tempfile::TempDir::new().expect("create tempdir");

    // Scaffold the project.
    ridge_cmd()
        .arg("new")
        .arg("my-app")
        .current_dir(td.path())
        .assert()
        .success();

    let project_dir = td.path().join("my-app");

    // ridge.toml exists and contains "my-app".
    let toml_path = project_dir.join("ridge.toml");
    assert!(toml_path.exists(), "ridge.toml not created");
    let toml_content = fs::read_to_string(&toml_path).expect("read ridge.toml");
    assert!(
        toml_content.contains("my-app"),
        "ridge.toml does not contain project name 'my-app': {toml_content}"
    );
    // {NAME} placeholder must not remain.
    assert!(
        !toml_content.contains("{NAME}"),
        "ridge.toml still contains unreplaced {{NAME}} placeholder"
    );

    // src/Main.ridge exists and contains "my-app".
    let main_ridge_path = project_dir.join("src").join("Main.ridge");
    assert!(main_ridge_path.exists(), "src/Main.ridge not created");
    let main_ridge_content = fs::read_to_string(&main_ridge_path).expect("read Main.ridge");
    assert!(
        main_ridge_content.contains("my-app"),
        "Main.ridge does not contain project name 'my-app': {main_ridge_content}"
    );
    assert!(
        !main_ridge_content.contains("{NAME}"),
        "Main.ridge still contains unreplaced {{NAME}} placeholder"
    );

    // README.md exists and contains "my-app".
    let readme_path = project_dir.join("README.md");
    assert!(readme_path.exists(), "README.md not created");
    let readme_content = fs::read_to_string(&readme_path).expect("read README.md");
    assert!(
        readme_content.contains("my-app"),
        "README.md does not contain project name 'my-app': {readme_content}"
    );

    // `ridge build` inside the scaffolded project.
    let output = ridge_cmd()
        .arg("build")
        .current_dir(&project_dir)
        .output()
        .expect("ridge build spawn failed");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // C004 (no OTP) is acceptable on machines without Erlang/OTP.
        assert!(
            stderr.contains("C004")
                || stderr.contains("erlang")
                || stderr.contains("erl")
                || stderr.contains("erlc"),
            "unexpected ridge build failure in scaffolded project.\n\
             stdout: {stdout}\nstderr: {stderr}"
        );
    }
}

// ── Test 2: C201 InvalidProjectName ───────────────────────────────────────────

/// `ridge new` with a name that contains a path separator exits non-zero with
/// `C201` in stderr.
#[test]
fn new_invalid_name() {
    let td = tempfile::TempDir::new().expect("create tempdir");

    // "bad/name" contains a forward slash.
    ridge_cmd()
        .arg("new")
        .arg("bad/name")
        .current_dir(td.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("C201"));
}

/// Empty name also triggers C201.
#[test]
fn new_empty_name() {
    let td = tempfile::TempDir::new().expect("create tempdir");

    ridge_cmd()
        .arg("new")
        .arg("")
        .current_dir(td.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("C201"));
}

// ── Test 3: C202 DirectoryExists ─────────────────────────────────────────────

/// `ridge new my-app` when `my-app/` already exists exits non-zero with `C202`.
#[test]
fn new_directory_exists() {
    let td = tempfile::TempDir::new().expect("create tempdir");

    // Pre-create the directory.
    fs::create_dir(td.path().join("my-app")).expect("pre-create my-app/");

    ridge_cmd()
        .arg("new")
        .arg("my-app")
        .current_dir(td.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("C202"));
}

// ── Test 4: C203 ReservedName ─────────────────────────────────────────────────

/// `ridge new std` (exact casing) exits non-zero with `C203`.
#[test]
fn new_reserved_name_lower() {
    let td = tempfile::TempDir::new().expect("create tempdir");

    ridge_cmd()
        .arg("new")
        .arg("std")
        .current_dir(td.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("C203"));
}

/// `ridge new STD` (uppercase) exits non-zero with `C203` (case-insensitive).
#[test]
fn new_reserved_name_upper() {
    let td = tempfile::TempDir::new().expect("create tempdir");

    ridge_cmd()
        .arg("new")
        .arg("STD")
        .current_dir(td.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("C203"));
}
