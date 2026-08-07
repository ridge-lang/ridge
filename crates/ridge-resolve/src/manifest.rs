//! Manifest parsing, delegated to `ridge-manifest`.
//!
//! This crate used to carry its own copy of the parser — the same 1200 lines of
//! reading and validation, and its own `M001`–`M020` on the other end of them.
//! Two parsers meant two answers to the same `ridge.toml`, and the codes said
//! whichever one happened to run. `ridge-manifest` is now the only parser; what
//! is left here is the one thing it should not know about, which is identity.
//!
//! A manifest says what a project *is*. Which project it is in the graph being
//! resolved is this crate's question, so a [`Project`] is a [`ProjectId`] beside
//! the manifest it was parsed from, and reading a fact through
//! `project.manifest.name` says where the fact came from.

use std::path::Path;

use ridge_manifest::ManifestError;

use crate::ProjectId;

// ── Re-exports ────────────────────────────────────────────────────────────────

pub use ridge_manifest::{
    parse_workspace as parse_workspace_manifest, ForbidRule, GitRev, ProjectDependency,
    ProjectKind, SharedDependency, WorkspaceManifest,
};

/// The manifest of a project, as parsed — without the identity this crate gives
/// it.
pub type ProjectManifest = ridge_manifest::Project;

// ── Project ───────────────────────────────────────────────────────────────────

/// A project in the workspace being resolved: its identity, and what its
/// manifest says.
#[derive(Debug)]
pub struct Project {
    /// Identity within the workspace graph. Assigned by discovery, not by the
    /// manifest — nothing in `ridge.toml` decides it.
    pub id: ProjectId,

    /// Everything the manifest declares.
    pub manifest: ProjectManifest,
}

/// Parse a project manifest and give it an identity in the graph.
///
/// # Errors
///
/// Propagates every `M###` [`ManifestError`] the parser reports.
pub fn parse_project_manifest(
    toml_src: &str,
    manifest_path: &Path,
    project_id: ProjectId,
) -> Result<Project, ManifestError> {
    ridge_manifest::parse_project(toml_src, manifest_path).map(|manifest| Project {
        id: project_id,
        manifest,
    })
}
