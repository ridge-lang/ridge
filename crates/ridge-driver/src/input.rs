//! Source selection shared by checking, compilation and execution.
use std::path::{Path, PathBuf};

use ridge_manifest::{find_workspace_root, ProjectKind, WorkspaceRoot};
use ridge_resolve::{discover_standalone, discover_workspace, DiscoveryResult};
use sha2::{Digest, Sha256};

use crate::{CompileError, SourceFileError};

/// The source selected by a driver invocation.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub enum SourceInput {
    /// Discover the workspace at the options' root, as usual.
    #[default]
    Workspace,
    /// Select one source file, retaining any workspace that governs it.
    File {
        /// Absolute source path; discovery validates and canonicalizes it.
        path: PathBuf,
        /// Require this file to provide the executable `main` entry point.
        require_main: bool,
    },
}

/// Validate a regular `.ridge` source and resolve symlinks before policy lookup.
///
/// # Errors
/// Returns `C017` for missing, unreadable or non-source inputs.
pub fn validate_source_file(path: &Path) -> Result<PathBuf, SourceFileError> {
    let canonical =
        ridge_manifest::canonicalize(path).map_err(|error| invalid(path, error.to_string()))?;
    if !canonical.is_file() || canonical.extension().is_none_or(|ext| ext != "ridge") {
        return Err(invalid(path, "expected a regular .ridge source file"));
    }
    Ok(canonical)
}

/// Per-file build root in the user's cache, without writing beside the source.
///
/// The canonical path is hashed so identically named files in different
/// directories do not share artefacts. The source itself stays at its real path.
///
/// # Errors
/// Returns the source validation error or the package cache-location error.
pub fn file_build_root(path: &Path) -> Result<PathBuf, CompileError> {
    let path = validate_source_file(path)?;
    let root =
        ridge_pkg::cache_root().map_err(|source| CompileError::PkgResolutionFailed { source })?;
    let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
    Ok(root.join("file-builds").join(key))
}

/// Construct one source-selection failure without losing the input path.
fn invalid(path: &Path, reason: impl Into<String>) -> SourceFileError {
    SourceFileError::InvalidSourceFile {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

impl SourceInput {
    /// Whether a discovered path is selected for entry or test execution.
    /// Workspace invocations select every module; explicit files select one.
    #[must_use]
    pub fn selects_path(&self, candidate: &Path) -> bool {
        match self {
            Self::Workspace => true,
            Self::File { path, .. } => {
                candidate == path
                    || match (
                        ridge_manifest::canonicalize(path),
                        ridge_manifest::canonicalize(candidate),
                    ) {
                        (Ok(a), Ok(b)) => a == b,
                        _ => false,
                    }
            }
        }
    }

    /// Discover the selected graph and say whether it has no on-disk manifest.
    pub(crate) fn discover(&self, root: &Path) -> Result<(DiscoveryResult, bool), SourceFileError> {
        let Self::File { path, require_main } = self else {
            return Ok((discover_workspace(root), false));
        };
        let path = validate_source_file(path)?;
        let (mut result, standalone) = match find_workspace_root(&path) {
            WorkspaceRoot::Found(root) => (discover_workspace(&root), false),
            WorkspaceRoot::Malformed(manifest) => (
                discover_workspace(manifest.parent().unwrap_or(&manifest)),
                false,
            ),
            WorkspaceRoot::NotFound => (
                DiscoveryResult {
                    graph: Some(discover_standalone(std::slice::from_ref(&path))),
                    manifest_errors: Vec::new(),
                    resolve_errors: Vec::new(),
                },
                true,
            ),
        };
        // A malformed project may have been omitted from an otherwise valid
        // graph. Preserve its actual manifest error before membership checks.
        if !result.manifest_errors.is_empty() {
            return Ok((result, standalone));
        }
        if let Some(graph) = &mut result.graph {
            let selected = graph
                .modules
                .iter()
                .find(|module| {
                    module.file_path == path
                        || ridge_manifest::canonicalize(&module.file_path)
                            .is_ok_and(|candidate| candidate == path)
                })
                .ok_or_else(|| {
                    invalid(
                        &path,
                        "file is not a member of the workspace that governs it",
                    )
                })?;
            if *require_main {
                // Keep every capability/dependency policy. Only the entry is
                // selected explicitly rather than taken from a project default.
                let project = &mut graph.projects[selected.project.0 as usize].manifest;
                project.kind = ProjectKind::App;
                project.entry = Some(path);
            }
        }
        Ok((result, standalone))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn standalone_graph_matches_editor_and_does_not_discover_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scratch.ridge");
        std::fs::write(&path, "pub fn value -> Int = 42\n").unwrap();
        std::fs::write(dir.path().join("Other.ridge"), "pub fn value -> Int = 0\n").unwrap();
        let path = validate_source_file(&path).unwrap();
        let input = SourceInput::File {
            path: path.clone(),
            require_main: false,
        };
        let (result, standalone) = input.discover(dir.path()).unwrap();
        assert!(standalone);
        let graph = result.graph.unwrap();
        let editor = discover_standalone(std::slice::from_ref(&path));
        assert_eq!(graph.modules.len(), 1);
        assert_eq!(graph.modules[0].file_path, editor.modules[0].file_path);
        assert_eq!(
            graph.modules[0].fully_qualified_name,
            editor.modules[0].fully_qualified_name
        );
        assert!(!dir.path().join("ridge.toml").exists());
    }

    #[test]
    fn cache_separates_same_named_sources_and_normalizes_paths() {
        let dir = tempfile::tempdir().unwrap();
        for folder in ["a", "b"] {
            std::fs::create_dir(dir.path().join(folder)).unwrap();
            std::fs::write(
                dir.path().join(folder).join("main.ridge"),
                "pub fn main -> Int = 0\n",
            )
            .unwrap();
        }
        let first = dir.path().join("a/main.ridge");
        let key = file_build_root(&first).unwrap();
        assert_eq!(
            key,
            file_build_root(&dir.path().join("a/../a/main.ridge")).unwrap()
        );
        assert_ne!(
            key,
            file_build_root(&dir.path().join("b/main.ridge")).unwrap()
        );
        assert!(!key.starts_with(dir.path()));
    }

    #[test]
    fn explicit_entry_keeps_workspace_policy_and_other_modules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
        std::fs::write(
            dir.path().join("ridge.toml"),
            "[workspace]\nname = \"workspace\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
        )
        .unwrap();
        let manifest = "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n[capabilities]\nallow = []\n";
        std::fs::write(dir.path().join("app/ridge.toml"), manifest).unwrap();
        let path = dir.path().join("app/src/Main.ridge");
        std::fs::write(&path, "pub fn main -> Int = 0\n").unwrap();
        std::fs::write(
            dir.path().join("app/src/Other.ridge"),
            "pub fn value -> Int = 1\n",
        )
        .unwrap();
        let input = SourceInput::File {
            path: path.clone(),
            require_main: true,
        };
        let (result, standalone) = input.discover(dir.path()).unwrap();
        assert!(!standalone);
        let graph = result.graph.unwrap();
        assert_eq!(graph.modules.len(), 2);
        assert_eq!(graph.projects[0].manifest.kind, ProjectKind::App);
        assert_eq!(
            graph.projects[0].manifest.entry.as_ref(),
            Some(&validate_source_file(&path).unwrap())
        );
        assert_eq!(
            graph.projects[0].manifest.capabilities_allow,
            Some(Vec::new())
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("app/ridge.toml")).unwrap(),
            manifest
        );
    }
}
