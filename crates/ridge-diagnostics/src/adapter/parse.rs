//! `Diagnostic::from_parse` adapter for `ridge-parser::ParseError`.

use ridge_ast::Span;
use ridge_parser::ParseError;
use ridge_resolve::{ModuleId, Severity};

use crate::diagnostic::{Diagnostic, SourceId};

impl Diagnostic {
    /// Build a [`Diagnostic`] from a parse error.
    ///
    /// `mid` is accepted for API symmetry with `from_lex`; `source_id`
    /// identifies the source file in the [`SourceCache`](crate::SourceCache).
    #[must_use]
    pub fn from_parse(mid: ModuleId, e: &ParseError, source_id: SourceId) -> Self {
        let _ = mid;
        Self::new(
            e.code(),
            Severity::Error,
            span_of(e),
            e.to_string(),
            source_id,
        )
    }
}

const fn span_of(e: &ParseError) -> Span {
    e.span()
}
