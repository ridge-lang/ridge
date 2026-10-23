//! CLI-level error codes not covered by `ridge-driver`.
//!
//! These errors are raised by `ridge-cli` before or after handing off to the
//! driver, when the CLI detects a structural problem in the workspace or the
//! user's invocation.
//!
//! The CLI owns `C005`–`C008`, `C011`, `C102`–`C105`, `C201`–`C205`,
//! `C301`–`C306`, `C401`, `C403`–`C409`, `C501`–`C505` and `C601`. It does not own
//! `C001` or the runtime-launch codes: those are the driver's, and
//! [`CliError`] forwards them rather than restating them, because a code
//! restated in a second crate is a code with a second wording.
//!
//! `C402` is retired. It said "erl and erlc must be on PATH to run `ridge
//! migrate add`", which is `C004` with a different number and without naming
//! which of the two binaries was actually missing.

use std::fmt;
use std::path::{Path, PathBuf};

use ridge_driver::{ToolchainError, WorkspaceError};

// ── CLI error enum ────────────────────────────────────────────────────────────

/// A fatal CLI-level error.
///
/// Each variant carries the stable error code in its `Display` output.
#[derive(Debug)]
#[non_exhaustive]
pub enum CliError {
    /// The workspace could not be read — `C001`, `C002` or `C003`.
    ///
    /// Forwarded from the driver, which owns these codes. The CLI used to
    /// declare its own field-free `C001`, which said "at or above the current
    /// directory" where the driver's names the directory it searched. Two
    /// wordings for one code, and a variant so cheap to build that seventeen
    /// unrelated failures reached for it when they needed *some* error.
    Workspace(WorkspaceError),

    /// `C005` — `--member` named a member that does not exist in the workspace.
    UnknownMember {
        /// The member name supplied by the user.
        name: String,
    },

    /// `C006` — no `app` or `service` member found in the workspace (for `ridge run`).
    NoExecutableMember,

    /// A member's manifest could not be read, so the workspace looked as though
    /// it had no runnable member at all.
    ///
    /// Reported in place of `C006`, which would blame the `kind` key on a
    /// manifest whose real problem is somewhere else entirely. The manifest
    /// error carries its own `M###` code and message.
    MemberManifestInvalid {
        /// The manifest error, already rendered with its code.
        rendered: String,
    },

    /// The workspace manifest could not be parsed, so the walk that looks for
    /// the workspace root had nothing to read.
    ///
    /// Reported in place of `C001`, which says no manifest was found and sends
    /// the reader after a file that is sitting in front of them. The sibling of
    /// [`Self::MemberManifestInvalid`] one level up: same shape, same reason,
    /// and the manifest error carries its own `M###` code.
    ///
    /// Raised here rather than left to the compile pipeline because not every
    /// subcommand runs one. `ridge fmt` walks files and never parses the
    /// manifest, so leaving this to the pipeline turned a wrong message into no
    /// message at all.
    WorkspaceManifestInvalid {
        /// The manifest error, already rendered with its code.
        rendered: String,
    },

    /// `C011` — `--watch` requested but multiple executable members exist and
    /// `--member` was not specified.
    WatchAmbiguousMember,

    /// `C007` — `--member` names a `library` member, which is not executable.
    LibraryNotExecutable {
        /// The member name supplied by the user.
        name: String,
    },

    /// `C008` — `--observer` requires the Erlang cookie but
    /// `~/.erlang.cookie` (`%USERPROFILE%\.erlang.cookie` on Windows) was not
    /// found and `--cookie` was not provided.
    ObserverNoCookie,

    /// `C201` — the project name supplied to `ridge new` is not a valid
    /// portable directory name (contains `/`, `\`, starts with `.`, contains
    /// `..`, is empty, or contains characters not portable across Linux,
    /// macOS, and Windows).
    InvalidProjectName {
        /// The invalid name supplied by the user.
        name: String,
    },

