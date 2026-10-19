//! `ridge-manifest` — workspace and project manifest parsing for `ridge.toml`.
//!
//! This crate is the canonical parser for Ridge workspace and project manifests.
//! It is shared between `ridge-resolve` (workspace graph construction)
//! and `ridge-pkg` (dependency resolution) so that both consumers
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
//! table.  Codes are **stable from 1.0** — from there an assigned code is never
//! renumbered and a retired number is never reused.

// Test scaffolding may panic on a broken fixture; production paths may not.
// Same convention as `ridge-resolve`, `ridge-parser` and five other crates.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod error;
pub mod find;
pub mod globs;
pub mod path;
pub mod project;
pub mod workspace;

// ── Flat re-exports (convenience surface) ────────────────────────────────────

pub use error::ManifestError;
pub use find::{find_workspace_root, WorkspaceRoot};
pub use globs::{CompiledGlob, GlobError, GlobPattern};
pub use path::{canonicalize, plain};
pub use project::{parse_project, Project, ProjectDependency, ProjectKind};
pub use workspace::{parse_workspace, ForbidRule, GitRev, SharedDependency, WorkspaceManifest};

/// Alias for [`Project`] under the name `ProjectManifest`.
///
/// [`Project`] remains the canonical name. The alias exposes the longer name
/// without requiring callers to adopt a breaking rename.
pub type ProjectManifest = Project;
