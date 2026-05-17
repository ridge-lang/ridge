//! `From<(ModuleId, &LexError, SourceId)>` adapter for `ridge-lexer::LexError`.

use ridge_ast::Span;
use ridge_lexer::LexError;
use ridge_resolve::{ModuleId, Severity};

use crate::diagnostic::{Diagnostic, SourceId};

impl Diagnostic {
    /// Build a [`Diagnostic`] from a lexer error.
    ///
    /// The `mid` parameter is used only for the `primary_message` context
    /// (e.g. "module 0"); `source_id` identifies the source file in the cache.
    #[must_use]
    pub fn from_lex(mid: ModuleId, e: &LexError, source_id: SourceId) -> Self {
        let _ = mid; // used contextually in primary_message if needed
        Self::new(
            e.code(),
            Severity::Error,
            span_of(e),
            e.to_string(),
            source_id,
        )
    }
}

const fn span_of(e: &LexError) -> Span {
    e.span()
}
