//! Implementation of [`check_workspace`] and [`check_workspace_typed`].
//!
//! Runs `discover → resolve → typecheck` only — no lowering, no codegen, no
//! BEAM files produced.  Collects all diagnostics and returns them.

use std::sync::Arc;

use ridge_diagnostics::Diagnostic;
use ridge_manifest::find_workspace_root;
use ridge_resolve::{
    discover_standalone, discover_workspace, resolve_workspace_with, ModuleId, ResolveError,
    ResolvedWorkspace,
};
use ridge_typecheck::{typecheck_workspace, TypeError, TypedWorkspace};

use crate::diag_adapters::diag_from_typecheck;
use crate::error::CheckError;
use crate::incremental::IncrementalState;
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
/// Extends [`CheckArtefacts`] with the fully-typed workspace and the resolved
/// workspace, which `ridge test` uses to discover test functions, inspect their
/// types and capability sets, and derive BEAM module names from file paths. The
/// resolved workspace also carries per-module resolution data (symbols,
/// bindings, node-id maps) that the LSP queries.
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
    /// The resolved workspace. Its `graph` provides per-module `file_path`
    /// (for BEAM module names and source URIs); its `modules` carry the
    /// symbols, bindings, and node-id maps the LSP queries.
    pub resolved: ResolvedWorkspace,
}

// ── Diagnostic aggregation ────────────────────────────────────────────────────

/// Flatten every structured diagnostic — discovery, lex, parse, resolve, and
/// typecheck — into one list, resolving each to its source via `sources`.
///
/// Shared by the full-check entry points and the LSP's incremental path so they
/// produce byte-identical diagnostic sets.
#[must_use]
pub fn collect_diagnostics(
    disc_resolve_errors: &[ResolveError],
    resolved: &ResolvedWorkspace,
    type_errors: &[(ModuleId, TypeError)],
    sources: &WorkspaceSourceCache,
) -> Vec<Diagnostic> {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    // Discovery-phase errors (e.g. R023 for legacy .rg files) have no module
    // source location; use the unknown source placeholder.
    for e in disc_resolve_errors {
        diagnostics.push(Diagnostic::from_resolve(
            e,
            WorkspaceSourceCache::unknown_source_id(),
        ));
    }
    for (mid, e) in &resolved.lex_errors {
        diagnostics.push(Diagnostic::from_lex(*mid, e, sources.id_for_module(*mid)));
    }
    for (mid, e) in &resolved.parse_errors {
        diagnostics.push(Diagnostic::from_parse(*mid, e, sources.id_for_module(*mid)));
    }
    for (mid, e) in &resolved.errors {
        diagnostics.push(Diagnostic::from_resolve(e, sources.id_for_module(*mid)));
    }
    for (mid, e) in type_errors {
        diagnostics.push(diag_from_typecheck(e, sources.id_for_module(*mid)));
    }

    diagnostics
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

    let resolved = resolve_workspace_with(ws_graph, options.retain_indices);
    let typecheck_result = typecheck_workspace(&resolved);

    // ── 3. Collect diagnostics ────────────────────────────────────────────────
    // Build source cache from the workspace graph.
    let sources = WorkspaceSourceCache::from_workspace(&resolved.graph);

    // Surface lex + parse errors first — they are upstream of resolve and
    // typecheck, and missing them silently was a real bug (a malformed source
    // would falsely report "type-check passed" because items_parsed was 0).
    let diagnostics = collect_diagnostics(
        &disc_resolve_errors,
        &resolved,
        &typecheck_result.errors,
        &sources,
    );

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

    let resolved = resolve_workspace_with(ws_graph, options.retain_indices);
    let typecheck_result = typecheck_workspace(&resolved);

    // ── 3. Collect diagnostics ────────────────────────────────────────────────
    let sources = WorkspaceSourceCache::from_workspace(&resolved.graph);

    let diagnostics = collect_diagnostics(
        &disc_resolve_errors,
        &resolved,
        &typecheck_result.errors,
        &sources,
    );

    Ok(CheckTypedArtefacts {
        diagnostics,
        sources,
        typed: typecheck_result.typed,
        resolved,
    })
}

