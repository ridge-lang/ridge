//! Core diagnostic value types for the Ridge compiler.
//!
//! This module defines the owned, lifetime-free value type that every
//! diagnostic rendering and LSP adapter pipeline consumes.  Construction
//! happens exclusively through the per-error-enum `From<&XError>` adapters
//! in `crate::adapter`.

use std::sync::Arc;

use ridge_ast::Span;
pub use ridge_resolve::Severity;

// ── SourceId ──────────────────────────────────────────────────────────────────

/// Opaque source identifier.
///
/// The [`SourceCache`] resolves a `SourceId` to source text and a display
/// name.  In `ridge-driver`, a `SourceId` wraps the workspace-relative path
/// string; in `ridge-lsp`, it wraps an LSP `Url` string.  The renderer never
/// inspects the inside of a `SourceId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(pub(crate) Arc<str>);

impl SourceId {
    /// Construct a new `SourceId` from any string-like value.
    pub fn new(name: impl Into<String>) -> Self {
        Self(Arc::from(name.into()))
    }

    /// Return the inner identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── SourceCache ───────────────────────────────────────────────────────────────

/// Cache of source-text for diagnostic rendering.
///
/// Implemented by `ridge-driver` (file-backed) and `ridge-lsp` (in-memory
/// edit-buffer-backed).  The renderer never reads files itself — it asks the
/// cache.
pub trait SourceCache {
    /// Return the source text for the given identifier.
    ///
    /// Returns `None` if the source is unavailable; the renderer falls back
    /// to a context-less render (code prefix + message, no underline).
    fn fetch(&self, id: &SourceId) -> Option<&str>;

    /// Return a human-readable display name for `id`.
    ///
    /// Used in the `--> path:line:col` header line.  Defaults to
    /// [`SourceId::as_str`]; implementers may override to produce shorter or
    /// prettier paths.
    fn display_name<'a>(&'a self, id: &'a SourceId) -> &'a str {
        id.as_str()
    }
}

// ── NoteSeverity ──────────────────────────────────────────────────────────────

/// Severity of a secondary diagnostic note.
///
/// Distinct from top-level [`Severity`].  Maps to ariadne's colour palette:
/// `Help` → green, `Note` → blue, `Hint` → yellow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteSeverity {
    /// A helpful suggestion for how to fix the error.
    Help,
    /// An informational note about the error.
    Note,
    /// A light hint about the context.
    Hint,
}

// ── DiagnosticNote ────────────────────────────────────────────────────────────

/// A secondary annotation in a diagnostic.
///
/// Used for secondary spans — e.g. "first declared here" or
/// "did you mean `foo`?" — rendered alongside the primary span.
#[derive(Debug, Clone)]
pub struct DiagnosticNote {
    /// Source span of this note.
    pub span: Span,
    /// Human-readable message for this note.
    pub message: String,
    /// Severity / colour class of this note.
    pub severity: NoteSeverity,
}

// ── MessageParts ──────────────────────────────────────────────────────────────

/// The pieces of a [`Diagnostic::primary_message`], separated for rendering.
///
/// Most messages are a single line and split into a headline with nothing
/// else. Longer ones follow a convention the whole compiler shares: a summary
/// line first, then an indented body explaining it, and `hint:` or `help:`
/// lines carrying the suggested fix. [`Diagnostic::message_parts`] recovers
/// that structure so each piece can go where it belongs — the headline next to
/// the caret, the rest below the snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageParts<'a> {
    /// First line of the message, without a redundant `"{code}: "` prefix.
    ///
    /// Short enough to sit next to the caret without wrapping.
    pub headline: &'a str,
    /// The explanatory body, dedented, or `None` for a single-line message.
    ///
    /// Interior indentation is preserved, so nested lists still read as lists.
    pub note: Option<String>,
    /// Suggested fixes, taken from the body's `hint:` / `help:` lines.
    pub helps: Vec<String>,
}

/// Strip the longest indentation common to every non-blank line.
fn common_indent(lines: &[&str]) -> usize {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0)
}

