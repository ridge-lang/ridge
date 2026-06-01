//! Workspace-scoped file-backed source cache for diagnostic rendering.
//!
//! [`WorkspaceSourceCache`] is built once per compile invocation from the
//! [`ridge_resolve::WorkspaceGraph`] that the driver already has in memory.
//! Source text is read from disk (via the module file paths) so the driver
//! does not need to retain `ParsedModule.source` across the pipeline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ridge_diagnostics::{SourceCache, SourceId};
use ridge_resolve::{ModuleId, WorkspaceGraph};

// ── WorkspaceSourceCache ──────────────────────────────────────────────────────

/// Workspace-scoped file-backed source cache.
///
/// Maps each [`SourceId`] (workspace-relative path string) to the raw source
/// text of the corresponding `.ridge` file.  Built once per compile invocation
/// from the [`WorkspaceGraph`] that `ridge-resolve` produces.
#[derive(Debug, Default)]
pub struct WorkspaceSourceCache {
    /// Map from `SourceId` string to source text.
    sources: HashMap<String, Arc<String>>,
    /// Map from `SourceId` string to human-readable display name.
    names: HashMap<String, String>,
    /// Map from [`ModuleId`] to `SourceId` string (for per-module lookups).
    module_to_id: HashMap<u32, String>,
}

impl WorkspaceSourceCache {
    /// Build a cache from the workspace graph.
    ///
    /// Reads each `.ridge` file from disk.  Files that cannot be read are silently
    /// skipped — the renderer falls back to context-less rendering for those
    /// modules (no underline, no caret).
    #[must_use]
    pub fn from_workspace(graph: &WorkspaceGraph) -> Self {
        let mut cache = Self::default();
        let workspace_root = &graph.root;

        for module in &graph.modules {
            let source_id_str = source_id_for(workspace_root, &module.file_path);

            // Read source from disk.  Files that cannot be read are silently
            // skipped — the renderer falls back to context-less rendering for
            // those modules.
            if let Ok(text) = std::fs::read_to_string(&module.file_path) {
                cache.sources.insert(source_id_str.clone(), Arc::new(text));
            }

            cache
                .names
                .insert(source_id_str.clone(), source_id_str.clone());
            cache.module_to_id.insert(module.id.0, source_id_str);
        }

        cache
    }

    /// Build a cache from in-memory per-module source text instead of disk.
    ///
    /// `texts[ModuleId.0]` is the source for that module. The LSP uses this:
    /// its incremental engine tracks each module's current buffer text, so the
    /// cache (and the diagnostics + index built from it) match exactly what was
    /// compiled. Modules without a matching text entry are skipped.
    #[must_use]
    pub fn from_module_texts(graph: &WorkspaceGraph, texts: &[Arc<String>]) -> Self {
        let mut cache = Self::default();
        let workspace_root = &graph.root;

        for module in &graph.modules {
            let source_id_str = source_id_for(workspace_root, &module.file_path);
            if let Some(text) = texts.get(module.id.0 as usize) {
                cache
                    .sources
                    .insert(source_id_str.clone(), Arc::clone(text));
            }
            cache
                .names
                .insert(source_id_str.clone(), source_id_str.clone());
            cache.module_to_id.insert(module.id.0, source_id_str);
        }

        cache
    }

    /// Return a [`SourceId`] for the given module.
    ///
    /// Used by the driver when constructing per-module diagnostics (lex /
    /// parse errors that are keyed by `ModuleId`).
    #[must_use]
    pub fn id_for_module(&self, mid: ModuleId) -> SourceId {
        self.module_to_id.get(&mid.0).map_or_else(
            || SourceId::new(format!("<module {}>", mid.0)),
            SourceId::new,
        )
    }

    /// Return a placeholder [`SourceId`] for errors without a known source
    /// location (e.g. manifest errors, codegen toolchain errors).
    #[must_use]
    pub fn unknown_source_id() -> SourceId {
        SourceId::new("<unknown>")
    }

    /// Return the on-disk source text for a `source_id`, if this cache holds it.
    ///
    /// The LSP resolves diagnostic spans against this text rather than the
    /// editor's open-document buffer: `check_workspace` compiles the on-disk
    /// state, so a diagnostic's byte offsets index the text read from disk, not
    /// whatever an unsaved buffer currently contains.
    #[must_use]
    pub fn text(&self, source_id: &str) -> Option<&str> {
        self.sources.get(source_id).map(|arc| arc.as_str())
    }
}

impl SourceCache for WorkspaceSourceCache {
    fn fetch(&self, id: &SourceId) -> Option<&str> {
        self.sources.get(id.as_str()).map(|arc| arc.as_str())
    }

    fn display_name<'a>(&'a self, id: &'a SourceId) -> &'a str {
        self.names
            .get(id.as_str())
            .map_or_else(|| id.as_str(), String::as_str)
    }
}

/// A module's workspace-relative source id (forward slashes for
/// platform-neutral, CI-stable output).
fn source_id_for(workspace_root: &Path, file_path: &Path) -> String {
    file_path
        .strip_prefix(workspace_root)
        .unwrap_or(file_path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

// ── Per-module-id path resolution ─────────────────────────────────────────────

/// Look up the file path for a module by its `ModuleId`.
///
/// Used by `compile.rs` / `check.rs` when building `WorkspaceSourceCache`
/// and for diagnostic source-id resolution.
#[must_use]
pub fn module_file_path(graph: &WorkspaceGraph, mid: ModuleId) -> Option<&PathBuf> {
    graph.modules.get(mid.0 as usize).map(|m| &m.file_path)
}
