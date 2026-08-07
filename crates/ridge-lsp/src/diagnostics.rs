//! Conversion from `ridge_diagnostics::Diagnostic` to `tower_lsp::lsp_types::Diagnostic`.
//!
//! The LSP `Diagnostic` is assembled from the Ridge `Diagnostic` fields:
//! - `severity` → `DiagnosticSeverity`
//! - `code` → `NumberOrString`, linked to its entry in the published index
//! - `notes` → `DiagnosticRelatedInformation`
//! - `primary_span` → `Range` (via `LineMap` byte-offset conversion)

use std::path::Path;

use ridge_diagnostics::{lookup_code, Diagnostic, NoteSeverity, Severity, INDEX_URL};
use ridge_lexer::Span;
use tower_lsp::lsp_types::{
    CodeDescription, Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity,
    Location, NumberOrString, Position, Range, Url,
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

/// A normalization-stable key for a `file:` URI.
///
/// Editors disagree on how to spell the same path in a URI. On Windows in
/// particular, VS Code sends a lower-case drive letter with a percent-encoded
/// colon (`file:///c%3A/dir/x.ridge`), whereas a URI the server derives from an
/// on-disk path carries an upper-case drive and a literal colon
/// (`file:///C:/dir/x.ridge`). Comparing those two `Url`s directly then misses,
/// so every position query (hover, definition, ...) returns nothing even though
/// pushed diagnostics — which the client re-normalizes on its side — still land.
///
/// Folding both forms to the decoded filesystem path, lower-cased on Windows
/// (whose filesystem is case-insensitive), yields a key that matches whichever
/// spelling arrives. On case-sensitive platforms the path is returned as-is.
#[must_use]
pub fn uri_key(uri: &Url) -> String {
    match uri.to_file_path() {
        Ok(path) => {
            let s = path.to_string_lossy();
            if cfg!(windows) {
                s.to_lowercase()
            } else {
                s.into_owned()
            }
        }
        Err(()) => uri.as_str().to_owned(),
    }
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

/// Build the message an editor shows for `diag`.
///
/// Editors render the whole body, so unlike the terminal we keep it — but the
/// `"{code}: "` prefix goes, because `code` travels as its own LSP field and
/// would otherwise show up twice in the problems panel.
fn lsp_message(diag: &Diagnostic) -> String {
    let parts = diag.message_parts();
    let mut message = parts.headline.to_owned();
    if let Some(note) = &parts.note {
        message.push('\n');
        message.push_str(note);
    }
    for help in &parts.helps {
        message.push_str("\nhelp: ");
        message.push_str(help);
    }
    message
}

/// Where the editor should send someone who clicks the code.
///
/// An editor renders `code` as a link only when this is set, so filling it in
/// is the difference between a code being a label and being something you can
/// follow. A code the registry does not know gets no link rather than one that
/// leads to an anchor that isn't there — the census makes that unreachable
/// today, and this is not the place to find out that it stopped being true.
fn code_description(code: &str) -> Option<CodeDescription> {
    lookup_code(code)?;
    let href = Url::parse(&format!("{INDEX_URL}#{}", code.to_lowercase())).ok()?;
    Some(CodeDescription { href })
}

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
        code_description: code_description(diag.code),
        source: Some("ridge".to_owned()),
        message: lsp_message(diag),
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

    /// A known code links to its own anchor on the published index.
    ///
    /// The anchor is lower-cased because that is what a Markdown heading
    /// slugifies to; getting it wrong lands the reader at the top of a page
    /// listing every code there is, which reads the same as a broken link.
    #[test]
    fn a_known_code_links_to_its_entry() {
        let described = code_description("T001");
        assert!(described.is_some(), "T001 is in the registry");
        let Some(described) = described else { return };
        assert!(
            described.href.as_str().ends_with("/diagnostics.md#t001"),
            "got {}",
            described.href
        );
    }

    /// A code with no entry gets no link, rather than one that goes nowhere.
    #[test]
    fn an_unknown_code_gets_no_link() {
        assert!(code_description("Q001").is_none());
    }

    #[test]
    fn uri_key_is_stable_for_the_same_uri() {
        let u = Url::parse("file:///home/u/app/src/Main.ridge").unwrap();
        assert_eq!(uri_key(&u), uri_key(&u));
    }

    #[cfg(windows)]
    #[test]
    fn uri_key_folds_windows_drive_case_and_colon_encoding() {
        // The two spellings of one path that a client (VS Code) and the server
        // produce on Windows must resolve to the same key, or every position
        // query misses the index while pushed diagnostics still land.
        let from_client = Url::parse("file:///c%3A/Dir/App/src/Main.ridge").unwrap();
        let from_server = Url::parse("file:///C:/Dir/App/src/Main.ridge").unwrap();
        assert_eq!(uri_key(&from_client), uri_key(&from_server));
    }
}
