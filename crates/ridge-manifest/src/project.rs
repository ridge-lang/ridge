//! Project manifest types and parser.
//!
//! Provides [`Project`], [`ProjectKind`], [`ProjectDependency`], and
//! [`parse_project`] for parsing per-project `ridge.toml` manifests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ridge_ast::Capability;
use serde::Deserialize;

use crate::error::ManifestError;
use crate::globs::{GlobError, GlobPattern};
use crate::workspace::{
    extract_unknown_key_error, parse_capability, parse_git_rev, DependencyRaw, GitRev,
};

// ── Public types ──────────────────────────────────────────────────────────────

/// Parsed per-project `ridge.toml`.
#[derive(Debug)]
pub struct Project {
    /// Canonical namespace, e.g. `"acme.domain"`.
    pub name: String,
    /// Project version string (stored verbatim).
    pub version: String,
    /// Project kind.
    pub kind: ProjectKind,
    /// Entry-point path (relative to manifest dir), required for App/Service.
    pub entry: Option<PathBuf>,
    /// Absolute path to the project `ridge.toml`.
    pub manifest_path: PathBuf,
    /// Absolute path to the source root directory.
    pub src_root: PathBuf,
    /// Glob patterns for the project's public-export surface.
    pub exports_public: Vec<GlobPattern>,
    /// Glob patterns for the project's internal-export surface.
    pub exports_internal: Vec<GlobPattern>,
    /// Project-local dependency list.
    pub dependencies: Vec<ProjectDependency>,
    /// `None` = inherit from workspace; `Some([…])` = explicit allow list.
    pub capabilities_allow: Option<Vec<Capability>>,
    /// Capabilities denied at project level (merged with workspace deny).
    pub capabilities_deny: Vec<Capability>,
}

/// Project kind discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    /// A reusable library.
    Library,
    /// An executable application.
    App,
    /// A long-running service (actor-based entry point).
    Service,
    /// A test project.
    Test,
}

/// A project-local dependency reference.
#[derive(Debug)]
pub enum ProjectDependency {
    /// `{ workspace-member = "shared" }` — names a sibling member.
    WorkspaceMember {
        /// Local alias for the dependency.
        local_name: String,
        /// Member name in the workspace.
        member: String,
    },
    /// `{ workspace = true }` — inherit from workspace dependencies.
    Workspace {
        /// Local alias for the dependency.
        local_name: String,
    },
    /// `{ path = "../helpers" }`.
    Path {
        /// Local alias for the dependency.
        local_name: String,
        /// Filesystem path to the dependency.
        path: PathBuf,
    },
    /// `{ git = "…", tag/branch/commit = "…" }`.
    Git {
        /// Local alias for the dependency.
        local_name: String,
        /// Git remote URL.
        url: String,
        /// Git revision selector.
        rev: GitRev,
    },
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Parse a per-project `ridge.toml` from its raw TOML source.
///
/// `manifest_path` is the absolute path to the project manifest file.
///
/// # Errors
///
/// Returns the first fatal `M0NN` error encountered.
pub fn parse_project(toml_src: &str, manifest_path: &Path) -> Result<Project, ManifestError> {
    // Step 1 — TOML parse.
    let raw: ProjectManifestFile = toml::from_str(toml_src).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("unknown field") {
            extract_unknown_key_error(&msg, "project", manifest_path)
        } else {
            ManifestError::TomlParseFailed {
                path: manifest_path.to_owned(),
                message: msg,
            }
        }
    })?;

    // Step 2 — [project] table presence.
    let proj = raw
        .project
        .ok_or_else(|| ManifestError::MissingProjectTable {
            path: manifest_path.to_owned(),
        })?;

    // Step 3 — required fields.
    let name = proj
        .name
        .ok_or_else(|| ManifestError::MissingRequiredField {
            table: "project".to_owned(),
            field: "name".to_owned(),
            path: manifest_path.to_owned(),
        })?;

    let version = proj
        .version
        .ok_or_else(|| ManifestError::MissingRequiredField {
            table: "project".to_owned(),
            field: "version".to_owned(),
            path: manifest_path.to_owned(),
        })?;

    let kind_str = proj
        .kind
        .ok_or_else(|| ManifestError::MissingRequiredField {
            table: "project".to_owned(),
            field: "kind".to_owned(),
            path: manifest_path.to_owned(),
        })?;

    // Step 4 — kind parsing.
    let kind = parse_project_kind(&kind_str, manifest_path)?;

    // entry required for app / service.
    let entry = proj.entry.map(PathBuf::from);
    if matches!(kind, ProjectKind::App | ProjectKind::Service) && entry.is_none() {
        return Err(ManifestError::MissingRequiredField {
            table: "project".to_owned(),
            field: "entry".to_owned(),
            path: manifest_path.to_owned(),
        });
    }

    // Step 5 — dependency shapes.
    let mut dependencies = Vec::new();
    for (dep_name, dep_raw) in raw.dependencies.unwrap_or_default() {
        let dep = parse_project_dependency(&dep_name, dep_raw, manifest_path)?;
        dependencies.push(dep);
    }

    // Step 6 — export globs.
    let exports = proj.exports.unwrap_or_default();
    let mut exports_public = Vec::new();
    for pat_str in exports.public.unwrap_or_default() {
        let pat = GlobPattern::new(&pat_str)
            .map_err(|e: GlobError| e.into_export_pattern_invalid(manifest_path.to_owned()))?;
        exports_public.push(pat);
    }
    let mut exports_internal = Vec::new();
    for pat_str in exports.internal.unwrap_or_default() {
        let pat = GlobPattern::new(&pat_str)
            .map_err(|e: GlobError| e.into_export_pattern_invalid(manifest_path.to_owned()))?;
        exports_internal.push(pat);
    }

    // Step 7 — capability names.
    let caps_raw = raw.capabilities.unwrap_or_default();
    let capabilities_allow = if let Some(allow_list) = caps_raw.allow {
        let mut allow = Vec::new();
        for cap_str in allow_list {
            allow.push(parse_capability(&cap_str, manifest_path)?);
        }
        Some(allow)
    } else {
        None
    };
    let mut capabilities_deny = Vec::new();
    for cap_str in caps_raw.deny.unwrap_or_default() {
        capabilities_deny.push(parse_capability(&cap_str, manifest_path)?);
    }

    // Compute src_root from [project.src].root (default "src").
    let src_root_rel = proj
        .src
        .and_then(|s| s.root)
        .unwrap_or_else(|| "src".to_owned());
    let src_root = manifest_path
        .parent()
        .unwrap_or(manifest_path)
        .join(&src_root_rel);

    Ok(Project {
        name,
        version,
        kind,
        entry,
        manifest_path: manifest_path.to_owned(),
        src_root,
        exports_public,
        exports_internal,
        dependencies,
        capabilities_allow,
        capabilities_deny,
    })
}

