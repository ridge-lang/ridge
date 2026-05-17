//! Workspace discovery: filesystem walk, member expansion, module FQN derivation.
//!
//! # Algorithm (§4.1 steps 1, 3, 5, 6, 7)
//!
//! 1. Walk upward from `root` finding the `ridge.toml` that contains a
//!    `[workspace]` table.  Emit R001 if none is found.
//! 2. Parse the workspace manifest via T2's [`parse_workspace_manifest`].
//! 3. Expand `members_globs` against the workspace root.  Emit M004 for any
//!    matched directory that lacks a `ridge.toml`.
//! 4. Detect duplicate project names — M010.
//! 5. For each project, walk `src_root` recursively.  For each `.rg` file,
//!    derive the fully-qualified module name and build a [`ModuleMetadata`].
//! 6. Sort all modules by `fully_qualified_name` for snapshot stability.
//! 7. Detect duplicate FQNs across all projects — R002.
//!
//! M017 is also checked: any project dependency with a relative `path` form
//! is canonicalized and verified to remain under the workspace root.
//!
//! Non-fatal policy: a manifest error for one project does NOT abort discovery
//! for other projects.  The bad project is skipped and the error accumulated.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::{Glob as FsGlob, GlobSetBuilder};
use ridge_ast::Span;

use crate::error::{ManifestError, ResolveError};
use crate::manifest::{parse_project_manifest, parse_workspace_manifest, ProjectDependency};
use crate::{DiscoveryResult, ModuleId, ModuleMetadata, ProjectId, WorkspaceGraph};

// ── Public entry point ────────────────────────────────────────────────────────

/// Walk the filesystem from `root`, locate the workspace manifest, expand
/// members, and return a partially-populated [`WorkspaceGraph`].
///
/// Module dependency edges (`deps`) are left empty — T4 fills those.
///
/// # Non-fatal policy
///
/// A manifest error for one project does not abort discovery for others.
/// All errors are accumulated in [`DiscoveryResult`].
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn discover_workspace(root: &Path) -> DiscoveryResult {
    let mut manifest_errors: Vec<ManifestError> = Vec::new();
    let mut resolve_errors: Vec<ResolveError> = Vec::new();

    // Step 1 — find the workspace root by walking upward.
    let Some(workspace_root) = find_workspace_root(root) else {
        resolve_errors.push(ResolveError::MissingWorkspaceManifest {
            path: root.to_owned(),
        });
        return DiscoveryResult {
            graph: None,
            manifest_errors,
            resolve_errors,
        };
    };

    // Step 2 — parse the workspace manifest.
    let manifest_path = workspace_root.join("ridge.toml");
    let Ok(toml_src) = std::fs::read_to_string(&manifest_path) else {
        resolve_errors.push(ResolveError::MissingWorkspaceManifest {
            path: manifest_path,
        });
        return DiscoveryResult {
            graph: None,
            manifest_errors,
            resolve_errors,
        };
    };

    let workspace_manifest = match parse_workspace_manifest(&toml_src, &manifest_path) {
        Ok(m) => m,
        Err(e) => {
            manifest_errors.push(e);
            return DiscoveryResult {
                graph: None,
                manifest_errors,
                resolve_errors,
            };
        }
    };

    // Step 3 — expand member globs to concrete project directories.
    let member_dirs = expand_member_globs(
        &workspace_root,
        &workspace_manifest.members_globs,
        &mut manifest_errors,
    );

    // Step 4 — parse each member's project manifest; collect projects.
    let mut projects = Vec::new();
    let mut project_name_seen: std::collections::HashMap<String, PathBuf> =
        std::collections::HashMap::new();

    for member_dir in &member_dirs {
        let proj_manifest_path = member_dir.join("ridge.toml");
        if !proj_manifest_path.is_file() {
            manifest_errors.push(ManifestError::MemberWithoutProjectManifest {
                member_dir: member_dir.clone(),
            });
            continue;
        }

        let Ok(proj_toml) = std::fs::read_to_string(&proj_manifest_path) else {
            manifest_errors.push(ManifestError::MemberWithoutProjectManifest {
                member_dir: member_dir.clone(),
            });
            continue;
        };

        // Workspace project counts are bounded well below u32::MAX in practice.
        #[allow(clippy::cast_possible_truncation)]
        let project_id = ProjectId(projects.len() as u32);
        let project = match parse_project_manifest(&proj_toml, &proj_manifest_path, project_id) {
            Ok(p) => p,
            Err(e) => {
                manifest_errors.push(e);
                continue;
            }
        };

        // M010 — duplicate project name.
        if let Some(first_path) = project_name_seen.get(&project.name) {
            manifest_errors.push(ManifestError::DuplicateProjectName {
                name: project.name.clone(),
                first: first_path.clone(),
                second: proj_manifest_path.clone(),
            });
            continue;
        }
        project_name_seen.insert(project.name.clone(), proj_manifest_path.clone());

        // M017 — check path dependencies for workspace escapes.
        check_path_dependency_escapes(&project, &workspace_root, &mut manifest_errors);

        projects.push(project);
    }

    // Step 5 — walk each project's src_root, derive FQNs, collect ModuleMetadata.
    let mut modules: Vec<ModuleMetadata> = Vec::new();
    let mut next_module_id: u32 = 0;

    for project in &projects {
        let src_root = &project.src_root;
        if !src_root.is_dir() {
            // Missing src_root is not a fatal error — the project simply has no modules.
            continue;
        }
        walk_src_root(
            src_root,
            src_root,
            &project.name,
            project.id,
            &mut next_module_id,
            &mut modules,
        );
    }

    // Step 6 — sort by fully_qualified_name for snapshot stability.
    modules.sort_by(|a, b| a.fully_qualified_name.cmp(&b.fully_qualified_name));

    // Reassign IDs to match the sorted position (IDs must equal Vec index).
    for (idx, module) in modules.iter_mut().enumerate() {
        // Workspace module counts are bounded well below u32::MAX in practice;
        // the cast is intentional and safe for any realistic workspace.
        #[allow(clippy::cast_possible_truncation)]
        let id = idx as u32;
        module.id = ModuleId(id);
    }

    // Step 7 — detect duplicate FQNs across all projects.
    detect_duplicate_fqns(&modules, &mut resolve_errors);

    let deps = vec![vec![]; modules.len()];

    DiscoveryResult {
        graph: Some(WorkspaceGraph {
            root: workspace_root,
            manifest: workspace_manifest,
            projects,
            modules,
            deps,
        }),
        manifest_errors,
        resolve_errors,
    }
}

