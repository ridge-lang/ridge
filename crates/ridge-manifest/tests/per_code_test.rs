//! One case per diagnostic code `ridge-manifest`'s parser can report, plus the
//! happy paths for both manifest kinds.
//!
//! These cases were the `ridge-resolve` copy's own unit tests. That copy is the
//! redundant one — the CLI, the LSP and the driver all parse through
//! `ridge-manifest` — and it carried a systematic per-code suite this crate had
//! none of. Moving them here tests the parser that actually runs, and is what
//! makes retiring the other copy something other than a deletion of coverage.
//!
//! The `*_deferred_*` cases assert the opposite of the others: those codes are
//! reported by a later pass, so a manifest that would earn one has to parse
//! cleanly here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ridge_ast::Capability;
use ridge_manifest as rm;
use ridge_manifest::ProjectKind;

const DUMMY_PATH: &str = "/workspace/ridge.toml";
const DUMMY_PROJ_PATH: &str = "/workspace/apps/myapp/ridge.toml";

fn wp() -> &'static Path {
    Path::new(DUMMY_PATH)
}

fn pp() -> &'static Path {
    Path::new(DUMMY_PROJ_PATH)
}

// ── M001 TomlParseFailed ──────────────────────────────────────────────────

#[test]
fn m001_workspace_invalid_toml() {
    let toml = include_str!("fixtures/manifest/M001_workspace_invalid_toml.toml");
    let result = rm::parse_workspace(toml, wp());
    let err = result.unwrap_err();
    assert_eq!(err.code(), "M001", "expected M001, got: {err:?}");
}

#[test]
fn m001_project_invalid_toml() {
    let toml = include_str!("fixtures/manifest/M001_project_invalid_toml.toml");
    let result = rm::parse_project(toml, pp());
    let err = result.unwrap_err();
    assert_eq!(err.code(), "M001");
}

// ── M002 MissingWorkspaceTable ────────────────────────────────────────────

#[test]
fn m002_missing_workspace_table() {
    let toml = include_str!("fixtures/manifest/M002_missing_workspace_table.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M002");
}

// ── M003 MissingProjectTable ──────────────────────────────────────────────

#[test]
fn m003_missing_project_table() {
    let toml = include_str!("fixtures/manifest/M003_missing_project_table.toml");
    let err = rm::parse_project(toml, pp()).unwrap_err();
    assert_eq!(err.code(), "M003");
}

// ── M004 MemberWithoutProjectManifest — deferred to filesystem expansion ──

#[test]
fn m004_deferred_to_t3() {
    // M004 fires during filesystem expansion, not manifest parsing.
    // Manifest parsing never emits M004. This fixture documents that a well-formed
    // workspace manifest with members globs parses successfully; filesystem
    // expansion validates that each expanded member directory contains a ridge.toml.
    let toml = include_str!("fixtures/manifest/M004_deferred_member_without_manifest.toml");
    let result = rm::parse_workspace(toml, wp());
    assert!(
        result.is_ok(),
        "manifest parsing must not emit M004; filesystem validation is deferred"
    );
}

// ── M005 BadMemberGlob ────────────────────────────────────────────────────

#[test]
fn m005_invalid_member_glob() {
    let toml = include_str!("fixtures/manifest/M005_invalid_member_glob.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M005");
}

#[test]
fn m005_empty_member_glob() {
    let toml = include_str!("fixtures/manifest/M005_empty_member_glob.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M005");
}

// ── M006 MissingRequiredField ─────────────────────────────────────────────

#[test]
fn m006_workspace_missing_name() {
    let toml = include_str!("fixtures/manifest/M006_workspace_missing_name.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M006");
    assert!(err.to_string().contains("name"));
}

#[test]
fn m006_workspace_missing_version() {
    let toml = include_str!("fixtures/manifest/M006_workspace_missing_version.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M006");
    assert!(err.to_string().contains("version"));
}

#[test]
fn m006_workspace_missing_members() {
    let toml = include_str!("fixtures/manifest/M006_workspace_missing_members.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M006");
    assert!(err.to_string().contains("members"));
}

#[test]
fn m006_project_missing_kind() {
    let toml = include_str!("fixtures/manifest/M006_project_missing_kind.toml");
    let err = rm::parse_project(toml, pp()).unwrap_err();
    assert_eq!(err.code(), "M006");
    assert!(err.to_string().contains("kind"));
}

#[test]
fn m006_app_missing_entry() {
    let toml = include_str!("fixtures/manifest/M006_app_missing_entry.toml");
    let err = rm::parse_project(toml, pp()).unwrap_err();
    assert_eq!(err.code(), "M006");
    assert!(err.to_string().contains("entry"));
}

