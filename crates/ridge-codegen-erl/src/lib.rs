//! Ridge Phase 6 codegen engine — IR to Core Erlang.
//!
//! Entry points:
//! - [`codegen_workspace`] — lower a whole [`LoweredWorkspace`] to `.core` files.
//! - [`codegen_module`] — lower a single [`LoweredModule`] (snapshot tests, LSP).

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

pub mod core_ast;
pub mod erlc;
pub mod error;
pub mod escript;
pub mod output_layout;
pub mod printer;
pub mod runtime;
pub mod stdlib_map;

pub(crate) mod actor;
pub(crate) mod anf;
pub(crate) mod expr;
pub(crate) mod handler;
pub(crate) mod init;
pub(crate) mod item;
pub(crate) mod letrec_detect;
pub(crate) mod lit;
pub(crate) mod messaging;
pub(crate) mod module;
pub(crate) mod pat;
pub(crate) mod return_;
pub(crate) mod scope;
pub(crate) mod symbol;

pub use core_ast::*;
pub use error::CodegenError;

use ridge_ir::{LoweredModule, LoweredWorkspace};
use ridge_resolve::ModuleId;
use std::path::PathBuf;

// ── Public types ──────────────────────────────────────────────────────────────

/// Top-level codegen result over a whole workspace.
///
/// `modules[i]` is `Some(CodegenModuleResult)` if the matching
/// `LoweredWorkspace.modules[i]` was `Some(LoweredModule)` AND codegen
/// succeeded; `None` otherwise.  `CodegenResult` is always returned (Phase 6
/// emits no fatal pre-aggregated errors); per-module errors are surfaced via
/// `errors`.
#[derive(Debug)]
#[non_exhaustive]
pub struct CodegenResult {
    /// One entry per workspace module, indexed by `ModuleId.0`.
    pub modules: Vec<Option<CodegenModuleResult>>,
    /// Aggregated `E###` diagnostics from across all modules (codegen + erlc + layout).
    pub errors: Vec<CodegenError>,
    /// The output-directory root that `output_layout` resolved to
    /// (e.g. `target/ridge/debug/`).  See §3.3.
    pub out_root: PathBuf,
}

/// Per-module codegen result.
#[derive(Debug)]
#[non_exhaustive]
pub struct CodegenModuleResult {
    /// The originating module's stable index.
    pub module: ModuleId,
    /// Mangled BEAM module name (e.g. `ridge_examples_log_analyzer`).
    pub beam_module_name: String,
    /// Path to the emitted `.core` file (always written if codegen succeeded).
    pub core_path: PathBuf,
    /// Path to the produced `.beam` file (if `erlc` was invoked and succeeded).
    pub beam_path: Option<PathBuf>,
    /// Captured `erlc` stderr if non-empty (warnings, info).
    pub erlc_stderr: Option<String>,
}

/// Codegen options (default-derived for typical builds).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CodegenOptions {
    /// Output root (default `target/ridge/<profile>/`); subdirs `core/` and `beam/`.
    pub out_root: PathBuf,
    /// Build profile (debug | release).  Affects `out_root` default but not the
    /// emitted Core Erlang.
    pub profile: BuildProfile,
    /// Whether to invoke `erlc` (default `true`).  Set to `false` for snapshot tests.
    pub invoke_erlc: bool,
    /// Optional override for the `erlc` executable path.
    pub erlc_path: Option<PathBuf>,
    /// Whether to copy the bundled `ridge_rt.erl` into the out-dir (default `true`
    /// on first call per process; idempotent).
    pub install_runtime: bool,
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            out_root: PathBuf::from("target/ridge/debug"),
            profile: BuildProfile::Debug,
            invoke_erlc: true,
            erlc_path: None,
            install_runtime: true,
        }
    }
}

/// Build-profile selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuildProfile {
    /// `target/ridge/debug/` — verbose `.core` annotations, no stripping.
    Debug,
    /// `target/ridge/release/` — same `.core` (BEAM does the optimisation), but
    /// `erlc +bin_opt_info` enabled, `+debug_info` stripped.
    Release,
}