    /// `C202` — `ridge new <name>` refused because `<name>/` already exists
    /// in the current directory.
    DirectoryExists {
        /// The directory name that already exists.
        name: String,
    },

    /// `C203` — the project name is reserved by the Ridge toolchain
    /// (`std`, `test`, `core`).  Match is case-insensitive.
    ReservedName {
        /// The reserved name supplied by the user.
        name: String,
    },

    /// `C204` — `ridge init` refused because the current directory is not
    /// empty (contains files other than `.git/` and `.gitignore`).
    DirectoryNotEmpty,

    /// `C205` — `ridge init` could not read the current working directory.
    CwdUnreadable,

    /// `C102` — a `<paths>` argument supplied to `ridge fmt` does not exist.
    FmtPathNotFound {
        /// The path that was not found.
        path: std::path::PathBuf,
    },

    /// `C103` — a file could not be read from or written to during `ridge fmt`.
    FmtIoError {
        /// The file or stream that caused the error.
        path: std::path::PathBuf,
        /// The underlying I/O error, rendered as a string.
        source: String,
    },

    /// `C104` — `--check` mode found files that would be reformatted.
    ///
    /// The `count` field records how many files would change (or were
    /// unparseable and therefore treated as needing change).
    FmtCheckFailed {
        /// Number of files that would be reformatted.
        count: usize,
    },

    /// `C105` — `ridge fmt` encountered a file with the legacy `.rg` extension.
    ///
    /// Sources must end in `.ridge`. Rename the file and update `ridge.toml`.
    LegacyRgFile {
        /// The path of the legacy source file.
        path: std::path::PathBuf,
    },

    /// A test signature rejected by the shared runner/editor contract.
    TestRejected {
        /// Qualified name shown in the test report.
        qualified_name: String,
        /// Stable rejection code and message.
        reason: ridge_driver::test_contract::TestRejection,
    },

    /// `C401` — `<src_root>/migrations/Model.ridge` is missing.
    MigrateModelMissing {
        /// The path where `Model.ridge` was expected.
        path: std::path::PathBuf,
    },

    /// `C403` — the model failed to compile.
    ///
    /// The compile diagnostics have already been rendered to stderr before
    /// this error is returned.
    MigrateCompileFailed,

    /// `C404` — an unexpected internal failure while generating the
    /// migration (e.g. the generated driver module could not be located
    /// after a clean compile, or the BEAM child process that runs it could
    /// not be spawned or produced no output).
    MigrateInternal {
        /// A description of what went wrong.
        message: String,
    },

    /// `C405` — the name given to `ridge migrate add` is not valid.
    MigrateInvalidName {
        /// The invalid name supplied by the user.
        name: String,
    },

    /// `C406` — `ridge migrate apply`/`ridge migrate status` needs a database
    /// to connect to, and one or more required environment variables
    /// (`RIDGE_DB_DATABASE`, `RIDGE_DB_USER`) are missing or empty.
    MigrateEnvMissing {
        /// The required variable names that are missing or empty.
        vars: Vec<String>,
    },

    /// `C407` — `ridge migrate apply` reached the database but the migration
    /// run itself failed (a bad connection, or a migration step that failed).
    MigrateApplyFailed {
        /// The error message the driver reported.
        message: String,
    },

    /// `C408` — `ridge migrate status` could not read the set of applied
    /// migrations (a bad connection, or the tracking table could not be read).
    MigrateStatusFailed {
        /// The error message the driver reported.
        message: String,
    },

    /// `C409` — `ridge migrate rollback` reached the database but the rollback
    /// run itself failed (a bad connection, or a migration with no derivable
    /// reverse and no explicit down).
    MigrateRollbackFailed {
        /// The error message the driver reported.
        message: String,
    },

    /// The Erlang runtime is missing or would not start — `C004` or `C013`.
    ///
    /// `repl`, `reload`, `run --watch` and `run --observer` launch a node
    /// themselves instead of going through `run_workspace`, so they hit the
    /// driver's failures without being the driver. Forwarding its type keeps
    /// one wording for a problem the reader meets on their first run.
    Toolchain(ToolchainError),

