//! Ridge parser: transforms a token stream into a typed AST.
//!
//! # Entry points
//!
//! - [`parse_source`] — convenience wrapper: lex then parse in one call.
//! - [`parse_module`] — parse a pre-lexed token slice into a [`Module`].
//!
//! # Error handling
//!
//! Both functions return a [`ParseResult`] that always contains a (possibly
//! partial) [`Module`].  Callers that require a fully valid parse must check
//! that `errors` and `lex_errors` are both empty.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use ridge_ast::{Module, Span};
use ridge_lexer::Token;

mod actor_ops;
mod block;
mod ctrl;
mod cursor;
mod decl;
mod error;
mod expr;
mod pattern;
mod ty;

pub use error::ParseError;

// ── Public result type ────────────────────────────────────────────────────────

/// The result of parsing a single Ridge source file.
///
/// Mirrors [`ridge_lexer::LexOutput`]: always present, possibly partial.
pub struct ParseResult {
    /// The parsed module.  Always present; may be empty / partial if errors
    /// occurred.
    pub module: Module,
    /// Parse errors accumulated during parsing.  Empty iff the source is
    /// well-formed from the parser's perspective.
    pub errors: Vec<ParseError>,
    /// Lexical errors from the initial lex pass.  Only populated by
    /// [`parse_source`]; always empty when calling [`parse_module`] directly.
    pub lex_errors: Vec<ridge_lexer::LexError>,
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Parse a full Ridge source module from a pre-lexed token stream.
///
/// The token stream **must** be the output of `ridge_lexer::tokenize` — the
/// parser relies on the layout tokens (`Indent`/`Dedent`/`Newline`) and the
/// invariant that `Eof` is always the last element.
///
/// # Doc-comment attachment algorithm (D067, T11)
///
/// 1. Consecutive `DocComment` tokens (separated only by `Newline`s) form a
///    *doc run*.
/// 2. If a doc run is immediately followed by a top-level item, the entire run
///    is flattened into a single `DocComment` (lines joined by `"\n"`) and
///    attached to that item's `doc` field.
/// 3. If the file contains **only** doc comments (no items), the run is stored
///    in `Module::doc` as individual `DocComment` entries.
/// 4. A doc run that is NOT followed by an item (trailing run at EOF, mid-file
///    run that precedes nothing due to parse error) causes one `P019
///    OrphanDocComment` per token in the run.
#[must_use]
pub fn parse_module(tokens: &[(Token, Span)]) -> ParseResult {
    let mut cur = cursor::Cursor::new(tokens);
    parse_module_inner(&mut cur)
}

/// Core parsing loop, shared by [`parse_module`] and [`parse_module_with_line_map`].
/// Accepts a pre-constructed cursor so that callers can configure it (e.g. with
/// a `LineMap` for the nested-match column rule).
fn parse_module_inner(cur: &mut cursor::Cursor<'_>) -> ParseResult {
    use decl::{parse_item, parse_visibility};
    use ridge_ast::DocComment;

    let mut errors: Vec<ParseError> = Vec::new();
    let mut items: Vec<ridge_ast::Item> = Vec::new();

    // Record the start span for the module.
    let start_span = cur.span();

    // Accumulates DocComment tokens collected since the last item was parsed (or
    // since file start).  When an item is encountered the run is flushed and
    // attached as a single flattened DocComment.
    let mut pending_docs: Vec<DocComment> = Vec::new();

    // ── Main parse loop ───────────────────────────────────────────────────────
    loop {
        // ── Skip blank lines; collect DocComment tokens ───────────────────────
        loop {
            match cur.peek() {
                Token::Newline => {
                    cur.bump();
                }
                Token::DocComment(_) => {
                    // Collect the doc token into `pending_docs`.
                    let span = cur.span();
                    if let Token::DocComment(text) = cur.bump().clone() {
                        pending_docs.push(DocComment { text, span });
                    }
                }
                _ => break,
            }
        }

        if cur.at_eof() {
            break;
        }

        // ── Flatten the pending doc run into one DocComment for this item ─────
        // Join all pending lines with "\n" and use the merged span.
        let item_doc: Option<DocComment> = if pending_docs.is_empty() {
            None
        } else {
            let text = pending_docs
                .iter()
                .map(|d| d.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let span = pending_docs
                .iter()
                .fold(pending_docs[0].span, |acc, d| acc.merge(d.span));
            pending_docs.clear();
            Some(DocComment { text, span })
        };

        // ── Parse visibility prefix ───────────────────────────────────────────
        let vis = match parse_visibility(cur) {
            Ok(v) => v,
            Err(e) => {
                errors.push(e);
                // If we had a doc for this item, it cannot be attached; emit
                // P019 for it (the item failed to parse).
                if let Some(doc) = item_doc {
                    errors.push(ParseError::OrphanDocComment { span: doc.span });
                }
                sync_to_next_item(cur);
                continue;
            }
        };

        // ── Dispatch on keyword ───────────────────────────────────────────────
        match parse_item(cur, item_doc, vis) {
            Ok(item) => {
                items.push(item);
            }
            Err(e) => {
                errors.push(e);
                sync_to_next_item(cur);
            }
        }

        // Consume trailing Newline after each item.
        while cur.peek() == &Token::Newline {
            cur.bump();
        }
    }

    // ── Handle trailing doc comments (after last item or file-only docs) ──────
    let module_doc: Vec<DocComment> = if items.is_empty() {
        // File contains only doc comments (no items) — store them in Module::doc.
        std::mem::take(&mut pending_docs)
    } else {
        // Doc comments after the last item are orphans — emit P019 for each.
        for doc in std::mem::take(&mut pending_docs) {
            errors.push(ParseError::OrphanDocComment { span: doc.span });
        }
        vec![]
    };

    let end_span = cur.span();

    ParseResult {
        module: Module {
            items,
            doc: module_doc,
            span: start_span.merge(end_span),
        },
        errors,
        lex_errors: vec![],
    }
}

/// Advance the cursor past the current token(s) to the next synchronisation
/// point: a `Newline` at bracket-depth 0, or a top-level keyword at column 0,
/// or `Eof`.
///
/// This is a minimal panic-mode recovery step.  Full recovery (T12) will
/// implement the complete sync-point set from §4.7.
fn sync_to_next_item(cur: &mut cursor::Cursor<'_>) {
    loop {
        match cur.peek() {
            Token::Eof | Token::Newline => {
                // Consume the newline so the main loop can skip it cleanly.
                if cur.peek() == &Token::Newline {
                    cur.bump();
                }
                return;
            }
            Token::KwFn
            | Token::KwType
            | Token::KwActor
            | Token::KwConst
            | Token::KwImport
            | Token::KwPub => {
                // Stop before a top-level keyword so the main loop re-dispatches.
                return;
            }
            _ => {
                cur.bump();
            }
        }
    }
}

/// A single trivia item extracted from source text before the layout pass
/// strips it.
///
/// Used by [`parse_module_with_trivia`] to give callers (notably `ridge-fmt`)
/// access to whitespace, comments, and blank lines that the layout algorithm
/// would otherwise discard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trivia {
    /// A line comment `-- ...` including the `--` prefix, without trailing
    /// newline.  `span` covers the full comment text (from `--` to end of
    /// line, exclusive of `\n`).
    LineComment {
        /// The raw comment text, e.g. `"-- some remark"`.
        text: String,
        /// Byte span of the comment in the (CRLF-normalised) source.
        span: ridge_ast::Span,
        /// 0-based line number in the normalised source.
        line: u32,
        /// 0-based byte column of the `--` within the line.
        col: u32,
    },
    /// A blank line (a line containing only whitespace or nothing).
    ///
    /// `line` is the 0-based line number.
    BlankLine {
        /// 0-based line number.
        line: u32,
    },
}

/// The result of parsing with trivia.
///
/// Contains the normal [`ParseResult`] plus the ordered list of trivia items
/// extracted from the source.  The trivia list is in source order (ascending
/// by line number).
pub struct ParseResultWithTrivia {
    /// The standard parse output (AST, errors, lex errors).
    pub result: ParseResult,
    /// Trivia items (line comments and blank lines) in source order.
    pub trivia: Vec<Trivia>,
    /// The CRLF-normalised source string used for all byte offsets in
    /// `trivia` spans.
    pub normalised_src: String,
}

/// Parse a full Ridge source module and preserve trivia (line comments, blank
/// lines) that the normal lexer pipeline strips.
///
/// This is an additive, non-semantic extension of [`parse_source`] — the
/// returned [`ParseResult`] is identical to what [`parse_source`] would
/// produce.  The only additional output is the `trivia` vector, which `ridge-fmt`
/// uses to re-insert comments and blank lines during pretty-printing.
///
/// **Allowed by §1.3 hard constraint #1** — promoted to `pub` for `ridge-fmt`.
/// This function does NOT change parser semantics.
#[must_use]
pub fn parse_module_with_trivia(src: &str) -> ParseResultWithTrivia {
    // Normalise line endings once so that all byte offsets are consistent.
    let normalised = normalise_crlf(src);

    // Scan for trivia (line comments + blank lines) before the lexer
    // discards them.
    let trivia = extract_trivia(&normalised);

    // Delegate to the standard parse path.
    let result = parse_source(&normalised);

    ParseResultWithTrivia {
        result,
        trivia,
        normalised_src: normalised,
    }
}

/// Normalise `\r\n` → `\n` and bare `\r` → `\n`.
///
/// Mirrors the normalisation done inside [`ridge_lexer::tokenize`] so that
/// byte offsets in [`Trivia`] spans are consistent with token spans.
fn normalise_crlf(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Scan a CRLF-normalised source string and collect trivia items.
///
/// This is a simple line-by-line pass that runs **before** the tokeniser so
/// that line comments and blank lines are not yet discarded.
///
/// # Casts
///
/// Byte offsets are cast `usize → u32`.  Ridge source files are always
/// well under 4 GiB, so truncation is impossible in practice.  This matches
/// the convention established in `ridge-lexer/src/raw_scan.rs` and
/// `ridge-lexer/src/span.rs`.
#[allow(clippy::cast_possible_truncation)]
fn extract_trivia(normalised: &str) -> Vec<Trivia> {
    let mut trivia = Vec::new();
    let mut byte_offset: u32 = 0;
    // B-D010 #1 hotfix v3 Wave 3: track `---…---` doc-block depth across
    // lines.  Pre-fix, the per-line scanner treated `--` inside a doc-comment
    // body as a real line comment, so `ridge fmt` extracted it as trivia,
    // stripped it from the source line, and the operator-spacing pass then
    // mutated whatever code-shaped fragment remained — turning a doc body
    // like `  ridge run -- <file>` into `  ridge run  -- <file>` (an extra
    // space) and stripping `<file>` into a re-attached trailing comment.
    let mut in_doc_block = false;

    for (line_idx, line_text) in normalised.split('\n').enumerate() {
        let line_no = line_idx as u32;
        let trimmed = line_text.trim_start();
        let is_doc_marker = trimmed == "---" || trimmed.starts_with("--- ");

        if is_doc_marker {
            in_doc_block = !in_doc_block;
            // The marker line itself: no line-comment trivia, no blank.
        } else if trimmed.is_empty() {
            // Blank line (includes whitespace-only lines).
            trivia.push(Trivia::BlankLine { line: line_no });
        } else if !in_doc_block {
            if let Some(comment_pos) = find_line_comment(line_text) {
                // There is a `--` somewhere on this line that is not inside a
                // `---` doc-comment opener. Extract it.
                let col = comment_pos as u32;
                let comment_text = line_text[comment_pos..].to_string();
                let start = byte_offset + col;
                let end = byte_offset + line_text.len() as u32;
                trivia.push(Trivia::LineComment {
                    text: comment_text,
                    span: ridge_ast::Span::new(start, end),
                    line: line_no,
                    col,
                });
            }
        }
        // (When in_doc_block: skip both blank and line-comment trivia.  The
        // doc body is captured by the lexer as a single DocComment token; we
        // do not need any per-line trivia inside the body for fmt to
        // reconstruct it faithfully.)

        // Advance byte_offset past this line + the '\n' separator.
        byte_offset += line_text.len() as u32 + 1; // +1 for '\n'
    }

    trivia
}

/// Find the byte position of a `--` line comment within a source line,
/// returning `None` if no line comment exists on this line.
///
/// The function skips:
/// - `---` doc-comment openers (not a line comment).
/// - `--` that appears inside a `"..."` string literal.
/// - `--` that appears inside a `$"..."` interpolated string.
fn find_line_comment(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_string = false; // inside a plain `"..."` or `$"..."` string

    while i < len {
        match bytes[i] {
            // String literal start / end (covers both `"..."` and `$"..."`).
            b'"' => {
                in_string = !in_string;
                i += 1;
            }
            b'-' if !in_string && i + 1 < len && bytes[i + 1] == b'-' => {
                // Is this `---` (doc comment marker)?
                if i + 2 < len && bytes[i + 2] == b'-' {
                    // `---` → doc comment boundary, not a line comment.
                    return None;
                }
                // `--` line comment found.
                return Some(i);
            }
            // Escape inside string: skip next char.
            b'\\' if in_string => {
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

/// Convenience: lex and parse in one call.
///
/// Returns combined lex + parse errors so downstream callers (driver, LSP,
/// tests) see a single error stream per source file.
///
/// Unlike [`parse_module`], this function threads the lexer's [`LineMap`] into
/// the parser cursor so that the nested-match column rule (E2, E3) can fire.
#[must_use]
pub fn parse_source(src: &str) -> ParseResult {
    let lex = ridge_lexer::tokenize(src);
    let mut cur = cursor::Cursor::new_with_line_map(&lex.tokens, &lex.line_map);
    let mut result = parse_module_inner(&mut cur);
    result.lex_errors = lex.errors;
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // T2-required: empty input → empty Module, no errors.
    #[test]
    fn empty_input_produces_empty_module() {
        let result = parse_source("");
        assert!(
            result.errors.is_empty(),
            "expected no parse errors: {:?}",
            result.errors
        );
        assert!(
            result.lex_errors.is_empty(),
            "expected no lex errors: {:?}",
            result.lex_errors
        );
        assert!(result.module.items.is_empty());
        assert!(result.module.doc.is_empty());
    }

    // T2-required: whitespace-only input → same as empty.
    #[test]
    fn whitespace_only_input_produces_empty_module() {
        let result = parse_source("   ");
        assert!(
            result.errors.is_empty(),
            "expected no parse errors: {:?}",
            result.errors
        );
        assert!(
            result.lex_errors.is_empty(),
            "expected no lex errors: {:?}",
            result.lex_errors
        );
        assert!(result.module.items.is_empty());
    }

    // T2-required: single NEWLINE input → consumed, empty Module, no errors.
    #[test]
    fn single_newline_input_produces_empty_module() {
        let result = parse_source("\n");
        assert!(
            result.errors.is_empty(),
            "expected no parse errors: {:?}",
            result.errors
        );
        assert!(
            result.lex_errors.is_empty(),
            "expected no lex errors: {:?}",
            result.lex_errors
        );
        assert!(result.module.items.is_empty());
    }

    // T2-required: ParseError::code() returns stable strings for all four
    // variants that must exist in T2.
    #[test]
    fn parse_error_codes_are_stable() {
        let span = Span::point(0);

        let e1 = ParseError::Expected {
            span,
            expected: "<EOF>",
            found: "foo".to_string(),
        };
        assert_eq!(e1.code(), "P001");

        let e2 = ParseError::UnexpectedToken {
            span,
            description: "unexpected `foo`".to_string(),
        };
        assert_eq!(e2.code(), "P002");

        let e6 = ParseError::LayoutMismatch {
            span,
            hint: "unexpected DEDENT",
        };
        assert_eq!(e6.code(), "P006");

        let e999 = ParseError::InternalLayoutInvariantViolated { span };
        assert_eq!(e999.code(), "P999");
    }

    // Bonus: span() accessor returns the carried Span.
    #[test]
    fn parse_error_span_accessor() {
        let span = Span::new(10, 20);
        let e = ParseError::Expected {
            span,
            expected: "something",
            found: "nothing".to_string(),
        };
        assert_eq!(e.span(), span);
    }

    // ── T11: doc-comment attachment ───────────────────────────────────────────

    // T11-1: empty input → empty Module with no doc comments.
    #[test]
    fn parse_module_empty() {
        let result = parse_source("");
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        assert!(result.module.items.is_empty());
        assert!(result.module.doc.is_empty());
        assert_eq!(result.module.span, Span::new(0, 0));
    }

    // T11-2: file contains only a doc comment and no items.
    // Per D067: doc-only files store the comments in Module::doc.
    #[test]
    fn parse_module_zero_items_only_doc() {
        let src = "---\nfile-level doc\n---\n";
        let result = parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        assert!(result.module.items.is_empty());
        // The file-leading doc run goes into Module::doc.
        assert_eq!(result.module.doc.len(), 1);
        assert!(
            result.module.doc[0].text.contains("file-level doc"),
            "expected doc text to contain 'file-level doc', got: {:?}",
            result.module.doc[0].text
        );
    }

    // T11-3: single doc comment immediately before an fn → attaches to Item::doc.
    #[test]
    fn parse_module_one_item_with_doc() {
        let src = "---\ngreet people\n---\nfn greet x = x\n";
        let result = parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        assert_eq!(result.module.items.len(), 1);
        // Doc comment is on the item, not the module.
        assert!(
            result.module.doc.is_empty(),
            "Module::doc should be empty when item follows doc"
        );
        let ridge_ast::Item::Fn(fn_decl) = &result.module.items[0] else {
            unreachable!("expected Item::Fn")
        };
        assert!(fn_decl.doc.is_some(), "fn doc should be Some(_)");
        if let Some(doc) = &fn_decl.doc {
            assert!(
                doc.text.contains("greet people"),
                "expected doc text 'greet people', got: {:?}",
                doc.text
            );
        }
    }

    // T11-4: two items each with their own doc comment — both attach correctly.
    #[test]
    fn parse_module_two_items_with_docs() {
        let src = "---\ndoc for foo\n---\nfn foo x = x\n---\ndoc for bar\n---\nfn bar y = y\n";
        let result = parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        assert_eq!(result.module.items.len(), 2);
        assert!(result.module.doc.is_empty());

        let ridge_ast::Item::Fn(fn_foo) = &result.module.items[0] else {
            unreachable!("expected Item::Fn for foo")
        };
        assert!(fn_foo.doc.is_some(), "foo should have a doc comment");
        if let Some(doc) = &fn_foo.doc {
            assert!(
                doc.text.contains("doc for foo"),
                "unexpected foo doc: {:?}",
                doc.text
            );
        }

        let ridge_ast::Item::Fn(fn_bar) = &result.module.items[1] else {
            unreachable!("expected Item::Fn for bar")
        };
        assert!(fn_bar.doc.is_some(), "bar should have a doc comment");
        if let Some(doc) = &fn_bar.doc {
            assert!(
                doc.text.contains("doc for bar"),
                "unexpected bar doc: {:?}",
                doc.text
            );
        }
    }

    // T11-5: trailing doc comment after the last item → P019 OrphanDocComment.
    #[test]
    fn parse_module_orphan_doc_at_eof() {
        let src = "fn foo x = x\n---\norphan comment\n---\n";
        let result = parse_source(src);
        assert_eq!(result.module.items.len(), 1, "should still parse the fn");
        // There must be exactly one P019 error for the orphan.
        let p019_count = result.errors.iter().filter(|e| e.code() == "P019").count();
        assert_eq!(
            p019_count, 1,
            "expected 1 P019 OrphanDocComment error, got: {:?}",
            result.errors
        );
    }

    // T11-6: two items without any doc comments (regression — prior tests must still pass).
    #[test]
    fn parse_module_two_items_plain() {
        let src = "fn foo x = x\nfn bar y = y\n";
        let result = parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        assert_eq!(result.module.items.len(), 2);
        assert!(result.module.doc.is_empty());
        // Both items have no doc.
        for item in &result.module.items {
            if let ridge_ast::Item::Fn(f) = item {
                assert!(
                    f.doc.is_none(),
                    "expected no doc on plain fn, got {:?}",
                    f.doc
                );
            }
        }
    }

    // T11-7: multi-line doc run before an item → flattened into one DocComment.
    #[test]
    fn parse_module_multiline_doc_run_attaches_as_one() {
        // Two consecutive doc blocks before a single fn.
        let src = "---\nfirst block\n---\n---\nsecond block\n---\nfn greet x = x\n";
        let result = parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        assert_eq!(result.module.items.len(), 1);
        assert!(result.module.doc.is_empty());
        let ridge_ast::Item::Fn(fn_decl) = &result.module.items[0] else {
            unreachable!("expected Item::Fn")
        };
        // The two blocks should be joined into a single DocComment.
        assert!(fn_decl.doc.is_some(), "fn should have a merged doc comment");
        if let Some(doc) = &fn_decl.doc {
            assert!(
                doc.text.contains("first block"),
                "merged doc should contain first block: {:?}",
                doc.text
            );
            assert!(
                doc.text.contains("second block"),
                "merged doc should contain second block: {:?}",
                doc.text
            );
        }
    }

    // T11-8: parse_source returns correct ParseResult fields for empty vs non-empty.
    #[test]
    fn parse_source_wrapper_empty_vs_non_empty() {
        let empty = parse_source("");
        assert!(empty.errors.is_empty());
        assert!(empty.lex_errors.is_empty());
        assert!(empty.module.items.is_empty());

        let non_empty = parse_source("const x: Int = 42\n");
        assert!(
            non_empty.errors.is_empty(),
            "unexpected errors: {:?}",
            non_empty.errors
        );
        assert!(
            non_empty.lex_errors.is_empty(),
            "unexpected lex errors: {:?}",
            non_empty.lex_errors
        );
        assert_eq!(non_empty.module.items.len(), 1);
    }

    // T11-9: P019 code and display are stable.
    #[test]
    fn p019_code_and_display() {
        let span = Span::point(10);
        let e = ParseError::OrphanDocComment { span };
        assert_eq!(e.code(), "P019");
        assert_eq!(e.span(), span);
        let msg = e.to_string();
        assert!(
            msg.contains("doc comment"),
            "display should mention 'doc comment': {msg}"
        );
    }
}
