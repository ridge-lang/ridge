//! Integration tests for `ridge init`.
//!
//! 4 tests:
//! 1. Happy path — empty tempdir; files created with NAME = dir name.
//! 2. `C204 DirectoryNotEmpty` — non-empty dir rejected; .git/ and .gitignore are allowed.
//! 3. `C205 CwdUnreadable` via root path — `Path::new("/").file_name()` is `None`.
//! 4. `C205 CwdUnreadable` via non-existent path — `read_dir` fails.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use std::fs;

// ── helper ────────────────────────────────────────────────────────────────────

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

// ── Test 1: happy path ────────────────────────────────────────────────────────

/// `ridge init` in an empty directory scaffolds the project using the
/// directory name as the project name.
///
/// The test also verifies that the generated project passes `ridge build`
/// (or exits with C004 when OTP is absent — same permissive gate as existing
/// build tests).
#[test]
fn init_happy_path() {
    // Create a subdirectory with a valid name that we can run `ridge init` in.
    let td = tempfile::TempDir::new().expect("create tempdir");
    let project_dir = td.path().join("hello-world");
    fs::create_dir(&project_dir).expect("create project dir");

    // Run `ridge init` inside the empty project directory.
    ridge_cmd()
        .arg("init")
        .current_dir(&project_dir)
        .assert()
        .success();

    // Verify files exist.
    assert!(
        project_dir.join("ridge.toml").exists(),
        "ridge.toml not created"
    );
    assert!(
        project_dir.join("src").join("Main.ridge").exists(),
        "src/Main.ridge not created"
    );
    assert!(
        project_dir.join("README.md").exists(),
        "README.md not created"
    );

    // The project name in ridge.toml should be the directory name.
    let toml_content = fs::read_to_string(project_dir.join("ridge.toml")).expect("read ridge.toml");
    assert!(
        toml_content.contains("hello-world"),
        "ridge.toml does not contain expected project name 'hello-world': {toml_content}"
    );
    assert!(
        !toml_content.contains("{NAME}"),
        "ridge.toml still contains unreplaced {{NAME}} placeholder"
    );

    // Smoke: `ridge build` inside the scaffolded project.
    let output = ridge_cmd()
        .arg("build")
        .current_dir(&project_dir)
        .output()
        .expect("ridge build spawn failed");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // C004 (no OTP) is acceptable.
        assert!(
            stderr.contains("C004")
                || stderr.contains("erlang")
                || stderr.contains("erl")
                || stderr.contains("erlc"),
            "unexpected ridge build failure in init-scaffolded project.\n\
             stdout: {stdout}\nstderr: {stderr}"
        );
    }
}

// ── Test 2: C204 DirectoryNotEmpty ────────────────────────────────────────────

/// `ridge init` in a directory that contains `dummy.txt` exits non-zero with
/// `C204`.
///
/// The test also asserts that a directory containing **only** `.git/` and
/// `.gitignore` is accepted (those two artefacts must NOT trigger C204).
#[test]
fn init_directory_not_empty() {
    let td = tempfile::TempDir::new().expect("create tempdir");
    let project_dir = td.path().join("my-proj");
    fs::create_dir(&project_dir).expect("create project dir");

    // Pre-populate with a foreign file.
    fs::write(project_dir.join("dummy.txt"), "not empty").expect("write dummy.txt");

    ridge_cmd()
        .arg("init")
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("C204"));

    // Now verify that .git/ and .gitignore do NOT trigger C204.
    let td2 = tempfile::TempDir::new().expect("create tempdir");
    let project_dir2 = td2.path().join("my-git-proj");
    fs::create_dir(&project_dir2).expect("create project dir 2");

    // Create .git/ directory and .gitignore file.
    fs::create_dir(project_dir2.join(".git")).expect("create .git/");
    fs::write(project_dir2.join(".gitignore"), "target/\n").expect("write .gitignore");

    // Should succeed (or at worst C004 from ridge build — but init itself should be fine).
    ridge_cmd()
        .arg("init")
        .current_dir(&project_dir2)
        .assert()
        .success();
}

// ── Tests 3 & 4: C205 CwdUnreadable ──────────────────────────────────────────

/// Validates `C205 CwdUnreadable` via `Path::new("/")`.
///
/// `Path::new("/").file_name()` returns `None` on every platform, which
/// exercises the first `CwdUnreadable` branch in `scaffold::init_project`
/// directly — no binary spawn needed.
#[test]
fn init_cwd_unreadable_via_root_path() {
    use std::path::Path;

    use ridge_cli::error::CliError;
    use ridge_cli::scaffold::init_project;

    // Path::new("/").file_name() == None on every platform → CwdUnreadable.
    let result = init_project(Path::new("/"));
    assert!(
        matches!(result, Err(CliError::CwdUnreadable)),
        "expected CliError::CwdUnreadable for path with no file_name(), got: {result:?}"
    );
}

/// Validates `C205 CwdUnreadable` via a non-existent path.
///
/// A path with a valid `file_name()` that does not exist on disk causes
/// `read_dir` to fail, exercising the second `CwdUnreadable` branch in
/// `scaffold::init_project` directly.
#[test]
fn init_cwd_unreadable_via_nonexistent_path() {
    use ridge_cli::error::CliError;
    use ridge_cli::scaffold::init_project;

    // A non-existent path with a valid file_name() — read_dir fails → CwdUnreadable.
    // Use tempfile to find a path guaranteed not to exist.
    let td = tempfile::TempDir::new().expect("create tempdir");
    let nonexistent = td.path().join("__ridge_does_not_exist_xyzzy");
    // Do NOT create the directory — we want read_dir to fail.

    let result = init_project(&nonexistent);
    assert!(
        matches!(result, Err(CliError::CwdUnreadable)),
        "expected CliError::CwdUnreadable for non-existent path, got: {result:?}"
    );
}