/// Phrase a set of near-miss suggestions as one line.
///
/// Callers used to push one note per suggestion, which drew a separate arrow
/// at the same span for each — three identical carets under `Io.printn` for
/// `print`, `println` and `eprint`. One line reads better and points once.
///
/// Returns `None` for an empty list.
#[must_use]
pub fn did_you_mean(suggestions: &[String]) -> Option<String> {
    match suggestions {
        [] => None,
        [one] => Some(format!("did you mean `{one}`?")),
        [rest @ .., last] => {
            let head = rest
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("did you mean {head} or `{last}`?"))
        }
    }
}

/// Return the text after a leading `hint:` / `help:` marker, if present.
///
/// Only matches at the very start of the line: a marker that appears further
/// in (inside an `Options:` list, say) belongs to the body, not to a help.
fn strip_help_marker(line: &str) -> Option<&str> {
    for marker in ["hint:", "help:"] {
        if line.len() >= marker.len() && line[..marker.len()].eq_ignore_ascii_case(marker) {
            return Some(line[marker.len()..].trim_start());
        }
    }
    None
}

// ── Diagnostic ────────────────────────────────────────────────────────────────

/// A structured diagnostic suitable for human or machine rendering.
///
/// Owned, lifetime-free, `Clone`.  The primary construction path is
/// `From<&XError> for Diagnostic` adapters in [`crate::adapter`].
///
/// # LSP forward-compat
///
/// Every field needed by an LSP `Diagnostic` is present.  T11 (`ridge-lsp`)
/// provides the `From<Diagnostic> for lsp_types::Diagnostic` adapter using
/// the existing `LineMap` byte-offset-to-line-col conversion.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Diagnostic {
    /// Stable error code, e.g. `"T015"`, `"R013"`, `"P001"`, `"E007"`.
    pub code: &'static str,
    /// Severity (`Error` / `Warning`).
    pub severity: Severity,
    /// Primary source span — the location ariadne underlines with the caret.
    pub primary_span: Span,
    /// The message, following the compiler-wide convention: a summary line
    /// first, then an optional indented body, then optional `hint:` / `help:`
    /// lines.
    ///
    /// Renderers should not print this verbatim next to the caret — call
    /// [`Self::message_parts`], which separates the pieces so each lands where
    /// it belongs.
    pub primary_message: String,
    /// Source identifier — opaque key the [`SourceCache`] uses to retrieve text.
    pub source_id: SourceId,
    /// Secondary annotations.
    ///
    /// Each note carries its own [`Span`], message, and [`NoteSeverity`].
    /// For example, `R005 DuplicateDeclaration` produces two notes:
    /// "first defined here" and "redefined here".
    pub notes: Vec<DiagnosticNote>,
}

impl Diagnostic {
    /// Construct a `Diagnostic` with no secondary notes.
    #[must_use]
    pub fn new(
        code: &'static str,
        severity: Severity,
        primary_span: Span,
        primary_message: impl Into<String>,
        source_id: SourceId,
    ) -> Self {
        Self {
            code,
            severity,
            primary_span,
            primary_message: primary_message.into(),
            source_id,
            notes: Vec::new(),
        }
    }

    /// Split [`Self::primary_message`] into the parts a renderer places
    /// separately.
    ///
    /// A single-line message yields just a headline. A multi-line one yields
    /// the summary line plus the body, with any `hint:` / `help:` lines lifted
    /// out. A `"{code}: "` prefix on the first line is dropped, since every
    /// renderer already shows the code — the CLI in the report title, the
    /// language server in its own `code` field.
    #[must_use]
    pub fn message_parts(&self) -> MessageParts<'_> {
        let mut lines = self.primary_message.split('\n');
        let first = lines.next().unwrap_or_default();

        let prefix = format!("{}: ", self.code);
        let headline = first
            .strip_prefix(prefix.as_str())
            .unwrap_or(first)
            .trim_end();

        let rest: Vec<&str> = lines.collect();
        if rest.iter().all(|l| l.trim().is_empty()) {
            return MessageParts {
                headline,
                note: None,
                helps: Vec::new(),
            };
        }

        let indent = common_indent(&rest);
        let mut body: Vec<&str> = Vec::new();
        let mut helps: Vec<String> = Vec::new();
        for line in rest {
            let dedented = if line.len() >= indent {
                &line[indent..]
            } else {
                line.trim_start()
            };
            match strip_help_marker(dedented) {
                Some(help) => helps.push(help.to_owned()),
                None => body.push(dedented),
            }
        }