    /// `C501` — the file watcher could not be created.
    WatcherStartFailed {
        /// What the watcher reported.
        message: String,
    },

    /// `C502` — the workspace directory could not be watched for changes.
    WatchPathFailed {
        /// The directory that could not be watched.
        path: PathBuf,
        /// What the watcher reported.
        message: String,
    },

    /// `C503` — the REPL session could not be started.
    ReplSessionFailed {
        /// What the session reported.
        message: String,
    },

    /// `C504` — the watch loop's shared state was left unusable by a thread
    /// that panicked while holding it.
    ///
    /// Nothing the reader did causes this, which is exactly why it needs a code
    /// of its own: it is the one failure here that should be reported rather
    /// than worked around.
    WatchStateCorrupted,

    /// `C505` — a watched rebuild could not be restarted, and neither could the
    /// placeholder that would have kept the loop alive.
    WatchRestartFailed {
        /// What the OS reported.
        message: String,
    },

    /// `C601` — `ridge explain` was handed something in neither table: not a
    /// code the compiler emits, and not one it has retired.
    ExplainUnknownCode {
        /// What was asked about, already normalised: upper-cased, and with the
        /// brackets a rendered diagnostic puts around a code taken off.
        code: String,
    },

    /// A failure whose specific cause has already been printed to stderr
    /// (rendered diagnostics, a `no .beam produced` line, an escript packaging
    /// error, …). Carries only the non-zero exit; the top-level handler prints
    /// nothing further for it, so a failed build no longer tacks on a spurious
    /// `C001 NoWorkspaceRoot`.
    AlreadyReported,
}

impl CliError {
    /// The stable code for this failure, when it has one.
    ///
    /// Two variants answer `None` on purpose. `AlreadyReported` is a sentinel
    /// for "the real cause is already on stderr", and `MemberManifestInvalid`
    /// forwards a rendered manifest error that carries its own `M###`. Giving
    /// either one a `C###` would invent a code for something that is not a
    /// distinct failure.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        Some(match self {
            Self::UnknownMember { .. } => "C005",
            Self::NoExecutableMember => "C006",
            Self::LibraryNotExecutable { .. } => "C007",
            Self::ObserverNoCookie { .. } => "C008",
            Self::WatchAmbiguousMember => "C011",
            Self::FmtPathNotFound { .. } => "C102",
            Self::FmtIoError { .. } => "C103",
            Self::FmtCheckFailed { .. } => "C104",
            Self::LegacyRgFile { .. } => "C105",
            Self::InvalidProjectName { .. } => "C201",
            Self::DirectoryExists { .. } => "C202",
            Self::ReservedName { .. } => "C203",
            Self::DirectoryNotEmpty { .. } => "C204",
            Self::CwdUnreadable { .. } => "C205",
            Self::TestRejected { reason, .. } => reason.code(),
            Self::MigrateModelMissing { .. } => "C401",
            Self::MigrateCompileFailed => "C403",
            Self::MigrateInternal { .. } => "C404",
            Self::MigrateInvalidName { .. } => "C405",
            Self::MigrateEnvMissing { .. } => "C406",
            Self::MigrateApplyFailed { .. } => "C407",
            Self::MigrateStatusFailed { .. } => "C408",
            Self::MigrateRollbackFailed { .. } => "C409",
            Self::WatcherStartFailed { .. } => "C501",
            Self::WatchPathFailed { .. } => "C502",
            Self::ReplSessionFailed { .. } => "C503",
            Self::WatchStateCorrupted => "C504",
            Self::WatchRestartFailed { .. } => "C505",
            Self::ExplainUnknownCode { .. } => "C601",
            Self::Toolchain(e) => e.code(),
            Self::Workspace(e) => e.code(),
            Self::AlreadyReported
            | Self::MemberManifestInvalid { .. }
            | Self::WorkspaceManifestInvalid { .. } => return None,
        })
    }
}

