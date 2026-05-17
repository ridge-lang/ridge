//! T8 — pkg-integration end-to-end test.
//!
//! Verifies the G5 contract: `compile_workspace` on a workspace with a git-tag
//! dependency populates the package cache.  Uses a local bare-repo fixture
//! (no network).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use ridge_driver::{compile_workspace, CompileOptions};

// ── Bare-repo fixture helpers ─────────────────────────────────────────────────

/// Minimal valid `ridge.toml` for the dependency package.
fn dep_toml(name: &str) -> String {
    format!(
        r#"[project]
name    = "{name}"
version = "0.1.0"
kind    = "library"
"#
    )
}

/// Build a working-tree repo, commit a `ridge.toml`, tag it, then
/// `git clone --bare` into a sibling directory.  Returns the bare-repo path.
///
/// Matches the pattern in `crates/ridge-pkg/tests/git_test.rs`.
fn make_bare_repo_with_tag(tmp: &TempDir, pkg_name: &str, tag: &str) -> PathBuf {
    let work_dir = tmp.path().join("dep_work");
    fs::create_dir_all(&work_dir).unwrap();

    run_git(&work_dir, &["init", "-b", "main"]);
    run_git(&work_dir, &["config", "user.email", "test@ridge"]);
    run_git(&work_dir, &["config", "user.name", "Test"]);

    fs::write(work_dir.join("ridge.toml"), dep_toml(pkg_name)).unwrap();
    run_git(&work_dir, &["add", "ridge.toml"]);
    run_git(&work_dir, &["commit", "-m", "init"]);
    run_git(&work_dir, &["tag", tag]);

    let bare_dir = tmp.path().join("dep_bare.git");
    run_git(
        tmp.path(),
        &[
            "clone",
            "--bare",
            work_dir.to_str().unwrap(),
            bare_dir.to_str().unwrap(),
        ],
    );

    bare_dir
}

/// Convert a local path to a `file://` URL, Windows-safe.
///
/// Matches the `file_url` helper in `crates/ridge-pkg/tests/git_test.rs`.
fn file_url(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let s = canonical.to_string_lossy();

    #[cfg(windows)]
    {
        let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
        format!("file:///{}", stripped.replace('\\', "/"))
    }
    #[cfg(not(windows))]
    {
        format!("file://{s}")
    }
}

/// Run a git command; panic on fixture failure (test scaffolding only).
fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git must be available in the test environment");
    assert!(status.success(), "git {args:?} failed with status {status}");
}

// ── Test ──────────────────────────────────────────────────────────────────────

/// G5 verification: `compile_workspace` on a workspace that declares a
/// `git = "...", tag = "v1.0"` dependency populates the package cache.
///
/// Uses a local bare-repo fixture — no network traffic.
#[test]
fn compile_workspace_with_git_tag_dep_populates_cache() {
    // ── 1. Build the bare-repo fixture ───────────────────────────────────────
    let repo_tmp = TempDir::new().unwrap();
    let bare = make_bare_repo_with_tag(&repo_tmp, "depfoo", "v1.0");
    let url = file_url(&bare);

    // ── 2. Build the workspace directory ─────────────────────────────────────
    let ws_tmp = TempDir::new().unwrap();
    let ws_root = ws_tmp.path();

    // Root ridge.toml — workspace manifest (no dependencies here, deps live in
    // the member project per the DoD wording).
    fs::write(
        ws_root.join("ridge.toml"),
        r#"[workspace]
name    = "ws"
version = "0.1.0"
members = ["app"]
"#,
    )
    .unwrap();

    // app/ridge.toml — project manifest with the git dependency.
    let app_dir = ws_root.join("app");
    fs::create_dir_all(&app_dir).unwrap();

    let app_toml = format!(
        r#"[project]
name    = "ws.app"
version = "0.1.0"
kind    = "library"

[dependencies]
depfoo = {{ git = "{url}", tag = "v1.0" }}
"#
    );
    fs::write(app_dir.join("ridge.toml"), &app_toml).unwrap();

    // app/src/Main.rg — trivial valid Ridge module.
    let src_dir = app_dir.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("Main.rg"), "pub fn answer () -> Int = 42\n").unwrap();

    // ── 3. Isolated cache directory ───────────────────────────────────────────
    let cache_tmp = TempDir::new().unwrap();
    let cache_dir = cache_tmp.path().to_owned();

    // ── 4. Compile ────────────────────────────────────────────────────────────
    let opts = CompileOptions::new(ws_root.to_owned()).with_cache_root(cache_dir.clone());

    let result = compile_workspace(opts);
    assert!(
        result.is_ok(),
        "compile_workspace failed: {:?}",
        result.err()
    );

    // ── 5. Assert cache was populated ─────────────────────────────────────────
    // The bare-repo URL `file:///<...>/dep_bare.git` is parsed by
    // `ridge_pkg::cache::parse_file_url` into `("_local", parent-dir-name,
    // "dep_bare")`.  The last path segment of `bare` (after stripping `.git`)
    // becomes `repo`; the second-to-last becomes `owner`.
    //
    // Cache layout: <cache_root>/git/_local/<owner>/<repo>/v1.0/ridge.toml
    let bare_canonical = bare.canonicalize().unwrap_or_else(|_| bare.clone());

    // `repo` = last segment without `.git` suffix.
    let repo_raw = bare_canonical
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let repo = repo_raw
        .strip_suffix(".git")
        .unwrap_or(&repo_raw)
        .to_owned();

    // `owner` = second-to-last path segment.
    let owner = bare_canonical
        .parent()
        .and_then(|p| p.file_name())
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let expected_ridge_toml = cache_dir
        .join("git")
        .join("_local")
        .join(&owner)
        .join(&repo)
        .join("v1.0")
        .join("ridge.toml");

    assert!(
        expected_ridge_toml.exists(),
        "cache not populated — expected ridge.toml at: {}",
        expected_ridge_toml.display()
    );
}
