//! Identifier primitive used throughout the AST.

use crate::Span;

/// A raw lexeme identifier, case-preserved.
///
/// Used for every named entity in the AST (variable names, type names,
/// constructor names, module segments, etc.).  Classification by case is
/// handled at the parser layer; the AST layer stores only the raw text and
/// source location.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ident {
    /// Raw lexeme; case is preserved exactly as written in the source.
    pub text: String,
    /// Source location of this identifier.
    pub span: Span,
}

impl Ident {
    /// Construct a new `Ident`.
    #[must_use]
    pub fn new(text: impl Into<String>, span: Span) -> Self {
        Self {
            text: text.into(),
            span,
        }
    }

    /// Returns `true` if this identifier begins with a lowercase ASCII letter
    /// or an underscore (i.e. a value-level or type-variable name).
    #[must_use]
    pub fn is_lower(&self) -> bool {
        self.text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
    }

    /// Returns `true` if this identifier begins with an uppercase ASCII letter
    /// (i.e. a type or constructor name).
    #[must_use]
    pub fn is_upper(&self) -> bool {
        self.text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
    }

    /// Returns `true` if this identifier begins with `_` followed by at least
    /// one more character (i.e. a private/internal name like `_helper`).
    ///
    /// A bare `_` wildcard returns `false`; it is a wildcard pattern, not a
    /// private name.
    #[must_use]
    pub fn is_priv(&self) -> bool {
        self.text.starts_with('_') && self.text.len() > 1
    }
}