impl CliError {
    /// `C001` — no workspace manifest was found at or above `searched_from`.
    ///
    /// Takes the directory it searched, so the message can name it. The
    /// variant it builds has a field for exactly that reason: an error nobody
    /// can construct without saying what failed is one nobody reaches for as a
    /// placeholder.
    #[must_use]
    pub fn no_workspace_root(searched_from: &Path) -> Self {
        Self::Workspace(WorkspaceError::NoWorkspaceRoot {
            path: searched_from.to_path_buf(),
        })
    }
}

impl From<ToolchainError> for CliError {
    fn from(e: ToolchainError) -> Self {
        Self::Toolchain(e)
    }
}

impl From<WorkspaceError> for CliError {
    fn from(e: WorkspaceError) -> Self {
        Self::Workspace(e)
    }
}

// ── CliWarning ──

/// An advisory the CLI reports without failing.
///
/// Kept apart from [`CliError`] for the reason `ridge-pkg` keeps `PkgWarning`
/// apart from `PkgError`: when severity is the type, an emit site cannot get it
/// wrong. Both of these used to be raw `eprintln!` calls, outside the
/// diagnostic system entirely — one of them shadowed by a `CliError`
/// variant that was never constructed.
#[derive(Debug)]
#[non_exhaustive]
pub enum CliWarning {
    /// `C303` — a discovered test returns `Bool` rather than `Result Unit Text`.
    BoolTestDeprecated {
        /// Fully qualified `Module.function` name of the test.
        qualified_name: String,
    },

    /// `C304` — a test was found by its `test_` prefix rather than `@test`.
    PrefixTestDeprecated {
        /// Module the test lives in.
        module: String,
        /// The function name, prefix included.
        name: String,
    },
}

impl CliWarning {
    /// The stable code for this advisory.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BoolTestDeprecated { .. } => "C303",
            Self::PrefixTestDeprecated { .. } => "C304",
        }
    }
}

impl fmt::Display for CliWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            ridge_diagnostics::terminal::terminal_text(RawCliWarning(self))
        )
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = RawCliError(self);
        if self.code().is_some() {
            write!(f, "{}", ridge_diagnostics::terminal::terminal_text(raw))
        } else {
            // Manifest diagnostics already own their layout and line endings.
            write!(f, "{raw}")
        }
    }
}

struct RawCliWarning<'a>(&'a CliWarning);

impl fmt::Display for RawCliWarning<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = self.0.code();
        match self.0 {
            CliWarning::BoolTestDeprecated { qualified_name } => write!(
                f,
                "warning: {c} BoolTestDeprecated: '{qualified_name}' returns Bool (deprecated); \
                 migrate: change the return type to Result Unit Text, and replace \
                 'true' with 'Ok ()' and 'false' with 'Err \"<reason>\"'"
            ),
            CliWarning::PrefixTestDeprecated { module, name } => write!(
                f,
                "warning: {c} PrefixTestDeprecated: '{module}.{name}' uses the deprecated `test_` \
                 prefix for discovery; add `@test \"{stem}\"` above the function; \
                 the function name can stay unchanged",
                stem = name.strip_prefix("test_").unwrap_or(name)
            ),
        }
    }
}

struct RawCliError<'a>(&'a CliError);