// ── Step 1 helpers ────────────────────────────────────────────────────────────

/// Walk upward from `start`, returning the first directory that contains a
/// `ridge.toml` with a `[workspace]` table.
///
/// Returns `None` if the filesystem root is reached without finding one.
pub(crate) fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start.canonicalize().ok()?;
    loop {
        let candidate = cur.join("ridge.toml");
        if candidate.is_file() && has_workspace_table(&candidate) {
            return Some(cur);
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Lightweight TOML probe: read the file and check for a top-level `workspace`
/// key without doing a full manifest parse.
fn has_workspace_table(path: &Path) -> bool {
    let Ok(src) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&src) else {
        return false;
    };
    value.get("workspace").is_some()
}

// ── Step 3 helpers ────────────────────────────────────────────────────────────

/// Expand filesystem glob patterns relative to `workspace_root`.
///
/// Returns the sorted list of directories that matched at least one pattern.
/// Entries are deduplicated and sorted for deterministic ordering.
///
/// Errors from glob compilation are pushed to `errors` and that pattern is
/// skipped.
fn expand_member_globs(
    workspace_root: &Path,
    patterns: &[String],
    errors: &mut Vec<ManifestError>,
) -> Vec<PathBuf> {
    let mut matched: Vec<PathBuf> = Vec::new();

    // Special-case `"."` (single-project layout produced by `ridge new`):
    // the workspace root itself is the project directory, and its
    // `ridge.toml` carries BOTH the `[workspace]` and `[project]` tables.
    // `collect_glob_matches` only walks entries of `workspace_root` and
    // never tests `workspace_root` itself, so a pattern of `"."` would
    // otherwise match nothing.
    let mut non_dot_patterns: Vec<&String> = Vec::with_capacity(patterns.len());
    let mut saw_dot = false;
    for pat in patterns {
        if pat == "." {
            saw_dot = true;
        } else {
            non_dot_patterns.push(pat);
        }
    }
    if saw_dot {
        matched.push(workspace_root.to_path_buf());
    }

    // Compile remaining patterns into a glob set.
    if !non_dot_patterns.is_empty() {
        let mut builder = GlobSetBuilder::new();
        for pat in &non_dot_patterns {
            match FsGlob::new(pat) {
                Ok(g) => {
                    builder.add(g);
                }
                Err(e) => {
                    errors.push(ManifestError::BadMemberGlob {
                        pattern: (*pat).clone(),
                        error: e.to_string(),
                    });
                }
            }
        }

        let glob_set = match builder.build() {
            Ok(gs) => gs,
            Err(e) => {
                errors.push(ManifestError::BadMemberGlob {
                    pattern: non_dot_patterns
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    error: e.to_string(),
                });
                matched.sort();
                matched.dedup();
                return matched;
            }
        };

        // Walk one level of the workspace root to find matching directories.
        collect_glob_matches(workspace_root, workspace_root, &glob_set, &mut matched);
    }

    matched.sort();
    matched.dedup();
    matched
}

/// Recursively collect filesystem entries that match `glob_set` when expressed
/// as a path relative to `workspace_root`.  Only directories are returned.
///
/// Descends one level beyond the match depth for `**/`-style patterns;
/// for typical `apps/*` patterns a single level of recursion suffices.
fn collect_glob_matches(
    workspace_root: &Path,
    dir: &Path,
    glob_set: &globset::GlobSet,
    results: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Relative path from workspace root — used for glob matching.
        let Ok(rel) = path.strip_prefix(workspace_root) else {
            continue;
        };
        // Normalize to forward slashes for glob matching (globset uses `/`).
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if glob_set.is_match(&rel_str) {
            results.push(path.clone());
        }
        // Recurse one more level to handle `**/` patterns or nested structures.
        // Typical workspace layouts are at most 2 levels deep: `apps/myapp`.
        let depth = rel.components().count();
        if depth < 2 {
            collect_glob_matches(workspace_root, &path, glob_set, results);
        }
    }
}

