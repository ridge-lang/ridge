//! Diagnostic rendering primitives for the Ridge compiler.
//!
//! This crate exposes the [`HasErrorCode`] trait, which every diagnostic type
//! (`T###`, `R###`, `M###`) implements, plus a structured [`Diagnostic`] value
//! type, a [`SourceCache`] trait, and a [`render_with_ariadne`] renderer.
//!
//! # Rendering pipeline
//!
//! 1. `ridge-driver` builds a `Vec<Diagnostic>` from its error enums using the
//!    per-error adapters in [`adapter`].
//! 2. `ridge-driver` also builds a [`crate::diagnostic::SourceCache`] implementation
//!    (`WorkspaceSourceCache`) that maps each [`SourceId`] to source text.
//! 3. The CLI passes both to [`render_with_ariadne`], which writes human-readable
//!    diagnostics (with source-line context and a caret) to stderr.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_ast::Span;

pub mod adapter;
pub mod diagnostic;
pub mod render;

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use diagnostic::{
    Diagnostic, DiagnosticNote, NoteSeverity, RenderError, SourceCache, SourceId,
};

/// Re-export `Severity` from `ridge_resolve` — one canonical type workspace-wide.
///
/// Per FROZEN-03 (approved 2026-05-01): a fork would create silent drift risk
/// when `ridge_resolve::Severity` adds new variants in the future.
pub use ridge_resolve::Severity;

pub use render::render_with_ariadne;

// ── HasErrorCode ──────────────────────────────────────────────────────────────

/// Uniform interface for all Ridge compiler diagnostics.
///
/// Every diagnostic type — [`ridge_resolve::ResolveError`],
/// `ridge_typecheck::TypeError`, etc. — implements this trait so that
/// `ridge-diagnostics` can render them uniformly without importing every
/// upstream error type.
///
pub trait HasErrorCode {
    /// Returns the stable error code string, e.g. `"T001"`, `"R003"`, `"M011"`.
    fn code(&self) -> &'static str;

    /// Returns the primary source span associated with this diagnostic.
    ///
    /// For manifest errors that have no source location, implementors should
    /// return `Span::point(0)` as a sentinel.
    fn span(&self) -> Span;

    /// Returns the severity of this diagnostic.
    fn severity(&self) -> ridge_resolve::Severity;
}

// ── HasErrorCode on ResolveError ──────────────────────────────────────────────

impl HasErrorCode for ridge_resolve::ResolveError {
    fn code(&self) -> &'static str {
        self.code()
    }

    fn span(&self) -> Span {
        self.span()
    }

    fn severity(&self) -> ridge_resolve::Severity {
        self.severity()
    }
}

// ── HasErrorCode on ManifestError ─────────────────────────────────────────────

impl HasErrorCode for ridge_resolve::ManifestError {
    fn code(&self) -> &'static str {
        self.code()
    }

    /// Manifest errors carry no source span; returns `Span::point(0)` as a
    /// sentinel value.
    fn span(&self) -> Span {
        Span::point(0)
    }

    /// Manifest errors are always hard errors.
    fn severity(&self) -> ridge_resolve::Severity {
        ridge_resolve::Severity::Error
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_resolve::{ManifestError, ResolveError};

    #[test]
    fn smoke() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn resolve_error_has_error_code() {
        let err = ResolveError::MissingWorkspaceManifest { path: "/x".into() };
        assert_eq!(<ResolveError as HasErrorCode>::code(&err), "R001");
        assert_eq!(
            <ResolveError as HasErrorCode>::severity(&err),
            Severity::Error
        );
    }

    #[test]
    fn manifest_error_span_is_sentinel() {
        let err = ManifestError::TomlParseFailed {
            path: "/x/ridge.toml".into(),
            message: "unexpected eof".into(),
        };
        let s = <ManifestError as HasErrorCode>::span(&err);
        assert_eq!(s, Span::point(0));
    }
}