impl fmt::Display for RawCliError<'_> {
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per error code; splitting it up would scatter the C-code registry"
    )]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            // Forwarded verbatim: the driver owns the wording as well as the code.
            CliError::Workspace(e) => write!(f, "{e}"),
            // Never rendered through the top-level handler (main special-cases
            // it), but give it an honest message in case some other caller
            // prints it directly.
            CliError::AlreadyReported => write!(f, "build failed"),
            CliError::UnknownMember { name } => write!(
                f,
                "C005 UnknownMember: workspace has no member named '{name}'"
            ),
            CliError::NoExecutableMember => write!(
                f,
                "C006 NoExecutableMember: workspace has no member with kind = \"app\" or kind = \"service\""
            ),
            // Both carry a message the manifest parser already rendered with
            // its own code; there is nothing for this layer to add.
            CliError::MemberManifestInvalid { rendered }
            | CliError::WorkspaceManifestInvalid { rendered } => write!(f, "{rendered}"),
            CliError::WatchAmbiguousMember => write!(
                f,
                "C011 WatchAmbiguousMember: --watch requires --member when the workspace has multiple executable members"
            ),
            CliError::LibraryNotExecutable { name } => write!(
                f,
                "C007 LibraryNotExecutable: member '{name}' has kind = \"library\" and cannot be run"
            ),
            CliError::ObserverNoCookie => write!(
                f,
                "C008 ObserverNoCookie: --observer requires an Erlang cookie; \
                 ~/.erlang.cookie was not found. \
                 Provide one with --cookie <value>"
            ),
            CliError::InvalidProjectName { name } => write!(
                f,
                "C201 InvalidProjectName: '{name}' is not a valid portable project name; \
                 names must be non-empty, must not contain '/', '\\', '..', or non-portable \
                 characters, and must not start with '.'"
            ),
            CliError::DirectoryExists { name } => write!(
                f,
                "C202 DirectoryExists: directory '{name}' already exists"
            ),
            CliError::ReservedName { name } => write!(
                f,
                "C203 ReservedName: '{name}' is reserved by the Ridge toolchain"
            ),
            CliError::DirectoryNotEmpty => write!(
                f,
                "C204 DirectoryNotEmpty: the current directory is not empty; \
                 ridge init requires an empty directory \
                 (only .git/ and .gitignore are permitted)"
            ),
            CliError::CwdUnreadable => write!(
                f,
                "C205 CwdUnreadable: could not read the current working directory"
            ),
            CliError::FmtPathNotFound { path } => write!(
                f,
                "C102 FmtPathNotFound: path '{}' does not exist",
                path.display()
            ),
            CliError::FmtIoError { path, source } => write!(
                f,
                "C103 FmtIoError: I/O error on '{}': {source}",
                path.display()
            ),
            CliError::FmtCheckFailed { count } => write!(
                f,
                "C104 FmtCheckFailed: {count} file(s) would be reformatted"
            ),
            CliError::LegacyRgFile { path } => {
                let ridge_path = path.with_extension("ridge");
                write!(
                    f,
                    "C105 LegacyRgFile: '{}' uses the legacy `.rg` extension; \
                     rename it to `.ridge` (e.g. `git mv {} {}`) \
                     and update the `entry` field in `ridge.toml` if needed",
                    path.display(),
                    path.display(),
                    ridge_path.display(),
                )
            }
            CliError::TestRejected { qualified_name, reason } => {
                write!(f, "{} {}", reason.code(), reason.message(qualified_name))
            }
            CliError::MigrateModelMissing { path } => write!(
                f,
                "C401 MigrateModelMissing: '{}' was not found; \
                 create it with `pub fn model () -> List (EntitySchema Unit) = ...`",
                path.display()
            ),
            CliError::MigrateCompileFailed => write!(
                f,
                "C403 MigrateCompileFailed: the model failed to compile; \
                 see the diagnostics above"
            ),
            CliError::MigrateInternal { message } => write!(f, "C404 MigrateInternal: {message}"),
            CliError::MigrateInvalidName { name } => write!(
                f,
                "C405 MigrateInvalidName: '{name}' is not a valid migration name; \
                 use only ASCII letters, digits, '_', and '-'"
            ),
            CliError::MigrateEnvMissing { vars } => write!(
                f,
                "C406 MigrateEnvMissing: missing required environment variable(s): {}; \
                 ridge migrate apply/status needs these to connect to the database",
                vars.join(", ")
            ),
            CliError::MigrateApplyFailed { message } => {
                write!(f, "C407 MigrateApplyFailed: {message}")
            }
            CliError::MigrateStatusFailed { message } => {
                write!(f, "C408 MigrateStatusFailed: {message}")
            }
            CliError::MigrateRollbackFailed { message } => {
                write!(f, "C409 MigrateRollbackFailed: {message}")
            }
            // Forwarded verbatim: the driver owns the wording as well as the code.
            CliError::Toolchain(e) => write!(f, "{e}"),
            CliError::WatcherStartFailed { message } => write!(
                f,
                "C501 WatcherStartFailed: could not start the file watcher: {message}"
            ),
            CliError::WatchPathFailed { path, message } => write!(
                f,
                "C502 WatchPathFailed: could not watch '{}' for changes: {message}",
                path.display()
            ),
            CliError::ReplSessionFailed { message } => write!(
                f,
                "C503 ReplSessionFailed: could not start the REPL session: {message}"
            ),
            CliError::WatchStateCorrupted => write!(
                f,
                "C504 WatchStateCorrupted: the watch loop's shared state was left \
                 unusable by a panicking thread; restart the command"
            ),
            CliError::WatchRestartFailed { message } => write!(
                f,
                "C505 WatchRestartFailed: could not restart after the change, and the \
                 placeholder process would not start either: {message}"
            ),
            CliError::ExplainUnknownCode { code } => write!(
                f,
                "C601 ExplainUnknownCode: '{code}' is not a code this compiler emits, \
                 and not one it has retired; `ridge explain --list` prints every code \
                 it can emit"
            ),
        }
    }
}

