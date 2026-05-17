//! Tests for `P010 PkgVersionDepUnsupported` — registry-based version deps
//! are rejected in 0.1.0.

// Test scaffolding legitimately uses unwrap/expect on infallible fixtures.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;

use tempfile::TempDir;

/// A `workspace = true` dep that maps to a `version = "1.0"` shared dep must
/// return `P010`, not a confusing `P101` path-resolution error.
#[test]
fn version_dep_via_workspace_returns_p010() {
    let tmp = TempDir::new().unwrap();

    // Workspace manifest with a shared version-only dep.
    let ws_toml_path = tmp.path().join("ridge.toml");
    fs::write(
        &ws_toml_path,
        r#"[workspace]
name    = "test-ws"
version = "0.1.0"
members = ["myapp"]

[workspace.dependencies]
mylib = { version = "1.0" }
"#,
    )
    .unwrap();

    // Project that inherits the workspace dep.
    let app_dir = tmp.path().join("myapp");
    fs::create_dir_all(&app_dir).unwrap();
    let app_toml_path = app_dir.join("ridge.toml");
    fs::write(
        &app_toml_path,
        r#"[project]
name    = "myapp"
version = "0.1.0"
kind    = "app"
entry   = "src/main.rdg"

[dependencies]
mylib = { workspace = true }
"#,
    )
    .unwrap();

    let ws_src = fs::read_to_string(&ws_toml_path).unwrap();
    let workspace = ridge_manifest::parse_workspace(&ws_src, &ws_toml_path).unwrap();

    let app_src = fs::read_to_string(&app_toml_path).unwrap();
    let app_manifest = ridge_manifest::parse_project(&app_src, &app_toml_path).unwrap();

    let cache_tmp = TempDir::new().unwrap();

    let result = ridge_pkg::resolve_dependencies(&workspace, &app_manifest, cache_tmp.path());
    let Err(err) = result else {
        panic!("expected Err(P010) for version-only workspace dep, got Ok")
    };

    assert_eq!(
        err.code(),
        "P010",
        "expected P010 for version-only workspace dep, got: {err}"
    );
}