        while body.last().is_some_and(|l| l.trim().is_empty()) {
            body.pop();
        }
        let note = if body.is_empty() {
            None
        } else {
            Some(body.join("\n"))
        };

        MessageParts {
            headline,
            note,
            helps,
        }
    }

    /// Add a secondary note to this diagnostic.
    #[must_use]
    pub fn with_note(mut self, span: Span, message: impl Into<String>, sev: NoteSeverity) -> Self {
        self.notes.push(DiagnosticNote {
            span,
            message: message.into(),
            severity: sev,
        });
        self
    }
}

// ── RenderError ───────────────────────────────────────────────────────────────

/// Error returned by [`super::render_with_ariadne`].
///
/// Currently only wraps `std::io::Error`.  Source-cache misses are not errors
/// — the diagnostic is rendered context-lessly.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The underlying writer returned an I/O error.
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ridge_ast::Span;

    fn diag(code: &'static str, message: &str) -> Diagnostic {
        Diagnostic::new(
            code,
            Severity::Error,
            Span::new(0, 1),
            message,
            SourceId::new("test.ridge"),
        )
    }

    #[test]
    fn single_line_message_is_all_headline() {
        let d = diag("P001", "unexpected token `then`");
        let parts = d.message_parts();
        assert_eq!(parts.headline, "unexpected token `then`");
        assert_eq!(parts.note, None);
        assert!(parts.helps.is_empty());
    }

    #[test]
    fn redundant_code_prefix_is_dropped() {
        let d = diag("T052", "T052: arithmetic on non-numeric type");
        assert_eq!(d.message_parts().headline, "arithmetic on non-numeric type");
    }

    /// A different code at the start of the message is content, not a prefix.
    #[test]
    fn foreign_code_prefix_is_kept() {
        let d = diag("T052", "T016: something about another code");
        assert_eq!(
            d.message_parts().headline,
            "T016: something about another code"
        );
    }

    #[test]
    fn body_and_hint_are_separated() {
        let d = diag(
            "T052",
            "T052: arithmetic on non-numeric type\n  \
             `+` requires `Int` or `Float` operands, found `Text`\n  \
             hint: use `++` to concatenate text",
        );
        let parts = d.message_parts();
        assert_eq!(parts.headline, "arithmetic on non-numeric type");
        assert_eq!(
            parts.note.as_deref(),
            Some("`+` requires `Int` or `Float` operands, found `Text`")
        );
        assert_eq!(parts.helps, vec!["use `++` to concatenate text"]);
    }

    /// Nested list items keep their relative indentation, so an `Options:`
    /// block still reads as a block after the common indent comes off.
    #[test]
    fn nested_indentation_survives_dedent() {
        let d = diag(
            "T014",
            "T014: capability not declared\n  \
             function `main` declared as `fn {}` uses capability `{io}`\n  \
             Options:\n    \
             - Add `{io}` to the signature\n    \
             - Remove the call requiring `{io}`",
        );
        let note = d.message_parts().note.unwrap();
        assert!(note.contains("\nOptions:\n"), "note was: {note:?}");
        assert!(
            note.contains("\n  - Add `{io}` to the signature"),
            "note was: {note:?}"
        );
    }

    /// A `hint:` nested inside the body is content, not a top-level help.
    #[test]
    fn indented_hint_marker_stays_in_the_body() {
        let d = diag(
            "T014",
            "T014: headline\n  body line\n    hint: nested, not a help",
        );
        let parts = d.message_parts();
        assert!(parts.helps.is_empty(), "helps were: {:?}", parts.helps);
        assert!(parts.note.unwrap().contains("hint: nested, not a help"));
    }

    #[test]
    fn suggestions_read_as_one_line() {
        assert_eq!(did_you_mean(&[]), None);
        assert_eq!(
            did_you_mean(&["print".to_owned()]).unwrap(),
            "did you mean `print`?"
        );
        assert_eq!(
            did_you_mean(&[
                "print".to_owned(),
                "println".to_owned(),
                "eprint".to_owned()
            ])
            .unwrap(),
            "did you mean `print`, `println` or `eprint`?"
        );
    }
}
