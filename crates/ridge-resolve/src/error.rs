//! Resolve error types.
//!
//! Error codes are **stable across releases** — downstream tooling (LSP,
//! `ariadne` renderer) keys on these strings.  Never renumber an assigned code;
//! only append new ones at the end.
//!
//! ## `ResolveError` (R001..R024, R999; R018 reserved)
//!
//! Produced during the name-resolution pass over source files.  Every variant
//! carries a [`Span`] pointing to the offending source location and a stable
//! code returned by [`ResolveError::code`].
//!
//! R018 is **reserved** — the former variant was removed (bare imports are
//! unambiguous under the R001 qualified-namespace default).
//! The slot is kept to prevent code reuse; no variant emits "R018".
//!
//! ## `ManifestError` (M001..M020)
//!
//! Produced while parsing workspace / project manifest files (`ridge.toml`).
//! Manifest errors do NOT carry a `Span` (manifests are not `.ridge` source);
//! [`ManifestError::span`] always returns `None`.  Only [`ManifestError::code`]
//! is guaranteed stable.

use std::path::PathBuf;

use ridge_ast::{Capability, Span};

use crate::ModuleId;

// ── Severity ──────────────────────────────────────────────────────────────────

/// Diagnostic severity for a [`ResolveError`].
///
/// Phase 6 (`ridge-diagnostics`) uses this to drive rendering color, exit
/// codes, and IDE squiggles.  At resolve time both `Warning` and `Error`
/// variants are pushed into the same `Vec<ResolveError>`; downstream code
/// inspects [`ResolveError::severity`] to filter or rank them.
///
/// Resolved per R002 (cross-scope shadowing silent, same-scope duplicate
/// is `R011` hard error) and R005 (state-field shadowing is warn-level).
///
/// # Stability
///
/// Marked `#[non_exhaustive]` — future versions anticipate `Hint` and `Info`
/// levels to align with LSP `DiagnosticSeverity`.  Match arms must include a
/// wildcard (`_`) when exhaustive matching is not required.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Hard error — blocks compilation; non-zero exit code.
    Error,
    /// Warning — does not block compilation, but is rendered prominently.
    Warning,
}

// ── ResolveError ──────────────────────────────────────────────────────────────

/// A name-resolution error produced by `ridge-resolve`.
///
/// Every variant carries a [`Span`] pointing to the offending source location
/// and a stable error code returned by [`ResolveError::code`].
///
/// `Display` produces a human-readable message suitable for terminal output.
/// `ridge-diagnostics` will later render these with `ariadne`.
///
/// # Stability
///
/// Marked `#[non_exhaustive]` — new error codes may be added in future
/// versions (e.g. R022+).  Match arms outside this crate must include a
/// wildcard (`_`) arm.
#[non_exhaustive]
#[derive(Debug, Clone, thiserror::Error)]
pub enum ResolveError {
    /// R001 — no `ridge.toml` workspace manifest was found at the given path.
    #[error("missing workspace manifest at `{path}`")]
    MissingWorkspaceManifest {
        /// Path where the manifest was expected.
        path: PathBuf,
    },

    /// R002 — the same fully-qualified module name was declared more than once.
    #[error("duplicate module `{fqn}`")]
    DuplicateModule {
        /// The duplicated fully-qualified module name.
        fqn: String,
        /// Span of the first declaration.
        first: Span,
        /// Span of the second (conflicting) declaration.
        second: Span,
    },

    /// R003 — a cycle was detected in the import graph.
    #[error("cyclic import involving {}", cycle.iter().map(|id| id.0.to_string()).collect::<Vec<_>>().join(" -> "))]
    CyclicImport {
        /// The ordered list of module IDs forming the cycle.
        cycle: Vec<ModuleId>,
        /// Span of the first import edge in the cycle.
        first_edge: Span,
    },

    /// R004 — a module imports itself.
    #[error("a module may not import itself")]
    SelfImport {
        /// Span of the self-import statement.
        span: Span,
    },

    /// R005 — the same name was declared more than once at the top level of a module.
    #[error("duplicate declaration `{name}`")]
    DuplicateDeclaration {
        /// The duplicated name.
        name: String,
        /// Span of the first declaration.
        first_span: Span,
        /// Span of the second (conflicting) declaration.
        second_span: Span,
    },

