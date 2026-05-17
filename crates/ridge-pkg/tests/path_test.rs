//! Path resolver integration tests — §3.9 Track-A (5 tests).

// Test scaffolding legitimately uses unwrap/expect on infallible fixtures.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

// ── Helper ────────────────────────────────────────────────────────────────────

/// Minimal valid `ridge.toml` for a library project.
fn minimal_library_toml(name: &str) -> String {
    format!(
        r#"[project]
name    = "{name}"
version = "0.1.0"
kind    = "library"
"#
    )
}

/// Create a dep directory with a valid `ridge.toml` inside `parent`.
fn make_dep_dir(parent: &TempDir, dir_name: &str, pkg_name: &str) -> PathBuf {
    let dep_dir = parent.path().join(dir_name);
    fs::create_dir_all(&dep_dir).unwrap();
    fs::write(dep_dir.join("ridge.toml"), minimal_library_toml(pkg_name)).unwrap();
    dep_dir
}

/// Write a minimal consumer `ridge.toml` at `dir/ridge.toml` returning the
/// directory.
fn make_consumer(parent: &TempDir, dir_name: &str, pkg_name: &str) -> PathBuf {
    let dir = parent.path().join(dir_name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ridge.toml"), minimal_library_toml(pkg_name)).unwrap();
    dir
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Test 1: a valid relative path dep resolves successfully.
#[test]
fn path_resolves_valid_relative_dep() {
    let tmp = TempDir::new().unwrap();

    make_dep_dir(&tmp, "mylib", "mylib");
    let consumer_dir = make_consumer(&tmp, "myapp", "myapp");

    let (source_root, manifest) =
        ridge_pkg::path::resolve_path_dep(&consumer_dir, std::path::Path::new("../mylib"))
            .expect("should resolve");

    assert!(source_root.exists());
    assert_eq!(manifest.name, "mylib");
}

/// Test 2: a missing path dep returns P101.
#[test]
fn path_missing_dep_returns_p101() {
    let tmp = TempDir::new().unwrap();
    let consumer_dir = make_consumer(&tmp, "consumer", "consumer");

    let err =
        ridge_pkg::path::resolve_path_dep(&consumer_dir, std::path::Path::new("../does_not_exist"))
            .unwrap_err();

    assert_eq!(err.code(), "P101", "expected P101, got: {err}");
}

/// Test 3: a path that exists but has no `ridge.toml` returns P101.
#[test]
fn path_to_dir_without_ridge_toml_returns_p101() {
    let tmp = TempDir::new().unwrap();
    let consumer_dir = make_consumer(&tmp, "consumer", "consumer");

    // Create a sibling directory with no ridge.toml.
    let empty_dir = tmp.path().join("empty");
    fs::create_dir_all(&empty_dir).unwrap();

    let err = ridge_pkg::path::resolve_path_dep(&consumer_dir, std::path::Path::new("../empty"))
        .unwrap_err();

    assert_eq!(err.code(), "P101", "expected P101, got: {err}");
}

/// Test 4: an absolute path resolves correctly.
#[test]
fn path_resolves_absolute_path() {
    let tmp = TempDir::new().unwrap();

    let dep_dir = make_dep_dir(&tmp, "abslib", "abslib");
    let consumer_dir = make_consumer(&tmp, "consumer", "consumer");

    // Pass the absolute path directly.
    let (source_root, manifest) = ridge_pkg::path::resolve_path_dep(&consumer_dir, &dep_dir)
        .expect("should resolve absolute path");

    assert!(source_root.exists());
    assert_eq!(manifest.name, "abslib");
}

/// Test 6 (Issue #1): transitive cycle A → B → A returns P006.
///
/// Two manifests in a tempdir:
/// - A's `ridge.toml` lists a path dep on B.
/// - B's `ridge.toml` lists a path dep on A.
///
/// Calling `resolve_dependencies` on A must detect the transitive cycle and
/// return `P006 PkgDependencyCycle`.
#[test]
fn transitive_cycle_a_b_a_returns_p006() {
    let tmp = TempDir::new().unwrap();

    let a_dir = tmp.path().join("a");
    let b_dir = tmp.path().join("b");
    fs::create_dir_all(&a_dir).unwrap();
    fs::create_dir_all(&b_dir).unwrap();

    // A depends on B.
    fs::write(
        a_dir.join("ridge.toml"),
        r#"[project]
name    = "a"
version = "0.1.0"
kind    = "library"

[dependencies]
b = { path = "../b" }
"#,
    )
    .unwrap();

    // B depends on A — closing the cycle.
    fs::write(
        b_dir.join("ridge.toml"),
        r#"[project]
name    = "b"
version = "0.1.0"
kind    = "library"

[dependencies]
a = { path = "../a" }
"#,
    )
    .unwrap();

    // Minimal workspace manifest at the tmp root.
    let ws_toml_path = tmp.path().join("ridge.toml");
    fs::write(
        &ws_toml_path,
        r#"[workspace]
name    = "test-ws"
version = "0.1.0"
members = ["a", "b"]
"#,
    )
    .unwrap();

    let ws_src = fs::read_to_string(&ws_toml_path).unwrap();
    let workspace = ridge_manifest::parse_workspace(&ws_src, &ws_toml_path).unwrap();

    let a_toml_path = a_dir.join("ridge.toml");
    let a_src = fs::read_to_string(&a_toml_path).unwrap();
    let a_manifest = ridge_manifest::parse_project(&a_src, &a_toml_path).unwrap();

    let cache_tmp = TempDir::new().unwrap();

    let result = ridge_pkg::resolve_dependencies(&workspace, &a_manifest, cache_tmp.path());
    let Err(err) = result else {
        panic!("expected Err(P006) for transitive cycle A→B→A, got Ok")
    };

    assert_eq!(
        err.code(),
        "P006",
        "expected P006 for transitive cycle A→B→A, got: {err}"
    );
}

/// Test 5: a path with `..` traversal resolves correctly (permitted in 0.1.0,
/// §3.9 — "keep permissive for 0.1.0 but document").
#[test]
fn path_with_dotdot_traversal_resolves_correctly() {
    let tmp = TempDir::new().unwrap();

    // Layout:  <tmp>/
    //            deep/consumer/   (consumer)
    //            sibling/         (dep)
    let deep = tmp.path().join("deep");
    fs::create_dir_all(&deep).unwrap();
    let _consumer_dir = make_consumer(&TempDir::new().unwrap(), "x", "x");

    // Use the real consumer inside deep/consumer.
    let consumer_deep = deep.join("consumer");
    fs::create_dir_all(&consumer_deep).unwrap();
    fs::write(
        consumer_deep.join("ridge.toml"),
        minimal_library_toml("consumer"),
    )
    .unwrap();

    let sibling = tmp.path().join("sibling");
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("ridge.toml"), minimal_library_toml("sibling")).unwrap();

    // From deep/consumer, `../../sibling` traverses two levels up.
    let (source_root, manifest) =
        ridge_pkg::path::resolve_path_dep(&consumer_deep, std::path::Path::new("../../sibling"))
            .expect("dotdot traversal should be accepted in 0.1.0");

    assert!(source_root.exists());
    assert_eq!(manifest.name, "sibling");
}