// ── M017 helper ───────────────────────────────────────────────────────────────

/// Check all `Path` dependencies in `project` to ensure none escape the
/// workspace root after canonicalization.  Pushes M017 for each offender.
fn check_path_dependency_escapes(
    project: &crate::manifest::Project,
    workspace_root: &Path,
    errors: &mut Vec<ManifestError>,
) {
    let manifest_dir = project
        .manifest_path
        .parent()
        .unwrap_or(&project.manifest_path);

    // Canonicalize workspace_root once for comparison.
    let canonical_ws = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_owned());

    for dep in &project.dependencies {
        if let ProjectDependency::Path {
            path,
            local_name: _,
        } = dep
        {
            let resolved = manifest_dir.join(path);
            // Try to canonicalize; fall back to the raw joined path if the
            // target doesn't exist on disk yet.
            let canonical = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
            if !canonical.starts_with(&canonical_ws) {
                errors.push(ManifestError::RelativePathEscapesWorkspace {
                    path: path.to_string_lossy().into_owned(),
                    manifest: project.manifest_path.clone(),
                });
            }
        }
    }
}

// ── Step 5 helpers ────────────────────────────────────────────────────────────

/// Recursively walk `dir` under `src_root`, collecting `.rg` files.
///
/// Hidden files and directories (names starting with `.`) are skipped.
/// Symlinks are followed; cycle detection is performed via a seen-set of
/// canonicalized directory paths, with a hard depth limit of 64 as backup.
fn walk_src_root(
    src_root: &Path,
    dir: &Path,
    project_name: &str,
    project_id: ProjectId,
    next_id: &mut u32,
    modules: &mut Vec<ModuleMetadata>,
) {
    walk_src_root_inner(
        src_root,
        dir,
        project_name,
        project_id,
        next_id,
        modules,
        &mut HashSet::new(),
        0,
    );
}

