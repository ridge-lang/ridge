//! Implementation of [`compile_workspace`].
//!
//! Wires `ridge-resolve → ridge-typecheck → ridge-lower → ridge-codegen-erl`
//! per workspace member and accumulates artefacts.
//!
//! Output directory: `<workspace_root>/target/ridge/<profile>/beam/`.

use rustc_hash::FxHashMap;
use std::path::PathBuf;

use ridge_codegen_erl::{
    codegen_stdlib_module_with_fqn, codegen_workspace, erlc, BuildProfile, CodegenOptions,
};
use ridge_diagnostics::Diagnostic;
use ridge_ir::{IrNodeId, LoweredModule};
use ridge_lower::lower_workspace;
use ridge_manifest::find_workspace_root;
use ridge_resolve::{discover_workspace, resolve_workspace, ModuleId, NodeId, Severity};
use ridge_typecheck::typecheck_workspace;

use crate::diag_adapters::{diag_from_codegen, diag_from_typecheck};
use crate::error::CompileError;
use crate::options::{CompileOptions, Profile};
use crate::sources::WorkspaceSourceCache;

// ── Public types ──────────────────────────────────────────────────────────────

/// Source map for one module: maps IR node ids back to AST node ids.
///
/// Sparse — synthesised IR nodes (e.g. interpolation-emitted `ToText` calls)
/// have no upstream [`NodeId`] and are absent.  Used by the LSP to map
/// codegen-level errors back to source spans.
pub type SourceMap = FxHashMap<IrNodeId, NodeId>;

