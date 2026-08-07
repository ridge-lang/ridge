//! Error and warning types for `ridge-pkg`.
//!
//! Error codes are allocated in the `P###` namespace (§1.3 #3):
//! - `P201`–`P099` — git-transport and path-resolution failures
//! - `P101`–`P199` — manifest-parse failures within the package context
//!
//! `P204` is a **warning** (`PkgWarning`), not an error — floating-branch
//! tracking is advisory-only.

use thiserror::Error;

use ridge_manifest::ManifestError;

// ── Error type ────────────────────────────────────────────────────────────────

/// All errors emitted by `ridge-pkg`.
#[derive(Debug, Error)]
pub enum PkgError {
    // ── Git transport errors ─────────────────────────────────────────────────
    /// `P201` — `git clone` exited non-zero due to network failure.
    ///
    /// # Example
    ///
    /// ```text
    /// P201 PkgGitFetchFailed: git exited with status 128 (network unreachable)
    /// ```
    #[error("P201 PkgGitFetchFailed: git clone failed for '{url}': {message} (exit {exit_code})")]
    PkgGitFetchFailed {
        /// Remote URL that was being fetched.
        url: String,
        /// Human-readable reason extracted from `git` stderr.
        message: String,
        /// `git` process exit code.
        exit_code: i32,
    },

    /// `P202` — Cache directory write failed (disk full or permission denied).
    ///
    /// # Example
    ///
    /// ```text
    /// P202 PkgCacheWriteFailed: could not write to cache at /tmp/ridge/git/…
    /// ```
    #[error("P202 PkgCacheWriteFailed: could not write to cache at '{path}': {message}")]
    PkgCacheWriteFailed {
        /// Target cache path.
        path: std::path::PathBuf,
        /// Underlying I/O error message.
        message: String,
    },

    /// `P203` — Git URL uses SSH scheme (`git@…` or `ssh://…`), which is
    /// not supported in 0.1.0 (HTTPS-only).
    ///
    /// # Example
    ///
    /// ```text
    /// P203 PkgGitSchemeUnsupported: SSH URLs are not supported; use HTTPS
    /// ```
    #[error("P203 PkgGitSchemeUnsupported: SSH URL '{url}' is not supported in 0.1.0; use an HTTPS URL instead")]
    PkgGitSchemeUnsupported {
        /// The offending URL.
        url: String,
    },

    // P204 is PkgWarning::FloatingBranchAdvisory — not an error.
    /// `P205` — `git` binary not found on `PATH`.
    ///
    /// # Example
    ///
    /// ```text
    /// P205 PkgGitNotInstalled: 'git' binary not found on PATH
    /// ```
    #[error("P205 PkgGitNotInstalled: 'git' binary not found on PATH; install git and retry")]
    PkgGitNotInstalled,

    /// `P206` — Circular dependency detected during resolution.
    ///
    /// # Example
    ///
    /// ```text
    /// P206 PkgDependencyCycle: cycle detected: foo → bar → foo
    /// ```
    #[error("P206 PkgDependencyCycle: dependency cycle detected: {cycle_path}")]
    PkgDependencyCycle {
        /// Human-readable cycle description (e.g. `"A → B → A"`).
        cycle_path: String,
    },

    /// `P207` — The requested tag or branch does not exist on the remote.
    ///
    /// # Example
    ///
    /// ```text
    /// P207 PkgGitTagUnknown: tag 'v99.0' not found on 'https://github.com/x/y'
    /// ```
    #[error("P207 PkgGitTagUnknown: ref '{git_ref}' not found on remote '{url}'")]
    PkgGitTagUnknown {
        /// The tag or branch name that was not found.
        git_ref: String,
        /// Remote URL.
        url: String,
    },