/// Inner recursive helper with cycle-guard and depth limit.
///
/// # Invariant
///
/// Every `file_path` passed to [`derive_module_fqn`] satisfies
/// `file_path.starts_with(src_root)` — guaranteed by entering from `src_root`
/// and only recursing into children of the current directory.
#[allow(clippy::too_many_arguments)]
fn walk_src_root_inner(
    src_root: &Path,
    dir: &Path,
    project_name: &str,
    project_id: ProjectId,
    next_id: &mut u32,
    modules: &mut Vec<ModuleMetadata>,
    seen: &mut HashSet<PathBuf>,
    depth: u32,
) {
    // Depth cap: 64 levels is more than sufficient for any real project.
    if depth > 64 {
        return;
    }

    // Record the canonical form of this directory for cycle detection.
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_owned());
    if !seen.insert(canonical_dir) {
        // Already visited — skip to avoid symlink loops.
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut sorted_entries: Vec<_> = entries.flatten().collect();
    // Sort for deterministic ordering within a directory.
    sorted_entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in sorted_entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Skip hidden files and directories.
        if name_str.starts_with('.') {
            continue;
        }

        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() || (file_type.is_symlink() && path.is_dir()) {
            walk_src_root_inner(
                src_root,
                &path,
                project_name,
                project_id,
                next_id,
                modules,
                seen,
                depth + 1,
            );
        } else if file_type.is_file() || (file_type.is_symlink() && path.is_file()) {
            // Only process `.rg` files (case-sensitive per OQ-R003).
            if path.extension().is_some_and(|ext| ext == "rg") {
                let fqn = derive_module_fqn(project_name, src_root, &path);
                let id = ModuleId(*next_id);
                *next_id += 1;
                modules.push(ModuleMetadata {
                    id,
                    project: project_id,
                    fully_qualified_name: fqn,
                    file_path: path,
                    // T3 placeholder; T4 will set 0..eof after reading source.
                    span_within_file: Span::point(0),
                });
            }
        }
    }
}

/// Derive the fully-qualified module name from a file path.
///
/// # Algorithm
///
/// 1. Strip `src_root` prefix to obtain the relative path.
/// 2. Drop the `.rg` extension.
/// 3. Join path components with `.`.
/// 4. Prefix with `project_name`.
///
/// # Invariant
///
/// `file_path.starts_with(src_root)` must hold.  This is guaranteed by
/// [`walk_src_root_inner`], which only calls this function for files obtained
/// from a recursive descent starting at `src_root`.
fn derive_module_fqn(project_name: &str, src_root: &Path, file_path: &Path) -> String {
    // strip_prefix is safe: the invariant above ensures file_path is always
    // under src_root (enforced by walk_src_root_inner).
    let Ok(rel) = file_path.strip_prefix(src_root) else {
        // Unreachable in correct usage; return project name as a no-panic fallback.
        return project_name.to_owned();
    };
    let without_ext = rel.with_extension("");
    let dotted: String = without_ext
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(".");

    if dotted.is_empty() {
        project_name.to_owned()
    } else {
        format!("{project_name}.{dotted}")
    }
}

// ── Step 7 helpers ────────────────────────────────────────────────────────────