impl std::error::Error for CliError {}

#[cfg(test)]
#[allow(
    clippy::panic,
    reason = "a failed drift check should name the collision"
)]
mod tests {
    use super::{CliError, ToolchainError, WorkspaceError};
    use ridge_driver::test_contract::TestRejection;

    #[test]
    fn terminal_errors_cover_every_cli_variant_and_preserve_ordinary_messages() {
        const INPUT: &str = "café\\path\u{1b}[2J\u{9b}2J\n\t\r\0";
        for mut error in one_of_each() {
            assert_eq!(error.to_string(), super::RawCliError(&error).to_string());
            match &mut error {
                CliError::UnknownMember { name }
                | CliError::LibraryNotExecutable { name }
                | CliError::InvalidProjectName { name }
                | CliError::DirectoryExists { name }
                | CliError::ReservedName { name }
                | CliError::MigrateInvalidName { name } => *name = INPUT.into(),
                CliError::FmtPathNotFound { path }
                | CliError::LegacyRgFile { path }
                | CliError::MigrateModelMissing { path } => *path = INPUT.into(),
                CliError::FmtIoError { path, source } => {
                    *path = INPUT.into();
                    *source = INPUT.into();
                }
                CliError::TestRejected { qualified_name, .. } => *qualified_name = INPUT.into(),
                CliError::MigrateInternal { message }
                | CliError::MigrateApplyFailed { message }
                | CliError::MigrateStatusFailed { message }
                | CliError::MigrateRollbackFailed { message }
                | CliError::WatcherStartFailed { message }
                | CliError::ReplSessionFailed { message }
                | CliError::WatchRestartFailed { message } => *message = INPUT.into(),
                CliError::WatchPathFailed { path, message } => {
                    *path = INPUT.into();
                    *message = INPUT.into();
                }
                CliError::MigrateEnvMissing { vars } => *vars = vec![INPUT.into()],
                CliError::ExplainUnknownCode { code } => *code = INPUT.into(),
                CliError::Workspace(inner) => {
                    *inner = WorkspaceError::NoWorkspaceRoot { path: INPUT.into() }
                }
                CliError::Toolchain(inner) => {
                    *inner = ToolchainError::SpawnFailed {
                        binary: "erl",
                        message: INPUT.into(),
                    }
                }
                CliError::MemberManifestInvalid { rendered }
                | CliError::WorkspaceManifestInvalid { rendered } => {
                    *rendered = "[M001] first\n[M006] second".into();
                    assert_eq!(error.to_string(), "[M001] first\n[M006] second");
                    continue;
                }
                CliError::NoExecutableMember
                | CliError::WatchAmbiguousMember
                | CliError::ObserverNoCookie
                | CliError::DirectoryNotEmpty
                | CliError::CwdUnreadable
                | CliError::FmtCheckFailed { .. }
                | CliError::MigrateCompileFailed
                | CliError::WatchStateCorrupted
                | CliError::AlreadyReported => continue,
            }
            let rendered = error.to_string();
            assert!(rendered.chars().all(|c| !c.is_control()), "{rendered:?}");
            assert!(
                rendered.contains("café\\path\\u{1b}[2J\\u{9b}2J\\n\\t\\r\\u{0}"),
                "{rendered:?}"
            );
        }
    }

