//! Error-case and bidirectional manifest tests — 6 error + 1 bidirectional = 7 tests.
//!
//! Covers each M00x error code described in §3.11 and a bidirectional round-trip
//! that parses a workspace, expands its members glob against on-disk reality, and
//! validates each member project manifest.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::path::Path;

use ridge_manifest::{parse_project, parse_workspace};

const DUMMY_WS_PATH: &str = "/workspace/ridge.toml";
const DUMMY_PROJ_PATH: &str = "/workspace/libs/domain/ridge.toml";

fn wp() -> &'static Path {
    Path::new(DUMMY_WS_PATH)
}

fn pp() -> &'static Path {
    Path::new(DUMMY_PROJ_PATH)
}

// ── Error fixtures ────────────────────────────────────────────────────────────

/// T1 §3.11 error #1 — M001 TomlParseFailed: malformed TOML syntax.
#[test]
fn err_m001_toml_syntax() {
    let toml = include_str!("fixtures/err_m001_toml_syntax.toml");
    let err = parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(
        err.code(),
        "M001",
        "malformed TOML must produce M001, got: {err}"
    );
}

/// T1 §3.11 error #2 — M002 MissingWorkspaceTable: no [workspace] key.
#[test]
fn err_m002_missing_workspace_table() {
    let toml = include_str!("fixtures/err_m002_missing_workspace_table.toml");
    let err = parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(
        err.code(),
        "M002",
        "missing [workspace] must produce M002, got: {err}"
    );
}

/// T1 §3.11 error #3 — M003 MissingProjectTable: no [project] key.
#[test]
fn err_m003_missing_project_table() {
    let toml = include_str!("fixtures/err_m003_missing_project_table.toml");
    let err = parse_project(toml, pp()).unwrap_err();
    assert_eq!(
        err.code(),
        "M003",
        "missing [project] must produce M003, got: {err}"
    );
}

/// T1 §3.11 error #4 — M006 MissingRequiredField: workspace missing `name`.
#[test]
fn err_m006_missing_required_field() {
    let toml = include_str!("fixtures/err_m006_missing_field.toml");
    let err = parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(
        err.code(),
        "M006",
        "missing required field must produce M006, got: {err}"
    );
    assert!(
        err.to_string().contains("name"),
        "error message must mention the missing field"
    );
}

/// T1 §3.11 error #5 — M005 BadMemberGlob: invalid glob pattern in `members`.
#[test]
fn err_m005_bad_member_glob() {
    let toml = include_str!("fixtures/err_m005_bad_glob.toml");
    let err = parse_workspace(toml, wp()).unwrap_err();
    assert_eq!(
        err.code(),
        "M005",
        "invalid member glob must produce M005, got: {err}"
    );
}

/// T1 §3.11 error #6 — M007 InvalidProjectKind: unrecognised `kind` value.
#[test]
fn err_m007_invalid_project_kind() {
    let toml = include_str!("fixtures/err_m007_invalid_kind.toml");
    let err = parse_project(toml, pp()).unwrap_err();
    assert_eq!(
        err.code(),
        "M007",
        "invalid kind must produce M007, got: {err}"
    );
}

// ── Bidirectional test ────────────────────────────────────────────────────────

/// T1 §3.11 bidirectional — parse workspace manifest and all member project
/// manifests, then cross-validate that every member has a distinct project name.
///
/// Uses the on-disk fixture workspace at `tests/fixtures/bidir_workspace/`.
/// The workspace declares `members = ["libs/*"]`; the fixture directory contains
/// two member directories (`libs/domain`, `libs/shared`), each with a
/// `ridge.toml`.
#[test]
fn bidir_workspace_and_members_consistent() {
    use globset::{Glob, GlobSetBuilder};
    use std::collections::HashSet;

    // Locate the fixture workspace directory relative to CARGO_MANIFEST_DIR.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ws_dir = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("bidir_workspace");
    let ws_toml_path = ws_dir.join("ridge.toml");

    // Parse workspace manifest.
    let ws_src =
        std::fs::read_to_string(&ws_toml_path).expect("bidir_workspace/ridge.toml must exist");
    let ws =
        parse_workspace(&ws_src, &ws_toml_path).expect("bidir_workspace/ridge.toml must parse");

    assert_eq!(ws.name, "bidir-test");
    assert_eq!(ws.members_globs.len(), 1);

    // Expand the members glob against the on-disk directory tree.
    let mut builder = GlobSetBuilder::new();
    for pattern in &ws.members_globs {
        builder.add(Glob::new(pattern).expect("glob must compile"));
    }
    let glob_set = builder.build().expect("glob set must build");

    // Walk the workspace root to find matching member directories.
    // The members glob (e.g. "libs/*") is matched against relative paths from
    // the workspace root.  We do a two-level walk: top-level entries produce
    // candidate relative paths like "libs/domain" which are matched against
    // the compiled glob set.
    let mut member_manifests: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(top_entries) = ws_dir.read_dir() {
        for top in top_entries.flatten() {
            if !top.path().is_dir() {
                continue;
            }
            let top_name = top.file_name().to_string_lossy().into_owned();
            if let Ok(sub_entries) = top.path().read_dir() {
                for sub in sub_entries.flatten() {
                    if !sub.path().is_dir() {
                        continue;
                    }
                    let sub_name = sub.file_name().to_string_lossy().into_owned();
                    // Build the relative path as "top_name/sub_name".
                    let rel = format!("{top_name}/{sub_name}");
                    if glob_set.is_match(&rel) {
                        let sub_manifest = sub.path().join("ridge.toml");
                        if sub_manifest.is_file() {
                            member_manifests.push(sub_manifest);
                        }
                    }
                }
            }
        }
    }

    // Must have found exactly 2 member projects.
    assert_eq!(
        member_manifests.len(),
        2,
        "bidir workspace must expand to exactly 2 members, found: {member_manifests:?}"
    );

    // Parse each member and collect names — must be unique.
    let mut names: HashSet<String> = HashSet::new();
    for path in &member_manifests {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|_| panic!("member manifest must be readable: {path:?}"));
        let proj =
            parse_project(&src, path).unwrap_or_else(|e| panic!("member manifest must parse: {e}"));
        let inserted = names.insert(proj.name.clone());
        assert!(inserted, "duplicate project name: {}", proj.name);
    }

    // Both expected member names are present.
    assert!(
        names.contains("bidir.domain"),
        "bidir.domain must be a member"
    );
    assert!(
        names.contains("bidir.shared"),
        "bidir.shared must be a member"
    );
}
