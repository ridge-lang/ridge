//! Core diagnostic value types for the Ridge compiler.
//!
//! This module defines the owned, lifetime-free value type that every
//! diagnostic rendering and LSP adapter pipeline consumes.  Construction
//! happens exclusively through the per-error-enum `From<&XError>` adapters
//! in `crate::adapter`.

use std::sync::Arc;

use ridge_ast::Span;
pub use ridge_resolve::Severity;

// ── SourceId ──────────────────────────────────────────────────────────────────

/// Opaque source identifier.
///
/// The [`SourceCache`] resolves a `SourceId` to source text and a display
/// name.  In `ridge-driver`, a `SourceId` wraps the workspace-relative path
/// string; in `ridge-lsp`, it wraps an LSP `Url` string.  The renderer never
/// inspects the inside of a `SourceId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(pub(crate) Arc<str>);

impl SourceId {
    /// Construct a new `SourceId` from any string-like value.
    pub fn new(name: impl Into<String>) -> Self {
        Self(Arc::from(name.into()))
    }

    /// Return the inner identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── SourceCache ───────────────────────────────────────────────────────────────

/// Cache of source-text for diagnostic rendering.
///
/// Implemented by `ridge-driver` (file-backed) and `ridge-lsp` (in-memory
/// edit-buffer-backed).  The renderer never reads files itself — it asks the
/// cache.
pub trait SourceCache {
    /// Return the source text for the given identifier.
    ///
    /// Returns `None` if the source is unavailable; the renderer falls back
    /// to a context-less render (code prefix + message, no underline).
    fn fetch(&self, id: &SourceId) -> Option<&str>;

    /// Return a human-readable display name for `id`.
    ///
    /// Used in the `--> path:line:col` header line.  Defaults to
    /// [`SourceId::as_str`]; implementers may override to produce shorter or
    /// prettier paths.
    fn display_name<'a>(&'a self, id: &'a SourceId) -> &'a str {
        id.as_str()
    }
}

// ── NoteSeverity ──────────────────────────────────────────────────────────────

/// Severity of a secondary diagnostic note.
///
/// Distinct from top-level [`Severity`].  Maps to ariadne's colour palette:
/// `Help` → green, `Note` → blue, `Hint` → yellow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteSeverity {
    /// A helpful suggestion for how to fix the error.
    Help,
    /// An informational note about the error.
    Note,
    /// A light hint about the context.
    Hint,
}

// ── DiagnosticNote ────────────────────────────────────────────────────────────

/// A secondary annotation in a diagnostic.
///
/// Used for secondary spans — e.g. "first declared here" or
/// "did you mean `foo`?" — rendered alongside the primary span.
#[derive(Debug, Clone)]
pub struct DiagnosticNote {
    /// Source span of this note.
    pub span: Span,
    /// Human-readable message for this note.
    pub message: String,
    /// Severity / colour class of this note.
    pub severity: NoteSeverity,
}

// ── Diagnostic ────────────────────────────────────────────────────────────────

/// A structured diagnostic suitable for human or machine rendering.
///
/// Owned, lifetime-free, `Clone`.  The primary construction path is
/// `From<&XError> for Diagnostic` adapters in [`crate::adapter`].
///
/// # LSP forward-compat
///
/// Every field needed by an LSP `Diagnostic` is present.  T11 (`ridge-lsp`)
/// provides the `From<Diagnostic> for lsp_types::Diagnostic` adapter using
/// the existing `LineMap` byte-offset-to-line-col conversion.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Diagnostic {
    /// Stable error code, e.g. `"T015"`, `"R013"`, `"P001"`, `"E007"`.
    pub code: &'static str,
    /// Severity (`Error` / `Warning`).
    pub severity: Severity,
    /// Primary source span — the location ariadne underlines with the caret.
    pub primary_span: Span,
    /// Short, single-line message rendered next to the primary caret.
    pub primary_message: String,
    /// Source identifier — opaque key the [`SourceCache`] uses to retrieve text.
    pub source_id: SourceId,
    /// Secondary annotations.
    ///
    /// Each note carries its own [`Span`], message, and [`NoteSeverity`].
    /// For example, `R005 DuplicateDeclaration` produces two notes:
    /// "first defined here" and "redefined here".
    pub notes: Vec<DiagnosticNote>,
}

impl Diagnostic {
    /// Construct a `Diagnostic` with no secondary notes.
    #[must_use]
    pub fn new(
        code: &'static str,
        severity: Severity,
        primary_span: Span,
        primary_message: impl Into<String>,
        source_id: SourceId,
    ) -> Self {
        Self {
            code,
            severity,
            primary_span,
            primary_message: primary_message.into(),
            source_id,
            notes: Vec::new(),
        }
    }

    /// Add a secondary note to this diagnostic.
    #[must_use]
    pub fn with_note(mut self, span: Span, message: impl Into<String>, sev: NoteSeverity) -> Self {
        self.notes.push(DiagnosticNote {
            span,
            message: message.into(),
            severity: sev,
        });
        self
    }
}

// ── RenderError ───────────────────────────────────────────────────────────────

/// Error returned by [`super::render_with_ariadne`].
///
/// Currently only wraps `std::io::Error`.  Source-cache misses are not errors
/// — the diagnostic is rendered context-lessly.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The underlying writer returned an I/O error.
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
}
