//! CLI-level error codes (`C005`–`C007a`) not covered by `ridge-driver`.
//!
//! These errors are raised by `ridge-cli` before or after handing off to the
//! driver, when the CLI detects a structural problem in the workspace or the
//! user's invocation.

use std::fmt;

// ── CLI error enum ────────────────────────────────────────────────────────────

/// A fatal CLI-level error.
///
/// Each variant carries the stable error code in its `Display` output.
#[derive(Debug)]
#[non_exhaustive]
pub enum CliError {
    /// `C001` — no workspace root found at or above the current directory.
    NoWorkspaceRoot,

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

    /// `C301` — a `pub fn test_*` function has arity != 0.
    ///
    /// Test functions must take zero parameters.
    TestArityInvalid {
        /// The qualified name of the test function (e.g. `Demo.test_foo`).
        qualified_name: String,
    },

    /// `C302` — a `pub fn test_*` function declares the `ffi` capability.
    ///
    /// FFI tests are not permitted in `ridge test` 0.1.0 (per D017 / §1.3 #9).
    TestCapabilityForbidden {
        /// The qualified name of the test function.
        qualified_name: String,
    },

    /// `C401` — `<src_root>/migrations/Model.ridge` is missing.
    MigrateModelMissing {
        /// The path where `Model.ridge` was expected.
        path: std::path::PathBuf,
    },

    /// `C402` — `erl` or `erlc` is not on `PATH`.
    ///
    /// `ridge migrate add` needs a real BEAM runtime to run the diff/render
    /// pipeline that produces the migration and snapshot files.
    MigrateErlangNotFound,

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
            Self::NoWorkspaceRoot => "C001",
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
            Self::TestArityInvalid { .. } => "C301",
            Self::TestCapabilityForbidden { .. } => "C302",
            Self::MigrateModelMissing { .. } => "C401",
            Self::MigrateErlangNotFound => "C402",
            Self::MigrateCompileFailed => "C403",
            Self::MigrateInternal { .. } => "C404",
            Self::MigrateInvalidName { .. } => "C405",
            Self::MigrateEnvMissing { .. } => "C406",
            Self::MigrateApplyFailed { .. } => "C407",
            Self::MigrateStatusFailed { .. } => "C408",
            Self::MigrateRollbackFailed { .. } => "C409",
            Self::AlreadyReported | Self::MemberManifestInvalid { .. } => return None,
        })
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
        let c = self.code();
        match self {
            Self::BoolTestDeprecated { qualified_name } => write!(
                f,
                "warning: {c} BoolTestDeprecated: '{qualified_name}' returns Bool (deprecated);                  -- migrate: change return type to Result Unit Text;                  replace 'true' with 'Ok ()' and 'false' with 'Err \"<reason>\"'"
            ),
            Self::PrefixTestDeprecated { module, name } => write!(
                f,
                "warning: {c} PrefixTestDeprecated: '{module}.{name}' uses the deprecated `test_`                  prefix; add `@test \"{stem}\"` above the function and remove the prefix in 0.3.0",
                stem = name.strip_prefix("test_").unwrap_or(name)
            ),
        }
    }
}

