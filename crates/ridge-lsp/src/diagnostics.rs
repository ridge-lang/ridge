//! Conversion from `ridge_diagnostics::Diagnostic` to `tower_lsp::lsp_types::Diagnostic`.
//!
//! The LSP `Diagnostic` is assembled from the Ridge `Diagnostic` fields:
//! - `severity` → `DiagnosticSeverity`
//! - `code` → `NumberOrString`
//! - `notes` → `DiagnosticRelatedInformation`
//! - `primary_span` → `Range` (via `LineMap` byte-offset conversion)

use ridge_diagnostics::{Diagnostic, NoteSeverity, Severity};
use ridge_lexer::Span;
use tower_lsp::lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Position, Range, Url,
};

use crate::span_recovery::resolve_span_to_lsp;

// ── Severity mapping ──────────────────────────────────────────────────────────

/// Map a Ridge `Severity` to an LSP `DiagnosticSeverity`.
#[must_use]
pub const fn lsp_severity(s: Severity) -> DiagnosticSeverity {
    match s {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        // Severity is #[non_exhaustive]; future variants default to Information.
        _ => DiagnosticSeverity::INFORMATION,
    }
}

/// Map a Ridge `NoteSeverity` to an LSP `DiagnosticSeverity` (for related info).
///
/// `Help` and `Hint` both map to `HINT`; keeping the arms explicit documents
/// the intent better than collapsing into a single `|`-pattern.
#[must_use]
#[allow(clippy::match_same_arms)]
pub const fn lsp_note_severity(s: NoteSeverity) -> DiagnosticSeverity {
    match s {
        NoteSeverity::Help => DiagnosticSeverity::HINT,
        NoteSeverity::Note => DiagnosticSeverity::INFORMATION,
        NoteSeverity::Hint => DiagnosticSeverity::HINT,
    }
}

// ── Range / Position helpers ──────────────────────────────────────────────────

/// Convert a byte-offset `Span` to an LSP `Range` using a `LineMap`.
///
/// LSP positions are 0-indexed line / character (UTF-16 code unit offset).
/// We use UTF-8 byte column here — good enough for ASCII-dominant source
/// files; a 0.2.0 follow-up can switch to `tower_lsp_utf16` if needed.
#[must_use]
pub fn span_to_range(span: Span, src: &str) -> Range {
    use ridge_lexer::LineMap;
    let lm = LineMap::new(src);
    let (start_line, start_col) = lm.line_col(span.start);
    let (end_line, end_col) = lm.line_col(span.end);
    // LSP is 0-indexed; LineMap returns 1-indexed.
    Range {
        start: Position {
            line: start_line.saturating_sub(1),
            character: start_col.saturating_sub(1),
        },
        end: Position {
            line: end_line.saturating_sub(1),
            character: end_col.saturating_sub(1),
        },
    }
}

/// Return the top-left position of a file (line 0, character 0).
#[must_use]
pub const fn file_start_range() -> Range {
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: 0,
            character: 0,
        },
    }
}

// ── Diagnostic conversion ─────────────────────────────────────────────────────

/// Convert a Ridge `Diagnostic` to an LSP `Diagnostic`.
///
/// `file_uri`   — the document URI the diagnostic belongs to.
/// `src`        — raw source text of the file (used for span → line/col).
///
/// If `src` is `None`, the diagnostic is anchored to the file's first line.
#[must_use]
pub fn to_lsp_diagnostic(diag: &Diagnostic, file_uri: &Url, src: Option<&str>) -> LspDiagnostic {
    let range = src.map_or_else(file_start_range, |text| {
        resolve_span_to_lsp(diag.primary_span, text)
    });

    let related: Vec<DiagnosticRelatedInformation> = diag
        .notes
        .iter()
        .map(|note| {
            let note_range =
                src.map_or_else(file_start_range, |text| span_to_range(note.span, text));
            DiagnosticRelatedInformation {
                location: Location {
                    uri: file_uri.clone(),
                    range: note_range,
                },
                message: note.message.clone(),
            }
        })
        .collect();

    LspDiagnostic {
        range,
        severity: Some(lsp_severity(diag.severity)),
        code: Some(NumberOrString::String(diag.code.to_owned())),
        code_description: None,
        source: Some("ridge".to_owned()),
        message: diag.primary_message.clone(),
        related_information: if related.is_empty() {
            None
        } else {
            Some(related)
        },
        tags: None,
        data: None,
    }
}
