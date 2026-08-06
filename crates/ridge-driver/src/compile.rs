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
use ridge_ir::{IrItem, IrNodeId, LoweredModule};
use ridge_lower::lower_workspace;
use ridge_manifest::find_workspace_root;
use ridge_resolve::{discover_workspace, resolve_workspace, ModuleId, NodeId, Severity};
use ridge_typecheck::{typecheck_workspace, typecheck_workspace_with_history};

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
    /// Every module that defines a top-level `fn main` — the runnable entry
    /// points. Used to launch the module that actually carries `main` rather
    /// than the alphabetically-first compiled module. Empty for a library-only
    /// build.
    pub entry_modules: Vec<EntryModule>,
}

/// A module that defines a top-level `fn main`, i.e. a runnable entry point.
///
/// `ridge run` and `ridge build --bin` consult this to launch the module that
/// actually carries `main`, instead of assuming it is the first `.beam`
/// produced (the modules are ordered by fully-qualified name, so the entry
/// module is only first by coincidence).
#[derive(Debug, Clone)]
pub struct EntryModule {
    /// The owning project's `[project].name`, so a multi-app workspace can pick
    /// the entry point that matches the requested `--member`.
    pub project_name: String,
    /// The module's fully-qualified name (e.g. `acme.cli.Main`).
    pub module_fqn: String,
    /// The BEAM module atom to invoke (e.g. `ridge_module_2`).
    pub beam_module: String,
}

/// Pick the entry-point BEAM atom for a run or escript launch.
///
/// Prefers the entry module whose project matches `member`; failing that, uses
/// the sole entry module if there is exactly one. Returns `None` when the
/// choice is ambiguous (several apps, none matching `member`) or no module
/// defines `main`, letting the caller fall back to its legacy behaviour.
#[must_use]
pub fn select_entry_beam(entries: &[EntryModule], member: &str) -> Option<String> {
    if let Some(e) = entries.iter().find(|e| e.project_name == member) {
        return Some(e.beam_module.clone());
    }
    if let [only] = entries {
        return Some(only.beam_module.clone());
    }
    None
}

