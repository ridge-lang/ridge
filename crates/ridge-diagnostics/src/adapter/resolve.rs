//! `Diagnostic::from_resolve` adapter for `ridge-resolve::ResolveError`.

use ridge_resolve::{ResolveError, Severity};

use crate::diagnostic::{Diagnostic, DiagnosticNote, NoteSeverity, SourceId};

impl Diagnostic {
    /// Build a [`Diagnostic`] from a resolve error.
    ///
    /// Secondary spans (e.g. "first declared here" for `R002`, `R005`) are
    /// surfaced as secondary [`DiagnosticNote`]s.
    #[must_use]
    pub fn from_resolve(e: &ResolveError, source_id: SourceId) -> Self {
        let code = e.code();
        let severity = e.severity();
        let primary_span = e.span();
        let message = e.to_string();

        let mut diag = Self::new(code, severity, primary_span, message, source_id);

        // Surface secondary spans for variants that carry them.
        match e {
            ResolveError::DuplicateModule { first, second, .. } => {
                // primary is at `second`; note is at `first`
                diag.notes.push(DiagnosticNote {
                    span: *first,
                    message: "first declared here".to_owned(),
                    severity: NoteSeverity::Note,
                });
                let _ = second; // primary_span already set to *second
            }
            ResolveError::DuplicateDeclaration {
                first_span,
                second_span,
                ..
            } => {
                diag.notes.push(DiagnosticNote {
                    span: *first_span,
                    message: "first declaration".to_owned(),
                    severity: NoteSeverity::Note,
                });
                let _ = second_span;
            }
            ResolveError::DuplicateLocal {
                first_span,
                second_span,
                ..
            } => {
                diag.notes.push(DiagnosticNote {
                    span: *first_span,
                    message: "first binding".to_owned(),
                    severity: NoteSeverity::Note,
                });
                let _ = second_span;
            }
            ResolveError::VisibilityViolation { defined_at, .. } => {
                diag.notes.push(DiagnosticNote {
                    span: *defined_at,
                    message: "defined here (with restricted visibility)".to_owned(),
                    severity: NoteSeverity::Note,
                });
            }
            ResolveError::UnresolvedIdent { suggestions, .. }
            | ResolveError::UnresolvedImportItem { suggestions, .. }
            | ResolveError::UnresolvedQualifiedName { suggestions, .. }
            | ResolveError::UnknownStdlibSymbol { suggestions, .. } => {
                for sug in suggestions {
                    diag.notes.push(DiagnosticNote {
                        span: primary_span,
                        message: format!("did you mean `{sug}`?"),
                        severity: NoteSeverity::Help,
                    });
                }
            }
            ResolveError::ForbidViolation {
                manifest_span: Some(mspan),
                ..
            } => {
                diag.notes.push(DiagnosticNote {
                    span: *mspan,
                    message: "rule defined here".to_owned(),
                    severity: NoteSeverity::Note,
                });
            }
            ResolveError::StateFieldShadowedByLocal { field_span, .. } => {
                diag.notes.push(DiagnosticNote {
                    span: *field_span,
                    message: "state field declared here".to_owned(),
                    severity: NoteSeverity::Note,
                });
            }
            _ => {}
        }

        diag
    }
}

/// Adapt a `ResolveError::severity` to our `Severity` type (identity, same enum).
#[must_use]
pub const fn adapt_severity(s: ridge_resolve::Severity) -> Severity {
    s
}