    /// R006 — an import path could not be resolved to any known module.
    #[error("unresolved import path `{path}`")]
    UnresolvedImportPath {
        /// The import path that could not be resolved.
        path: String,
        /// Span of the import statement.
        span: Span,
    },

    /// R007 — a module in one project tried to import a non-exported symbol from another project.
    #[error("project export violation: `{target}` is not exported by project `{target_project}`")]
    ProjectExportViolation {
        /// The symbol or path that was not exported.
        target: String,
        /// The project that owns the symbol.
        target_project: String,
        /// Span of the violating import.
        span: Span,
    },

    /// R008 — a named import item could not be found in the target module.
    #[error("unresolved import item `{name}` from module `{module}`")]
    UnresolvedImportItem {
        /// The name that was not found.
        name: String,
        /// The module that was searched.
        module: String,
        /// Up to three Levenshtein-close exported-item names from `module`,
        /// pre-filtered for visibility from the importing project.
        suggestions: Vec<String>,
        /// Span of the import item.
        span: Span,
    },

    /// R009 — a name is referenced outside its declared visibility scope.
    #[error("visibility violation: `{name}` is not accessible here")]
    VisibilityViolation {
        /// The name whose visibility was violated.
        name: String,
        /// Span where the name was defined.
        defined_at: Span,
        /// Span of the use site.
        use_span: Span,
    },

    /// R010 — an identifier could not be resolved; suggestions are provided if available.
    #[error("unresolved identifier `{name}`")]
    UnresolvedIdent {
        /// The unresolved identifier.
        name: String,
        /// Up to three Levenshtein-close candidates visible at the error site.
        suggestions: Vec<String>,
        /// Span of the identifier.
        span: Span,
    },

    /// R011 — the same local variable name was bound more than once in the same scope.
    #[error("duplicate local binding `{name}`")]
    DuplicateLocal {
        /// The duplicated local name.
        name: String,
        /// Span of the first binding.
        first_span: Span,
        /// Span of the second (conflicting) binding.
        second_span: Span,
    },

    /// R012 — a qualified name (e.g. `Foo.Bar.baz`) could not be resolved.
    #[error("unresolved qualified name `{}`", segments.join("."))]
    UnresolvedQualifiedName {
        /// The segments of the qualified name.
        segments: Vec<String>,
        /// Up to three Levenshtein-close fully-rendered qualified-name
        /// candidates (e.g. `["List.map"]` for typo `Li.map`).
        suggestions: Vec<String>,
        /// Span of the qualified name.
        span: Span,
    },

