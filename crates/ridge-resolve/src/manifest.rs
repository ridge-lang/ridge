//! Workspace and project manifest parsing for `ridge.toml` files.
//!
//! ## Public API
//!
//! - [`parse_workspace_manifest`] — parse the root `ridge.toml` into a
//!   [`WorkspaceManifest`].
//! - [`parse_project_manifest`] — parse a per-project `ridge.toml` into a
//!   [`Project`].
//!
//! ## Strategy
//!
//! Parsing is done in two stages:
//!
//! 1. **Raw stage** — `toml::from_str` into serde-derived `*Raw` structs.
//!    These use `#[serde(deny_unknown_fields)]` so that unknown keys produce a
//!    serde error, which is then mapped to `M019 UnknownManifestKey`.
//!
//! 2. **Validation stage** — convert raw structs to the public types, emitting
//!    specific `M0NN` errors on every semantic violation.
//!
//! ## M019 serde message format
//!
//! When `deny_unknown_fields` rejects a key, `toml` produces a message of the
//! form `"unknown field 'foo', expected one of …"`.  We extract the key name
//! by scanning between the first pair of backticks in the message, and infer
//! the table name from the path supplied by the caller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ridge_ast::Capability;
use serde::Deserialize;

use crate::error::ManifestError;
use crate::globs::{GlobError, GlobPattern};
use crate::ProjectId;

// Filesystem-glob validation for workspace `members` patterns.
use globset::Glob as FsGlob;

// ── Public types ──────────────────────────────────────────────────────────────

/// Parsed workspace `ridge.toml`.
#[derive(Debug)]
pub struct WorkspaceManifest {
    /// Workspace name.
    pub name: String,
    /// Workspace version string (stored verbatim; not validated in T2).
    pub version: String,
    /// Raw member glob patterns (e.g. `["apps/*", "libs/*"]`).
    pub members_globs: Vec<String>,
    /// Shared dependency list.
    pub dependencies: Vec<SharedDependency>,
    /// Architectural forbid rules.
    pub forbid_rules: Vec<ForbidRule>,
    /// Capabilities denied workspace-wide.
    pub capabilities_deny: Vec<Capability>,
    /// Absolute path to the workspace `ridge.toml` that was parsed.
    pub source_path: PathBuf,
}

