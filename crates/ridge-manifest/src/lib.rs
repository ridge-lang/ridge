//! `ridge-manifest` — workspace and project manifest parsing for `ridge.toml`.
//!
//! This crate is the canonical parser for Ridge workspace and project manifests.
//! It is shared between `ridge-resolve` (Phase 3 workspace graph construction)
//! and `ridge-pkg` (Phase 8 dependency resolution) so that both consumers
//! operate on a single, consistent manifest model.
//!
//! # Front-door API
//!
//! ```rust,ignore
//! use ridge_manifest::{parse_workspace, parse_project, find_workspace_root};
//!
//! let ws = parse_workspace(toml_src, &manifest_path)?;
//! let proj = parse_project(toml_src, &manifest_path)?;
//! let root = find_workspace_root(Path::new("."));
//! ```
//!
//! # Error codes
//!
//! All errors are `M001`–`M020`; see [`error::ManifestError`] for the full
//! table.  Codes are **stable across releases** — never renumber an assigned
//! code.

pub mod error;
pub mod find;
pub mod globs;
pub mod project;
pub mod workspace;

// ── Flat re-exports (convenience surface) ────────────────────────────────────

pub use error::ManifestError;
pub use find::find_workspace_root;
pub use globs::{CompiledGlob, GlobError, GlobPattern};
pub use project::{parse_project, Project, ProjectDependency, ProjectKind};
pub use workspace::{parse_workspace, ForbidRule, GitRev, SharedDependency, WorkspaceManifest};

/// Alias for [`Project`] matching the §3.11 plan-spec name `ProjectManifest`.
///
/// The canonical name is [`Project`] (preserved from the existing
/// `ridge-resolve` parser to enable a clean re-export in T2).  This alias
/// preserves forward-compatibility with the plan's spec'd name without a
/// breaking rename in 0.2.0.
pub type ProjectManifest = Project;
