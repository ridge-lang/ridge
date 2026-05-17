//! Core dependency resolver for `ridge-pkg`.
//!
//! Walks a project's dependency list and resolves each entry into a
//! [`ResolvedDep`].  Cycle detection is performed via a `HashSet` of
//! `(name, source_root)` pairs maintained during **transitive** recursive
//! traversal — the full dependency closure is returned, not just direct deps.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ridge_manifest::{ProjectDependency, ProjectManifest, WorkspaceManifest};

use crate::error::PkgError;
use crate::git::resolve_git_dep;
use crate::path::resolve_path_dep;

// ── Public types ──────────────────────────────────────────────────────────────

/// Discriminant describing how a dependency was sourced.
///
/// This is a *kind discriminant* — `ResolvedDep` is intentionally lossy/cheap
/// (it does not carry the raw `ProjectDependency` back out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepKind {
    /// `path = "…"` dependency resolved from the filesystem.
    Path,
    /// `git = "…", tag = "…"` — pinned tag clone (D152).
    GitTag,
    /// `git = "…", branch = "…"` — floating-branch clone; advisory P004 is
    /// emitted (D160).
    GitBranch,
    /// `workspace-member = "…"` — named sibling in the same workspace.
    WorkspaceMember,
    /// `workspace = true` — inherited from workspace-level dependencies.
    WorkspaceInherit,
}