/// Artefacts produced by a successful [`compile_workspace`] call.
///
/// `diagnostics` is **empty** on a fully successful compile.  When non-empty,
/// the driver continued on a best-effort basis; callers should inspect and
/// render them via [`ridge_diagnostics::render_with_ariadne`].
#[derive(Debug)]
#[non_exhaustive]
pub struct CompileArtefacts {
    /// Paths to every `.beam` file written to disk.
    pub beam_files: Vec<PathBuf>,
    /// Paths to every `.core` (Core Erlang text) file written to disk.
    ///
    /// Non-empty only when [`EmitArtefacts::Core`] or [`EmitArtefacts::Both`]
    /// was requested.
    pub core_files: Vec<PathBuf>,
    /// Accumulated structured diagnostics (lex, parse, resolve, typecheck,
    /// codegen).  Empty on success.
    pub diagnostics: Vec<Diagnostic>,
    /// Source cache for rendering [`diagnostics`](Self::diagnostics).
    pub sources: WorkspaceSourceCache,
    /// Per-module source maps for the LSP (maps IR node ids to AST node ids).
    pub source_maps: FxHashMap<ModuleId, SourceMap>,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Compile a Ridge workspace, producing `.beam` and/or `.core` artefacts.
///
/// ## Pipeline
///
/// 1. Locate the workspace root via [`find_workspace_root`].
/// 2. Run `discover_workspace → resolve_workspace → typecheck_workspace →
///    lower_workspace → codegen_workspace`.
/// 3. Write output files to `<workspace_root>/target/ridge/<profile>/`.
/// 4. Return [`CompileArtefacts`] or a fatal [`CompileError`].
///
/// ## Errors
///
/// Fatal errors (`C001`–`C004`, `C009`) are returned as [`CompileError`].  Non-fatal
/// compile diagnostics are accumulated in [`CompileArtefacts::diagnostics`].
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn compile_workspace(options: CompileOptions) -> Result<CompileArtefacts, CompileError> {
    // ── 1. Verify workspace root ──────────────────────────────────────────────
    // Verify the provided root actually contains a workspace manifest.
    // `find_workspace_root` walks up; if the caller passed an exact root we
    // start our search there.
    let _manifest_dir = find_workspace_root(&options.workspace_root).ok_or_else(|| {
        CompileError::NoWorkspaceRoot {
            path: options.workspace_root.clone(),
        }
    })?;

    // ── 2. Pipeline: discover → resolve → typecheck → lower ──────────────────
    let disc = discover_workspace(&options.workspace_root);

    // Stash discovery-phase resolve errors (e.g. R023 LegacyRgExtension)
    // before consuming the struct.
    let disc_resolve_errors = disc.resolve_errors;

    // Surface R001 (no workspace manifest) as C001.
    let ws_graph = disc.graph.ok_or_else(|| CompileError::NoWorkspaceRoot {
        path: options.workspace_root.clone(),
    })?;

    // ── 2.5. Resolve external dependencies (T8) ──────────────────────────────
    // Populate the package cache so import resolution sees cached dep paths
    // before the per-project compile.  Re-parse the workspace and per-project
    // manifests via `ridge_manifest` because `ridge-resolve` and
    // `ridge-manifest` own independent, parallel manifest types — the Rust types
    // are distinct even though their shape is identical.  See T8 plan note and
    // `ridge-manifest/tests/parity_test.rs`. // T8
    let cache_root = match &options.cache_root {
        Some(p) => p.clone(),
        None => {
            ridge_pkg::cache_root().map_err(|e| CompileError::PkgResolutionFailed { source: e })?
        }
    };

    // Re-parse the workspace ridge.toml using ridge-manifest types.
    let workspace_manifest_path = ws_graph.root.join("ridge.toml");
    let workspace_toml_src =
        std::fs::read_to_string(&workspace_manifest_path).map_err(|e| CompileError::Io {
            message: format!("reading workspace manifest: {e}"),
        })?;
    let workspace_manifest =
        ridge_manifest::parse_workspace(&workspace_toml_src, &workspace_manifest_path).map_err(
            |e| CompileError::PkgResolutionFailed {
                source: ridge_pkg::PkgError::PkgManifestParseFailed {
                    path: workspace_manifest_path.clone(),
                    source: e,
                },
            },
        )?;

    // For each workspace member, resolve its package dependencies.
    // Projects with no declared deps are skipped to avoid pointless work.
    for project in &ws_graph.projects {
        let proj_manifest_path = &project.manifest_path;
        let proj_toml_src =
            std::fs::read_to_string(proj_manifest_path).map_err(|e| CompileError::Io {
                message: format!(
                    "reading project manifest {}: {e}",
                    proj_manifest_path.display()
                ),
            })?;
        let project_manifest = ridge_manifest::parse_project(&proj_toml_src, proj_manifest_path)
            .map_err(|e| CompileError::PkgResolutionFailed {
                source: ridge_pkg::PkgError::PkgManifestParseFailed {
                    path: proj_manifest_path.clone(),
                    source: e,
                },
            })?;

        // Skip projects that declare no deps — no cache work needed.
        if project_manifest.dependencies.is_empty() {
            continue;
        }

        // Resolve deps: populates the cache for each git/path dep.
        // The resolved paths are not yet threaded into ridge-resolve's import
        // resolver — cache population is the T8 DoD / G5 observable.
        // Threading resolved paths into the import resolver is deferred (T8
        // plan §3.9 + OQ-C-future). // T8
        let _resolved_deps =
            ridge_pkg::resolve_dependencies(&workspace_manifest, &project_manifest, &cache_root)?;
    }

    let resolved = resolve_workspace(ws_graph);
    let typecheck_result = typecheck_workspace(&resolved);
    let lowered = lower_workspace(&typecheck_result.typed, &resolved);

    // ── 3. Collect source maps ────────────────────────────────────────────────
    let source_maps = collect_source_maps(&lowered.modules);

    // ── 4. Codegen ───────────────────────────────────────────────────────────
    // Output root is `<workspace_root>/target/ridge/<profile>/`.
    let out_root = options
        .workspace_root
        .join("target")
        .join("ridge")
        .join(options.profile.dir_name());

    let codegen_profile = map_profile(options.profile);

    // Decide whether to invoke erlc based on EmitArtefacts.
    // EmitArtefacts::Core means .core only — no erlc invocation.
    let invoke_erlc = options.emit.emit_beam();

    // CodegenOptions is #[non_exhaustive], so we build via Default then patch.
    let mut codegen_opts = CodegenOptions::default();
    codegen_opts.out_root = out_root;
    codegen_opts.profile = codegen_profile;
    codegen_opts.invoke_erlc = invoke_erlc;
    codegen_opts.install_runtime = true;

    // Capture out_root before codegen_workspace moves codegen_opts.
    let codegen_out_root = codegen_opts.out_root.clone();
    let codegen_result = codegen_workspace(&lowered, codegen_opts);

    // ── 4b. Stdlib `.beam` distribution ──────────────────────────────────────
    // Compile the Ridge stdlib sources into `<out_root>/beam/` so that
    // `BridgeTarget::RidgeStdlibLocal` callers (e.g. `call 'std.list':head(1)`)
    // can find their BEAM modules at runtime.
    //
    // Idempotent: skipped when `std.list.beam` already exists in the beam dir,
    // which covers incremental rebuilds and repeated `ridge test` runs.
    //
    // Only runs when `invoke_erlc` is true — `.core`-only builds do not need
    // the stdlib on the BEAM code path.
    if invoke_erlc {
        let beam_dir = codegen_out_root.join("beam");
        // Non-fatal: stdlib compilation errors do not abort the user's build.
        let _ = compile_stdlib_beams(&beam_dir, &codegen_out_root, map_profile(options.profile));
    }

    // ── 5. Collect artefact paths and diagnostics ─────────────────────────────
    let mut beam_files: Vec<PathBuf> = Vec::new();
    let mut core_files: Vec<PathBuf> = Vec::new();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    for module_opt in &codegen_result.modules {
        let Some(m) = module_opt else { continue };
        if options.emit.emit_core() {
            core_files.push(m.core_path.clone());
        }
        if let Some(beam_path) = &m.beam_path {
            beam_files.push(beam_path.clone());
        }
    }

    // Build source cache from the workspace graph — used both here and
    // returned to the caller for rendering.
    let sources = WorkspaceSourceCache::from_workspace(&resolved.graph);

    // Discovery-phase errors (e.g. R023 for legacy .rg files) have no module
    // source location; use the unknown source placeholder.
    for e in &disc_resolve_errors {
        let sid = WorkspaceSourceCache::unknown_source_id();
        diagnostics.push(Diagnostic::from_resolve(e, sid));
    }

    // Surface lex + parse errors first — they are upstream of every other
    // pass.  Missing them silently meant `ridge build` would compile "0
    // modules" without telling the user the source was malformed.
    for (mid, e) in &resolved.lex_errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_lex(*mid, e, sid));
    }

    for (mid, e) in &resolved.parse_errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_parse(*mid, e, sid));
    }

    // Surface resolve errors.
    for (mid, e) in &resolved.errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(Diagnostic::from_resolve(e, sid));
    }

    // Surface typecheck errors.
    for (mid, e) in &typecheck_result.errors {
        let sid = sources.id_for_module(*mid);
        diagnostics.push(diag_from_typecheck(e, sid));
    }

    // Surface codegen errors (non-fatal; best-effort).
    for e in &codegen_result.errors {
        let sid = WorkspaceSourceCache::unknown_source_id();
        diagnostics.push(diag_from_codegen(e, sid));
    }

    Ok(CompileArtefacts {
        beam_files,
        core_files,
        diagnostics,
        sources,
        source_maps,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compile Ridge stdlib `.ridge` sources to `.beam` files and place them in `beam_dir`.
///
/// Each stdlib module's BEAM atom is its dotted FQN (e.g. `'std.list'`), so the
/// corresponding file is `std.list.beam`.  This is required for
/// `BridgeTarget::RidgeStdlibLocal` callers that emit `call 'std.list':head(1)`.
///
/// Idempotent: returns early if `beam_dir/std.list.beam` already exists.
///
/// The stdlib compilation lives in the user-facing build pipeline
/// (`compile_workspace`), NOT in a test-only harness shim.
///
/// # Errors
///
/// Returns the first `ridge_codegen_erl::CodegenError` encountered (output dir
/// creation, lowering, or `erlc` failure).
fn compile_stdlib_beams(
    beam_dir: &std::path::Path,
    out_root: &std::path::Path,
    profile: BuildProfile,
) -> Result<(), ridge_codegen_erl::CodegenError> {
    // Idempotency check: if `std.list.beam` already exists, stdlib is compiled.
    if beam_dir.join("std.list.beam").exists() {
        return Ok(());
    }

    // Locate stdlib sources via the `ridge-stdlib` crate's embedded manifest dir.
    let stdlib_src = ridge_stdlib::stdlib_sources_dir();

    // Build a temporary workspace pointing at the stdlib source directory.
    let td = tempfile::TempDir::new().map_err(|e| {
        ridge_codegen_erl::CodegenError::OutputDirNotWritable {
            path: out_root.to_path_buf(),
            io_err: e.to_string(),
        }
    })?;
    let ws_root = td.path();

    // Write workspace manifest.
    std::fs::write(
        ws_root.join("ridge.toml"),
        "[workspace]\nname = \"stdlib-build\"\nversion = \"0.1.0\"\nmembers = [\"std\"]\n",
    )
    .map_err(|e| ridge_codegen_erl::CodegenError::OutputDirNotWritable {
        path: ws_root.join("ridge.toml"),
        io_err: e.to_string(),
    })?;

    // Write project manifest with absolute src_root pointing at real stdlib.
    let std_dir = ws_root.join("std");
    std::fs::create_dir_all(&std_dir).map_err(|e| {
        ridge_codegen_erl::CodegenError::OutputDirNotWritable {
            path: std_dir.clone(),
            io_err: e.to_string(),
        }
    })?;
    // On Windows, Path::display() uses backslashes; TOML requires a string value
    // that the manifest parser stores verbatim, then feeds to Path::join.
    // Using forward slashes works on all platforms for the absolute path string.
    let stdlib_src_str = stdlib_src.to_string_lossy().replace('\\', "/");
    let proj_toml = format!(
        "[project]\nname = \"std\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.src]\nroot = \"{stdlib_src_str}\"\n\n[project.exports]\npublic = [\"std.**\"]\n"
    );
    std::fs::write(std_dir.join("ridge.toml"), &proj_toml).map_err(|e| {
        ridge_codegen_erl::CodegenError::OutputDirNotWritable {
            path: std_dir.join("ridge.toml"),
            io_err: e.to_string(),
        }
    })?;

    // Run the Ridge pipeline over the stdlib workspace.
    let disc = discover_workspace(ws_root);
    let Some(ws_graph) = disc.graph else {
        // Discovery failed — not a fatal codegen error, but we can't proceed.
        return Ok(());
    };
    let resolved = resolve_workspace(ws_graph);
    // Surface any errors that would prevent useful compilation.
    if resolved
        .errors
        .iter()
        .any(|(_, e)| e.severity() == Severity::Error)
    {
        return Ok(());
    }
    let typecheck_result = typecheck_workspace(&resolved);
    if !typecheck_result.errors.is_empty() {
        return Ok(());
    }
    let lowered = lower_workspace(&typecheck_result.typed, &resolved);

    // Build a FQN map: ModuleId -> fully_qualified_name.
    let fqn_map: std::collections::HashMap<ModuleId, String> = resolved
        .graph
        .modules
        .iter()
        .map(|m| (m.id, m.fully_qualified_name.clone()))
        .collect();

    // Ensure output dirs exist.
    std::fs::create_dir_all(beam_dir).map_err(|e| {
        ridge_codegen_erl::CodegenError::OutputDirNotWritable {
            path: beam_dir.to_path_buf(),
            io_err: e.to_string(),
        }
    })?;
    std::fs::create_dir_all(out_root.join("core")).map_err(|e| {
        ridge_codegen_erl::CodegenError::OutputDirNotWritable {
            path: out_root.join("core"),
            io_err: e.to_string(),
        }
    })?;

    // Probe erlc.
    let Ok(erlc_info) = erlc::probe(None) else {
        return Ok(()); // erlc not available — skip silently
    };

    // Compile each stdlib module with its FQN as the BEAM atom.
    // Skip `.test.ridge` modules (FQN contains ".test") — test files are not
    // distributable stdlib modules.
    for slot in &lowered.modules {
        let Some(m) = slot else { continue };
        let fqn = match fqn_map.get(&m.id) {
            Some(n) => n.clone(),
            None => continue,
        };
        // Skip test modules (std.list.test, std.option.test, etc.).
        if fqn.contains(".test") {
            continue;
        }
        // Skip if this module's .beam already exists (idempotent at module level).
        let beam_path = beam_dir.join(format!("{fqn}.beam"));
        if beam_path.exists() {
            continue;
        }
        // Compile the module with its FQN as the BEAM atom.
        codegen_stdlib_module_with_fqn(m, &lowered, &fqn, out_root, Some(&erlc_info), profile)?;
        // Move the produced .beam from out_root/beam/ to beam_dir (they should be the same).
        // (No move needed — out_root/beam IS beam_dir per compile_workspace convention.)
    }

    Ok(())
}

/// Map a driver [`Profile`] to a codegen [`BuildProfile`].
const fn map_profile(p: Profile) -> BuildProfile {
    match p {
        Profile::Debug => BuildProfile::Debug,
        Profile::Release => BuildProfile::Release,
    }
}

/// Collect per-module source maps from the lowered workspace.
fn collect_source_maps(
    modules: &[Option<LoweredModule>],
) -> FxHashMap<ModuleId, FxHashMap<IrNodeId, NodeId>> {
    let mut maps = FxHashMap::default();
    for slot in modules {
        let Some(m) = slot else { continue };
        maps.insert(m.id, m.source_map.clone());
    }
    maps
}
