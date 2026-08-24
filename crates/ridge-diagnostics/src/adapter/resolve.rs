//! `Diagnostic::from_resolve` adapter for `ridge-resolve::ResolveError`.

use ridge_resolve::{ExportingImport, ResolveError, Severity};

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
                diag.push_note(DiagnosticNote {
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
                diag.push_note(DiagnosticNote {
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
                diag.push_note(DiagnosticNote {
                    span: *first_span,
                    message: "first binding".to_owned(),
                    severity: NoteSeverity::Note,
                });
                let _ = second_span;
            }
            ResolveError::VisibilityViolation { defined_at, .. } => {
                diag.push_note(DiagnosticNote {
                    span: *defined_at,
                    message: "defined here (with restricted visibility)".to_owned(),
                    severity: NoteSeverity::Note,
                });
            }
            ResolveError::UnresolvedIdent {
                name,
                suggestions,
                importable,
                ..
            } => {
                if let Some(message) = unresolved_ident_note(name, suggestions, importable) {
                    diag.push_note(DiagnosticNote {
                        span: primary_span,
                        message,
                        severity: NoteSeverity::Help,
                    });
                }
            }
            ResolveError::UnresolvedImportItem { suggestions, .. }
            | ResolveError::UnresolvedQualifiedName { suggestions, .. }
            | ResolveError::UnknownStdlibSymbol { suggestions, .. } => {
                if let Some(message) = crate::diagnostic::did_you_mean(suggestions) {
                    diag.push_note(DiagnosticNote {
                        span: primary_span,
                        message,
                        severity: NoteSeverity::Help,
                    });
                }
            }
            ResolveError::ForbidViolation {
                manifest_span: Some(mspan),
                ..
            } => {
                diag.push_note(DiagnosticNote {
                    span: *mspan,
                    message: "rule defined here".to_owned(),
                    severity: NoteSeverity::Note,
                });
            }
            ResolveError::StateFieldShadowedByLocal { field_span, .. } => {
                diag.push_note(DiagnosticNote {
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

/// The one help line an `R010` carries.
///
/// A module that exports the exact name is a fact; a Levenshtein neighbour is a
/// guess. The walker does not compute the guess once it holds the fact, so the
/// two are mutually exclusive by construction and this reads as one note either
/// way — never two carets drawn at the same span.
fn unresolved_ident_note(
    name: &str,
    suggestions: &[String],
    importable: &[ExportingImport],
) -> Option<String> {
    let sources: Vec<(String, Option<String>)> = importable
        .iter()
        .map(|i| {
            (
                i.module.clone(),
                i.insertion.bare_alias().map(str::to_owned),
            )
        })
        .collect();
    crate::diagnostic::exported_by(name, &sources)
        .or_else(|| crate::diagnostic::did_you_mean(suggestions))
}

/// Adapt a `ResolveError::severity` to our `Severity` type (identity, same enum).
#[must_use]
pub const fn adapt_severity(s: ridge_resolve::Severity) -> Severity {
    s
}