// ── M007 InvalidProjectKind ───────────────────────────────────────────────

#[test]
fn m007_invalid_kind() {
    let toml = include_str!("fixtures/manifest/M007_invalid_kind.toml");
    let err = rm::parse_project(toml, pp()).unwrap_err();
    assert_eq!(err.code(), "M007");
}

// ── M008 InvalidForbidRule ────────────────────────────────────────────────

#[test]
fn m008_missing_to_field() {
    let toml = include_str!("fixtures/manifest/M008_missing_to_field.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M008");
}

#[test]
fn m008_missing_from_field() {
    let toml = include_str!("fixtures/manifest/M008_missing_from_field.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M008");
}

// ── M009 InvalidDependencyKind ────────────────────────────────────────────

#[test]
fn m009_workspace_dep_no_shape() {
    // A dep entry with none of the recognised shape keys → M009.
    let toml = include_str!("fixtures/manifest/M009_workspace_dep_no_shape.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M009");
}

#[test]
fn m009_project_dep_no_shape() {
    let toml = include_str!("fixtures/manifest/M009_project_dep_no_shape.toml");
    let err = rm::parse_project(toml, pp()).unwrap_err();
    assert_eq!(err.code(), "M009");
}

// ── M010 DuplicateProjectName — deferred to workspace integration ─────────

#[test]
fn m010_deferred_to_t3() {
    // M010 fires in the workspace integration layer when multiple project manifests
    // are collected and their names compared.  Manifest parsing validates only a
    // single project manifest at a time and cannot detect duplicates.
    let toml = include_str!("fixtures/manifest/M010_deferred_duplicate_project_name.toml");
    let result = rm::parse_project(toml, pp());
    assert!(
        result.is_ok(),
        "manifest parsing must not emit M010; duplicate detection is deferred to workspace integration"
    );
}

// ── M011 InvalidCapabilityName ────────────────────────────────────────────

#[test]
fn m011_unknown_capability_workspace() {
    let toml = include_str!("fixtures/manifest/M011_unknown_capability_workspace.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M011");
}

#[test]
fn m011_unknown_capability_project() {
    let toml = include_str!("fixtures/manifest/M011_unknown_capability_project.toml");
    let err = rm::parse_project(toml, pp()).unwrap_err();
    assert_eq!(err.code(), "M011");
}