    /// R013 — a `forbid` architectural rule was violated.
    ///
    /// `Display` renders the spec §8.6 multi-line diagnostic directly:
    /// `file:line` of the offending import, the rule text, the manifest
    /// provenance, and a fix suggestion.  Phase 8 (LSP) will serialize
    /// structured JSON using the individual fields rather than re-parsing
    /// the rendered string.
    #[error(
        "R013: forbidden dependency\n  \
         --> {importer_fqn}:{import_span}\n   \
         |\n   \
         | {importer_fqn} cannot depend on {target_fqn}\n   \
         |\n  \
         = rule: {rule_text}\n  \
         = suggestion: {}"
        ,
        suggestion.as_deref().unwrap_or("remove the import or update [workspace.rules].forbid")
    )]
    ForbidViolation {
        /// Raw forbid rule text, e.g. `from = "acme.domain.*"\nto = "acme.infra.*"`.
        rule_text: String,
        /// Fully-qualified name of the importing module.
        importer_fqn: String,
        /// Fully-qualified name of the forbidden dependency.
        target_fqn: String,
        /// Span of the `import` statement in the importer source file.
        import_span: Span,
        /// Span of the forbid rule in `ridge.toml`.
        ///
        /// Currently `None` — `toml_edit`-based span extraction is deferred.
        /// The Display render omits the `defined in:` line when `None`.
        manifest_span: Option<Span>,
        /// Optional refactoring hint shown in the suggestion line.
        suggestion: Option<String>,
    },

    /// R014 — a reference to a standard-library symbol that does not exist.
    #[error("unknown stdlib symbol `{name}` in module `{module}`")]
    UnknownStdlibSymbol {
        /// The stdlib module that was searched.
        module: String,
        /// The name that was not found.
        name: String,
        /// Up to three Levenshtein-close candidates from the same stdlib module.
        suggestions: Vec<String>,
        /// Span of the reference.
        span: Span,
    },

    /// R015 — a capability is used but denied by the project or workspace manifest.
    #[error("capability `{cap:?}` denied at `{denied_at}`")]
    CapabilityDenied {
        /// The denied capability.
        cap: Capability,
        /// The path (manifest or config location) that issued the denial.
        denied_at: String,
        /// Span of the capability keyword.
        span: Span,
    },

    /// R016 — a capability is declared on a function but the project's
    /// `capabilities_allow` list does not include it.
    #[error("capability `{cap:?}` is not allowed in project `{project}`")]
    CapabilityNotAllowed {
        /// The capability that is not in the allow list.
        cap: Capability,
        /// The project name.
        project: String,
        /// Span of the capability keyword.
        span: Span,
    },

    /// R017 — a local binding shadows an actor state field in the same scope.
    #[error("local `{name}` shadows actor state field")]
    StateFieldShadowedByLocal {
        /// The shadowed name.
        name: String,
        /// Span of the local binding.
        local_span: Span,
        /// Span of the state field declaration.
        field_span: Span,
    },

    // R018 is RESERVED — the former variant was removed.
    // Bare imports are resolved unambiguously (R001 provisional default accepted
    // 2026-04-25: bare `import foo.bar` exposes only a qualified namespace alias).
    // The numeric slot is kept reserved so existing diagnostic consumers that key
    // on "R018" see a gap rather than a repurposed code.
    /// R019 — an unrecognised capability keyword was encountered.
    #[error("unknown capability keyword `{text}`")]
    UnknownCapabilityKeyword {
        /// The unrecognised text.
        text: String,
        /// Span of the keyword.
        span: Span,
    },

    /// R020 — a capability list was attached to a declaration that does not
    /// support capability annotations (e.g. a `type` alias).
    #[error("capability list not allowed on this declaration")]
    CapabilityListOnWrongDecl {
        /// Span of the capability list.
        span: Span,
    },

    /// R021 — an actor state type has neither a `default` expression nor an
    /// `init` block, which means it can never be constructed.
    #[error("actor `{name}` state must have a `default` value or an `init` block")]
    ActorStateMissingDefaultOrInit {
        /// The actor name.
        name: String,
        /// Span of the actor declaration.
        span: Span,
    },

    /// R022 — an `@ffi` attribute was used outside the `crates/ridge-stdlib/`
    /// crate.  `@ffi` is stdlib-only in 0.1.0 (§5.5 / T003 `FfiOutsideStdlib`).
    #[error("`@ffi` is only allowed in the Ridge standard library (T003)")]
    FfiOutsideStdlib {
        /// Span of the function name carrying the `@ffi` annotation.
        span: Span,
    },

    /// R023 — a source file with the legacy `.rg` extension was found.
    ///
    /// Ridge no longer recognises `.rg`; sources must end in `.ridge`.
    /// Rename the file with `git mv` and update the `entry` field in `ridge.toml`.
    #[error(
        "`{}` uses the legacy `.rg` extension — \
         rename it to `.ridge` and update the `entry` field in `ridge.toml` if needed \
         (e.g. `git mv {} {}`)",
        path.display(),
        path.display(),
        path.with_extension("ridge").display()
    )]
    LegacyRgExtension {
        /// The file path of the legacy source file.
        path: PathBuf,
    },

    /// R024 — two distinct typeclasses declare the same method name, making a
    /// bare reference to that name ambiguous.
    ///
    /// Qualify the call with the instance type or rename one of the methods to
    /// eliminate the ambiguity.
    #[error("method name `{name}` is declared by multiple classes: `{first_class}` and `{second_class}`")]
    AmbiguousMethodName {
        /// The method name that appears in more than one class.
        name: String,
        /// The first class that declares the method.
        first_class: String,
        /// The second (conflicting) class that declares the method.
        second_class: String,
        /// Span of the method reference that triggered the error.
        span: Span,
    },

    /// R999 — two AST nodes were assigned the same `NodeId` (signals a
    /// compiler bug, not a user error).
    #[error("internal error: NodeId collision in `{node_kind}`")]
    InternalNodeIdCollision {
        /// The kind of AST node that collided.
        node_kind: String,
        /// Span of the node.
        span: Span,
    },
}

