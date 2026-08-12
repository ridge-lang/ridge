//! `From<&XError> for Diagnostic` adapters for `ridge-typecheck` and
//! `ridge-codegen-erl` error types.
//!
//! These adapters live in `ridge-driver` (not `ridge-diagnostics`) because
//! both `ridge-typecheck` and `ridge-codegen-erl` depend on `ridge-diagnostics`,
//! making `ridge-diagnostics → ridge-typecheck/ridge-codegen-erl` a dep cycle.
//! `ridge-driver` depends on all four crates and is the natural home.

use ridge_codegen_erl::CodegenError;
use ridge_diagnostics::{Diagnostic, DiagnosticNote, NoteSeverity, SourceId};
use ridge_ir::Span;
use ridge_lower::error::LowerError;
use ridge_resolve::Severity;
use ridge_typecheck::TypeError;

// ── TypeError → Diagnostic ────────────────────────────────────────────────────

/// Build a [`Diagnostic`] from a [`TypeError`].
///
/// Suggestions on `T005 UnknownField` and `T015 UnknownActorHandler` are
/// surfaced as `Help`-level notes.
#[must_use]
pub fn diag_from_typecheck(e: &TypeError, source_id: SourceId) -> Diagnostic {
    use ridge_diagnostics::HasErrorCode;

    let code = e.code();
    let severity = e.severity();
    let primary_span = e.span();
    let message = e.to_string();

    let mut diag = Diagnostic::new(code, severity, primary_span, message, source_id);

    // Surface per-variant secondary notes.
    match e {
        TypeError::UnknownField { suggestions, .. }
        | TypeError::UnknownActorHandler { suggestions, .. } => {
            if let Some(message) = ridge_diagnostics::diagnostic::did_you_mean(suggestions) {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message,
                    severity: NoteSeverity::Help,
                });
            }
        }
        TypeError::NonExhaustiveMatch {
            witnesses,
            total_missing,
            ..
        } => {
            for w in witnesses {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!("missing pattern: {w}"),
                    severity: NoteSeverity::Help,
                });
            }
            if *total_missing > witnesses.len() {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!(
                        "... and {} more missing pattern(s)",
                        total_missing - witnesses.len()
                    ),
                    severity: NoteSeverity::Note,
                });
            }
        }
        TypeError::InsertShapeFullEntity {
            companion, omitted, ..
        } if !omitted.is_empty() => {
            let cols = omitted
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let (plural, them) = if omitted.len() == 1 {
                ("", "it")
            } else {
                ("s", "them")
            };
            diag.notes.push(DiagnosticNote {
                span: primary_span,
                message: format!(
                    "`{companion}` drops the database-generated column{plural} {cols}; build a `{companion}` and leave {them} to the database"
                ),
                severity: NoteSeverity::Help,
            });
        }
        _ => {}
    }

    diag
}

// ── LowerError → Diagnostic ───────────────────────────────────────────────────

/// Build a [`Diagnostic`] from a [`LowerError`].
///
/// The seventh adapter, and the one that was missing. Phase 5 has always built
/// diagnostics and nothing ever read them, so an integer literal too large for
/// `Int` became a silent zero, and the channel the lowering reports its own
/// invariant violations on reached nobody.
///
/// It lives here rather than in `ridge-diagnostics` for the same reason the
/// typecheck and codegen adapters do: `ridge-lower` would otherwise have to
/// depend on `ridge-diagnostics` for the `HasErrorCode` trait, and
/// `ridge-driver` is the crate that already depends on both.
#[must_use]
pub fn diag_from_lower(e: &LowerError, source_id: SourceId) -> Diagnostic {
    Diagnostic::new(e.code(), e.severity(), e.span(), e.to_string(), source_id)
}

// ── CodegenError → Diagnostic ─────────────────────────────────────────────────

/// Build a [`Diagnostic`] from a [`CodegenError`].
///
/// Toolchain-oriented variants (`E003`–`E006`, `E101`, `E102`) carry no source
/// span; they use a sentinel span and render context-lessly.  Span-bearing
/// variants (`E001`, `E002`, `E007`, `E008`) anchor to their source location.
#[must_use]
pub fn diag_from_codegen(e: &CodegenError, source_id: SourceId) -> Diagnostic {
    let code = e.code();
    let primary_span = e.span().unwrap_or_else(|| Span::point(0));
    let severity = Severity::Error;

    let mut diag = Diagnostic::new(code, severity, primary_span, e.to_string(), source_id);

    // For E004/E102, surface erlc stderr as a note.
    match e {
        CodegenError::ErlcRejectedInput { stderr, .. } => {
            if !stderr.is_empty() {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!("erlc output:\n{stderr}"),
                    severity: NoteSeverity::Note,
                });
            }
        }
        CodegenError::ErlcUnexpectedOutput { stderr, .. } => {
            if !stderr.is_empty() {
                diag.notes.push(DiagnosticNote {
                    span: primary_span,
                    message: format!("erlc stderr:\n{stderr}"),
                    severity: NoteSeverity::Note,
                });
            }
        }
        _ => {}
    }

    diag
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The adapter used to re-write every `E###` message by hand, one arm per
    /// variant, with the code typed into the format string.  Two declarations
    /// of one code drift; this asserts the surviving one is the owner's.
    #[test]
    fn the_message_comes_from_the_error_itself() {
        let e = CodegenError::StdlibBridgeMissing {
            module: "std.io".into(),
            name: "println".into(),
            span: Span::point(0),
        };
        let diag = diag_from_codegen(&e, SourceId::new("std.io"));
        assert_eq!(diag.code, e.code());
        assert_eq!(diag.primary_message, e.to_string());
    }

    /// The renderer prints `[E002] <headline>`, so the code has to come out of
    /// the headline.  It does that by stripping `"{code}: "` — which only
    /// matches while the message opens with the code the diagnostic carries.
    #[test]
    fn the_headline_does_not_repeat_the_code() {
        let e = CodegenError::ErlcVersionTooOld {
            found: "OTP 24".into(),
            minimum: "OTP 26".into(),
        };
        let diag = diag_from_codegen(&e, SourceId::new("<toolchain>"));
        let headline = diag.message_parts().headline;
        assert!(
            !headline.contains("E101"),
            "the code survived the strip: {headline}"
        );
        assert!(headline.starts_with("erlc version"), "{headline}");
    }
}
