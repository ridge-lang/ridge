//! Structured error types for the `ridge-driver` crate.
//!
//! Cross-crate `C0NN` namespace map (§1.3 #3 — verify before allocating new codes):
//! - `C001`–`C004` — this crate (driver compile / run errors).
//! - `C005`–`C008`, `C006a` — `ridge-cli` (see `crates/ridge-cli/src/error.rs`).
//! - `C009` — this crate (`PkgResolutionFailed`).
//!
//! Resolve / typecheck / codegen errors are threaded through as diagnostics
//! in [`crate::CompileArtefacts::diagnostics`]; only fatal *driver* errors
//! use this module.

use std::path::PathBuf;
use thiserror::Error;

use ridge_diagnostics::Diagnostic;

use crate::sources::WorkspaceSourceCache;

// ── CompileError ──────────────────────────────────────────────────────────────

/// Fatal error from [`crate::compile_workspace`].
///
/// These errors prevent the driver from producing *any* output.  Non-fatal
/// compile errors (type errors, name-resolution errors) are returned as
/// [`crate::CompileArtefacts::diagnostics`] on a best-effort basis.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CompileError {
    /// `C001` — no `ridge.toml` with a `[workspace]` table was found at or
    /// above `workspace_root`.
    #[error("C001 NoWorkspaceRoot: no workspace manifest found at or above {path}")]
    NoWorkspaceRoot {
        /// The path that was searched from.
        path: PathBuf,
    },

    /// `C002` — a member listed in `[workspace] members` has no on-disk
    /// directory or no `ridge.toml`.
    #[error("C002 WorkspaceMemberMissing: workspace member '{member}' not found at {path}")]
    WorkspaceMemberMissing {
        /// The member name as it appears in the workspace manifest.
        member: String,
        /// The expected on-disk path.
        path: PathBuf,
    },

    /// `C003` — cyclic workspace dependency detected.
    ///
    /// Already detected by `ridge-resolve`; the driver surfaces it here.
    #[error(
        "C003 WorkspaceCycle: cyclic dependency detected among workspace members: {members:?}"
    )]
    WorkspaceCycle {
        /// Members involved in the cycle.
        members: Vec<String>,
    },

    /// `C004` — the Erlang/OTP toolchain (`erl` / `erlc`) is not on `PATH`.
    ///
    /// Probed once at driver startup and cached for the lifetime of the call.
    #[error("C004 ErlangNotFound: erlang toolchain not found on PATH (install OTP 26+)")]
    ErlangNotFound,

    /// Internal I/O error writing output files.
    #[error("I/O error: {message}")]
    Io {
        /// Human-readable description.
        message: String,
    },

    /// `C009` — package dependency resolution failed.
    ///
    /// Wraps a [`ridge_pkg::PkgError`] (`P0NN` / `P1NN` namespace).  The
    /// driver cannot proceed without all resolved dep paths, so this is fatal.
    /// `#[from]` enables `?` on `resolve_dependencies` calls in
    /// [`crate::compile_workspace`] (T8).
    ///
    /// The user-visible string surfaces the wrapped `P0NN` code — that is the
    /// actionable identifier — so the `C009` label appears only in this rustdoc
    /// and in the cross-crate namespace map at the top of the module.
    #[error("package resolution failed: {source}")]
    PkgResolutionFailed {
        /// Underlying `ridge-pkg` error.
        #[from]
        source: ridge_pkg::PkgError,
    },
}

// ── CheckError ────────────────────────────────────────────────────────────────

/// Fatal error from [`crate::check_workspace`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CheckError {
    /// `C001` — no workspace manifest found.
    #[error("C001 NoWorkspaceRoot: no workspace manifest found at or above {path}")]
    NoWorkspaceRoot {
        /// The path that was searched from.
        path: PathBuf,
    },

    /// `C002` — a declared member is missing from disk.
    #[error("C002 WorkspaceMemberMissing: workspace member '{member}' not found at {path}")]
    WorkspaceMemberMissing {
        /// The member name.
        member: String,
        /// The expected path.
        path: PathBuf,
    },

    /// `C003` — cyclic dependency detected.
    #[error(
        "C003 WorkspaceCycle: cyclic dependency detected among workspace members: {members:?}"
    )]
    WorkspaceCycle {
        /// Members involved in the cycle.
        members: Vec<String>,
    },
}

// ── CompileDiagnostics payload ────────────────────────────────────────────────

/// Payload for [`RunError::CompileDiagnostics`].
///
/// Carries the diagnostics emitted by the compile pipeline and the source
/// cache needed to render them.  Held behind a `Box` in `RunError` so the
/// enum's `Result` callsites do not trip `clippy::result_large_err`.
#[derive(Debug)]
pub struct CompileDiagnostics {
    /// Diagnostics emitted by the compile pipeline (errors and warnings).
    pub diagnostics: Vec<Diagnostic>,
    /// Source cache for rendering [`Self::diagnostics`].
    pub sources: WorkspaceSourceCache,
}

// ── RunError ──────────────────────────────────────────────────────────────────

/// Fatal error from [`crate::run_workspace`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunError {
    /// Compile phase failed — see inner [`CompileError`].
    #[error("compile failed: {0}")]
    CompileFailed(#[from] CompileError),

    /// Compile produced error-severity diagnostics; run aborts before BEAM
    /// launch.  Distinct from [`Self::CompileFailed`]: that variant carries a
    /// fatal driver-level error (no workspace root, package resolution
    /// failure); this one carries the resolve / typecheck / codegen errors
    /// that the compile pipeline accumulates on a best-effort basis (e.g.
    /// `R016` capability not declared in the manifest, `T001` type error).
    /// Without this gate `ridge run` would either re-execute a stale `.beam`
    /// from a previous successful compile or run partially-emitted output
    /// that bypasses the capability contract declared in `ridge.toml`.
    ///
    /// Payload is boxed because [`WorkspaceSourceCache`] is large enough to
    /// trigger `clippy::result_large_err` on every `Result<_, RunError>`.
    #[error("compile produced {} error-severity diagnostic(s)", .0.diagnostics.len())]
    CompileDiagnostics(Box<CompileDiagnostics>),

    /// `C004` — Erlang runtime (`erl`) not on PATH.
    #[error("C004 ErlangNotFound: erlang runtime not found on PATH (install OTP 26+)")]
    ErlangNotFound,

    /// The BEAM process exited with a non-zero code.
    #[error("erl exited with code {code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}")]
    ErlExitNonZero {
        /// Process exit code.
        code: i32,
        /// Captured standard output.
        stdout: String,
        /// Captured standard error.
        stderr: String,
    },

    /// The `erl` process could not be spawned.
    #[error("failed to spawn erl process: {message}")]
    SpawnFailed {
        /// Error message from the OS.
        message: String,
    },

    /// No BEAM module was produced (codegen produced no output).
    #[error("no BEAM module produced — codegen produced no output")]
    NoBeamModule,
}

/// Process exit code returned from a successful `run_workspace` call.
///
/// Zero indicates success; non-zero indicates the BEAM node exited non-zero
/// (which is treated as [`RunError::ErlExitNonZero`] — this value is only
/// returned on exit code 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessExitCode(pub i32);
