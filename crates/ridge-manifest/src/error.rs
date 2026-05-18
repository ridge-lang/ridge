//! Manifest error types — `M001`–`M020`.
//!
//! Error codes are **stable across releases** — downstream tooling (LSP,
//! `ariadne` renderer) keys on these strings.  Never renumber an assigned
//! code; only append new ones at the end.
//!
//! ## `ManifestError` (M001..M020)
//!
//! Produced while parsing workspace / project manifest files (`ridge.toml`).
//! Manifest errors do NOT carry a `Span` (manifests are not `.ridge` source);
//! [`ManifestError::span`] always returns `None`.  Only
//! [`ManifestError::code`] is guaranteed stable.

use std::path::PathBuf;

use ridge_ast::Span;

// ── ManifestError ─────────────────────────────────────────────────────────────

/// A manifest parsing or validation error produced while reading `ridge.toml`
/// files.
///
/// Manifest errors do **not** carry a [`Span`] (manifests are not `.ridge` source
/// files).  [`ManifestError::span`] always returns `None`.  Only
/// [`ManifestError::code`] is guaranteed stable across releases.
///
/// # Stability
///
/// Marked `#[non_exhaustive]` — new manifest error codes may be added in
/// future versions (e.g. M021+).  Match arms outside this crate must include
/// a wildcard (`_`) arm.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// M001 — the manifest TOML could not be parsed.
    #[error("TOML parse error in `{path}`: {message}")]
    TomlParseFailed {
        /// Path of the manifest file.
        path: PathBuf,
        /// Human-readable TOML parse error message.
        message: String,
    },

    /// M002 — the workspace manifest is missing the `[workspace]` table.
    #[error("`{path}` is missing the `[workspace]` table")]
    MissingWorkspaceTable {
        /// Path of the manifest file.
        path: PathBuf,
    },

    /// M003 — a project manifest is missing the `[project]` table.
    #[error("`{path}` is missing the `[project]` table")]
    MissingProjectTable {
        /// Path of the manifest file.
        path: PathBuf,
    },

    /// M004 — a workspace member directory has no `ridge.toml` project manifest.
    #[error("member directory `{member_dir}` has no `ridge.toml`")]
    MemberWithoutProjectManifest {
        /// The member directory that was missing a manifest.
        member_dir: PathBuf,
    },

    /// M005 — a workspace `members` glob pattern is invalid.
    #[error("invalid member glob `{pattern}`: {error}")]
    BadMemberGlob {
        /// The invalid glob pattern string.
        pattern: String,
        /// The error returned by the glob compiler.
        error: String,
    },

    /// M006 — a required field is absent from a manifest table.
    #[error("missing required field `{field}` in `[{table}]` in `{path}`")]
    MissingRequiredField {
        /// The TOML table name (e.g. `"project"`, `"workspace"`).
        table: String,
        /// The missing field name.
        field: String,
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M007 — the `kind` field contains an unrecognised project kind string.
    #[error("invalid project kind `{kind}` in `{path}`")]
    InvalidProjectKind {
        /// The unrecognised kind string.
        kind: String,
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M008 — a `forbid` rule entry is syntactically or semantically invalid.
    #[error("invalid forbid rule in `{path}`: {reason}")]
    InvalidForbidRule {
        /// Human-readable reason the rule is invalid.
        reason: String,
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M009 — a dependency entry uses an unrecognised `kind` value.
    #[error("invalid dependency kind `{raw}` in `{path}`")]
    InvalidDependencyKind {
        /// The raw unrecognised kind string.
        raw: String,
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M010 — two workspace members declared the same project name.
    #[error("duplicate project name `{name}`: first at `{first}`, second at `{second}`")]
    DuplicateProjectName {
        /// The duplicated project name.
        name: String,
        /// Path of the first manifest.
        first: PathBuf,
        /// Path of the second (conflicting) manifest.
        second: PathBuf,
    },

    /// M011 — an unrecognised capability name was used in a manifest.
    #[error("unknown capability name `{name}` in `{path}`")]
    InvalidCapabilityName {
        /// The unrecognised capability name.
        name: String,
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M012 — a dependency cycle was detected among workspace projects.
    #[error("dependency cycle: {}", chain.join(" -> "))]
    CycleInDependencies {
        /// The ordered chain of project names forming the cycle.
        chain: Vec<String>,
    },

    /// M013 — a dependency names a project not present in the workspace.
    #[error("unknown workspace member `{name}` referenced from `{path}`")]
    UnknownWorkspaceMember {
        /// The missing project name.
        name: String,
        /// Path of the manifest that referenced it.
        path: PathBuf,
    },

    /// M014 — a project `exports` pattern string is not a valid glob.
    #[error("invalid export pattern `{raw}` in `{path}`")]
    ProjectExportPatternInvalid {
        /// The invalid pattern string.
        raw: String,
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M015 — a manifest references a workspace-level dependency that is not
    /// declared in `[workspace.dependencies]`.
    #[error("workspace dependency `{name}` not declared in workspace manifest (`{path}`)")]
    WorkspaceDependencyAbsent {
        /// The missing dependency name.
        name: String,
        /// Path of the project manifest that referenced it.
        path: PathBuf,
    },

    /// M016 — a Git dependency specifies more than one of `tag`, `branch`, or
    /// `rev` simultaneously.
    #[error("git dependency in `{path}` specifies conflicting rev selectors")]
    GitRevConflict {
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M017 — a relative path dependency escapes the workspace root.
    #[error("relative path `{path}` in `{manifest}` escapes the workspace root")]
    RelativePathEscapesWorkspace {
        /// The relative path string.
        path: String,
        /// Path of the manifest.
        manifest: PathBuf,
    },

    /// M018 — a Hex (package-registry) dependency was used in a 0.1.0 workspace
    /// where only path and git dependencies are supported.
    #[error("hex dependency `{name}` in `{path}` is not supported in Ridge 0.1.0")]
    HexDependencyUsedIn010 {
        /// The dependency name.
        name: String,
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M019 — an unrecognised key appeared in a manifest table.
    #[error("unknown manifest key `{key}` in `[{table}]` in `{path}`")]
    UnknownManifestKey {
        /// The TOML table name.
        table: String,
        /// The unrecognised key.
        key: String,
        /// Path of the manifest.
        path: PathBuf,
    },

    /// M020 — a `[project.exports].public` pattern matched no symbol in the
    /// module's top-level table.
    ///
    /// This means the export pattern likely contains a typo or references a
    /// symbol that has been renamed or removed.  Update the pattern to match
    /// an existing name or remove it from the export list.
    #[error("export pattern `{name}` in `{manifest_path}` matched no symbols in the module")]
    ExportNotFound {
        /// The export pattern that matched nothing.
        name: String,
        /// Path of the project manifest.
        manifest_path: PathBuf,
    },
}

impl ManifestError {
    /// Return the stable error code string for this variant.
    ///
    /// Codes are **stable across releases** — never renumber an assigned code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TomlParseFailed { .. } => "M001",
            Self::MissingWorkspaceTable { .. } => "M002",
            Self::MissingProjectTable { .. } => "M003",
            Self::MemberWithoutProjectManifest { .. } => "M004",
            Self::BadMemberGlob { .. } => "M005",
            Self::MissingRequiredField { .. } => "M006",
            Self::InvalidProjectKind { .. } => "M007",
            Self::InvalidForbidRule { .. } => "M008",
            Self::InvalidDependencyKind { .. } => "M009",
            Self::DuplicateProjectName { .. } => "M010",
            Self::InvalidCapabilityName { .. } => "M011",
            Self::CycleInDependencies { .. } => "M012",
            Self::UnknownWorkspaceMember { .. } => "M013",
            Self::ProjectExportPatternInvalid { .. } => "M014",
            Self::WorkspaceDependencyAbsent { .. } => "M015",
            Self::GitRevConflict { .. } => "M016",
            Self::RelativePathEscapesWorkspace { .. } => "M017",
            Self::HexDependencyUsedIn010 { .. } => "M018",
            Self::UnknownManifestKey { .. } => "M019",
            Self::ExportNotFound { .. } => "M020",
        }
    }

    /// Return the source span associated with this error — always `None` for
    /// manifest errors because manifests are not `.ridge` source files.
    #[must_use]
    pub const fn span(&self) -> Option<Span> {
        None
    }
}