// ── check_workspace_incremental ───────────────────────────────────────────────

/// Seed an [`IncrementalState`] from a full check, for the LSP's incremental path.
///
/// Runs the same `discover → resolve → typecheck` pipeline as
/// [`check_workspace_typed`], then bundles the result into an engine that also
/// retains each module's source text, so later single-file edits can recompute
/// only what they affect and still reproduce diagnostics and an index.
///
/// ## Errors
///
/// Fatal errors (`C001`–`C003`) are returned as [`CheckError`].
#[allow(clippy::needless_pass_by_value)]
pub fn check_workspace_incremental(options: CheckOptions) -> Result<IncrementalState, CheckError> {
    let _manifest_dir = find_workspace_root(&options.workspace_root).ok_or_else(|| {
        CheckError::NoWorkspaceRoot {
            path: options.workspace_root.clone(),
        }
    })?;

    let disc = discover_workspace(&options.workspace_root);
    let disc_resolve_errors = disc.resolve_errors;
    let ws_graph = disc.graph.ok_or_else(|| CheckError::NoWorkspaceRoot {
        path: options.workspace_root.clone(),
    })?;

    let resolved = resolve_workspace_with(ws_graph, options.retain_indices);
    let typecheck_result = typecheck_workspace(&resolved);

    // Capture each module's on-disk text (indexed by ModuleId.0) so the engine
    // can track per-module source across edits.
    let sources = WorkspaceSourceCache::from_workspace(&resolved.graph);
    let mut module_sources: Vec<Arc<String>> = (0..resolved.modules.len())
        .map(|_| Arc::new(String::new()))
        .collect();
    for module in &resolved.graph.modules {
        let i = module.id.0 as usize;
        if let (Some(slot), Some(text)) = (
            module_sources.get_mut(i),
            sources.text(sources.id_for_module(module.id).as_str()),
        ) {
            *slot = Arc::new(text.to_owned());
        }
    }

    Ok(
        IncrementalState::new(resolved, typecheck_result, disc_resolve_errors)
            .with_module_sources(module_sources),
    )
}

// ── check_standalone_incremental ──────────────────────────────────────────────

/// Seed an [`IncrementalState`] from a set of standalone `.ridge` files that
/// live outside any workspace manifest.
///
/// The twin of [`check_workspace_incremental`] for the language server's
/// standalone mode: instead of discovering a workspace on disk, it synthesises a
/// graph where each file is its own isolated single-module project (see
/// [`discover_standalone`]), then runs the same `resolve → typecheck` pipeline.
/// Each file type-checks against the built-in prelude, so a loose buffer with no
/// project still gets diagnostics, hover, and navigation.
///
/// There is no workspace root to verify, so this is infallible; an empty `files`
/// slice yields an empty but valid state.
#[must_use]
pub fn check_standalone_incremental(files: &[std::path::PathBuf]) -> IncrementalState {
    let ws_graph = discover_standalone(files);

    let resolved = resolve_workspace_with(ws_graph, true);
    let typecheck_result = typecheck_workspace(&resolved);

    let sources = WorkspaceSourceCache::from_workspace(&resolved.graph);
    let mut module_sources: Vec<Arc<String>> = (0..resolved.modules.len())
        .map(|_| Arc::new(String::new()))
        .collect();
    for module in &resolved.graph.modules {
        let i = module.id.0 as usize;
        if let (Some(slot), Some(text)) = (
            module_sources.get_mut(i),
            sources.text(sources.id_for_module(module.id).as_str()),
        ) {
            *slot = Arc::new(text.to_owned());
        }
    }

    // Synthetic discovery produces no discovery-phase resolve errors.
    IncrementalState::new(resolved, typecheck_result, Vec::new())
        .with_module_sources(module_sources)
}