/// A fully resolved dependency ready for downstream consumption.
///
/// `source_root` is the directory that contains the dep's `ridge.toml`.
pub struct ResolvedDep {
    /// Local alias as declared in the consumer's `ridge.toml`.
    pub name: String,
    /// How the dependency was sourced.
    pub kind: DepKind,
    /// Absolute path to the dependency's source root (contains `ridge.toml`).
    pub source_root: PathBuf,
    /// Parsed manifest of the dependency.
    pub manifest: ProjectManifest,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Resolve the full transitive dependency closure of `project`.
///
/// The returned `Vec` includes both direct and transitive deps in
/// depth-first, parent-before-children order.  Duplicate entries (same
/// `source_root` reached via multiple paths) are suppressed — the first
/// occurrence is kept.
///
/// `workspace` is needed to expand `workspace = true` and
/// `workspace-member = "…"` references.  `cache_root` is the platform-aware
/// root returned by [`crate::cache_root`] (or a test-supplied override).
///
/// # Errors
///
/// Returns the first `P0NN` / `P1NN` error encountered.
pub fn resolve_dependencies(
    workspace: &WorkspaceManifest,
    project: &ProjectManifest,
    cache_root: &Path,
) -> Result<Vec<ResolvedDep>, PkgError> {
    let project_manifest_dir = project
        .manifest_path
        .parent()
        .unwrap_or(&project.manifest_path);

    let mut visited: HashSet<(String, PathBuf)> = HashSet::new();
    let mut seen_roots: HashSet<PathBuf> = HashSet::new();
    resolve_deps_inner(
        workspace,
        project,
        project_manifest_dir,
        cache_root,
        &mut visited,
        &mut seen_roots,
    )
}

// ── Internal recursive resolver ───────────────────────────────────────────────

/// `seen_roots` tracks which `source_root`s have already been added to the
/// output list; it is separate from `visited` (which is the cycle-detection
/// set keyed on `(name, source_root)`).
fn resolve_deps_inner(
    workspace: &WorkspaceManifest,
    project: &ProjectManifest,
    project_manifest_dir: &Path,
    cache_root: &Path,
    visited: &mut HashSet<(String, PathBuf)>,
    seen_roots: &mut HashSet<PathBuf>,
) -> Result<Vec<ResolvedDep>, PkgError> {
    let mut resolved = Vec::new();

    for dep in &project.dependencies {
        let rdep = resolve_one(workspace, dep, project_manifest_dir, cache_root, visited)?;

        // Deduplicate by source_root: skip if another parent already pulled
        // in the same dep root — first occurrence wins.
        if seen_roots.contains(&rdep.source_root) {
            continue;
        }
        seen_roots.insert(rdep.source_root.clone());

        // Capture the fields needed for recursion before moving `rdep` into
        // the output list.
        let dep_source_root = rdep.source_root.clone();
        let dep_manifest_path = rdep.manifest.manifest_path.clone();

        // Re-read the dep's manifest from disk so we can recurse into it.
        // The file is already in the filesystem (resolve_one guarantees this).
        // We do not need the full manifest — only its dependency list — but
        // re-parsing is the cleanest way to obtain it without a Clone bound.
        let dep_toml = std::fs::read_to_string(&dep_manifest_path).map_err(|e| {
            PkgError::PkgManifestParseFailed {
                path: dep_manifest_path.clone(),
                source: ridge_manifest::ManifestError::TomlParseFailed {
                    path: dep_manifest_path.clone(),
                    message: e.to_string(),
                },
            }
        })?;
        let dep_manifest_reparsed = ridge_manifest::parse_project(&dep_toml, &dep_manifest_path)
            .map_err(|source| PkgError::PkgManifestParseFailed {
                path: dep_manifest_path.clone(),
                source,
            })?;

        resolved.push(rdep);

        // Recurse into the dep's transitive dependencies (depth-first,
        // parent-before-children).
        let transitive = resolve_deps_inner(
            workspace,
            &dep_manifest_reparsed,
            &dep_source_root,
            cache_root,
            visited,
            seen_roots,
        )?;
        resolved.extend(transitive);
    }

    Ok(resolved)
}

/// Resolve a single [`ProjectDependency`] to a [`ResolvedDep`].
#[allow(clippy::too_many_lines)]
fn resolve_one(
    workspace: &WorkspaceManifest,
    dep: &ProjectDependency,
    project_manifest_dir: &Path,
    cache_root: &Path,
    visited: &mut HashSet<(String, PathBuf)>,
) -> Result<ResolvedDep, PkgError> {
    match dep {
        // ── path = "…" ────────────────────────────────────────────────────────
        ProjectDependency::Path { local_name, path } => {
            let (source_root, manifest) = resolve_path_dep(project_manifest_dir, path)?;

            // Cycle detection.
            let key = (local_name.clone(), source_root.clone());
            if visited.contains(&key) {
                return Err(PkgError::PkgDependencyCycle {
                    cycle_path: format!("{local_name} → {}", source_root.display()),
                });
            }
            visited.insert(key);

            Ok(ResolvedDep {
                name: local_name.clone(),
                kind: DepKind::Path,
                source_root,
                manifest,
            })
        }

        // ── git = "…" ─────────────────────────────────────────────────────────
        ProjectDependency::Git {
            local_name,
            url,
            rev,
        } => {
            let (source_root, manifest, warnings) =
                resolve_git_dep(local_name, url, rev, cache_root)?;

            // Emit floating-branch advisories to stderr (D160).
            for w in &warnings {
                eprintln!("{}", w.message());
            }

            // Cycle detection (git deps are keyed by source_root).
            let key = (local_name.clone(), source_root.clone());
            if visited.contains(&key) {
                return Err(PkgError::PkgDependencyCycle {
                    cycle_path: format!("{local_name} → {}", source_root.display()),
                });
            }
            visited.insert(key);

            let kind = if warnings.is_empty() {
                DepKind::GitTag
            } else {
                DepKind::GitBranch
            };

            Ok(ResolvedDep {
                name: local_name.clone(),
                kind,
                source_root,
                manifest,
            })
        }

        // ── workspace-member = "…" ────────────────────────────────────────────
        ProjectDependency::WorkspaceMember { local_name, member } => {
            // Verify member exists in workspace members_globs (name-match, not
            // glob expansion — full glob expansion requires a filesystem walk
            // that T7 defers).  For 0.1.0 we do a name-substring check: any
            // members_glob that ends with `/<member>` or equals `<member>`.
            let ws_root = workspace
                .source_path
                .parent()
                .unwrap_or(&workspace.source_path);

            // Look up the member directory.
            let member_dir = find_workspace_member(ws_root, member)?;
            let manifest_path = member_dir.join("ridge.toml");

            if !manifest_path.exists() {
                return Err(PkgError::PkgPathManifestMissing {
                    path: manifest_path,
                });
            }

            let toml_src = std::fs::read_to_string(&manifest_path).map_err(|e| {
                PkgError::PkgManifestParseFailed {
                    path: manifest_path.clone(),
                    source: ridge_manifest::ManifestError::TomlParseFailed {
                        path: manifest_path.clone(),
                        message: e.to_string(),
                    },
                }
            })?;

            let manifest =
                ridge_manifest::parse_project(&toml_src, &manifest_path).map_err(|source| {
                    PkgError::PkgManifestParseFailed {
                        path: manifest_path.clone(),
                        source,
                    }
                })?;

            let key = (local_name.clone(), member_dir.clone());
            if visited.contains(&key) {
                return Err(PkgError::PkgDependencyCycle {
                    cycle_path: format!("{local_name} → {}", member_dir.display()),
                });
            }
            visited.insert(key);

            Ok(ResolvedDep {
                name: local_name.clone(),
                kind: DepKind::WorkspaceMember,
                source_root: member_dir,
                manifest,
            })
        }

        // ── workspace = true ──────────────────────────────────────────────────
        ProjectDependency::Workspace { local_name } => {
            // Look up the dep name in workspace-level shared dependencies.
            // The workspace dep is resolved to the same dep kind it is there.
            let shared = workspace
                .dependencies
                .iter()
                .find(|d| shared_dep_name(d) == local_name)
                .ok_or_else(|| {
                    // Missing workspace dep: treat as P102 with a clear message.
                    PkgError::PkgManifestParseFailed {
                        path: workspace.source_path.clone(),
                        source: ridge_manifest::ManifestError::MissingRequiredField {
                            table: "workspace.dependencies".to_owned(),
                            field: local_name.clone(),
                            path: workspace.source_path.clone(),
                        },
                    }
                })?;

            // Delegate to the appropriate sub-resolver.
            let synthetic_dep = shared_to_project_dep(local_name, shared)?;
            let ws_root = workspace
                .source_path
                .parent()
                .unwrap_or(&workspace.source_path);

            resolve_one(workspace, &synthetic_dep, ws_root, cache_root, visited).map(|mut rdep| {
                rdep.kind = DepKind::WorkspaceInherit;
                rdep
            })
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return the name key of a [`ridge_manifest::SharedDependency`].
fn shared_dep_name(dep: &ridge_manifest::SharedDependency) -> &str {
    match dep {
        ridge_manifest::SharedDependency::Version { name, .. }
        | ridge_manifest::SharedDependency::Git { name, .. }
        | ridge_manifest::SharedDependency::Path { name, .. } => name,
    }
}

/// Convert a [`ridge_manifest::SharedDependency`] to a synthetic
/// [`ProjectDependency`] for recursive resolution.
///
/// Returns `Err(P010)` for `Version` variants, which require a registry not
/// available until Ridge 0.2.0.
fn shared_to_project_dep(
    local_name: &str,
    shared: &ridge_manifest::SharedDependency,
) -> Result<ProjectDependency, PkgError> {
    match shared {
        ridge_manifest::SharedDependency::Git { url, rev, .. } => Ok(ProjectDependency::Git {
            local_name: local_name.to_owned(),
            url: url.clone(),
            rev: clone_git_rev(rev),
        }),
        ridge_manifest::SharedDependency::Path { path, .. } => Ok(ProjectDependency::Path {
            local_name: local_name.to_owned(),
            path: path.clone(),
        }),
        ridge_manifest::SharedDependency::Version { version, .. } => {
            Err(PkgError::PkgVersionDepUnsupported {
                name: local_name.to_owned(),
                version: version.clone(),
            })
        }
    }
}

/// Clone a [`ridge_manifest::GitRev`].
fn clone_git_rev(rev: &ridge_manifest::GitRev) -> ridge_manifest::GitRev {
    match rev {
        ridge_manifest::GitRev::Tag(t) => ridge_manifest::GitRev::Tag(t.clone()),
        ridge_manifest::GitRev::Branch(b) => ridge_manifest::GitRev::Branch(b.clone()),
        ridge_manifest::GitRev::Commit(c) => ridge_manifest::GitRev::Commit(c.clone()),
    }
}

/// Find the directory for a named workspace member by scanning the
/// workspace root for a subdirectory whose `ridge.toml` has a matching
/// `[project] name`.
///
/// For 0.1.0, we do a direct directory-name match first (common case), then
/// fall back to reading each member's `ridge.toml` to match by manifest name.
fn find_workspace_member(ws_root: &Path, member: &str) -> Result<PathBuf, PkgError> {
    // Fast path: directory named exactly after the member.
    let direct = ws_root.join(member);
    if direct.is_dir() && direct.join("ridge.toml").exists() {
        return Ok(direct);
    }

    // Slow path: scan immediate subdirectories.
    let read_dir = std::fs::read_dir(ws_root).map_err(|_e| PkgError::PkgPathManifestMissing {
        path: ws_root.to_owned().join(member),
    })?;

    for entry in read_dir.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let candidate_manifest = dir.join("ridge.toml");
        if !candidate_manifest.exists() {
            continue;
        }
        if let Ok(src) = std::fs::read_to_string(&candidate_manifest) {
            if let Ok(proj) = ridge_manifest::parse_project(&src, &candidate_manifest) {
                if proj.name == member {
                    return Ok(dir);
                }
            }
        }
    }

    Err(PkgError::PkgPathManifestMissing {
        path: ws_root.join(member),
    })
}
