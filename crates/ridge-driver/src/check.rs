//! Implementation of [`check_workspace`] and [`check_workspace_typed`].
//!
//! Runs `discover → resolve → typecheck` only — no lowering, no codegen, no
//! BEAM files produced.  Collects all diagnostics and returns them.

use ridge_diagnostics::Diagnostic;
use ridge_manifest::find_workspace_root;
use ridge_resolve::{discover_workspace, resolve_workspace, WorkspaceGraph};
use ridge_typecheck::{typecheck_workspace, TypedWorkspace};

use crate::diag_adapters::diag_from_typecheck;
use crate::error::CheckError;
use crate::options::CheckOptions;
use crate::sources::WorkspaceSourceCache;

// ── Public types ──────────────────────────────────────────────────────────────

/// Artefacts produced by a successful [`check_workspace`] call.
///
/// `diagnostics` is **empty** on a fully successful check.  When non-empty,
/// render them via [`ridge_diagnostics::render_with_ariadne`] using `sources`
/// as the source cache.
#[derive(Debug)]
#[non_exhaustive]
pub struct CheckArtefacts {
    /// Accumulated structured diagnostics (lex, parse, resolve, typecheck).
    /// Empty on success.
    pub diagnostics: Vec<Diagnostic>,
    /// Source cache for rendering [`diagnostics`](Self::diagnostics).
    pub sources: WorkspaceSourceCache,
}

/// Artefacts produced by a successful [`check_workspace_typed`] call.
///
/// Extends [`CheckArtefacts`] with the fully-typed workspace and the workspace
/// graph, which `ridge test` uses to discover test functions, inspect their
/// types and capability sets, and derive BEAM module names from file paths.
#[derive(Debug)]
#[non_exhaustive]
pub struct CheckTypedArtefacts {
    /// Accumulated structured diagnostics (lex, parse, resolve, typecheck).
    /// Empty on success.
    pub diagnostics: Vec<Diagnostic>,
    /// Source cache for rendering [`diagnostics`](Self::diagnostics).
    pub sources: WorkspaceSourceCache,
    /// The fully type-checked workspace — used by `ridge test` for test
    /// function discovery, return-type inspection, and capability checks.
    pub typed: TypedWorkspace,
    /// The workspace graph — provides per-module `file_path` via
    /// [`WorkspaceGraph::modules`] so callers can derive BEAM module names.
    pub graph: WorkspaceGraph,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Type-check a Ridge workspace without producing any output artefacts.
///
/// ## Pipeline
///
/// Runs `discover_workspace → resolve_workspace → typecheck_workspace`.
/// Returns [`CheckArtefacts`] (diagnostics empty on success) or a fatal
/// [`CheckError`].
///
/// ## Errors
///
/// Fatal errors (`C001`–`C003`) are returned as [`CheckError`].
#[allow(clippy::needless_pass_by_value)]
pub fn check_workspace(options: CheckOptions) -> Result<CheckArtefacts, CheckError> {
    // ── 1. Verify workspace root ──────────────────────────────────────────────
    let _manifest_dir = find_workspace_root(&options.workspace_root).ok_or_else(|| {
        CheckError::NoWorkspaceRoot {
            path: options.workspace_root.clone(),
        }
    })?;

    // ── 2. Pipeline: discover → resolve → typecheck ───────────────────────────
    let disc = discover_workspace(&options.workspace_root);

    // Stash discovery-phase resolve errors (e.g. R023 LegacyRgExtension)
    // before consuming the struct.
    let disc_resolve_errors = disc.resolve_errors;

    let ws_graph = disc.graph.ok_or_else(|| CheckError::NoWorkspaceRoot {
        path: options.workspace_root.clone(),
    })?;

    let resolved = resolve_workspace(ws_graph);
    let typecheck_result = typecheck_workspace(&resolved);

    // ── 3. Collect diagnostics ────────────────────────────────────────────────
    // Build source cache from the workspace graph.
    let sources = WorkspaceSourceCache::from_workspace(&resolved.graph);

    // Surface lex + parse errors first — they are upstream of resolve and
    // typecheck, and missing them silently was a real bug (a malformed source
    // would falsely report "type-check passed" because items_parsed was 0).
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    // Discovery-phase errors (e.g. R023 for legacy .rg files) have no module
    // source location; use the unknown source placeholder.
    for e in &disc_resolve_errors {
        let sid = WorkspaceSourceCache::unknown_source_id();
        diagnostics.push(Diagnostic::from_resolve(e, sid));
    }

    for (mid, e) in &resolved.lex_errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_lex(*mid, e, sid));
    }

    for (mid, e) in &resolved.parse_errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_parse(*mid, e, sid));
    }

    for (mid, e) in &resolved.errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_resolve(e, sid));
    }

    for (mid, e) in &typecheck_result.errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(diag_from_typecheck(e, sid));
    }

    Ok(CheckArtefacts {
        diagnostics,
        sources,
    })
}

// ── check_workspace_typed ─────────────────────────────────────────────────────

/// Type-check a Ridge workspace and return the fully-typed representation.
///
/// Like [`check_workspace`] but also returns the [`TypedWorkspace`] so that
/// `ridge test` can discover test functions, inspect return types, and read
/// inferred capability sets without re-running the pipeline.
///
/// ## Errors
///
/// Fatal errors (`C001`–`C003`) are returned as [`CheckError`].
#[allow(clippy::needless_pass_by_value)]
pub fn check_workspace_typed(options: CheckOptions) -> Result<CheckTypedArtefacts, CheckError> {
    // ── 1. Verify workspace root ──────────────────────────────────────────────
    let _manifest_dir = find_workspace_root(&options.workspace_root).ok_or_else(|| {
        CheckError::NoWorkspaceRoot {
            path: options.workspace_root.clone(),
        }
    })?;

    // ── 2. Pipeline: discover → resolve → typecheck ───────────────────────────
    let disc = discover_workspace(&options.workspace_root);

    // Stash discovery-phase resolve errors (e.g. R023 LegacyRgExtension)
    // before consuming the struct.
    let disc_resolve_errors = disc.resolve_errors;

    let ws_graph = disc.graph.ok_or_else(|| CheckError::NoWorkspaceRoot {
        path: options.workspace_root.clone(),
    })?;

    let resolved = resolve_workspace(ws_graph);
    let typecheck_result = typecheck_workspace(&resolved);

    // ── 3. Collect diagnostics ────────────────────────────────────────────────
    let sources = WorkspaceSourceCache::from_workspace(&resolved.graph);

    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    // Discovery-phase errors (e.g. R023 for legacy .rg files) have no module
    // source location; use the unknown source placeholder.
    for e in &disc_resolve_errors {
        let sid = WorkspaceSourceCache::unknown_source_id();
        diagnostics.push(Diagnostic::from_resolve(e, sid));
    }

    for (mid, e) in &resolved.lex_errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_lex(*mid, e, sid));
    }

    for (mid, e) in &resolved.parse_errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_parse(*mid, e, sid));
    }

    for (mid, e) in &resolved.errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_resolve(e, sid));
    }

    for (mid, e) in &typecheck_result.errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(diag_from_typecheck(e, sid));
    }

    Ok(CheckTypedArtefacts {
        diagnostics,
        sources,
        typed: typecheck_result.typed,
        graph: resolved.graph,
    })
}
