//! Happy-path project manifest tests — 5 tests.
//!
//! Covers the `parse_project` function against the five project fixture
//! shapes described in §3.11.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ridge_manifest::{parse_project, ProjectKind};

const DUMMY_PROJ_PATH: &str = "/workspace/libs/domain/ridge.toml";

fn pp() -> &'static Path {
    Path::new(DUMMY_PROJ_PATH)
}

/// T1 §3.11 — happy project #1: library with exports, deps, capabilities.
#[test]
fn proj_happy_library() {
    use ridge_ast::Capability;
    let toml = include_str!("fixtures/proj_library.toml");
    let proj = parse_project(toml, pp()).expect("library project must parse");
    assert_eq!(proj.name, "acme.domain");
    assert_eq!(proj.version, "0.1.0");
    assert!(matches!(proj.kind, ProjectKind::Library));
    assert_eq!(proj.exports_public.len(), 2);
    assert_eq!(proj.exports_internal.len(), 0);
    assert_eq!(proj.dependencies.len(), 4);
    assert!(
        matches!(&proj.capabilities_allow, Some(v) if v.len() == 2),
        "should have 2 allowed capabilities"
    );
    let allow = proj.capabilities_allow.as_ref().unwrap();
    assert!(allow.contains(&Capability::Io));
    assert!(allow.contains(&Capability::Net));
}

/// T1 §3.11 — happy project #2: app with entry point.
#[test]
fn proj_happy_app() {
    let toml = include_str!("fixtures/proj_app.toml");
    let proj = parse_project(toml, pp()).expect("app project must parse");
    assert!(matches!(proj.kind, ProjectKind::App));
    assert!(proj.entry.is_some(), "app must have an entry point");
    assert!(proj
        .entry
        .as_ref()
        .unwrap()
        .to_string_lossy()
        .contains("Main.ridge"));
}

/// T1 §3.11 — happy project #3: service with entry point.
#[test]
fn proj_happy_service() {
    let toml = include_str!("fixtures/proj_service.toml");
    let proj = parse_project(toml, pp()).expect("service project must parse");
    assert!(matches!(proj.kind, ProjectKind::Service));
    assert!(proj.entry.is_some(), "service must have an entry point");
}

/// T1 §3.11 — happy project #4: test project (no entry required).
#[test]
fn proj_happy_test_project() {
    let toml = include_str!("fixtures/proj_test.toml");
    let proj = parse_project(toml, pp()).expect("test project must parse");
    assert!(matches!(proj.kind, ProjectKind::Test));
    assert!(proj.entry.is_none(), "test project must not require entry");
    assert!(
        proj.capabilities_allow.is_none(),
        "absent [capabilities].allow → None (inherit from workspace)"
    );
}

/// T1 §3.11 — happy project #5: project with public and internal exports.
#[test]
fn proj_happy_with_exports() {
    let toml = include_str!("fixtures/proj_with_exports.toml");
    let proj = parse_project(toml, pp()).expect("project with exports must parse");
    assert_eq!(proj.exports_public.len(), 2);
    assert_eq!(proj.exports_internal.len(), 1);
    // Patterns are "Types.*" and "Internal.*" — match direct children.
    assert!(proj.exports_public[0].matches("Types.Foo"));
    assert!(proj.exports_internal[0].matches("Internal.Bar"));
}
