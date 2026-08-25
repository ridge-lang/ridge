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
            ResolveError::ModuleAsValue { name, member, .. } => {
                diag.push_note(DiagnosticNote {
                    span: primary_span,
                    message: module_as_value_note(name, member.as_deref()),
                    severity: NoteSeverity::Help,
                });
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

/// The help line an `R030` carries, which differs by how the alias is spelled
/// and by whether the use site said what it wanted from the module.
///
/// A lower-case alias — what the bare form of `import` binds, since every
/// standard-library path ends in a lower-case segment — cannot prefix a
/// qualified name at all, so telling the reader to write one would be advice
/// they cannot take. For that spelling the fix is at the import, and the note
/// says so.
///
/// `member` is the name reached off the alias (`length` in `list.length`).
/// Naming it is what makes the line a fix rather than a description.
fn module_as_value_note(name: &str, member: Option<&str>) -> String {
    let reach = member.unwrap_or("someExport");
    let first = name.chars().next();
    if first.is_some_and(char::is_uppercase) {
        return format!("reach what the module exports with a qualified name: `{name}.{reach}`");
    }
    let upper: String = first.map_or_else(
        || name.to_owned(),
        |c| c.to_uppercase().chain(name.chars().skip(1)).collect(),
    );
    format!(
        "a qualified name begins with an upper-case name, so `{name}` can never prefix one — import the module `as {upper}` and write `{upper}.{reach}`"
    )
}

/// Adapt a `ResolveError::severity` to our `Severity` type (identity, same enum).
#[must_use]
pub const fn adapt_severity(s: ridge_resolve::Severity) -> Severity {
    s
}