impl ResolveError {
    /// Return the stable error code string for this variant.
    ///
    /// Codes are **stable across releases** — never renumber an assigned code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingWorkspaceManifest { .. } => "R001",
            Self::DuplicateModule { .. } => "R002",
            Self::CyclicImport { .. } => "R003",
            Self::SelfImport { .. } => "R004",
            Self::DuplicateDeclaration { .. } => "R005",
            Self::UnresolvedImportPath { .. } => "R006",
            Self::ProjectExportViolation { .. } => "R007",
            Self::UnresolvedImportItem { .. } => "R008",
            Self::VisibilityViolation { .. } => "R009",
            Self::UnresolvedIdent { .. } => "R010",
            Self::DuplicateLocal { .. } => "R011",
            Self::UnresolvedQualifiedName { .. } => "R012",
            Self::ForbidViolation { .. } => "R013",
            Self::UnknownStdlibSymbol { .. } => "R014",
            Self::CapabilityDenied { .. } => "R015",
            Self::CapabilityNotAllowed { .. } => "R016",
            Self::StateFieldShadowedByLocal { .. } => "R017",
            // R018 reserved — slot removed; see module-level rustdoc
            Self::UnknownCapabilityKeyword { .. } => "R019",
            Self::CapabilityListOnWrongDecl { .. } => "R020",
            Self::ActorStateMissingDefaultOrInit { .. } => "R021",
            Self::FfiOutsideStdlib { .. } => "R022",
            Self::LegacyRgExtension { .. } => "R023",
            Self::AmbiguousMethodName { .. } => "R024",
            Self::InternalNodeIdCollision { .. } => "R999",
        }
    }

    /// Return the diagnostic severity for this error.
    ///
    /// Per R005 (resolved 2026-04-25), `R017 StateFieldShadowedByLocal`
    /// is **warn-level** — actor-state shadowing by a local is legal but
    /// suspect, and a warning is preferred over a hard error.  All other
    /// variants are hard errors.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        match self {
            Self::StateFieldShadowedByLocal { .. } => Severity::Warning,
            _ => Severity::Error,
        }
    }

    /// Return the source span associated with this error.
    ///
    /// Every `ResolveError` variant carries a span.  For
    /// [`ResolveError::MissingWorkspaceManifest`] and
    /// [`ResolveError::LegacyRgExtension`], which have no meaningful source
    /// location, a zero-length sentinel span at byte 0 is returned.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::MissingWorkspaceManifest { .. } | Self::LegacyRgExtension { .. } => {
                Span::point(0)
            }
            Self::DuplicateModule { second: span, .. }
            | Self::SelfImport { span }
            | Self::UnresolvedImportPath { span, .. }
            | Self::ProjectExportViolation { span, .. }
            | Self::UnresolvedImportItem { span, .. }
            | Self::UnresolvedIdent { span, .. }
            | Self::UnresolvedQualifiedName { span, .. }
            | Self::ForbidViolation {
                import_span: span, ..
            }
            | Self::UnknownStdlibSymbol { span, .. }
            | Self::CapabilityDenied { span, .. }
            | Self::CapabilityNotAllowed { span, .. }
            | Self::UnknownCapabilityKeyword { span, .. }
            | Self::CapabilityListOnWrongDecl { span }
            | Self::ActorStateMissingDefaultOrInit { span, .. }
            | Self::FfiOutsideStdlib { span }
            | Self::AmbiguousMethodName { span, .. }
            | Self::InternalNodeIdCollision { span, .. }
            | Self::DuplicateDeclaration {
                second_span: span, ..
            }
            | Self::DuplicateLocal {
                second_span: span, ..
            }
            | Self::StateFieldShadowedByLocal {
                local_span: span, ..
            }
            | Self::VisibilityViolation { use_span: span, .. }
            | Self::CyclicImport {
                first_edge: span, ..
            } => *span,
        }
    }
}

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to produce a zero-byte span for tests.
    fn sp() -> Span {
        Span::point(0)
    }

    // ── ResolveError code stability tests ──────────────────────────────────────

    #[test]
    fn r001_code_is_stable() {
        let err = ResolveError::MissingWorkspaceManifest { path: "/x".into() };
        assert_eq!(err.code(), "R001");
    }

    #[test]
    fn r002_code_is_stable() {
        let err = ResolveError::DuplicateModule {
            fqn: "acme.domain.User".into(),
            first: sp(),
            second: sp(),
        };
        assert_eq!(err.code(), "R002");
    }

    #[test]
    fn r003_code_is_stable() {
        let err = ResolveError::CyclicImport {
            cycle: vec![crate::ModuleId(0), crate::ModuleId(1)],
            first_edge: sp(),
        };
        assert_eq!(err.code(), "R003");
    }

    #[test]
    fn r006_code_is_stable() {
        let err = ResolveError::UnresolvedImportPath {
            path: "acme.domain.Missing".into(),
            span: sp(),
        };
        assert_eq!(err.code(), "R006");
    }

    #[test]
    fn r010_code_is_stable() {
        let err = ResolveError::UnresolvedIdent {
            name: "missing".into(),
            suggestions: vec![],
            span: sp(),
        };
        assert_eq!(err.code(), "R010");
    }

    #[test]
    fn r013_code_is_stable() {
        let err = ResolveError::ForbidViolation {
            rule_text: "from = \"acme.ui.*\"\nto = \"acme.db.*\"".into(),
            importer_fqn: "acme.ui.Screen".into(),
            target_fqn: "acme.db.Repo".into(),
            import_span: sp(),
            manifest_span: None,
            suggestion: None,
        };
        assert_eq!(err.code(), "R013");
    }

    #[test]
    fn r015_code_is_stable() {
        let err = ResolveError::CapabilityDenied {
            cap: Capability::Io,
            denied_at: "workspace".into(),
            span: sp(),
        };
        assert_eq!(err.code(), "R015");
    }

    #[test]
    fn r023_code_is_stable() {
        let err = ResolveError::LegacyRgExtension {
            path: "src/Main.rg".into(),
        };
        assert_eq!(err.code(), "R023");
    }

    #[test]
    fn r023_severity_is_error() {
        let err = ResolveError::LegacyRgExtension {
            path: "src/Main.rg".into(),
        };
        assert_eq!(err.severity(), Severity::Error);
    }

    #[test]
    fn r999_code_is_stable() {
        let err = ResolveError::InternalNodeIdCollision {
            node_kind: "Ident".into(),
            span: sp(),
        };
        assert_eq!(err.code(), "R999");
    }

    // ── ManifestError code stability tests ─────────────────────────────────────

    #[test]
    fn m001_code_is_stable() {
        let err = ManifestError::TomlParseFailed {
            path: "/x/ridge.toml".into(),
            message: "unexpected eof".into(),
        };
        assert_eq!(err.code(), "M001");
    }

    #[test]
    fn m002_code_is_stable() {
        let err = ManifestError::MissingWorkspaceTable {
            path: "/x/ridge.toml".into(),
        };
        assert_eq!(err.code(), "M002");
    }

    #[test]
    fn m006_code_is_stable() {
        let err = ManifestError::MissingRequiredField {
            table: "project".into(),
            field: "name".into(),
            path: "/x/ridge.toml".into(),
        };
        assert_eq!(err.code(), "M006");
    }

    #[test]
    fn m011_code_is_stable() {
        let err = ManifestError::InvalidCapabilityName {
            name: "teleport".into(),
            path: "/x/ridge.toml".into(),
        };
        assert_eq!(err.code(), "M011");
    }

    // ── Severity ────────────────────────────────────────────────────────────────

    #[test]
    fn r017_severity_is_warning() {
        let err = ResolveError::StateFieldShadowedByLocal {
            name: "count".into(),
            local_span: sp(),
            field_span: sp(),
        };
        assert_eq!(err.severity(), Severity::Warning);
    }

    #[test]
    fn r011_severity_is_error() {
        let err = ResolveError::DuplicateLocal {
            name: "x".into(),
            first_span: sp(),
            second_span: sp(),
        };
        assert_eq!(err.severity(), Severity::Error);
    }

    #[test]
    fn r010_severity_is_error() {
        let err = ResolveError::UnresolvedIdent {
            name: "y".into(),
            suggestions: vec![],
            span: sp(),
        };
        assert_eq!(err.severity(), Severity::Error);
    }

    // ── ManifestError::span always returns None ────────────────────────────────

    #[test]
    fn manifest_error_span_is_always_none() {
        let err = ManifestError::TomlParseFailed {
            path: "/x/ridge.toml".into(),
            message: "err".into(),
        };
        assert!(err.span().is_none());
    }
}
