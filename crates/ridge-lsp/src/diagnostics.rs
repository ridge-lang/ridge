//! Conversion from `ridge_diagnostics::Diagnostic` to `tower_lsp::lsp_types::Diagnostic`.
//!
//! The LSP `Diagnostic` is assembled from the Ridge `Diagnostic` fields:
//! - `severity` → `DiagnosticSeverity`
//! - `code` → `NumberOrString`
//! - `notes` → `DiagnosticRelatedInformation`
//! - `primary_span` → `Range` (via `LineMap` byte-offset conversion)

use std::path::Path;

use ridge_diagnostics::{Diagnostic, NoteSeverity, Severity};
use ridge_lexer::Span;
use tower_lsp::lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Position, Range, Url,
};

use crate::span_recovery::resolve_span_to_lsp;

// ── Source-id → URI resolution ────────────────────────────────────────────────

/// The static fallback URI for diagnostics with no resolvable file path.
fn unknown_uri() -> Url {
    // `file:///unknown` is a compile-time constant; `Url::parse` cannot fail.
    #[allow(clippy::expect_used)]
    Url::parse("file:///unknown").expect("static URL is valid")
}

/// Resolve the document URI a diagnostic belongs to from its `source_id`.
///
/// `source_id` is the workspace-relative, forward-slash path that
/// `WorkspaceSourceCache` keys every module by (e.g. `app/src/main.ridge`).
/// Joining it onto the absolute workspace root yields the same URI the editor
/// opened the file with, so diagnostics land on the right document regardless
/// of whether that document is currently open.
///
/// Diagnostics without a real source location are keyed `<unknown>` (or
/// `<module N>` for an unmapped module id); those anchor to the workspace root.
#[must_use]
pub fn source_id_to_uri(workspace_root: &Path, source_id: &str) -> Url {
    if source_id == "<unknown>" || source_id.starts_with("<module ") {
        return Url::from_file_path(workspace_root).unwrap_or_else(|()| unknown_uri());
    }
    Url::from_file_path(workspace_root.join(source_id)).unwrap_or_else(|()| unknown_uri())
}

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

/// Convert a byte-offset `Span` to an LSP `Range` using a `LineIndex`.
///
/// LSP positions are 0-indexed line / character, where `character` counts
/// UTF-16 code units. `LineIndex::byte_to_utf16` performs that conversion
/// exactly, so columns are correct on lines containing non-ASCII text.
#[must_use]
pub fn span_to_range(span: Span, src: &str) -> Range {
    use ridge_lexer::LineIndex;
    let li = LineIndex::new(src);
    let (start_line, start_char) = li.byte_to_utf16(span.start);
    let (end_line, end_char) = li.byte_to_utf16(span.end);
    Range {
        start: Position {
            line: start_line,
            character: start_char,
        },
        end: Position {
            line: end_line,
            character: end_char,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_to_range_uses_utf16_columns() {
        // "café x": bytes c,a,f (1 each), é (2), space (1), x (1) → 'x' at byte 6.
        // In UTF-16 the é is a single unit, so 'x' is column 5, not 6.
        let src = "café x";
        let range = span_to_range(Span::new(6, 7), src);
        assert_eq!(range.start.line, 0);
        assert_eq!(
            range.start.character, 5,
            "x must be UTF-16 column 5, not byte column 6"
        );
        assert_eq!(range.end.character, 6);
    }

    #[test]
    fn span_to_range_ascii_unchanged() {
        let src = "fn foo = 42";
        let range = span_to_range(Span::new(3, 6), src);
        assert_eq!(range.start.character, 3);
        assert_eq!(range.end.character, 6);
    }
}