// ── Entry points ──────────────────────────────────────────────────────────────

/// Lower a workspace's IR to `.core` files and (optionally) `.beam`.
///
/// ## Flow
///
/// 1. Ensure output subdirs (`core/`, `beam/`, `runtime/`) exist; bail early on
///    failure.
/// 2. Optionally install the bundled `ridge_rt.erl` runtime; errors are pushed
///    but processing continues (`.core` files are still useful).
/// 3. Optionally probe `erlc`; on failure push E003/E101 and disable invocation
///    for this run (`.core` writes still happen).
/// 4. For each `Some` module slot: lower via `module::lower_module_all`, print,
///    write `.core`, and (if `invoke_erlc`) compile to `.beam`.
/// 5. Aggregate per-module errors into `CodegenResult.errors`.
///
/// Always returns [`CodegenResult`] — Phase 6 emits no fatal pre-aggregated errors.
/// Per-module errors land in [`CodegenResult::errors`].
///
/// `opts` is taken by value so later tasks can move out of its fields (per plan §2.2).
// `clippy::needless_pass_by_value` fires because `opts` is currently consumed only
// via field access; the by-value signature is intentional per plan §2.2.
#[allow(clippy::needless_pass_by_value)]
#[must_use]
pub fn codegen_workspace(lowered: &LoweredWorkspace, opts: CodegenOptions) -> CodegenResult {
    let out_root = opts.out_root.clone();
    let mut modules: Vec<Option<CodegenModuleResult>> =
        (0..lowered.modules.len()).map(|_| None).collect();
    let mut errors: Vec<CodegenError> = Vec::new();

    // ── 1. Ensure output subdirs ──────────────────────────────────────────────
    if let Err(e) = output_layout::ensure_out_dirs(&out_root) {
        errors.push(e);
        return CodegenResult {
            modules,
            errors,
            out_root,
        };
    }

    // ── 2. Install runtime (optional; failure non-fatal) ─────────────────────
    if opts.install_runtime {
        if let Err(e) = runtime::install_runtime(&out_root) {
            errors.push(e);
        }
    }

    // ── 3. Probe erlc (optional; failure disables invocation) ────────────────
    let mut effective_invoke_erlc = opts.invoke_erlc;
    let erlc_info = if opts.invoke_erlc {
        match erlc::probe(opts.erlc_path.as_deref()) {
            Ok(info) => Some(info),
            Err(e) => {
                errors.push(e);
                effective_invoke_erlc = false;
                None
            }
        }
    } else {
        None
    };

    // ── 3b. Compile ridge_rt.erl → ridge_rt.beam (if runtime installed + erlc available) ──
    // ridge_rt.beam must be in the beam/ dir so `erl -pa <beam_dir>` finds it.
    if opts.install_runtime {
        if let Some(info) = erlc_info.as_ref() {
            if let Err(e) = runtime::compile_runtime(&info.path, &out_root) {
                errors.push(e);
            }
        }
    }

    // ── 4. Per-module lowering + .core write + optional erlc ─────────────────
    for (idx, slot) in lowered.modules.iter().enumerate() {
        let Some(m) = slot else { continue };
        codegen_one_module(
            m,
            lowered,
            &out_root,
            effective_invoke_erlc,
            erlc_info.as_ref(),
            opts.profile,
            &mut modules[idx],
            &mut errors,
        );
    }

    CodegenResult {
        modules,
        errors,
        out_root,
    }
}

