//! Structured error types for the `ridge-driver` crate.
//!
//! Cross-crate `C0NN` namespace map (verify before allocating new codes):
//! - `C001`–`C004`, `C010`, `C012`–`C014` — this crate.
//! - `C005`–`C008`, `C011` — `ridge-cli` (see `crates/ridge-cli/src/error.rs`).
//! - `C009` — retired; see [`CompileError::PkgResolutionFailed`].
//!
//! Every code in this module is declared exactly once. Where two of the three
//! entry points below fail the same way, they share the type that owns the
//! failure rather than each spelling out its own copy: `C004` had two different
//! message texts for exactly as long as it had two declarations.
//!
//! Resolve / typecheck / codegen errors are threaded through as diagnostics
//! in [`crate::CompileArtefacts::diagnostics`]; only fatal *driver* errors
//! use this module.

use std::path::PathBuf;
use thiserror::Error;

use ridge_diagnostics::Diagnostic;

use crate::sources::WorkspaceSourceCache;

// ── WorkspaceError ────────────────────────────────────────────────────────────

/// The workspace itself could not be read.
///
/// Compiling and checking both start by finding the workspace and walking its
/// members, and they fail identically when that does not work out. Sharing the
/// type means a caller handles "the workspace is unusable" once, and there is
/// one place to edit when the wording changes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WorkspaceError {
    /// `C001` — no `ridge.toml` with a `[workspace]` table was found at or
    /// above the search root.
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
}

impl WorkspaceError {
    /// The stable code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoWorkspaceRoot { .. } => "C001",
            Self::WorkspaceMemberMissing { .. } => "C002",
        }
    }
}

// ── ToolchainError ────────────────────────────────────────────────────────────

/// The Erlang toolchain is missing, or would not start.
///
/// Its own type because more than one crate reaches for OTP: the driver before
/// it runs a program, and the `ridge-cli` commands that launch a node without
/// going through the driver (`repl`, `reload`, `run --watch`, `run
/// --observer`). They hit the same two failures, so they share the type rather
/// than each writing the code into a string of its own — which is how `C004`
/// came to have five different message texts across two crates.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ToolchainError {
    /// `C004` — an OTP binary (`erl`, `erlc`) is not on `PATH`.
    ///
    /// Naming the binary matters: a runtime-only OTP package gives you `erl`
    /// without `erlc`, and "erlang not found" sends the reader off to reinstall
    /// something they already have.
    #[error("C004 ErlangNotFound: {binary} not found on PATH (install OTP 26+)")]
    ErlangNotFound {
        /// The binary that was probed for, e.g. `"erl"` or `"erlc"`.
        binary: &'static str,
    },

    /// `C013` — the BEAM process could not be spawned.
    ///
    /// OTP is on `PATH` — that case is `C004` — but the process would not
    /// start: no permission to execute, a broken install, or the OS refusing
    /// a new process.
    #[error("C013 BeamSpawnFailed: could not spawn {binary}: {message}")]
    SpawnFailed {
        /// The binary that failed to start.
        binary: &'static str,
        /// What the OS reported.
        message: String,
    },
}

impl ToolchainError {
    /// The stable code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ErlangNotFound { .. } => "C004",
            Self::SpawnFailed { .. } => "C013",
        }
    }

    /// `erl` was not found on `PATH`.
    #[must_use]
    pub const fn erl_not_found() -> Self {
        Self::ErlangNotFound { binary: "erl" }
    }

    /// `erlc` was not found on `PATH`.
    #[must_use]
    pub const fn erlc_not_found() -> Self {
        Self::ErlangNotFound { binary: "erlc" }
    }

    /// `erl` was found but the process would not start.
    #[must_use]
    pub fn erl_spawn_failed(source: &std::io::Error) -> Self {
        Self::SpawnFailed {
            binary: "erl",
            message: source.to_string(),
        }
    }
}

// ── CompileError ──────────────────────────────────────────────────────────────

/// Fatal error from [`crate::compile_workspace`].
///
/// These errors prevent the driver from producing *any* output.  Non-fatal
/// compile errors (type errors, name-resolution errors) are returned as
/// [`crate::CompileArtefacts::diagnostics`] on a best-effort basis.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CompileError {
    /// The workspace could not be read — `C001`, `C002` or `C003`.
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    /// `C010` — the Ridge standard library could not be compiled to BEAM.
    ///
    /// Only fatal when the caller is about to execute what it built
    /// ([`crate::CompileOptions::will_execute`]). A `ridge build` still
    /// succeeds with a warning, because the artefacts are for a later step that
    /// can act on the problem; `ridge run` and `ridge test` *are* that step, and
    /// launching a program whose stdlib is missing only trades a diagnostic for
    /// an Erlang `undef` crash report.
    #[error(
        "C010 StdlibBundleFailed: the Ridge standard library could not be compiled to BEAM ({message})\n  \
         a program calling any Ridge-bodied stdlib function would fail at startup with `undef`"
    )]
    StdlibBundleFailed {
        /// What the stdlib compile reported.
        message: String,
    },

    /// `C012` — an output file could not be written.
    ///
    /// A full disk and a read-only output directory both land here, and both
    /// are things the reader fixes outside Ridge — so the code exists mostly so
    /// they can tell this apart from a compile that failed on their source.
    #[error("C012 DriverIo: {message}")]
    Io {
        /// Human-readable description.
        message: String,
    },

    /// Package dependency resolution failed.
    ///
    /// Wraps a [`ridge_pkg::PkgError`] (`P0NN` / `P1NN` namespace).  The
    /// driver cannot proceed without all resolved dep paths, so this is fatal.
    /// `#[from]` enables `?` on `resolve_dependencies` calls in
    /// [`crate::compile_workspace`].
    ///
    /// This variant once carried `C009`, in rustdoc only — the user-visible
    /// string has always surfaced the wrapped `P0NN` code, which is the
    /// actionable one. A code nobody can be shown is not a code, so `C009` is
    /// retired rather than kept as an alias for whatever `ridge-pkg` reports.
    #[error("package resolution failed: {source}")]
    PkgResolutionFailed {
        /// Underlying `ridge-pkg` error.
        #[from]
        source: ridge_pkg::PkgError,
    },
}

