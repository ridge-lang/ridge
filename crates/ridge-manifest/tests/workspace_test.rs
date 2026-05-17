//! Happy-path workspace manifest tests — 5 tests.
//!
//! Covers the `parse_workspace` function against the five workspace fixture
//! shapes described in §3.11.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ridge_manifest::parse_workspace;

const DUMMY_WS_PATH: &str = "/workspace/ridge.toml";

fn wp() -> &'static Path {
    Path::new(DUMMY_WS_PATH)
}

/// T1 §3.11 — happy workspace #1: single-project workspace.
#[test]
fn ws_happy_single_project() {
    let toml = include_str!("fixtures/ws_single_project.toml");
    let ws = parse_workspace(toml, wp()).expect("single-project workspace must parse");
    assert_eq!(ws.name, "hello-world");
    assert_eq!(ws.version, "0.1.0");
    assert_eq!(ws.members_globs, vec!["app"]);
    assert!(ws.forbid_rules.is_empty());
    assert!(ws.dependencies.is_empty());
    assert!(ws.capabilities_deny.is_empty());
}

/// T1 §3.11 — happy workspace #2: multi-member glob workspace.
#[test]
fn ws_happy_multi_member() {
    let toml = include_str!("fixtures/ws_multi_member.toml");
    let ws = parse_workspace(toml, wp()).expect("multi-member workspace must parse");
    assert_eq!(ws.name, "acme-platform");
    assert_eq!(ws.members_globs.len(), 3);
    assert!(ws.members_globs.contains(&"apps/*".to_owned()));
    assert!(ws.members_globs.contains(&"libs/*".to_owned()));
    assert!(ws.members_globs.contains(&"tests/*".to_owned()));
}

/// T1 §3.11 — happy workspace #3: workspace with forbid rules.
#[test]
fn ws_happy_with_forbid_rules() {
    let toml = include_str!("fixtures/ws_with_forbid_rules.toml");
    let ws = parse_workspace(toml, wp()).expect("workspace with forbid rules must parse");
    assert_eq!(ws.forbid_rules.len(), 2);
    assert!(ws.forbid_rules[0].from.matches("acme.domain.Foo"));
    assert!(ws.forbid_rules[0].to.matches("acme.infra.Bar"));
    assert!(!ws.forbid_rules[0].from.matches("acme.infra.Baz"));
}

/// T1 §3.11 — happy workspace #4: workspace with shared dependencies.
#[test]
fn ws_happy_with_deps() {
    let toml = include_str!("fixtures/ws_with_deps.toml");
    let ws = parse_workspace(toml, wp()).expect("workspace with deps must parse");
    assert_eq!(ws.dependencies.len(), 3);
    assert_eq!(ws.version, "0.2.0");
}

/// T1 §3.11 — happy workspace #5: workspace with capabilities.deny.
#[test]
fn ws_happy_with_capabilities() {
    use ridge_ast::Capability;
    let toml = include_str!("fixtures/ws_with_capabilities.toml");
    let ws = parse_workspace(toml, wp()).expect("workspace with capabilities must parse");
    assert_eq!(ws.capabilities_deny.len(), 2);
    assert!(ws.capabilities_deny.contains(&Capability::Ffi));
    assert!(ws.capabilities_deny.contains(&Capability::Proc));
}