// ── Raw serde structs ─────────────────────────────────────────────────────────

/// Top-level file wrapper for a project manifest.
///
/// Unknown top-level tables are silently ignored; absence of `[project]` →
/// M003.  M019 is enforced at the inner table level via `deny_unknown_fields`.
#[derive(Deserialize)]
struct ProjectManifestFile {
    project: Option<ProjectTableRaw>,
    dependencies: Option<HashMap<String, DependencyRaw>>,
    capabilities: Option<ProjectCapabilitiesRaw>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectTableRaw {
    name: Option<String>,
    version: Option<String>,
    kind: Option<String>,
    entry: Option<String>,
    src: Option<ProjectSrcRaw>,
    exports: Option<ProjectExportsRaw>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectSrcRaw {
    root: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ProjectExportsRaw {
    public: Option<Vec<String>>,
    internal: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ProjectCapabilitiesRaw {
    allow: Option<Vec<String>>,
    deny: Option<Vec<String>>,
}

// ── Validation helpers ────────────────────────────────────────────────────────

fn parse_project_kind(kind_str: &str, manifest_path: &Path) -> Result<ProjectKind, ManifestError> {
    match kind_str {
        "library" => Ok(ProjectKind::Library),
        "app" => Ok(ProjectKind::App),
        "service" => Ok(ProjectKind::Service),
        "test" => Ok(ProjectKind::Test),
        _ => Err(ManifestError::InvalidProjectKind {
            kind: kind_str.to_owned(),
            path: manifest_path.to_owned(),
        }),
    }
}

fn parse_project_dependency(
    name: &str,
    raw: DependencyRaw,
    manifest_path: &Path,
) -> Result<ProjectDependency, ManifestError> {
    // Hex → M018.
    if raw.hex.is_some() {
        return Err(ManifestError::HexDependencyUsedIn010 {
            name: name.to_owned(),
            path: manifest_path.to_owned(),
        });
    }

    // workspace-member.
    if let Some(member) = raw.workspace_member {
        return Ok(ProjectDependency::WorkspaceMember {
            local_name: name.to_owned(),
            member,
        });
    }

    // workspace = true.
    if raw.workspace_dep == Some(true) {
        return Ok(ProjectDependency::Workspace {
            local_name: name.to_owned(),
        });
    }

    // Count remaining shapes.
    let shape_count = u8::from(raw.version.is_some())
        + u8::from(raw.git.is_some())
        + u8::from(raw.path.is_some());

    if shape_count == 0 {
        return Err(ManifestError::InvalidDependencyKind {
            raw: name.to_owned(),
            path: manifest_path.to_owned(),
        });
    }
    if shape_count > 1 {
        return Err(ManifestError::InvalidDependencyKind {
            raw: name.to_owned(),
            path: manifest_path.to_owned(),
        });
    }

    if let Some(url) = raw.git {
        let rev = parse_git_rev(raw.tag, raw.branch, raw.commit, manifest_path)?;
        return Ok(ProjectDependency::Git {
            local_name: name.to_owned(),
            url,
            rev,
        });
    }

    if let Some(path_str) = raw.path {
        return Ok(ProjectDependency::Path {
            local_name: name.to_owned(),
            path: PathBuf::from(path_str),
        });
    }

    // version-only is not valid for project deps — must use workspace = true
    // or workspace-member.  Map to InvalidDependencyKind (M009).
    if let Some(ver) = raw.version {
        return Err(ManifestError::InvalidDependencyKind {
            raw: ver,
            path: manifest_path.to_owned(),
        });
    }

    Err(ManifestError::InvalidDependencyKind {
        raw: name.to_owned(),
        path: manifest_path.to_owned(),
    })
}