impl fmt::Display for CliError {
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per error code; splitting it up would scatter the C-code registry"
    )]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWorkspaceRoot => write!(
                f,
                "C001 NoWorkspaceRoot: no workspace manifest found at or above the current directory"
            ),
            // Never rendered through the top-level handler (main special-cases
            // it), but give it an honest message in case some other caller
            // prints it directly.
            Self::AlreadyReported => write!(f, "build failed"),
            Self::UnknownMember { name } => write!(
                f,
                "C005 UnknownMember: workspace has no member named '{name}'"
            ),
            Self::NoExecutableMember => write!(
                f,
                "C006 NoExecutableMember: workspace has no member with kind = \"app\" or kind = \"service\""
            ),
            Self::MemberManifestInvalid { rendered } => write!(f, "{rendered}"),
            Self::WatchAmbiguousMember => write!(
                f,
                "C011 WatchAmbiguousMember: --watch requires --member when the workspace has multiple executable members"
            ),
            Self::LibraryNotExecutable { name } => write!(
                f,
                "C007 LibraryNotExecutable: member '{name}' has kind = \"library\" and cannot be run"
            ),
            Self::ObserverNoCookie => write!(
                f,
                "C008 ObserverNoCookie: --observer requires an Erlang cookie; \
                 ~/.erlang.cookie was not found. \
                 Provide one with --cookie <value>"
            ),
            Self::InvalidProjectName { name } => write!(
                f,
                "C201 InvalidProjectName: '{name}' is not a valid portable project name; \
                 names must be non-empty, must not contain '/', '\\', '..', or non-portable \
                 characters, and must not start with '.'"
            ),
            Self::DirectoryExists { name } => write!(
                f,
                "C202 DirectoryExists: directory '{name}' already exists"
            ),
            Self::ReservedName { name } => write!(
                f,
                "C203 ReservedName: '{name}' is reserved by the Ridge toolchain"
            ),
            Self::DirectoryNotEmpty => write!(
                f,
                "C204 DirectoryNotEmpty: the current directory is not empty; \
                 ridge init requires an empty directory \
                 (only .git/ and .gitignore are permitted)"
            ),
            Self::CwdUnreadable => write!(
                f,
                "C205 CwdUnreadable: could not read the current working directory"
            ),
            Self::FmtPathNotFound { path } => write!(
                f,
                "C102 FmtPathNotFound: path '{}' does not exist",
                path.display()
            ),
            Self::FmtIoError { path, source } => write!(
                f,
                "C103 FmtIoError: I/O error on '{}': {source}",
                path.display()
            ),
            Self::FmtCheckFailed { count } => write!(
                f,
                "C104 FmtCheckFailed: {count} file(s) would be reformatted"
            ),
            Self::LegacyRgFile { path } => {
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
            Self::TestArityInvalid { qualified_name } => write!(
                f,
                "C301 TestArityInvalid: '{qualified_name}' must have zero parameters; \
                 test functions cannot take arguments"
            ),
            Self::TestCapabilityForbidden { qualified_name } => write!(
                f,
                "C302 TestCapabilityForbidden: '{qualified_name}' declares the 'ffi' capability; \
                 ffi tests are not permitted in ridge test 0.1.0"
            ),
            Self::MigrateModelMissing { path } => write!(
                f,
                "C401 MigrateModelMissing: '{}' was not found; \
                 create it with `pub fn model () -> List (EntitySchema Unit) = ...`",
                path.display()
            ),
            Self::MigrateErlangNotFound => write!(
                f,
                "C402 MigrateErlangNotFound: erl and erlc must be on PATH \
                 to run `ridge migrate add` (install OTP 26+)"
            ),
            Self::MigrateCompileFailed => write!(
                f,
                "C403 MigrateCompileFailed: the model failed to compile; \
                 see the diagnostics above"
            ),
            Self::MigrateInternal { message } => write!(f, "C404 MigrateInternal: {message}"),
            Self::MigrateInvalidName { name } => write!(
                f,
                "C405 MigrateInvalidName: '{name}' is not a valid migration name; \
                 use only ASCII letters, digits, '_', and '-'"
            ),
            Self::MigrateEnvMissing { vars } => write!(
                f,
                "C406 MigrateEnvMissing: missing required environment variable(s): {}; \
                 ridge migrate apply/status needs these to connect to the database",
                vars.join(", ")
            ),
            Self::MigrateApplyFailed { message } => {
                write!(f, "C407 MigrateApplyFailed: {message}")
            }
            Self::MigrateStatusFailed { message } => {
                write!(f, "C408 MigrateStatusFailed: {message}")
            }
            Self::MigrateRollbackFailed { message } => {
                write!(f, "C409 MigrateRollbackFailed: {message}")
            }
        }
    }
}

impl std::error::Error for CliError {}