    /// `P208` — Installed `git` is older than the minimum required version 2.20.
    ///
    /// Upgrade hint is platform-specific.
    ///
    /// # Example
    ///
    /// ```text
    /// P208 PkgGitTooOld: git 2.10.0 is below minimum 2.20; upgrade with: brew upgrade git
    /// ```
    #[error(
        "P208 PkgGitTooOld: git {found_version} is below minimum required version 2.20; {upgrade_hint}"
    )]
    PkgGitTooOld {
        /// Detected version string (e.g. `"2.10.0"`).
        found_version: String,
        /// Platform-specific upgrade hint.
        upgrade_hint: String,
    },

    /// `P209` — `git --version` output could not be parsed (exotic distro or
    /// custom build).  R17 mitigation — lenient parse; only this code fires if
    /// the output truly has no recognisable version token.
    ///
    /// # Example
    ///
    /// ```text
    /// P209 PkgGitVersionUnparseable: could not parse version from: 'git version ???'
    /// ```
    #[error("P209 PkgGitVersionUnparseable: could not parse git version from output: '{output}'")]
    PkgGitVersionUnparseable {
        /// Raw `git --version` output.
        output: String,
    },

    /// `P210` — A registry-based version dependency was encountered.
    ///
    /// Version-only deps (`version = "1.0"`) require a registry (`hex.pm`
    /// or equivalent) which is not available until 0.2.0.  Use a `path` or
    /// `git` dep instead.
    ///
    /// # Example
    ///
    /// ```text
    /// P210 PkgVersionDepUnsupported: version dep 'mylib = "1.0"' requires a
    /// registry which is not available until Ridge 0.2.0; use path or git
    /// ```
    #[error(
        "P210 PkgVersionDepUnsupported: version dep '{name} = \"{version}\"' requires a \
         registry which is not available until Ridge 0.2.0; use a path or git dependency instead"
    )]
    PkgVersionDepUnsupported {
        /// Local dependency name.
        name: String,
        /// Version string as written in `ridge.toml`.
        version: String,
    },

    // ── Path / manifest errors ────────────────────────────────────────────────
    /// `P101` — Path dependency's `ridge.toml` is missing or the path does not
    /// exist.
    ///
    /// # Example
    ///
    /// ```text
    /// P101 PkgPathManifestMissing: no ridge.toml found at '../foo'
    /// ```
    #[error("P101 PkgPathManifestMissing: no ridge.toml found at '{path}'")]
    PkgPathManifestMissing {
        /// Resolved path that was checked.
        path: std::path::PathBuf,
    },

    /// `P102` — A `ridge.toml` was found but could not be parsed.
    ///
    /// Wraps `ridge_manifest::ManifestError`.
    #[error("P102 PkgManifestParseFailed: manifest parse failed at '{path}': {source}")]
    PkgManifestParseFailed {
        /// Path to the manifest that failed to parse.
        path: std::path::PathBuf,
        /// Underlying manifest parse error.
        #[source]
        source: ManifestError,
    },

    /// `P103` — Cache root could not be determined (no home directory available).
    #[error("P103 PkgCacheRootUnavailable: could not determine platform cache directory; set XDG_CACHE_HOME (Linux) or HOME")]
    PkgCacheRootUnavailable,

    /// `P104` — `GitRev::Commit` was encountered; commit-pinned git dependencies
    /// are not yet supported in 0.1.0.
    #[error("P104 PkgGitCommitUnsupported: commit-pinned git dependency '{name}' is not supported in 0.1.0; use tag or branch")]
    PkgGitCommitUnsupported {
        /// Local dependency name.
        name: String,
    },
}

impl PkgError {
    /// Return the canonical error code string (e.g. `"P201"`).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PkgGitFetchFailed { .. } => "P201",
            Self::PkgCacheWriteFailed { .. } => "P202",
            Self::PkgGitSchemeUnsupported { .. } => "P203",
            Self::PkgGitNotInstalled => "P205",
            Self::PkgDependencyCycle { .. } => "P206",
            Self::PkgGitTagUnknown { .. } => "P207",
            Self::PkgGitTooOld { .. } => "P208",
            Self::PkgGitVersionUnparseable { .. } => "P209",
            Self::PkgVersionDepUnsupported { .. } => "P210",
            Self::PkgPathManifestMissing { .. } => "P101",
            Self::PkgManifestParseFailed { .. } => "P102",
            Self::PkgCacheRootUnavailable => "P103",
            Self::PkgGitCommitUnsupported { .. } => "P104",
        }
    }
}

// ── Warning type ──────────────────────────────────────────────────────────────

/// Non-fatal advisories emitted alongside dependency resolution.
///
/// `P204 FloatingBranchAdvisory` is a **warning**, not an error —
/// floating-branch tracking is allowed but degrades reproducibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PkgWarning {
    /// `P204` — A git dependency tracks a mutable branch rather than a pinned
    /// tag.  Build reproducibility is not guaranteed.
    ///
    /// # Example
    ///
    /// ```text
    /// P204 FloatingBranchAdvisory: dep 'mylib' tracks branch 'main' which is
    /// not pinned; build reproducibility is not guaranteed
    /// ```
    FloatingBranchAdvisory {
        /// Local dependency name.
        dep_name: String,
        /// Branch name being tracked.
        branch: String,
    },
}

impl PkgWarning {
    /// Return the canonical code string (`"P204"`).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::FloatingBranchAdvisory { .. } => "P204",
        }
    }

    /// Format the warning as a human-readable string suitable for `eprintln!`.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::FloatingBranchAdvisory { dep_name, branch } => format!(
                "warning[P204]: dep '{dep_name}' tracks branch '{branch}' which is not pinned; \
                 build reproducibility is not guaranteed"
            ),
        }
    }
}