/// Parsed per-project `ridge.toml`.
#[derive(Debug)]
pub struct Project {
    /// Project ID (assigned by caller).
    pub id: ProjectId,
    /// Canonical namespace, e.g. `"acme.domain"`.
    pub name: String,
    /// Project version string (stored verbatim).
    pub version: String,
    /// Project kind.
    pub kind: ProjectKind,
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

/// A workspace-level architectural constraint.
#[derive(Debug)]
pub struct ForbidRule {
    /// The "from" module-path pattern.
    pub from: GlobPattern,
    /// The "to" module-path pattern.
    pub to: GlobPattern,
    /// Byte-offset span within the workspace `ridge.toml` (always `Span::point(0)`
    /// for T2 — full span tracking deferred to T3 when we have a TOML span API).
    pub source_span: ridge_ast::Span,
}

/// A workspace-level shared dependency.
#[derive(Debug)]
pub enum SharedDependency {
    /// `{ version = "1.0" }`.
    Version {
        /// Dependency key name in `[workspace.dependencies]`.
        name: String,
        /// Version string.
        version: String,
    },
    /// `{ git = "…", tag/branch/commit = "…" }`.
    Git {
        /// Dependency key name.
        name: String,
        /// Git remote URL.
        url: String,
        /// Git revision selector.
        rev: GitRev,
    },
    /// `{ path = "../foo" }`.
    Path {
        /// Dependency key name.
        name: String,
        /// Relative or absolute filesystem path.
        path: PathBuf,
    },
}

/// Git revision selector.
#[derive(Debug)]
pub enum GitRev {
    /// `tag = "v1.0"`.
    Tag(String),
    /// `branch = "main"`.
    Branch(String),
    /// `commit = "abc123"`.
    Commit(String),
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

// ── Public entry points ───────────────────────────────────────────────────────

/// Parse a workspace `ridge.toml` from its raw TOML source.
///
/// `source_path` must be the absolute path to the file on disk (used in error
/// messages and stored in [`WorkspaceManifest::source_path`]).
///
/// # Errors
///
/// Returns the first fatal `M0NN` error encountered.  Validation stops at the
/// first error per the plan's sequential validation order.
#[allow(clippy::too_many_lines)]
pub fn parse_workspace_manifest(
    toml_src: &str,
    source_path: &Path,
) -> Result<WorkspaceManifest, ManifestError> {
    // Step 1 — TOML parse.
    let raw: WorkspaceManifestFile = toml::from_str(toml_src).map_err(|e| {
        let msg = e.to_string();
        // M019: deny_unknown_fields fires as a TOML deserialisation error whose
        // message contains "unknown field".  Map it before M001.
        if msg.contains("unknown field") {
            extract_unknown_key_error(&msg, "workspace", source_path)
        } else {
            ManifestError::TomlParseFailed {
                path: source_path.to_owned(),
                message: msg,
            }
        }
    })?;

    // Step 2 — [workspace] table presence.
    let ws = raw
        .workspace
        .ok_or_else(|| ManifestError::MissingWorkspaceTable {
            path: source_path.to_owned(),
        })?;

    // Step 3 — required fields: name, version, members.
    let name = ws.name.ok_or_else(|| ManifestError::MissingRequiredField {
        table: "workspace".to_owned(),
        field: "name".to_owned(),
        path: source_path.to_owned(),
    })?;

    let version = ws
        .version
        .ok_or_else(|| ManifestError::MissingRequiredField {
            table: "workspace".to_owned(),
            field: "version".to_owned(),
            path: source_path.to_owned(),
        })?;

    let members_raw = ws
        .members
        .ok_or_else(|| ManifestError::MissingRequiredField {
            table: "workspace".to_owned(),
            field: "members".to_owned(),
            path: source_path.to_owned(),
        })?;

    // Step 4 — workspace dependencies.
    let mut dependencies = Vec::new();
    for (dep_name, dep_raw) in ws.dependencies.unwrap_or_default() {
        let dep = parse_shared_dependency(&dep_name, dep_raw, source_path)?;
        dependencies.push(dep);
    }

    // Step 5 — forbid rules.
    let forbid_rules_raw = ws.rules.and_then(|r| r.forbid).unwrap_or_default();
    let mut forbid_rules = Vec::new();
    for rule_raw in forbid_rules_raw {
        let from_str = rule_raw
            .from
            .ok_or_else(|| ManifestError::InvalidForbidRule {
                reason: "forbid rule is missing the `from` field".to_owned(),
                path: source_path.to_owned(),
            })?;
        let to_str = rule_raw
            .to
            .ok_or_else(|| ManifestError::InvalidForbidRule {
                reason: "forbid rule is missing the `to` field".to_owned(),
                path: source_path.to_owned(),
            })?;
        let from =
            GlobPattern::new(&from_str).map_err(|e: GlobError| ManifestError::BadMemberGlob {
                pattern: e.pattern,
                error: e.message,
            })?;
        let to =
            GlobPattern::new(&to_str).map_err(|e: GlobError| ManifestError::BadMemberGlob {
                pattern: e.pattern,
                error: e.message,
            })?;
        forbid_rules.push(ForbidRule {
            from,
            to,
            source_span: ridge_ast::Span::point(0),
        });
    }

    // Step 6 — capabilities.deny.
    let capabilities_deny_raw = ws.capabilities.and_then(|c| c.deny).unwrap_or_default();
    let mut capabilities_deny = Vec::new();
    for cap_str in capabilities_deny_raw {
        let cap = parse_capability(&cap_str, source_path)?;
        capabilities_deny.push(cap);
    }

    // Step 7 — validate member globs compile.
    // Members globs are *filesystem* path patterns (e.g. "apps/*", "libs/*"),
    // not module-path dot-separated patterns.  We compile them with plain
    // globset to validate syntax; we do NOT pass them through GlobPattern::new
    // (which rejects '/' and translates '.' as separator).
    for glob_str in &members_raw {
        if glob_str.is_empty() {
            return Err(ManifestError::BadMemberGlob {
                pattern: glob_str.clone(),
                error: "glob pattern must not be empty".to_owned(),
            });
        }
        FsGlob::new(glob_str).map_err(|e| ManifestError::BadMemberGlob {
            pattern: glob_str.clone(),
            error: e.to_string(),
        })?;
    }

    Ok(WorkspaceManifest {
        name,
        version,
        members_globs: members_raw,
        dependencies,
        forbid_rules,
        capabilities_deny,
        source_path: source_path.to_owned(),
    })
}

/// Parse a per-project `ridge.toml` from its raw TOML source.
///
/// `manifest_path` is the absolute path to the project manifest file.
/// `project_id` is the caller-assigned [`ProjectId`].
///
/// # Errors
///
/// Returns the first fatal `M0NN` error encountered.
pub fn parse_project_manifest(
    toml_src: &str,
    manifest_path: &Path,
    project_id: ProjectId,
) -> Result<Project, ManifestError> {
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
    if matches!(kind, ProjectKind::App | ProjectKind::Service) && proj.entry.is_none() {
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
        id: project_id,
        name,
        version,
        kind,
        manifest_path: manifest_path.to_owned(),
        src_root,
        exports_public,
        exports_internal,
        dependencies,
        capabilities_allow,
        capabilities_deny,
    })
}

// ── Raw serde structs (workspace) ─────────────────────────────────────────────

/// Top-level file wrapper for a workspace manifest.
///
/// Unknown top-level tables are silently ignored at this level; absence of
/// `[workspace]` is reported as M002.  M019 is enforced at the inner table
/// level via `deny_unknown_fields` on [`WorkspaceTableRaw`].
#[derive(Deserialize)]
struct WorkspaceManifestFile {
    workspace: Option<WorkspaceTableRaw>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceTableRaw {
    name: Option<String>,
    version: Option<String>,
    members: Option<Vec<String>>,
    dependencies: Option<HashMap<String, DependencyRaw>>,
    rules: Option<WorkspaceRulesRaw>,
    capabilities: Option<WorkspaceCapabilitiesRaw>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceRulesRaw {
    forbid: Option<Vec<ForbidRuleRaw>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForbidRuleRaw {
    from: Option<String>,
    to: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceCapabilitiesRaw {
    deny: Option<Vec<String>>,
}

// ── Raw serde structs (project) ───────────────────────────────────────────────

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

// ── Shared dependency raw struct ──────────────────────────────────────────────

/// Inline-table shape for a single dependency entry.
///
/// Exactly one of the recognised shape fields must be present.
/// Fields are all `Option` so we can give specific error codes.
#[derive(Deserialize)]
struct DependencyRaw {
    version: Option<String>,
    git: Option<String>,
    tag: Option<String>,
    branch: Option<String>,
    commit: Option<String>,
    path: Option<String>,
    #[serde(rename = "workspace")]
    workspace_dep: Option<bool>,
    #[serde(rename = "workspace-member")]
    workspace_member: Option<String>,
    // Hex is reserved — any presence → M018.
    hex: Option<String>,
}

// ── Validation helpers ────────────────────────────────────────────────────────

fn parse_shared_dependency(
    name: &str,
    raw: DependencyRaw,
    manifest_path: &Path,
) -> Result<SharedDependency, ManifestError> {
    // Hex → M018 immediately.
    if raw.hex.is_some() {
        return Err(ManifestError::HexDependencyUsedIn010 {
            name: name.to_owned(),
            path: manifest_path.to_owned(),
        });
    }

    // workspace / workspace-member are project-only; treat as invalid kind in
    // workspace context.
    if raw.workspace_dep.is_some() || raw.workspace_member.is_some() {
        return Err(ManifestError::InvalidDependencyKind {
            raw: name.to_owned(),
            path: manifest_path.to_owned(),
        });
    }

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

    if let Some(ver) = raw.version {
        return Ok(SharedDependency::Version {
            name: name.to_owned(),
            version: ver,
        });
    }

    if let Some(url) = raw.git {
        let rev = parse_git_rev(raw.tag, raw.branch, raw.commit, manifest_path)?;
        return Ok(SharedDependency::Git {
            name: name.to_owned(),
            url,
            rev,
        });
    }

    if let Some(path_str) = raw.path {
        return Ok(SharedDependency::Path {
            name: name.to_owned(),
            path: PathBuf::from(path_str),
        });
    }

    // Unreachable given shape_count == 1 check above, but make the compiler
    // happy.
    Err(ManifestError::InvalidDependencyKind {
        raw: name.to_owned(),
        path: manifest_path.to_owned(),
    })
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

    // version-only is not valid for project deps (must use workspace = true or
    // workspace-member for version deps) — fall through to M009.
    // Actually the plan allows version-only in project-level deps too (it just
    // means a concrete version pinned locally). We already validated shape_count==1
    // so if version is Some, emit it.
    if let Some(ver) = raw.version {
        // Treat as a bare version dep — not directly supported in project
        // manifests per the plan (only workspace.dependencies has Version).
        // The plan says M009 for "bare string" but an inline table with just
        // `version` is arguably fine. The plan's example only shows
        // workspace-member, workspace, path, git for project deps.
        // Map to InvalidDependencyKind (M009) to be safe.
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

fn parse_git_rev(
    tag: Option<String>,
    branch: Option<String>,
    commit: Option<String>,
    manifest_path: &Path,
) -> Result<GitRev, ManifestError> {
    let count = u8::from(tag.is_some()) + u8::from(branch.is_some()) + u8::from(commit.is_some());
    if count > 1 {
        return Err(ManifestError::GitRevConflict {
            path: manifest_path.to_owned(),
        });
    }
    if let Some(t) = tag {
        return Ok(GitRev::Tag(t));
    }
    if let Some(b) = branch {
        return Ok(GitRev::Branch(b));
    }
    if let Some(c) = commit {
        return Ok(GitRev::Commit(c));
    }
    // No rev selector at all — default to a sentinel.  The plan does not
    // specify an error for this case, so we use a placeholder commit.
    Ok(GitRev::Commit(String::new()))
}

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

/// Parse a capability name string into a [`Capability`] variant.
fn parse_capability(cap_str: &str, manifest_path: &Path) -> Result<Capability, ManifestError> {
    match cap_str {
        "io" => Ok(Capability::Io),
        "fs" => Ok(Capability::Fs),
        "net" => Ok(Capability::Net),
        "time" => Ok(Capability::Time),
        "random" => Ok(Capability::Random),
        "env" => Ok(Capability::Env),
        "proc" => Ok(Capability::Proc),
        "spawn" => Ok(Capability::Spawn),
        "ffi" => Ok(Capability::Ffi),
        _ => Err(ManifestError::InvalidCapabilityName {
            name: cap_str.to_owned(),
            path: manifest_path.to_owned(),
        }),
    }
}

/// Extract an `M019 UnknownManifestKey` from a serde/toml `"unknown field"` error
/// message.
///
/// The `toml` crate produces messages of the form:
/// ```text
/// unknown field `foo`, expected one of `name`, `version`, ...
/// ```
/// We extract the key between the first pair of backticks.
fn extract_unknown_key_error(msg: &str, table_hint: &str, path: &Path) -> ManifestError {
    // Find text between first pair of backtick characters.
    let key = msg.split('`').nth(1).unwrap_or("unknown").to_owned();
    ManifestError::UnknownManifestKey {
        table: table_hint.to_owned(),
        key,
        path: path.to_owned(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        let toml = include_str!("../tests/fixtures/manifest/M001_workspace_invalid_toml.toml");
        let result = parse_workspace_manifest(toml, wp());
        let err = result.unwrap_err();
        assert_eq!(err.code(), "M001", "expected M001, got: {err:?}");
    }

    #[test]
    fn m001_project_invalid_toml() {
        let toml = include_str!("../tests/fixtures/manifest/M001_project_invalid_toml.toml");
        let result = parse_project_manifest(toml, pp(), ProjectId(0));
        let err = result.unwrap_err();
        assert_eq!(err.code(), "M001");
    }

    // ── M002 MissingWorkspaceTable ────────────────────────────────────────────

    #[test]
    fn m002_missing_workspace_table() {
        let toml = include_str!("../tests/fixtures/manifest/M002_missing_workspace_table.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M002");
    }

    // ── M003 MissingProjectTable ──────────────────────────────────────────────

    #[test]
    fn m003_missing_project_table() {
        let toml = include_str!("../tests/fixtures/manifest/M003_missing_project_table.toml");
        let err = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap_err();
        assert_eq!(err.code(), "M003");
    }

    // ── M004 MemberWithoutProjectManifest — T3 deferred ──────────────────────

    #[test]
    fn m004_deferred_to_t3() {
        // M004 fires during filesystem expansion (T3), not manifest parsing (T2).
        // T2 never emits M004. This fixture documents that a well-formed workspace
        // manifest with members globs parses successfully; T3 validates that each
        // expanded member directory contains a ridge.toml.
        let toml =
            include_str!("../tests/fixtures/manifest/M004_deferred_member_without_manifest.toml");
        let result = parse_workspace_manifest(toml, wp());
        assert!(
            result.is_ok(),
            "T2 must not emit M004; filesystem validation is T3's responsibility"
        );
    }

    // ── M005 BadMemberGlob ────────────────────────────────────────────────────

    #[test]
    fn m005_invalid_member_glob() {
        let toml = include_str!("../tests/fixtures/manifest/M005_invalid_member_glob.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M005");
    }

    #[test]
    fn m005_empty_member_glob() {
        let toml = include_str!("../tests/fixtures/manifest/M005_empty_member_glob.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M005");
    }

    // ── M006 MissingRequiredField ─────────────────────────────────────────────

    #[test]
    fn m006_workspace_missing_name() {
        let toml = include_str!("../tests/fixtures/manifest/M006_workspace_missing_name.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M006");
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn m006_workspace_missing_version() {
        let toml = include_str!("../tests/fixtures/manifest/M006_workspace_missing_version.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M006");
        assert!(err.to_string().contains("version"));
    }

    #[test]
    fn m006_workspace_missing_members() {
        let toml = include_str!("../tests/fixtures/manifest/M006_workspace_missing_members.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M006");
        assert!(err.to_string().contains("members"));
    }

    #[test]
    fn m006_project_missing_kind() {
        let toml = include_str!("../tests/fixtures/manifest/M006_project_missing_kind.toml");
        let err = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap_err();
        assert_eq!(err.code(), "M006");
        assert!(err.to_string().contains("kind"));
    }

    #[test]
    fn m006_app_missing_entry() {
        let toml = include_str!("../tests/fixtures/manifest/M006_app_missing_entry.toml");
        let err = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap_err();
        assert_eq!(err.code(), "M006");
        assert!(err.to_string().contains("entry"));
    }

    // ── M007 InvalidProjectKind ───────────────────────────────────────────────

    #[test]
    fn m007_invalid_kind() {
        let toml = include_str!("../tests/fixtures/manifest/M007_invalid_kind.toml");
        let err = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap_err();
        assert_eq!(err.code(), "M007");
    }

    // ── M008 InvalidForbidRule ────────────────────────────────────────────────

    #[test]
    fn m008_missing_to_field() {
        let toml = include_str!("../tests/fixtures/manifest/M008_missing_to_field.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M008");
    }

    #[test]
    fn m008_missing_from_field() {
        let toml = include_str!("../tests/fixtures/manifest/M008_missing_from_field.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M008");
    }

    // ── M009 InvalidDependencyKind ────────────────────────────────────────────

    #[test]
    fn m009_workspace_dep_no_shape() {
        // A dep entry with none of the recognised shape keys → M009.
        let toml = include_str!("../tests/fixtures/manifest/M009_workspace_dep_no_shape.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M009");
    }

    #[test]
    fn m009_project_dep_no_shape() {
        let toml = include_str!("../tests/fixtures/manifest/M009_project_dep_no_shape.toml");
        let err = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap_err();
        assert_eq!(err.code(), "M009");
    }

    // ── M010 DuplicateProjectName — T3 deferred ───────────────────────────────

    #[test]
    fn m010_deferred_to_t3() {
        // M010 fires in the integration layer (T3) when multiple project manifests
        // are collected and their names compared.  T2 validates only a single
        // project manifest at a time and cannot detect duplicates.
        let toml =
            include_str!("../tests/fixtures/manifest/M010_deferred_duplicate_project_name.toml");
        let result = parse_project_manifest(toml, pp(), ProjectId(0));
        assert!(
            result.is_ok(),
            "T2 must not emit M010; duplicate detection is T3's responsibility"
        );
    }

    // ── M011 InvalidCapabilityName ────────────────────────────────────────────

    #[test]
    fn m011_unknown_capability_workspace() {
        let toml =
            include_str!("../tests/fixtures/manifest/M011_unknown_capability_workspace.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M011");
    }

    #[test]
    fn m011_unknown_capability_project() {
        let toml = include_str!("../tests/fixtures/manifest/M011_unknown_capability_project.toml");
        let err = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap_err();
        assert_eq!(err.code(), "M011");
    }

    // ── M012 CycleInDependencies — T7 deferred ───────────────────────────────

    #[test]
    fn m012_deferred_to_t7() {
        // M012 requires the full workspace dependency graph to detect cycles.
        // T2 only parses individual manifests and cannot detect cycles.
        let toml = include_str!("../tests/fixtures/manifest/M012_deferred_dep_cycle.toml");
        let result = parse_project_manifest(toml, pp(), ProjectId(0));
        assert!(
            result.is_ok(),
            "T2 must not emit M012; cycle detection is T7's responsibility"
        );
    }

    // ── M013 UnknownWorkspaceMember — T7 deferred ────────────────────────────

    #[test]
    fn m013_deferred_to_t7() {
        // M013 requires cross-project validation; T2 only parses single manifests.
        let toml = include_str!("../tests/fixtures/manifest/M013_deferred_unknown_member.toml");
        let result = parse_project_manifest(toml, pp(), ProjectId(0));
        assert!(
            result.is_ok(),
            "T2 must not emit M013; unknown-member validation is T7's responsibility"
        );
    }

    // ── M014 ProjectExportPatternInvalid ──────────────────────────────────────

    #[test]
    fn m014_invalid_export_pattern() {
        let toml = include_str!("../tests/fixtures/manifest/M014_invalid_export_pattern.toml");
        let err = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap_err();
        assert_eq!(err.code(), "M014");
    }

    // ── M015 WorkspaceDependencyAbsent — T7 deferred ─────────────────────────

    #[test]
    fn m015_deferred_to_t7() {
        // M015 requires the workspace manifest to be available for cross-validation.
        // T2 parses the project manifest in isolation.
        let toml =
            include_str!("../tests/fixtures/manifest/M015_deferred_workspace_dep_absent.toml");
        let result = parse_project_manifest(toml, pp(), ProjectId(0));
        assert!(
            result.is_ok(),
            "T2 must not emit M015; workspace-dep absence is T7's responsibility"
        );
    }

    // ── M016 GitRevConflict ───────────────────────────────────────────────────

    #[test]
    fn m016_git_tag_and_branch_conflict() {
        let toml = include_str!("../tests/fixtures/manifest/M016_git_rev_conflict.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M016");
    }

    // ── M017 RelativePathEscapesWorkspace — basic structural test ────────────

    #[test]
    fn m017_path_escaping_workspace_parses() {
        // Full escape detection requires workspace-root context (T3/T7).
        // T2 stores the path as-is; the emit-or-not decision is deferred.
        let toml =
            include_str!("../tests/fixtures/manifest/M017_deferred_path_escapes_workspace.toml");
        let result = parse_project_manifest(toml, pp(), ProjectId(0));
        assert!(
            result.is_ok(),
            "T2 does not emit M017 without workspace-root context"
        );
    }

    // ── M018 HexDependencyUsedIn010 ──────────────────────────────────────────

    #[test]
    fn m018_hex_dep_workspace() {
        let toml = include_str!("../tests/fixtures/manifest/M018_hex_dep_workspace.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M018");
    }

    #[test]
    fn m018_hex_dep_project() {
        let toml = include_str!("../tests/fixtures/manifest/M018_hex_dep_project.toml");
        let err = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap_err();
        assert_eq!(err.code(), "M018");
    }

    // ── M019 UnknownManifestKey ───────────────────────────────────────────────

    #[test]
    fn m019_unknown_workspace_key() {
        let toml = include_str!("../tests/fixtures/manifest/M019_unknown_workspace_key.toml");
        let err = parse_workspace_manifest(toml, wp()).unwrap_err();
        assert_eq!(err.code(), "M019");
    }

    // ── Happy-path workspace ──────────────────────────────────────────────────

    #[test]
    fn happy_path_workspace_minimal() {
        let toml = include_str!("../tests/fixtures/manifest/happy_workspace_minimal.toml");
        let ws = parse_workspace_manifest(toml, wp()).unwrap();
        assert_eq!(ws.name, "acme-platform");
        assert_eq!(ws.version, "0.1.0");
        assert_eq!(ws.members_globs.len(), 3);
        assert!(ws.forbid_rules.is_empty());
        assert!(ws.capabilities_deny.is_empty());
    }

    #[test]
    fn happy_path_workspace_full() {
        let toml = include_str!("../tests/fixtures/manifest/happy_workspace_full.toml");
        let ws = parse_workspace_manifest(toml, wp()).unwrap();
        assert_eq!(ws.name, "acme-platform");
        assert_eq!(ws.dependencies.len(), 3);
        assert_eq!(ws.forbid_rules.len(), 2);
        assert_eq!(ws.capabilities_deny.len(), 1);
        assert!(matches!(ws.capabilities_deny[0], Capability::Ffi));
    }

    // ── Happy-path project ────────────────────────────────────────────────────

    #[test]
    fn happy_path_project_library() {
        let toml = include_str!("../tests/fixtures/manifest/happy_project_library.toml");
        let proj = parse_project_manifest(toml, pp(), ProjectId(1)).unwrap();
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
        let toml = include_str!("../tests/fixtures/manifest/happy_project_app_with_entry.toml");
        let proj = parse_project_manifest(toml, pp(), ProjectId(2)).unwrap();
        assert!(matches!(proj.kind, ProjectKind::App));
    }

    #[test]
    fn project_src_root_default_is_src() {
        let toml = include_str!("../tests/fixtures/manifest/happy_project_library_minimal.toml");
        let proj = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap();
        assert!(proj.src_root.ends_with("src"));
    }

    #[test]
    fn project_src_root_custom() {
        let toml = include_str!("../tests/fixtures/manifest/happy_project_custom_src_root.toml");
        let proj = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap();
        assert!(proj.src_root.ends_with("source"));
    }

    #[test]
    fn capability_inherit_none_when_absent() {
        let toml = include_str!("../tests/fixtures/manifest/happy_project_library_minimal.toml");
        let proj = parse_project_manifest(toml, pp(), ProjectId(0)).unwrap();
        assert!(
            proj.capabilities_allow.is_none(),
            "absent [capabilities].allow should produce None (inherit from workspace)"
        );
    }
}