/// Whether `module` is the entry its project declared, or belongs to a project
/// that declares none.
///
/// A project with no `entry` — a library, a test project — has no declared
/// entry to disagree with, so every module carrying a `main` stays a
/// candidate, as before.
fn is_declared_entry(graph: &ridge_resolve::WorkspaceGraph, module: ModuleId) -> bool {
    let Some(meta) = graph.modules.iter().find(|m| m.id == module) else {
        return false;
    };
    let Some(project) = graph.projects.get(meta.project.0 as usize) else {
        return false;
    };
    let Some(entry) = &project.entry else {
        return true;
    };
    meta.file_path == *entry
        || match (meta.file_path.canonicalize(), entry.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
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
    let _manifest_dir = find_workspace_root(&options.workspace_root)
        .ok_or_else(|| CompileError::no_workspace_root(options.workspace_root.clone()))?;

    // ── 2. Pipeline: discover → resolve → typecheck → lower ──────────────────
    let disc = discover_workspace(&options.workspace_root);

    // Stash discovery-phase resolve errors (e.g. R023 LegacyRgExtension)
    // before consuming the struct.
    let disc_resolve_errors = disc.resolve_errors;

    // Surface R001 (no workspace manifest) as C001.
    let mut ws_graph = disc
        .graph
        .ok_or_else(|| CompileError::no_workspace_root(options.workspace_root.clone()))?;
    ws_graph.is_stdlib = options.is_stdlib;

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
    // Read the previous build's snapshot before anything overwrites it: its
    // version history is injected into typechecking so `migrate` hooks can
    // resolve `Name@N`, and it is passed back into `extract_snapshot` at the
    // end of the compile. Both are best-effort — a missing snapshot or one in
    // a NEWER format is an empty history, i.e. a fresh build.
    let prev_snapshot =
        crate::reload::read_prev_snapshot(&options.workspace_root, options.profile.dir_name())
            .filter(|s| s.format <= ridge_reload::snapshot::SNAPSHOT_FORMAT);
    let version_history = prev_snapshot.as_ref().map_or_else(
        ridge_reload::VersionHistory::default,
        ridge_reload::snapshot::history_of,
    );
    let typecheck_result = typecheck_workspace_with_history(&resolved, &version_history);
    let mut lowered = lower_workspace(&typecheck_result.typed, &resolved);

    // Seed stable FQN-derived target module names so codegen never derives
    // names from ModuleId ordering. Mangling cannot fail here: user module
    // FQNs always carry at least a project prefix, so the reserved `ridge_rt`
    // atom is unreachable — but fall back to the legacy segment defensively.
    lowered.target_names = resolved
        .graph
        .modules
        .iter()
        .map(|m| {
            ridge_codegen_erl::module::beam_name_for_fqn(&m.fully_qualified_name, m.id)
                .unwrap_or_else(|_| format!("ridge_module_{}", m.id.0))
        })
        .collect();
    // FQNs alongside: codegen renders field types for the shared shape hash.
    lowered.module_fqns = resolved
        .graph
        .modules
        .iter()
        .map(|m| m.fully_qualified_name.clone())
        .collect();

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
    // Idempotent: skipped when the emitted set is complete, written by this
    // compiler version, and no older than the compiler binary — which covers
    // incremental rebuilds and repeated `ridge test` runs without treating one
    // file as proof of the whole standard library.
    //
    // Only runs when `invoke_erlc` is true — `.core`-only builds do not need
    // the stdlib on the BEAM code path.
    if invoke_erlc {
        let beam_dir = codegen_out_root.join("beam");
        // A failure here leaves a typecheck-clean build whose stdlib modules are
        // absent, so anything calling a Ridge-bodied stdlib function dies at
        // startup with `undef`. How loudly to say so depends on whether this
        // invocation is the one that runs it: `build` hands the artefacts to a
        // later step that can act on a warning, while `run` and `test` are that
        // step and gain nothing from starting a program already known to be
        // broken.
        if let Err(e) =
            compile_stdlib_beams(&beam_dir, &codegen_out_root, map_profile(options.profile))
        {
            let reason = describe_stdlib_bundle_failure(&e);
            if options.will_execute {
                return Err(CompileError::StdlibBundleFailed { message: reason });
            }
            eprintln!("warning: stdlib BEAM bundling failed: {reason}");
            eprintln!(
                "warning: programs calling Ridge-bodied stdlib functions (List.head, Option.withDefault, ...) will crash at runtime with `undef`."
            );
        }
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

    // ── 5b. Entry-point modules (the modules that define `fn main`) ──────────
    // Record the BEAM atom of every module that carries a top-level `fn main`,
    // tagged with its project name, so `ridge run` / `ridge build --bin` launch
    // the real entry point rather than `beam_files[0]` (which is merely the
    // first module by fully-qualified name).
    let beam_by_module: FxHashMap<ModuleId, String> = codegen_result
        .modules
        .iter()
        .filter_map(|slot| slot.as_ref())
        .map(|m| (m.module, m.beam_module_name.clone()))
        .collect();
    let mut entry_modules: Vec<EntryModule> = Vec::new();
    for slot in &lowered.modules {
        let Some(m) = slot else { continue };
        let has_main = m
            .items
            .iter()
            .any(|item| matches!(item, IrItem::Fn(f) if f.is_main));
        if !has_main {
            continue;
        }
        // A project that declares an entry has exactly one: the module the
        // manifest names. Any other `main` in the same project is an ordinary
        // function that happens to carry the name, not a second candidate —
        // treating it as one made the alphabetically-first module win over the
        // declared entry. Projects with no declared entry (libraries) keep the
        // has-a-`main` rule.
        if !is_declared_entry(&resolved.graph, m.id) {
            continue;
        }
        let Some(beam_module) = beam_by_module.get(&m.id).cloned() else {
            continue;
        };
        let (module_fqn, project_name) = resolved
            .graph
            .modules
            .iter()
            .find(|mm| mm.id == m.id)
            .map(|mm| {
                let proj = resolved
                    .graph
                    .projects
                    .get(mm.project.0 as usize)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                (mm.fully_qualified_name.clone(), proj)
            })
            .unwrap_or_default();
        entry_modules.push(EntryModule {
            project_name,
            module_fqn,
            beam_module,
        });
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

    // A project whose manifest did not parse was skipped by discovery, so it
    // contributes no modules and no errors of any other kind. Without this the
    // build reports "Compiled 0 module(s)" and exits 0.
    for e in &resolved.manifest_errors {
        let sid = WorkspaceSourceCache::unknown_source_id();
        diagnostics.push(Diagnostic::from_manifest(e, sid));
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

    // ── 6. Reload snapshot (auxiliary state — never fails the build) ─────────
    // Persist the public surface so a later `reload --check` can diff against
    // it. Only written when the workspace compiled without errors; write
    // failures are logged and ignored.
    if !diagnostics
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
    {
        let snapshot = ridge_reload::snapshot::extract_snapshot(
            &resolved,
            &typecheck_result.typed,
            prev_snapshot.as_ref(),
        );
        match serde_json::to_string_pretty(&snapshot) {
            Ok(json) => {
                let path = codegen_out_root.join("reload-snapshot.json");
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!(
                        "warning: could not write reload snapshot at {}: {e}",
                        path.display()
                    );
                }
            }
            Err(e) => eprintln!("warning: could not serialise reload snapshot: {e}"),
        }
    }

    Ok(CompileArtefacts {
        beam_files,
        core_files,
        diagnostics,
        sources,
        source_maps,
        entry_modules,
    })
}

/// Materialise a self-contained workspace holding the embedded standard library
/// under `ws_root`, ready to be compiled AS the stdlib (`CheckOptions::is_stdlib
/// = true`).
///
/// Used by `ridge test --stdlib` to run the stdlib's own `.test.ridge` suite
/// against exactly the sources the compiler carries. Only the compiler's own
/// embedded stdlib is written — never any caller-supplied code — so enabling
/// `is_stdlib` for this workspace cannot expose `@ffi` to a user project.
///
/// The caller owns `ws_root` (typically a [`tempfile::TempDir`]) and keeps it
/// alive for the duration of the compile.
///
/// # Errors
///
/// Returns the first I/O error encountered while unpacking the sources or
/// writing the workspace/project manifests.
pub fn write_stdlib_test_workspace(ws_root: &std::path::Path) -> std::io::Result<()> {
    let std_dir = ws_root.join("std");
    ridge_stdlib::write_stdlib_sources_to(&std_dir.join("src"))?;
    std::fs::write(
        ws_root.join("ridge.toml"),
        "[workspace]\nname = \"stdlib-test\"\nversion = \"0.1.0\"\nmembers = [\"std\"]\n",
    )?;
    std::fs::write(
        std_dir.join("ridge.toml"),
        "[project]\nname = \"std\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.src]\nroot = \"src\"\n\n[project.exports]\npublic = [\"std.**\"]\n",
    )?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Describe why the stdlib could not be bundled, in terms a user can act on.
///
/// `CodegenError` has no `Display`, and its `Debug` form buries the useful part
/// — which module failed and what the toolchain said — inside a struct dump
/// with escaped newlines.
fn describe_stdlib_bundle_failure(e: &ridge_codegen_erl::CodegenError) -> String {
    use ridge_codegen_erl::CodegenError as E;
    match e {
        E::ErlcRejectedInput {
            core_path,
            stderr,
            exit_code,
        } => {
            let module = core_path.file_stem().map_or_else(
                || core_path.display().to_string(),
                |s| s.to_string_lossy().into_owned(),
            );
            let detail = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            format!("erlc rejected `{module}` (exit {exit_code}): {detail}")
        }
        E::ErlcNotFound { .. } => "erlc was not found on PATH".to_owned(),
        E::OutputDirNotWritable { path, io_err } => {
            format!(
                "output directory {} is not writable: {io_err}",
                path.display()
            )
        }
        other => format!("{other:?}"),
    }
}

/// Compile Ridge stdlib `.ridge` sources to `.beam` files and place them in `beam_dir`.
///
/// Each stdlib module's BEAM atom is its dotted FQN (e.g. `'std.list'`), so the
/// corresponding file is `std.list.beam`. This is required for
/// `BridgeTarget::RidgeStdlibLocal` callers that emit `call 'std.list':head(1)`.
///
/// Sources are unpacked from the [`ridge_stdlib::STDLIB_SOURCES`] slice into a
/// per-build tempdir, not read from a compile-time path. Released binaries are
/// therefore independent of the absolute layout of the machine that built them.
///
/// Idempotent, but on the whole emitted set rather than on one file of it — see
/// [`stdlib_beams_are_current`].
///
/// The stdlib compilation lives in the user-facing build pipeline
/// (`compile_workspace`), NOT in a test-only harness shim.
///
/// # Errors
///
/// Returns the first `ridge_codegen_erl::CodegenError` encountered (output dir
/// creation, source unpacking, lowering, or `erlc` failure).
#[allow(clippy::too_many_lines)]
fn compile_stdlib_beams(
    beam_dir: &std::path::Path,
    out_root: &std::path::Path,
    profile: BuildProfile,
) -> Result<(), ridge_codegen_erl::CodegenError> {
    if stdlib_beams_are_current(beam_dir, out_root) {
        return Ok(());
    }

    // Build a temporary workspace and unpack the embedded stdlib sources into
    // it. Released binaries cannot rely on a compile-time absolute path to the
    // stdlib sources because that path only resolves on the build machine —
    // so we unpack from the `STDLIB_SOURCES` slice embedded by ridge-stdlib's
    // build script.
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

    // Unpack embedded sources into `<ws_root>/std/src/`.
    let std_dir = ws_root.join("std");
    let std_src_dir = std_dir.join("src");
    ridge_stdlib::write_stdlib_sources_to(&std_src_dir).map_err(|e| {
        ridge_codegen_erl::CodegenError::OutputDirNotWritable {
            path: std_src_dir.clone(),
            io_err: e.to_string(),
        }
    })?;

    // Write project manifest with src_root pointing at the unpacked sources.
    let proj_toml = "[project]\nname = \"std\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.src]\nroot = \"src\"\n\n[project.exports]\npublic = [\"std.**\"]\n";
    std::fs::write(std_dir.join("ridge.toml"), proj_toml).map_err(|e| {
        ridge_codegen_erl::CodegenError::OutputDirNotWritable {
            path: std_dir.join("ridge.toml"),
            io_err: e.to_string(),
        }
    })?;

    // Run the Ridge pipeline over the stdlib workspace.
    let disc = discover_workspace(ws_root);
    let Some(mut ws_graph) = disc.graph else {
        eprintln!(
            "warning: stdlib BEAM bundling: workspace discovery failed at {}",
            ws_root.display()
        );
        return Ok(());
    };
    ws_graph.is_stdlib = true; // these are stdlib sources; R022 permits @ffi
    let resolved = resolve_workspace(ws_graph);
    if resolved
        .errors
        .iter()
        .any(|(_, e)| e.severity() == Severity::Error)
    {
        eprintln!(
            "warning: stdlib BEAM bundling: resolve produced {} error(s)",
            resolved.errors.len()
        );
        return Ok(());
    }
    let typecheck_result = typecheck_workspace(&resolved);
    if !typecheck_result.errors.is_empty() {
        eprintln!(
            "warning: stdlib BEAM bundling: typecheck produced {} error(s)",
            typecheck_result.errors.len()
        );
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
        eprintln!("warning: stdlib BEAM bundling: erlc not found on PATH; install Erlang/OTP");
        return Ok(());
    };

    // Compile each stdlib module with its FQN as the BEAM atom.
    // Skip `.test.ridge` modules (FQN contains ".test") — test files are not
    // distributable stdlib modules.
    let compiler_mtime = compiler_mtime();
    let mut emitted: Vec<String> = Vec::new();
    for slot in &lowered.modules {
        let Some(m) = slot else { continue };
        let fqn = match fqn_map.get(&m.id) {
            Some(n) => n.clone(),
            None => continue,
        };
        // Skip test files (`std.list.test`, `std.option.test`, …), whose FQN
        // carries a trailing `.test` from the `.test.ridge` source. The
        // `std.test` module is a real distributable module that merely happens
        // to be named "test"; its FQN contains `.test` too, so guard against it.
        if fqn.contains(".test") && fqn != "std.test" {
            continue;
        }
        emitted.push(fqn.clone());
        // Skip if this module's .beam is already there and not older than the
        // compiler that would write it.
        let beam_path = beam_dir.join(format!("{fqn}.beam"));
        if beam_is_current(&beam_path, compiler_mtime) {
            continue;
        }
        // Compile the module with its FQN as the BEAM atom.
        codegen_stdlib_module_with_fqn(m, &lowered, &fqn, out_root, Some(&erlc_info), profile)?;
        // Move the produced .beam from out_root/beam/ to beam_dir (they should be the same).
        // (No move needed — out_root/beam IS beam_dir per compile_workspace convention.)
    }

    // Record what the set is, so the next build can tell a complete one from a
    // partial one without recompiling to find out.
    write_stdlib_manifest(out_root, &emitted);

    Ok(())
}

/// Name of the file recording which stdlib modules were emitted, and by which
/// compiler.
const STDLIB_MANIFEST: &str = ".stdlib-manifest";

/// Where the manifest lives, next to `beam/` under the build root.
fn stdlib_manifest_path(out_root: &std::path::Path) -> std::path::PathBuf {
    out_root.join(STDLIB_MANIFEST)
}

/// Modification time of the running compiler, when it can be determined.
///
/// The stdlib sources are embedded in the binary rather than read from disk, so
/// there is no source file to compare a `.beam` against. The executable holding
/// those sources is the closest thing: a build that produced new embedded
/// sources also produced a newer binary. `None` means the question cannot be
/// answered, and every caller then treats the artefact as current rather than
/// recompiling the standard library on every single build.
fn compiler_mtime() -> Option<std::time::SystemTime> {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

/// Whether one emitted `.beam` is present and not older than the compiler.
fn beam_is_current(
    beam_path: &std::path::Path,
    compiler_mtime: Option<std::time::SystemTime>,
) -> bool {
    let Ok(meta) = std::fs::metadata(beam_path) else {
        return false;
    };
    let (Some(compiler), Ok(beam)) = (compiler_mtime, meta.modified()) else {
        // Either the compiler's own timestamp or the artefact's is unavailable.
        // The file exists, which is as much as can be established here.
        return true;
    };
    beam >= compiler
}

/// Whether the emitted standard library in `beam_dir` can be reused as it is.
///
/// The previous test was whether `std.list.beam` existed, which treated one file
/// as proof of the whole set. A build directory holding an older or partial
/// stdlib was then never repaired: `check` passed, `build` reported success, and
/// the program died at run time on a stdlib function that was simply not there.
///
/// Reuse now requires all three of:
///
/// - a manifest, written by the emit pass that produced the set,
/// - written by this same compiler version, and
/// - every module it names present, and no older than the compiler binary.
///
/// The version line catches a stdlib that gained or lost a module between
/// releases. The per-module timestamp catches a set that is complete but stale —
/// rebuilding the compiler over edited stdlib sources leaves every `.beam` in
/// place, and without this the edit never reaches an existing build directory.
/// Anything short of all three falls through to the emit pass, which is itself
/// per-module and so recompiles only what is actually out of date.
fn stdlib_beams_are_current(beam_dir: &std::path::Path, out_root: &std::path::Path) -> bool {
    let Ok(manifest) = std::fs::read_to_string(stdlib_manifest_path(out_root)) else {
        return false;
    };
    let mut lines = manifest.lines();
    if lines.next() != Some(env!("CARGO_PKG_VERSION")) {
        return false;
    }
    let compiler = compiler_mtime();
    let mut any = false;
    for fqn in lines.filter(|l| !l.trim().is_empty()) {
        any = true;
        if !beam_is_current(&beam_dir.join(format!("{fqn}.beam")), compiler) {
            return false;
        }
    }
    // A manifest naming nothing is not evidence of a compiled stdlib.
    any
}

/// Record the emitted module set beside `beam/`.
///
/// Best effort: a build directory that cannot hold the manifest still holds a
/// correct standard library, and the only cost of losing it is that the next
/// build recompiles. Failing the build over it would trade a real artefact for
/// a bookkeeping file.
fn write_stdlib_manifest(out_root: &std::path::Path, emitted: &[String]) {
    let mut body = String::from(env!("CARGO_PKG_VERSION"));
    for fqn in emitted {
        body.push('\n');
        body.push_str(fqn);
    }
    body.push('\n');
    let _ = std::fs::write(stdlib_manifest_path(out_root), body);
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

#[cfg(test)]
mod entry_select_tests {
    use super::{
        describe_stdlib_bundle_failure, select_entry_beam, CompileOptions, EntryModule, PathBuf,
    };

    fn em(project: &str, fqn: &str, beam: &str) -> EntryModule {
        EntryModule {
            project_name: project.to_owned(),
            module_fqn: fqn.to_owned(),
            beam_module: beam.to_owned(),
        }
    }

    #[test]
    fn prefers_the_entry_module_matching_the_requested_member() {
        let entries = vec![
            em("acme.cli", "acme.cli.Main", "ridge_module_1"),
            em("acme.worker", "acme.worker.Main", "ridge_module_3"),
        ];
        assert_eq!(
            select_entry_beam(&entries, "acme.worker").as_deref(),
            Some("ridge_module_3")
        );
    }

    #[test]
    fn falls_back_to_the_sole_entry_when_member_does_not_match() {
        let entries = vec![em("demo", "demo.Main", "ridge_module_1")];
        // Even a member name that does not match resolves to the only entry.
        assert_eq!(
            select_entry_beam(&entries, "something-else").as_deref(),
            Some("ridge_module_1")
        );
    }

    #[test]
    fn returns_none_when_ambiguous_and_no_member_matches() {
        let entries = vec![
            em("acme.cli", "acme.cli.Main", "ridge_module_1"),
            em("acme.worker", "acme.worker.Main", "ridge_module_3"),
        ];
        assert_eq!(select_entry_beam(&entries, "acme.other"), None);
    }

    // ── stdlib bundle failure reporting ───────────────────────────────────────

    fn erlc_rejected(module: &str) -> ridge_codegen_erl::CodegenError {
        ridge_codegen_erl::CodegenError::ErlcRejectedInput {
            core_path: PathBuf::from(format!("target/ridge/debug/core/{module}.core")),
            stderr: "
Function: mixedStepConds/2
internal error in pass beam_ssa_codegen
"
            .to_owned(),
            exit_code: 1,
        }
    }

    /// The message names the module and what the toolchain said, rather than
    /// dumping the error struct with its newlines escaped.
    #[test]
    fn bundle_failure_reads_as_prose() {
        let text = describe_stdlib_bundle_failure(&erlc_rejected("std.query"));
        assert!(text.contains("std.query"), "{text}");
        assert!(text.contains("Function: mixedStepConds/2"), "{text}");
        assert!(!text.contains("ErlcRejectedInput"), "{text}");
        // The `Debug` form renders the captured stderr with its newlines
        // escaped, which is the tell that the struct dump leaked through.
        assert!(!text.contains(r"\n"), "escaped newlines leaked: {text}");
    }

    /// A missing toolchain says so plainly instead of listing probe paths.
    #[test]
    fn missing_erlc_reads_as_prose() {
        let e = ridge_codegen_erl::CodegenError::ErlcNotFound {
            searched_paths: vec![PathBuf::from("/usr/bin")],
        };
        assert_eq!(
            describe_stdlib_bundle_failure(&e),
            "erlc was not found on PATH"
        );
    }

    /// `will_execute` is what separates `build` (warn and carry on) from `run`
    /// and `test` (stop before launching a program known to be broken).
    #[test]
    fn only_executing_builds_treat_a_bundle_failure_as_fatal() {
        let build = CompileOptions::new(PathBuf::from("."));
        assert!(!build.will_execute, "plain builds stay non-fatal");
        assert!(build.executing().will_execute);
    }

    #[test]
    fn returns_none_when_there_is_no_entry_module() {
        assert_eq!(select_entry_beam(&[], "anything"), None);
    }
}
