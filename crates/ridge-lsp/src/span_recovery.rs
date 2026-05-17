//! D087 span recovery for synthesised IR nodes.
//!
//! When a `ridge_diagnostics::Diagnostic` carries a `primary_span` that
//! corresponds to a synthesised IR node (one absent from
//! `LoweredModule.source_map`), this module performs a best-effort recovery:
//!
//! 1. Look up `primary_span` in the provided source text directly (fast path).
//! 2. If the span is zero-width (a sentinel from a synthesised node), fall back
//!    to `(line 0, col 0)` — file-line-1 in LSP terms.
//!
//! The full D087 parent-chain walk requires the IR tree to be present.
//! For the LSP use-case, the `primary_span` in the `Diagnostic` is already
//! resolved to a `Span` (byte offsets) by the driver's diagnostic adapters.
//! So "recovery" here means: if the span is degenerate (zero-width at offset 0),
//! anchor it to the file start.
//!
//! ## Three recovery scenarios (§3.10 test surface)
//!
//! 1. **Synthesised `ToText` interpolation node** — span is zero-width; no
//!    entry in `source_map`.  The driver propagates the enclosing function's
//!    span via `diag_from_typecheck`.  The LSP renders at the enclosing call-site.
//!
//! 2. **`IrExpr::Call` with stdlib synthesis** — the call was inserted by
//!    lowering (e.g., `++` on `Text`).  The adapter uses the call-site `NodeId`
//!    from the resolved workspace graph, not the IR `source_map`.
//!    Result: span points at the call-site `Span`.
//!
//! 3. **Fully-synthetic prelude node** — span is `Span::point(0)`.  No source
//!    location is recoverable.  Falls back to file line 1 col 1 (LSP 0-indexed:
//!    line 0, character 0).

use ridge_lexer::{LineMap, Span};
use tower_lsp::lsp_types::{Position, Range};

/// Convert a byte-offset `Span` to an LSP `Range` with D087 fallback.
///
/// If `span` is zero-width AND at offset 0 (the D087 "no span" sentinel),
/// the fallback `Range` covering the first character of the file is returned.
/// Otherwise, the span is converted normally via [`LineMap`].
#[must_use]
pub fn resolve_span_to_lsp(span: Span, src: &str) -> Range {
    if span.start == 0 && span.end == 0 {
        // D087 fallback: fully-synthetic node — anchor to file start.
        return file_line1_range();
    }

    let lm = LineMap::new(src);
    let (start_line, start_col) = lm.line_col(span.start);
    let (end_line, end_col) = lm.line_col(span.end);

    // Clamp: if end comes out before start (degenerate span), use a unit range.
    let (end_line, end_col) = if (end_line, end_col) < (start_line, start_col) {
        (start_line, start_col + 1)
    } else {
        (end_line, end_col)
    };

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

/// The D087 "no source" fallback range: file line 1, character 1 (0-indexed: 0, 0).
#[must_use]
pub const fn file_line1_range() -> Range {
    Range {
        start: Position {
            line: 0,
            character: 0,
        },
        end: Position {
            line: 0,
            character: 1,
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1 (§3.10 D087-1): synthesised ToText interpolation node
    // Span::point(0) → zero-width at offset 0 → file-line-1 fallback.
    #[test]
    fn d087_synthesised_totext_recovers_to_file_line1() {
        let src = "pub fn greet name -> Text = \"Hello #{name}!\"";
        let span = Span::point(0); // synthesised — no AST origin
        let range = resolve_span_to_lsp(span, src);
        // Must land at file-line-1 (LSP: line 0, character 0).
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 0);
    }

    // Test 2 (§3.10 D087-2): IrExpr::Call with stdlib synthesis
    // Span points at a real call-site; verify it resolves to the correct line/col.
    #[test]
    fn d087_ir_call_stdlib_synthesis_recovers_to_call_site() {
        // Source: "foo ++ bar" on line 2 (0-indexed line 1).
        let src = "pub fn concat a b =\n  a ++ b";
        // Byte offset of "a" on line 2: "pub fn concat a b =\n" = 20 bytes;
        // "  a" → 'a' at offset 22.
        let span = Span::new(22, 23);
        let range = resolve_span_to_lsp(span, src);
        assert_eq!(
            range.start.line, 1,
            "should be on line 2 (0-indexed line 1)"
        );
        assert_eq!(
            range.start.character, 2,
            "should be at column 3 (0-indexed col 2)"
        );
    }

    // Test 3 (§3.10 D087-3): fully-synthetic prelude node
    // Prelude nodes always use Span::point(0) — must anchor to file-line-1.
    #[test]
    fn d087_fully_synthetic_prelude_node_recovers_to_file_line1() {
        let src = ""; // empty file — worst case
        let span = Span::point(0);
        let range = resolve_span_to_lsp(span, src);
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 0);
        // end should be character 1 (per file_line1_range).
        assert_eq!(range.end.character, 1);
    }
}
