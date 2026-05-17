//! `Diagnostic::from_manifest` adapter for `ridge-resolve::ManifestError`.

use ridge_ast::Span;
use ridge_resolve::{ManifestError, Severity};

use crate::diagnostic::{Diagnostic, SourceId};

impl Diagnostic {
    /// Build a [`Diagnostic`] from a manifest error.
    ///
    /// Manifest errors carry no source span; a sentinel `Span::point(0)` is
    /// used.  The renderer's source-cache-miss path produces a context-less
    /// render with no underline — which is correct for manifest errors since
    /// they reference `ridge.toml` rather than `.rg` source files.
    #[must_use]
    pub fn from_manifest(e: &ManifestError, source_id: SourceId) -> Self {
        Self::new(
            e.code(),
            Severity::Error,
            Span::point(0),
            e.to_string(),
            source_id,
        )
    }
}