#[test]
fn db_capability_parses_in_manifest() {
    let toml = "\
[workspace]
name = \"acme\"
version = \"0.1.0\"
members = [\"apps/*\"]

[workspace.capabilities]
deny = [\"db\"]
";
    let ws = rm::parse_workspace(toml, wp()).expect("manifest should parse");
    assert!(
        ws.capabilities_deny.contains(&ridge_ast::Capability::Db),
        "deny list should contain the `db` capability, got {:?}",
        ws.capabilities_deny
    );
}

// ── M012 CycleInDependencies — deferred to import resolution ─────────────

#[test]
fn m012_deferred_to_t7() {
    // M012 requires the full workspace dependency graph to detect cycles.
    // Manifest parsing only handles individual manifests and cannot detect cycles.
    let toml = include_str!("fixtures/manifest/M012_deferred_dep_cycle.toml");
    let result = rm::parse_project(toml, pp());
    assert!(
        result.is_ok(),
        "manifest parsing must not emit M012; cycle detection is deferred to import resolution"
    );
}

// ── M013 UnknownWorkspaceMember — deferred to import resolution ───────────

#[test]
fn m013_deferred_to_t7() {
    // M013 requires cross-project validation; manifest parsing handles single manifests.
    let toml = include_str!("fixtures/manifest/M013_deferred_unknown_member.toml");
    let result = rm::parse_project(toml, pp());
    assert!(
        result.is_ok(),
        "manifest parsing must not emit M013; unknown-member validation is deferred"
    );
}

// ── M014 ProjectExportPatternInvalid ──────────────────────────────────────

#[test]
fn m014_invalid_export_pattern() {
    let toml = include_str!("fixtures/manifest/M014_invalid_export_pattern.toml");
    let err = rm::parse_project(toml, pp()).unwrap_err();
    assert_eq!(err.code(), "M014");
}

// ── M015 WorkspaceDependencyAbsent — deferred to import resolution ────────

#[test]
fn m015_deferred_to_t7() {
    // M015 requires the workspace manifest to be available for cross-validation.
    // Manifest parsing handles the project manifest in isolation.
    let toml = include_str!("fixtures/manifest/M015_deferred_workspace_dep_absent.toml");
    let result = rm::parse_project(toml, pp());
    assert!(
        result.is_ok(),
        "manifest parsing must not emit M015; workspace-dep absence is deferred"
    );
}

// ── M016 GitRevConflict ───────────────────────────────────────────────────

#[test]
fn m016_git_tag_and_branch_conflict() {
    let toml = include_str!("fixtures/manifest/M016_git_rev_conflict.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M016");
}

// ── M017 RelativePathEscapesWorkspace — basic structural test ────────────

#[test]
fn m017_path_escaping_workspace_parses() {
    // Full escape detection requires workspace-root context (filesystem expansion / import resolution).
    // Manifest parsing stores the path as-is; the emit-or-not decision is deferred.
    let toml = include_str!("fixtures/manifest/M017_deferred_path_escapes_workspace.toml");
    let result = rm::parse_project(toml, pp());
    assert!(
        result.is_ok(),
        "manifest parsing does not emit M017 without workspace-root context"
    );
}

// ── M018 HexDependencyUsedIn010 ──────────────────────────────────────────

#[test]
fn m018_hex_dep_workspace() {
    let toml = include_str!("fixtures/manifest/M018_hex_dep_workspace.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M018");
}

#[test]
fn m018_hex_dep_project() {
    let toml = include_str!("fixtures/manifest/M018_hex_dep_project.toml");
    let err = rm::parse_project(toml, pp()).unwrap_err();
    assert_eq!(err.code(), "M018");
}

// ── M019 UnknownManifestKey ───────────────────────────────────────────────

#[test]
fn m019_unknown_workspace_key() {
    let toml = include_str!("fixtures/manifest/M019_unknown_workspace_key.toml");
    let err = rm::parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(err.code(), "M019");
}

// ── Happy-path workspace ──────────────────────────────────────────────────

#[test]
fn happy_path_workspace_minimal() {
    let toml = include_str!("fixtures/manifest/happy_workspace_minimal.toml");
    let ws = rm::parse_workspace(toml, wp()).unwrap();
    assert_eq!(ws.name, "acme-platform");
    assert_eq!(ws.version, "0.1.0");
    assert_eq!(ws.members_globs.len(), 3);
    assert!(ws.forbid_rules.is_empty());
    assert!(ws.capabilities_deny.is_empty());
}

#[test]
fn happy_path_workspace_full() {
    let toml = include_str!("fixtures/manifest/happy_workspace_full.toml");
    let ws = rm::parse_workspace(toml, wp()).unwrap();
    assert_eq!(ws.name, "acme-platform");
    assert_eq!(ws.dependencies.len(), 3);
    assert_eq!(ws.forbid_rules.len(), 2);
    assert_eq!(ws.capabilities_deny.len(), 1);
    assert!(matches!(ws.capabilities_deny[0], Capability::Ffi));
}

// ── Happy-path project ────────────────────────────────────────────────────

#[test]
fn happy_path_project_library() {
    let toml = include_str!("fixtures/manifest/happy_project_library.toml");
    let proj = rm::parse_project(toml, pp()).unwrap();
    assert_eq!(proj.name, "acme.domain");
    assert_eq!(proj.version, "0.1.0");
    assert!(matches!(proj.kind, ProjectKind::Library));
    assert_eq!(proj.exports_public.len(), 2);
    assert_eq!(proj.exports_internal.len(), 0);
    assert_eq!(proj.dependencies.len(), 4);
    assert!(matches!(
        proj.capabilities_allow,
        Some(ref v) if v.len() == 2
    ));
}

#[test]
fn happy_path_project_app_with_entry() {
    let toml = include_str!("fixtures/manifest/happy_project_app_with_entry.toml");
    let proj = rm::parse_project(toml, pp()).unwrap();
    assert!(matches!(proj.kind, ProjectKind::App));
}

#[test]
fn project_src_root_default_is_src() {
    let toml = include_str!("fixtures/manifest/happy_project_library_minimal.toml");
    let proj = rm::parse_project(toml, pp()).unwrap();
    assert!(proj.src_root.ends_with("src"));
}

#[test]
fn project_src_root_custom() {
    let toml = include_str!("fixtures/manifest/happy_project_custom_src_root.toml");
    let proj = rm::parse_project(toml, pp()).unwrap();
    assert!(proj.src_root.ends_with("source"));
}

#[test]
fn capability_inherit_none_when_absent() {
    let toml = include_str!("fixtures/manifest/happy_project_library_minimal.toml");
    let proj = rm::parse_project(toml, pp()).unwrap();
    assert!(
        proj.capabilities_allow.is_none(),
        "absent [capabilities].allow should produce None (inherit from workspace)"
    );
}