    #[test]
    fn terminal_errors_escape_both_cli_warning_variants() {
        for warning in [
            super::CliWarning::BoolTestDeprecated {
                qualified_name: "M.\u{1b}[2J".into(),
            },
            super::CliWarning::PrefixTestDeprecated {
                module: "M\u{9b}2J".into(),
                name: "test_x\n".into(),
            },
        ] {
            let rendered = warning.to_string();
            assert!(rendered.chars().all(|c| !c.is_control()), "{rendered:?}");
            assert!(rendered.contains("\\u{"));
        }
    }

    /// Name every variant, so that adding one stops the crate from compiling
    /// until someone comes back to this file.
    ///
    /// This is the whole reason the checks below are worth running: a test that
    /// samples whatever variants happened to exist the day it was written stops
    /// finding anything the day after. There is no wildcard arm on purpose.
    fn assert_every_variant_is_named(e: &CliError) {
        match e {
            CliError::Workspace(_)
            | CliError::UnknownMember { .. }
            | CliError::NoExecutableMember
            | CliError::MemberManifestInvalid { .. }
            | CliError::WorkspaceManifestInvalid { .. }
            | CliError::WatchAmbiguousMember
            | CliError::LibraryNotExecutable { .. }
            | CliError::ObserverNoCookie
            | CliError::InvalidProjectName { .. }
            | CliError::DirectoryExists { .. }
            | CliError::ReservedName { .. }
            | CliError::DirectoryNotEmpty
            | CliError::CwdUnreadable
            | CliError::FmtPathNotFound { .. }
            | CliError::FmtIoError { .. }
            | CliError::FmtCheckFailed { .. }
            | CliError::LegacyRgFile { .. }
            | CliError::MigrateModelMissing { .. }
            | CliError::MigrateCompileFailed
            | CliError::MigrateInternal { .. }
            | CliError::MigrateInvalidName { .. }
            | CliError::MigrateEnvMissing { .. }
            | CliError::MigrateApplyFailed { .. }
            | CliError::MigrateStatusFailed { .. }
            | CliError::MigrateRollbackFailed { .. }
            | CliError::Toolchain(_)
            | CliError::WatcherStartFailed { .. }
            | CliError::WatchPathFailed { .. }
            | CliError::ReplSessionFailed { .. }
            | CliError::WatchStateCorrupted
            | CliError::WatchRestartFailed { .. }
            | CliError::ExplainUnknownCode { .. }
            | CliError::AlreadyReported => {}
            CliError::TestRejected { reason, .. } => match reason {
                TestRejection::ArityInvalid
                | TestRejection::CapabilityForbidden
                | TestRejection::ReturnTypeInvalid
                | TestRejection::ReturnTypeMissing => {}
            },
        }
    }

