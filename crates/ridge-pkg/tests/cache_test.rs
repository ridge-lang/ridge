//! Cache helper integration tests — §3.9 Track-A (2 tests).

// Test scaffolding legitimately uses unwrap on infallible fixtures.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

// ── Test 1: XDG_CACHE_HOME is respected on Unix ───────────────────────────────

/// Verify that `cache_root()` honours `$XDG_CACHE_HOME` on Linux.
///
/// We temporarily set `XDG_CACHE_HOME` to a temp dir and confirm the returned
/// path uses it.  Restricted to Linux because:
/// - Windows uses `LOCALAPPDATA`, not `XDG_CACHE_HOME`.
/// - macOS uses `~/Library/Caches` (Apple convention) via the `directories`
///   crate and does NOT consult `XDG_CACHE_HOME`.
#[test]
#[cfg(target_os = "linux")]
fn cache_root_respects_xdg_cache_home_on_unix() {
    use std::env;
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let xdg_root = tmp.path().to_owned();

    // Safety: single-threaded test; env-var mutation is acceptable here.
    env::set_var("XDG_CACHE_HOME", &xdg_root);

    let root = ridge_pkg::cache_root().expect("cache_root should succeed");

    // The returned path should start with our override.
    assert!(
        root.starts_with(&xdg_root),
        "cache_root {root:?} should start with XDG_CACHE_HOME {xdg_root:?}"
    );

    // Clean up so we do not pollute other tests.
    env::remove_var("XDG_CACHE_HOME");
}

// ── Test 2: cache layout path has the correct shape ──────────────────────────

/// Verify that `git_cache_path` produces `<root>/git/<host>/<owner>/<repo>/<rev>`.
#[test]
fn cache_layout_path_is_correct() {
    let root = PathBuf::from("/tmp/ridge-test-cache");
    let path = ridge_pkg::cache::git_cache_path(&root, "github.com", "acme", "mylib", "v2.0");

    // Collect components to be OS-separator-agnostic.
    let components: Vec<_> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();

    // We only care that the relative tail is git/github.com/acme/mylib/v2.0.
    let git_idx = components.iter().position(|c| c == "git");
    assert!(
        git_idx.is_some(),
        "path should contain 'git' component: {path:?}"
    );
    let idx = git_idx.unwrap();
    assert_eq!(
        components.get(idx + 1).map(String::as_str),
        Some("github.com")
    );
    assert_eq!(components.get(idx + 2).map(String::as_str), Some("acme"));
    assert_eq!(components.get(idx + 3).map(String::as_str), Some("mylib"));
    assert_eq!(components.get(idx + 4).map(String::as_str), Some("v2.0"));
}