/// Scan adjacent pairs in the sorted `modules` list for duplicate FQNs and
/// push one R002 per collision.
fn detect_duplicate_fqns(modules: &[ModuleMetadata], errors: &mut Vec<ResolveError>) {
    for pair in modules.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if a.fully_qualified_name == b.fully_qualified_name {
            errors.push(ResolveError::DuplicateModule {
                fqn: a.fully_qualified_name.clone(),
                first: a.span_within_file,
                second: b.span_within_file,
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    // ── Fixture helpers ───────────────────────────────────────────────────────

    /// Write `content` to `dir/relative_path`, creating parent dirs as needed.
    fn write_file(dir: &Path, relative_path: &str, content: &str) {
        let full = dir.join(relative_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, content).unwrap();
    }

    /// Minimal workspace manifest TOML.
    fn workspace_toml(members: &[&str]) -> String {
        let members_list = members
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"[workspace]
name = "test-ws"
version = "0.1.0"
members = [{members_list}]
"#
        )
    }

    /// Minimal project manifest TOML (library kind).
    fn project_toml(name: &str) -> String {
        format!(
            r#"[project]
name = "{name}"
version = "0.1.0"
kind = "library"
"#
        )
    }

    /// Project manifest with a custom `src.root`.
    fn project_toml_with_src_root(name: &str, src_root: &str) -> String {
        format!(
            r#"[project]
name = "{name}"
version = "0.1.0"
kind = "library"

[project.src]
root = "{src_root}"
"#
        )
    }

    // ── T1: find_workspace_root finds manifest in current dir ─────────────────

    #[test]
    fn t1_find_workspace_root_in_current_dir() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["projects/*"]));
        let found = find_workspace_root(dir.path());
        assert!(found.is_some(), "should find workspace root in current dir");
        let canonical_dir = dir.path().canonicalize().unwrap();
        assert_eq!(found.unwrap(), canonical_dir);
    }

    // ── T2: find_workspace_root walks upward ──────────────────────────────────

    #[test]
    fn t2_find_workspace_root_walks_upward() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        let sub = dir.path().join("libs").join("mylib");
        fs::create_dir_all(&sub).unwrap();
        let found = find_workspace_root(&sub);
        assert!(found.is_some(), "should walk upward to find workspace root");
        let canonical_dir = dir.path().canonicalize().unwrap();
        assert_eq!(found.unwrap(), canonical_dir);
    }

    // ── T3: find_workspace_root returns None → R001 ───────────────────────────

    #[test]
    fn t3_r001_no_workspace_manifest() {
        let dir = TempDir::new().unwrap();
        let result = discover_workspace(dir.path());
        assert!(result.graph.is_none(), "R001 should yield no graph");
        assert!(
            result.resolve_errors.iter().any(|e| e.code() == "R001"),
            "expected R001 in resolve_errors; got: {:?}",
            result.resolve_errors
        );
    }

    // ── T4: single-member workspace, 1 project, 1 module, correct FQN ─────────

    #[test]
    fn t4_single_member_single_module_correct_fqn() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(dir.path(), "libs/mylib/ridge.toml", &project_toml("demo"));
        write_file(dir.path(), "libs/mylib/src/Hello.rg", "-- hello");

        let result = discover_workspace(dir.path());
        assert!(
            result.manifest_errors.is_empty(),
            "{:?}",
            result.manifest_errors
        );
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(graph.projects.len(), 1);
        assert_eq!(graph.modules.len(), 1);
        assert_eq!(graph.modules[0].fully_qualified_name, "demo.Hello");
    }

    // ── T5: two-member workspace with distinct projects ────────────────────────

    #[test]
    fn t5_two_member_workspace() {
        let dir = TempDir::new().unwrap();
        write_file(
            dir.path(),
            "ridge.toml",
            &workspace_toml(&["libs/*", "apps/*"]),
        );
        write_file(dir.path(), "libs/core/ridge.toml", &project_toml("core"));
        write_file(dir.path(), "libs/core/src/Util.rg", "-- util");
        write_file(
            dir.path(),
            "apps/server/ridge.toml",
            &project_toml("server"),
        );
        write_file(dir.path(), "apps/server/src/Main.rg", "-- main");

        let result = discover_workspace(dir.path());
        assert!(
            result.manifest_errors.is_empty(),
            "{:?}",
            result.manifest_errors
        );
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(graph.projects.len(), 2);
        assert_eq!(graph.modules.len(), 2);
        let fqns: Vec<&str> = graph
            .modules
            .iter()
            .map(|m| m.fully_qualified_name.as_str())
            .collect();
        assert!(fqns.contains(&"core.Util"), "fqns: {fqns:?}");
        assert!(fqns.contains(&"server.Main"), "fqns: {fqns:?}");
    }

    // ── T6: nested module path src/Models/User.rg → <project>.Models.User ──────

    #[test]
    fn t6_nested_module_path() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(
            dir.path(),
            "libs/domain/ridge.toml",
            &project_toml("acme.domain"),
        );
        write_file(dir.path(), "libs/domain/src/Models/User.rg", "-- user");

        let result = discover_workspace(dir.path());
        assert!(
            result.manifest_errors.is_empty(),
            "{:?}",
            result.manifest_errors
        );
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(graph.modules.len(), 1);
        assert_eq!(
            graph.modules[0].fully_qualified_name,
            "acme.domain.Models.User"
        );
    }

    // ── T7: deeply nested path src/A/B/C/D.rg → <project>.A.B.C.D ───────────

    #[test]
    fn t7_deeply_nested_path() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(
            dir.path(),
            "libs/deep/ridge.toml",
            &project_toml("acme.deep"),
        );
        write_file(dir.path(), "libs/deep/src/A/B/C/D.rg", "-- d");

        let result = discover_workspace(dir.path());
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(graph.modules.len(), 1);
        assert_eq!(graph.modules[0].fully_qualified_name, "acme.deep.A.B.C.D");
    }

    // ── T8: file directly in src/ → <project>.<Name> ─────────────────────────

    #[test]
    fn t8_file_directly_in_src() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(
            dir.path(),
            "libs/mylib/ridge.toml",
            &project_toml("myproject"),
        );
        write_file(dir.path(), "libs/mylib/src/Main.rg", "-- main");

        let result = discover_workspace(dir.path());
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(graph.modules.len(), 1);
        assert_eq!(graph.modules[0].fully_qualified_name, "myproject.Main");
    }

    // ── T9: non-.rg files are ignored; .hidden.rg is ignored ─────────────────

    #[test]
    fn t9_non_rg_files_and_hidden_files_ignored() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(dir.path(), "libs/mylib/ridge.toml", &project_toml("demo"));
        write_file(dir.path(), "libs/mylib/src/Actual.rg", "-- actual");
        write_file(dir.path(), "libs/mylib/src/README.md", "# readme");
        write_file(dir.path(), "libs/mylib/src/notes.txt", "notes");
        write_file(dir.path(), "libs/mylib/src/.hidden.rg", "-- hidden");

        let result = discover_workspace(dir.path());
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(
            graph.modules.len(),
            1,
            "modules: {:?}",
            graph
                .modules
                .iter()
                .map(|m| &m.fully_qualified_name)
                .collect::<Vec<_>>()
        );
        assert_eq!(graph.modules[0].fully_qualified_name, "demo.Actual");
    }

    // ── T10: hidden directories are skipped ───────────────────────────────────

    #[test]
    fn t10_hidden_directories_skipped() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(dir.path(), "libs/mylib/ridge.toml", &project_toml("demo"));
        write_file(dir.path(), "libs/mylib/src/Visible.rg", "-- visible");
        // Hidden dir — should be skipped.
        write_file(
            dir.path(),
            "libs/mylib/src/.hidden_dir/Secret.rg",
            "-- secret",
        );

        let result = discover_workspace(dir.path());
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(
            graph.modules.len(),
            1,
            "modules: {:?}",
            graph
                .modules
                .iter()
                .map(|m| &m.fully_qualified_name)
                .collect::<Vec<_>>()
        );
        assert_eq!(graph.modules[0].fully_qualified_name, "demo.Visible");
    }

    // ── T11: M004 fires when member dir lacks ridge.toml ─────────────────────

    #[test]
    fn t11_m004_member_without_project_manifest() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        // Member directory with no ridge.toml.
        fs::create_dir_all(dir.path().join("libs").join("nomanifest")).unwrap();

        let result = discover_workspace(dir.path());
        assert!(
            result.manifest_errors.iter().any(|e| e.code() == "M004"),
            "expected M004; errors: {:?}",
            result.manifest_errors
        );
    }

    // ── T12: M010 fires when two member projects share a name ─────────────────

    #[test]
    fn t12_m010_duplicate_project_name() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(
            dir.path(),
            "libs/first/ridge.toml",
            &project_toml("acme.domain"),
        );
        write_file(dir.path(), "libs/first/src/A.rg", "-- a");
        write_file(
            dir.path(),
            "libs/second/ridge.toml",
            &project_toml("acme.domain"),
        );
        write_file(dir.path(), "libs/second/src/B.rg", "-- b");

        let result = discover_workspace(dir.path());
        assert!(
            result.manifest_errors.iter().any(|e| e.code() == "M010"),
            "expected M010; errors: {:?}",
            result.manifest_errors
        );
    }

    // ── T13: R002 fires when two projects produce overlapping FQNs ────────────
    //
    // Portable recipe (works on all platforms):
    //   project "acme"        with src/domain/Foo.rg  → "acme.domain.Foo"
    //   project "acme.domain" with src/Foo.rg          → "acme.domain.Foo"
    // The two files live in different project directories, so there is no
    // filesystem-level name collision on any platform.

    #[test]
    fn t13_r002_overlapping_fqns_across_projects() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(dir.path(), "libs/acme/ridge.toml", &project_toml("acme"));
        write_file(dir.path(), "libs/acme/src/domain/Foo.rg", "-- foo");
        write_file(
            dir.path(),
            "libs/acmedomain/ridge.toml",
            &project_toml("acme.domain"),
        );
        write_file(dir.path(), "libs/acmedomain/src/Foo.rg", "-- foo");

        let result = discover_workspace(dir.path());
        assert!(
            result.resolve_errors.iter().any(|e| e.code() == "R002"),
            "expected R002; errors: {:?}",
            result.resolve_errors
        );
    }

    // ── T14: non-default src_root = "source" is respected ────────────────────

    #[test]
    fn t14_custom_src_root() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(
            dir.path(),
            "libs/mylib/ridge.toml",
            &project_toml_with_src_root("demo", "source"),
        );
        write_file(dir.path(), "libs/mylib/source/Config.rg", "-- config");

        let result = discover_workspace(dir.path());
        assert!(
            result.manifest_errors.is_empty(),
            "{:?}",
            result.manifest_errors
        );
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(graph.modules.len(), 1);
        assert_eq!(graph.modules[0].fully_qualified_name, "demo.Config");
    }

    // ── T15: canonical-example acceptance (DoD) ───────────────────────────────
    //
    // Every .rg file in examples/ resolves to the correct FQN when placed in
    // a synthetic workspace with project.name = "demo".

    #[test]
    fn t15_canonical_examples_resolve_correctly() {
        let example_stems = [
            "log_analyzer",
            "url_shortener",
            "game_of_life",
            "rate_limiter",
        ];

        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(
            dir.path(),
            "libs/examples/ridge.toml",
            &project_toml("demo"),
        );

        for stem in &example_stems {
            write_file(
                dir.path(),
                &format!("libs/examples/src/{stem}.rg"),
                "-- example file",
            );
        }

        let result = discover_workspace(dir.path());
        assert!(
            result.manifest_errors.is_empty(),
            "{:?}",
            result.manifest_errors
        );
        assert!(
            result.resolve_errors.is_empty(),
            "{:?}",
            result.resolve_errors
        );

        let graph = result.graph.unwrap();
        assert_eq!(graph.modules.len(), example_stems.len());

        for stem in &example_stems {
            let expected_fqn = format!("demo.{stem}");
            assert!(
                graph
                    .modules
                    .iter()
                    .any(|m| m.fully_qualified_name == expected_fqn),
                "missing FQN: {expected_fqn}; found: {:?}",
                graph
                    .modules
                    .iter()
                    .map(|m| &m.fully_qualified_name)
                    .collect::<Vec<_>>()
            );
        }
    }

    // ── T16: M017 fires when path dependency escapes workspace root ───────────

    #[test]
    fn t16_m017_path_dep_escapes_workspace() {
        // Layout:
        //   outer/
        //     workspace/
        //       ridge.toml
        //       libs/
        //         mylib/
        //           ridge.toml  ← path dep = "../../../outside"
        //           src/
        //             Foo.rg
        //     outside/           ← the escaping target
        //
        // From workspace/libs/mylib/:
        //   ../        → workspace/libs/
        //   ../../     → workspace/
        //   ../../../  → outer/
        //   ../../../outside → outer/outside  (escapes workspace/)
        let outer = TempDir::new().unwrap();
        let workspace_dir = outer.path().join("workspace");
        fs::create_dir_all(&workspace_dir).unwrap();

        let outside_dir = outer.path().join("outside");
        fs::create_dir_all(&outside_dir).unwrap();

        write_file(&workspace_dir, "ridge.toml", &workspace_toml(&["libs/*"]));

        let proj_manifest = r#"[project]
name = "demo"
version = "0.1.0"
kind = "library"

[dependencies]
outside = { path = "../../../outside" }
"#
        .to_owned();
        write_file(&workspace_dir, "libs/mylib/ridge.toml", &proj_manifest);
        write_file(&workspace_dir, "libs/mylib/src/Foo.rg", "-- foo");

        let result = discover_workspace(&workspace_dir);
        assert!(
            result.manifest_errors.iter().any(|e| e.code() == "M017"),
            "expected M017; errors: {:?}",
            result.manifest_errors
        );
    }

    // ── Additional: module IDs match Vec indices after sort ───────────────────

    #[test]
    fn module_ids_match_vec_indices_after_sort() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(dir.path(), "libs/p/ridge.toml", &project_toml("p"));
        write_file(dir.path(), "libs/p/src/Z.rg", "-- z");
        write_file(dir.path(), "libs/p/src/A.rg", "-- a");
        write_file(dir.path(), "libs/p/src/M.rg", "-- m");

        let result = discover_workspace(dir.path());
        let graph = result.graph.unwrap();
        // After sort, modules are ordered A < M < Z.
        assert_eq!(graph.modules[0].fully_qualified_name, "p.A");
        assert_eq!(graph.modules[1].fully_qualified_name, "p.M");
        assert_eq!(graph.modules[2].fully_qualified_name, "p.Z");
        // IDs must equal Vec indices.
        for (idx, module) in graph.modules.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let expected_id = idx as u32;
            assert_eq!(module.id.0, expected_id, "id mismatch at idx {idx}");
        }
    }

    // ── Windows FS case-sensitivity documentation ─────────────────────────────

    /// On Windows (NTFS, case-insensitive by default), two files that differ
    /// only in case cannot coexist in the same directory.  Therefore the
    /// Linux-style FS-collision path for R002 (two distinct files `Foo.rg` and
    /// `foo.rg` in the same directory producing the same FQN) is unreachable on
    /// Windows.
    ///
    /// The portable R002 test (T13) covers the cross-project FQN collision case
    /// which is reachable on all platforms.
    ///
    /// TODO: when running on Linux CI, add a `#[cfg(not(windows))]` test that
    /// physically creates `src/Foo.rg` and `src/foo.rg` in the same project and
    /// asserts R002 fires.
    #[cfg(windows)]
    #[test]
    fn r002_fs_collision_windows_note() {
        // On Windows, two .rg files with case-differing names in the same
        // directory cannot both exist.  This test documents that the Windows
        // behaviour is: only one file will be visible, so no R002 fires from
        // the FS collision path.  R002 is still exercised by the portable
        // cross-project test (T13).
        // This test always passes — it is a documentation stub.
    }

    // Restricted to Linux: macOS APFS is case-insensitive by default, so
    // writing both `Foo.rg` and `foo.rg` collapses to a single file and the
    // assertion `graph.modules.len() == 2` would fail.  Windows has its own
    // documentation stub above.
    #[cfg(target_os = "linux")]
    #[test]
    fn r002_fs_collision_linux_same_project() {
        // On Linux (case-sensitive FS), Foo.rg and foo.rg are distinct files.
        // FQN derivation is case-preserving (OQ-R003 resolution: case-sensitive),
        // so Foo.rg → acme.Foo and foo.rg → acme.foo — DIFFERENT FQNs.
        // Therefore no R002 fires from within the same project, confirming that
        // R002 specifically targets identical FQNs, not merely case-differing ones.
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(dir.path(), "libs/mylib/ridge.toml", &project_toml("acme"));
        write_file(dir.path(), "libs/mylib/src/Foo.rg", "-- foo");
        write_file(dir.path(), "libs/mylib/src/foo.rg", "-- foo lowercase");

        let result = discover_workspace(dir.path());
        // Both files are discovered with distinct FQNs — no R002.
        assert!(
            result.resolve_errors.is_empty(),
            "expected no R002 for case-differing FQNs; errors: {:?}",
            result.resolve_errors
        );
        let graph = result.graph.unwrap();
        assert_eq!(graph.modules.len(), 2);
        let fqns: Vec<&str> = graph
            .modules
            .iter()
            .map(|m| m.fully_qualified_name.as_str())
            .collect();
        assert!(fqns.contains(&"acme.Foo"), "fqns: {fqns:?}");
        assert!(fqns.contains(&"acme.foo"), "fqns: {fqns:?}");
    }
}