impl CompileError {
    /// The stable code for this failure, when it has one.
    ///
    /// [`Self::PkgResolutionFailed`] answers `None`: it forwards a
    /// `ridge-pkg` error that carries its own `P0NN` code, and inventing a
    /// second code for the same failure is how a reader ends up with two
    /// things to search for and one of them useless.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        Some(match self {
            Self::Workspace(e) => e.code(),
            Self::StdlibBundleFailed { .. } => "C010",
            Self::Io { .. } => "C012",
            Self::PkgResolutionFailed { .. } => return None,
        })
    }

    /// `C001` — no workspace manifest was found at or above `path`.
    #[must_use]
    pub const fn no_workspace_root(path: PathBuf) -> Self {
        Self::Workspace(WorkspaceError::NoWorkspaceRoot { path })
    }
}

// ── CheckError ────────────────────────────────────────────────────────────────

/// Fatal error from [`crate::check_workspace`].
///
/// Checking reads the workspace and stops; everything else it finds is a
/// diagnostic, not a fatal error. That is why this wraps [`WorkspaceError`]
/// and adds nothing.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CheckError {
    /// The workspace could not be read — `C001`, `C002` or `C003`.
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}

impl CheckError {
    /// The stable code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Workspace(e) => e.code(),
        }
    }

    /// `C001` — no workspace manifest was found at or above `path`.
    #[must_use]
    pub const fn no_workspace_root(path: PathBuf) -> Self {
        Self::Workspace(WorkspaceError::NoWorkspaceRoot { path })
    }
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

    /// The runtime could not be started — `C004` or `C013`.
    #[error(transparent)]
    Toolchain(#[from] ToolchainError),

    /// The program exited with a non-zero code.
    ///
    /// No code, deliberately. This is the program's own exit status, not
    /// something Ridge diagnosed: giving it a `C0NN` would put a compiler code
    /// on the user's `exit 1`.
    ///
    /// It carries no captured output for the same reason. Whatever the program
    /// wrote is already on the terminal, in the program's own voice, and
    /// reprinting it inside a banner signed by Ridge is how a well-typed
    /// program that simply returned `Err` came to read as a broken toolchain.
    #[error("the program exited with code {code}")]
    ProgramExitNonZero {
        /// Process exit code.
        code: i32,
    },

    /// `C016` - the program was still running when the run timeout elapsed.
    ///
    /// Distinct from [`Self::ProgramExitNonZero`] on purpose: there the
    /// program chose its exit status, here it never got to. One is the program
    /// reporting, the other is Ridge giving up on it, and a reader deciding
    /// what to do next needs to know which of the two happened.
    #[error("C016 RunTimedOut: the program did not finish within {seconds} seconds")]
    RunTimedOut {
        /// The run timeout that elapsed, in seconds.
        seconds: u64,
    },

    /// `C014` — codegen produced no BEAM module to run.
    #[error("C014 NoBeamModule: no BEAM module produced — codegen produced no output")]
    NoBeamModule,

    /// `C015` — the runtime started, but the OS stopped reporting on it.
    ///
    /// Separate from `C013`: there the process never started, here it did and
    /// then could not be waited on. Whatever the program did before that is
    /// already on the terminal, which is the difference that matters to the
    /// reader.
    #[error("C015 BeamWaitFailed: {message}")]
    WaitFailed {
        /// What the OS reported.
        message: String,
    },
}

impl RunError {
    /// The stable code for this failure, when it has one.
    ///
    /// Three variants answer `None`. [`Self::CompileDiagnostics`] holds
    /// diagnostics that each carry their own code, and
    /// [`Self::ProgramExitNonZero`] reports the program's exit status rather
    /// than a Ridge failure.
    /// [`Self::CompileFailed`] forwards whatever the compile phase answered,
    /// including its `None`.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        Some(match self {
            Self::CompileFailed(e) => return e.code(),
            Self::Toolchain(e) => e.code(),
            Self::NoBeamModule => "C014",
            Self::WaitFailed { .. } => "C015",
            Self::RunTimedOut { .. } => "C016",
            Self::CompileDiagnostics(_) | Self::ProgramExitNonZero { .. } => return None,
        })
    }
}

/// Process exit code returned from a successful `run_workspace` call.
///
/// Zero indicates success; a non-zero exit is reported as
/// [`RunError::ProgramExitNonZero`] instead, so this value is only ever
/// returned on exit code 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessExitCode(pub i32);