    /// One sample of every [`CliError`] variant.
    fn one_of_each() -> Vec<CliError> {
        let all = vec![
            CliError::Workspace(WorkspaceError::NoWorkspaceRoot { path: "ws".into() }),
            CliError::UnknownMember { name: "x".into() },
            CliError::NoExecutableMember,
            CliError::MemberManifestInvalid {
                rendered: "M001 …".into(),
            },
            CliError::WorkspaceManifestInvalid {
                rendered: "M001 …".into(),
            },
            CliError::WatchAmbiguousMember,
            CliError::LibraryNotExecutable { name: "x".into() },
            CliError::ObserverNoCookie,
            CliError::InvalidProjectName { name: "x/y".into() },
            CliError::DirectoryExists { name: "x".into() },
            CliError::ReservedName { name: "std".into() },
            CliError::DirectoryNotEmpty,
            CliError::CwdUnreadable,
            CliError::FmtPathNotFound {
                path: "a.ridge".into(),
            },
            CliError::FmtIoError {
                path: "a.ridge".into(),
                source: "denied".into(),
            },
            CliError::FmtCheckFailed { count: 2 },
            CliError::LegacyRgFile {
                path: "a.rg".into(),
            },
            CliError::TestRejected {
                qualified_name: "M.test_x".into(),
                reason: TestRejection::ArityInvalid,
            },
            CliError::TestRejected {
                qualified_name: "M.test_x".into(),
                reason: TestRejection::CapabilityForbidden,
            },
            CliError::TestRejected {
                qualified_name: "M.test_x".into(),
                reason: TestRejection::ReturnTypeInvalid,
            },
            CliError::TestRejected {
                qualified_name: "M.test_x".into(),
                reason: TestRejection::ReturnTypeMissing,
            },
            CliError::MigrateModelMissing {
                path: "Model.ridge".into(),
            },
            CliError::MigrateCompileFailed,
            CliError::MigrateInternal {
                message: "x".into(),
            },
            CliError::MigrateInvalidName { name: "x!".into() },
            CliError::MigrateEnvMissing {
                vars: vec!["RIDGE_DB_USER".into()],
            },
            CliError::MigrateApplyFailed {
                message: "x".into(),
            },
            CliError::MigrateStatusFailed {
                message: "x".into(),
            },
            CliError::MigrateRollbackFailed {
                message: "x".into(),
            },
            CliError::Toolchain(ToolchainError::erl_not_found()),
            CliError::WatcherStartFailed {
                message: "x".into(),
            },
            CliError::WatchPathFailed {
                path: "ws".into(),
                message: "x".into(),
            },
            CliError::ReplSessionFailed {
                message: "x".into(),
            },
            CliError::WatchStateCorrupted,
            CliError::WatchRestartFailed {
                message: "x".into(),
            },
            CliError::ExplainUnknownCode {
                code: "Q001".into(),
            },
            CliError::AlreadyReported,
        ];

        for e in &all {
            assert_every_variant_is_named(e);
        }

        all
    }

    /// A variant that claims a code opens its message with that code.
    ///
    /// `C004` had five different message texts across two crates for as long as
    /// the code lived inside the format string, where nothing could compare it
    /// against anything.
    #[test]
    fn every_code_carrying_variant_opens_with_its_code() {
        for e in one_of_each() {
            let text = e.to_string();
            match e.code() {
                Some(code) => assert!(
                    text.starts_with(code),
                    "`{code}` must open its own message, got: {text}"
                ),
                None => assert!(
                    !text.starts_with('C'),
                    "a variant with no code must not look like it has one: {text}"
                ),
            }
        }
    }

    /// No two variants answer with the same code.
    #[test]
    fn no_two_variants_share_a_code() {
        let mut seen: Vec<(&'static str, String)> = Vec::new();
        for e in one_of_each() {
            let Some(code) = e.code() else { continue };
            if let Some((_, first)) = seen.iter().find(|(c, _)| *c == code) {
                panic!("{code} is claimed twice: {first} — and — {e}");
            }
            seen.push((code, e.to_string()));
        }
    }
}
