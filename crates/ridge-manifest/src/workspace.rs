//! Workspace manifest types and parser.
//!
//! Provides [`WorkspaceManifest`] and [`parse_workspace`] for parsing the root
//! `ridge.toml` workspace manifest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ridge_ast::{Capability, Span};
use serde::Deserialize;

use crate::error::ManifestError;
use crate::globs::{GlobError, GlobPattern};

// Filesystem-glob validation for workspace `members` patterns.
use globset::Glob as FsGlob;

// ── Public types ──────────────────────────────────────────────────────────────

/// Parsed workspace `ridge.toml`.
#[derive(Debug)]
pub struct WorkspaceManifest {
    /// Workspace name.
    pub name: String,
    /// Workspace version string (stored verbatim; not validated).
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

/// A workspace-level architectural constraint.
#[derive(Debug)]
pub struct ForbidRule {
    /// The "from" module-path pattern.
    pub from: GlobPattern,
    /// The "to" module-path pattern.
    pub to: GlobPattern,
    /// Byte-offset span within the workspace `ridge.toml`.
    ///
    /// Currently always `Span::point(0)` — full span tracking is deferred
    /// until a TOML-with-spans API is available.
    pub source_span: Span,
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

// ── Public entry point ────────────────────────────────────────────────────────

/// Parse a workspace `ridge.toml` from its raw TOML source.
///
/// `source_path` must be the absolute path to the file on disk (used in error
/// messages and stored in [`WorkspaceManifest::source_path`]).
///
/// # Errors
///
/// Returns the first fatal `M0NN` error encountered.  Validation stops at the
/// first error per sequential validation order.
#[allow(clippy::too_many_lines)]
pub fn parse_workspace(
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
            source_span: Span::point(0),
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

// ── Raw serde structs ─────────────────────────────────────────────────────────

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

// ── Shared dependency raw struct ──────────────────────────────────────────────

/// Inline-table shape for a single dependency entry.
///
/// Exactly one of the recognised shape fields must be present.
/// Fields are all `Option` so we can give specific error codes.
#[derive(Deserialize)]
pub(crate) struct DependencyRaw {
    pub(crate) version: Option<String>,
    pub(crate) git: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) commit: Option<String>,
    pub(crate) path: Option<String>,
    #[serde(rename = "workspace")]
    pub(crate) workspace_dep: Option<bool>,
    #[serde(rename = "workspace-member")]
    pub(crate) workspace_member: Option<String>,
    // Hex is reserved — any presence → M018.
    pub(crate) hex: Option<String>,
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

    // Unreachable given shape_count == 1 check above.
    Err(ManifestError::InvalidDependencyKind {
        raw: name.to_owned(),
        path: manifest_path.to_owned(),
    })
}

pub(crate) fn parse_git_rev(
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
    // No rev selector — default to empty commit sentinel.
    Ok(GitRev::Commit(String::new()))
}

pub(crate) fn parse_capability(
    cap_str: &str,
    manifest_path: &Path,
) -> Result<Capability, ManifestError> {
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
pub(crate) fn extract_unknown_key_error(msg: &str, table_hint: &str, path: &Path) -> ManifestError {
    // Find text between first pair of backtick characters.
    let key = msg.split('`').nth(1).unwrap_or("unknown").to_owned();
    ManifestError::UnknownManifestKey {
        table: table_hint.to_owned(),
        key,
        path: path.to_owned(),
    }
}