/// Lower one module, write its `.core` file, and (optionally) compile it to `.beam`.
///
/// Errors are pushed into `errors`; on success `module_slot` is set to `Some`.
/// This is a private helper extracted from [`codegen_workspace`] to keep line counts
/// within the clippy limit.
#[allow(clippy::too_many_arguments)]
fn codegen_one_module(
    m: &ridge_ir::LoweredModule,
    ws: &LoweredWorkspace,
    out_root: &std::path::Path,
    invoke_erlc: bool,
    erlc_info: Option<&erlc::ErlcInfo>,
    profile: BuildProfile,
    module_slot: &mut Option<CodegenModuleResult>,
    errors: &mut Vec<CodegenError>,
) {
    // Derive placeholder beam-name segment from module id.
    // A future task will replace this with the FQN from workspace metadata.
    let id_segment = format!("module_{}", m.id.0);
    let path_ref: &str = &id_segment;

    let (main, actors) = match module::lower_module_all(m, ws, &[path_ref]) {
        Ok(pair) => pair,
        Err(e) => {
            errors.push(e);
            return;
        }
    };

    // Write main .core file.
    let core_path = output_layout::core_file_path(out_root, &main.name.0);
    let core_text = printer::print_module(&main);
    if let Err(e) = std::fs::write(&core_path, core_text.as_bytes()).map_err(|io_err| {
        CodegenError::OutputDirNotWritable {
            path: core_path.clone(),
            io_err: io_err.to_string(),
        }
    }) {
        errors.push(e);
        return;
    }

    // Write actor .core files (errors pushed; actor beam_paths not surfaced).
    for actor in &actors {
        let actor_core_path = output_layout::core_file_path(out_root, &actor.name.0);
        let actor_text = printer::print_module(actor);
        if let Err(e) = std::fs::write(&actor_core_path, actor_text.as_bytes()).map_err(|io_err| {
            CodegenError::OutputDirNotWritable {
                path: actor_core_path.clone(),
                io_err: io_err.to_string(),
            }
        }) {
            errors.push(e);
        }
    }

    // Optionally invoke erlc on main module and actor modules.
    let (beam_path, erlc_stderr) = compile_module_if_requested(
        invoke_erlc,
        erlc_info,
        &core_path,
        &actors,
        out_root,
        profile,
        errors,
    );

    *module_slot = Some(CodegenModuleResult {
        module: m.id,
        beam_module_name: main.name.0,
        core_path,
        beam_path,
        erlc_stderr,
    });
}

/// Invoke `erlc` on the main `.core` and all actor `.core` files if requested.
///
/// Returns `(beam_path, erlc_stderr)` for the main module.
/// Actor `beam_paths` are not surfaced in the schema.
fn compile_module_if_requested(
    invoke_erlc: bool,
    erlc_info: Option<&erlc::ErlcInfo>,
    core_path: &std::path::Path,
    actors: &[CErlModule],
    out_root: &std::path::Path,
    profile: BuildProfile,
    errors: &mut Vec<CodegenError>,
) -> (Option<PathBuf>, Option<String>) {
    if !invoke_erlc {
        return (None, None);
    }
    let Some(info) = erlc_info else {
        return (None, None);
    };

    let beam_out = output_layout::beam_dir(out_root);
    let rt_dir = output_layout::runtime_dir(out_root);

    let mut beam_path: Option<PathBuf> = None;
    let mut erlc_stderr: Option<String> = None;

    match erlc::compile_core(&info.path, core_path, &beam_out, &rt_dir, profile) {
        Ok(artifact) => {
            beam_path = Some(artifact.beam_path);
            if !artifact.stderr.is_empty() {
                erlc_stderr = Some(artifact.stderr);
            }
        }
        Err(e) => {
            errors.push(e);
        }
    }

    // Compile actor sub-modules (errors pushed; beam_paths not surfaced in schema).
    for actor in actors {
        let actor_core = output_layout::core_file_path(out_root, &actor.name.0);
        if let Err(e) = erlc::compile_core(&info.path, &actor_core, &beam_out, &rt_dir, profile) {
            errors.push(e);
        }
    }

    (beam_path, erlc_stderr)
}

/// Lower a single stdlib module to a `.core` file and optionally a `.beam` file,
/// using the Ridge fully-qualified name (e.g. `"std.list"`) as the BEAM module atom.
///
/// This is the Phase 8 path for compiling stdlib `.ridge` sources into
/// distributable `.beam` artefacts whose module atom matches the name expected
/// by `BridgeTarget::RidgeStdlibLocal` callers (e.g. `call 'std.list':head(1)`).
///
/// Unlike [`codegen_workspace`] (which mangles module names to `ridge_*`), this
/// function preserves the dotted FQN verbatim as the BEAM atom.  The resulting
/// `<fqn>.core` / `<fqn>.beam` files are placed in `<out_root>/core/` and
/// `<out_root>/beam/` respectively.
///
/// # Errors
///
/// Returns [`CodegenError`] if lowering, file I/O, or `erlc` fails.
pub fn codegen_stdlib_module_with_fqn(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    fqn: &str,
    out_root: &std::path::Path,
    erlc_info: Option<&erlc::ErlcInfo>,
    profile: BuildProfile,
) -> Result<CodegenModuleResult, CodegenError> {
    // Lower with the FQN as the BEAM module name (no mangling).
    let cerl = module::lower_module_with_name(m, ws, fqn)?;

    // Write the .core file.
    let core_path = output_layout::core_file_path(out_root, fqn);
    let core_text = printer::print_module(&cerl);
    std::fs::write(&core_path, core_text.as_bytes()).map_err(|io_err| {
        CodegenError::OutputDirNotWritable {
            path: core_path.clone(),
            io_err: io_err.to_string(),
        }
    })?;

    // Optionally compile to .beam.
    let (beam_path, erlc_stderr) = if let Some(info) = erlc_info {
        let beam_out_dir = out_root.join("beam");
        let runtime_dir = out_root.join("runtime");
        match erlc::compile_core(&info.path, &core_path, &beam_out_dir, &runtime_dir, profile) {
            Ok(artifact) => (Some(artifact.beam_path), Some(artifact.stderr)),
            Err(e) => return Err(e),
        }
    } else {
        (None, None)
    };

    Ok(CodegenModuleResult {
        module: m.id,
        beam_module_name: fqn.to_owned(),
        core_path,
        beam_path,
        erlc_stderr,
    })
}

/// Lower a single module's IR to a [`CErlModule`] (no actors split, no disk I/O).
///
/// Snapshot-test entry point — returns the typed Core Erlang AST directly.
pub fn codegen_module_ast(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
) -> Result<CErlModule, CodegenError> {
    let id_segment = format!("module_{}", m.id.0);
    let path_ref: &str = &id_segment;
    module::lower_module(m, ws, &[path_ref])
}

/// Lower a single module's IR to a [`CodegenModuleResult`] (no disk I/O, no `erlc`).
///
/// This is the snapshot-test and LSP hot-path entry point.  It lowers the module
/// through `module::lower_module` and returns the result with empty `core_path`
/// and `beam_path`.  Errors during lowering produce a degenerate result with an
/// empty `beam_module_name`.
///
/// `beam_module_name` is a stable placeholder derived from the module id
/// (`"ridge_module_<n>"`).  A future task will replace it with the FQN from
/// workspace metadata.
#[must_use]
pub fn codegen_module(
    m: &LoweredModule,
    ws: &LoweredWorkspace,
    _opts: &CodegenOptions,
) -> CodegenModuleResult {
    // Derive a stable path from the module id as a placeholder.
    let id_segment = format!("module_{}", m.id.0);
    let path_ref: &str = &id_segment;

    match module::lower_module(m, ws, &[path_ref]) {
        Ok(cerl_module) => CodegenModuleResult {
            module: m.id,
            beam_module_name: cerl_module.name.0,
            // No disk I/O: core_path and beam_path are intentionally empty.
            core_path: PathBuf::new(),
            beam_path: None,
            erlc_stderr: None,
        },
        Err(_e) => {
            // Lowering errors are swallowed here; the caller (snapshot tests,
            // LSP) sees a degenerate result.  Workspace-level error aggregation
            // is handled by codegen_workspace.
            CodegenModuleResult {
                module: m.id,
                beam_module_name: String::new(),
                core_path: PathBuf::new(),
                beam_path: None,
                erlc_stderr: None,
            }
        }
    }
}
