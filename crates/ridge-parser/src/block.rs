//! Block parsing (grammar §5.3 line 517, plan §4.4).
//!
//! A block is an `INDENT`-delimited sequence of expressions each separated by
//! `NEWLINE`.  The block value is the last expression; the type checker
//! (Phase 4) enforces that all prior expressions return `Unit`.
//!
//! ## Layout contract (§4.4 verbatim)
//!
//! ```text
//! parse_block():
//!   expect(INDENT)
//!   stmts = [parse_expr()]
//!   while peek() == NEWLINE:
//!     bump()
//!     if peek() == DEDENT: break
//!     stmts.push(parse_expr())
//!   expect(DEDENT)
//!   return Block { stmts, span }
//! ```
//!
//! **Empty block:** An immediate `DEDENT` after `INDENT` is `P014 EmptyBlock`.
//!
//! ## Error recovery (T12, §4.7)
//!
//! When a statement inside a block fails to parse, `parse_block` recovers by
//! skipping tokens until the next `NEWLINE` (sibling statement boundary) or
//! `DEDENT` (end of block).  This ensures that a single malformed statement
//! produces exactly one diagnostic and the block continues parsing.

#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use ridge_ast::{Block, Expr};
use ridge_lexer::Token;

use crate::{cursor::Cursor, error::ParseError, expr::parse_expr};

/// Parse a multi-statement `Block` from an `INDENT`/`DEDENT`-delimited region.
///
/// Grammar (§5.3, line 517): `Block ::= INDENT { Expr NEWLINE } DEDENT`
///
/// Precondition: `cur.peek() == &Token::Indent`.
///
/// On success the cursor is positioned after the closing `DEDENT` and the
/// returned [`Block::span`] covers from the `INDENT` token through the
/// `DEDENT` token (inclusive convex hull).
///
/// On `P014 EmptyBlock` the cursor is positioned after the `INDENT` and at
/// the `DEDENT`; callers may choose to recover or propagate.
pub(crate) fn parse_block(cur: &mut Cursor<'_>) -> Result<Block, ParseError> {
    let indent_span = cur.expect(&Token::Indent)?;

    // Empty block: immediate DEDENT is an error.
    if cur.peek() == &Token::Dedent {
        let span = indent_span.merge(cur.span());
        cur.bump(); // consume DEDENT for recovery
        return Err(ParseError::EmptyBlock { span });
    }

    let mut stmts: Vec<Expr> = Vec::new();
    let mut errors: Vec<ParseError> = Vec::new();

    // Parse the first statement.
    match parse_expr(cur) {
        Ok(first) => {
            stmts.push(first);
        }
        Err(e) => {
            errors.push(e);
            sync_to_next_stmt(cur);
        }
    }

    // Parse subsequent statements separated by NEWLINE.
    while cur.peek() == &Token::Newline {
        cur.bump(); // consume NEWLINE
        if cur.peek() == &Token::Dedent {
            break;
        }
        // Skip orphan newlines (e.g. blank lines).
        if cur.peek() == &Token::Newline {
            continue;
        }
        match parse_expr(cur) {
            Ok(stmt) => {
                stmts.push(stmt);
            }
            Err(e) => {
                errors.push(e);
                sync_to_next_stmt(cur);
            }
        }
    }

    let dedent_span = expect_dedent(cur)?;
    let end_span = stmts.last().map_or(indent_span, ridge_ast::Expr::span);
    let span = indent_span.merge(dedent_span).merge(end_span);

    if errors.is_empty() {
        Ok(Block { stmts, span })
    } else {
        // Return any successfully parsed statements as a partial block, but
        // propagate the first error upward so the caller can collect it.
        // Subsequent errors are re-collected at the parse_block_recovering entry
        // point used by callers that can absorb multiple errors.
        Err(errors.remove(0))
    }
}

/// Parse a block and collect all errors into `errors_out` rather than
/// short-circuiting on the first one.
///
/// Returns a `Block` (possibly with fewer stmts than the source has lines)
/// alongside all parse errors encountered.  Used by `parse_actor_body` and
/// anywhere else that needs a partial AST with all errors reported.
pub(crate) fn parse_block_recovering(
    cur: &mut Cursor<'_>,
    errors_out: &mut Vec<ParseError>,
) -> Block {
    let indent_span = match cur.expect(&Token::Indent) {
        Ok(s) => s,
        Err(e) => {
            errors_out.push(e);
            return Block {
                stmts: vec![],
                span: cur.span(),
            };
        }
    };

    // Empty block.
    if cur.peek() == &Token::Dedent {
        let span = indent_span.merge(cur.span());
        cur.bump();
        errors_out.push(ParseError::EmptyBlock { span });
        return Block {
            stmts: vec![],
            span,
        };
    }

    let mut stmts: Vec<Expr> = Vec::new();

    // First statement.
    match parse_expr(cur) {
        Ok(e) => stmts.push(e),
        Err(e) => {
            errors_out.push(e);
            sync_to_next_stmt(cur);
        }
    }

    // Subsequent statements.
    while cur.peek() == &Token::Newline {
        cur.bump();
        if cur.peek() == &Token::Dedent {
            break;
        }
        if cur.peek() == &Token::Newline {
            continue;
        }
        match parse_expr(cur) {
            Ok(e) => stmts.push(e),
            Err(e) => {
                errors_out.push(e);
                sync_to_next_stmt(cur);
            }
        }
    }

    let dedent_span = match expect_dedent(cur) {
        Ok(s) => s,
        Err(e) => {
            errors_out.push(e);
            cur.span()
        }
    };

    let end_span = stmts.last().map_or(indent_span, ridge_ast::Expr::span);
    let span = indent_span.merge(dedent_span).merge(end_span);
    Block { stmts, span }
}

/// Skip tokens until the next statement boundary inside a block.
///
/// Sync points (§4.7 block-level recovery):
/// - `NEWLINE` at depth 0 — sibling statement boundary.
/// - `DEDENT` — end of block.
/// - `EOF` — unconditional stop.
fn sync_to_next_stmt(cur: &mut Cursor<'_>) {
    loop {
        match cur.peek() {
            Token::Newline | Token::Dedent | Token::Eof => return,
            _ => {
                cur.bump();
            }
        }
    }
}

/// Expect a `DEDENT` token.
///
/// If the next token is `INDENT` (unexpected nested indentation) instead of
/// `DEDENT`, returns `P006 LayoutMismatch` rather than the generic `P001
/// Expected`.  For all other mismatches, falls back to the standard `P001`.
fn expect_dedent(cur: &mut Cursor<'_>) -> Result<ridge_ast::Span, crate::error::ParseError> {
    use crate::error::ParseError;
    if cur.peek() == &Token::Indent {
        let span = cur.span();
        return Err(ParseError::LayoutMismatch {
            span,
            hint: "unexpected INDENT where DEDENT expected (mismatched indentation level)",
        });
    }
    cur.expect(&Token::Dedent)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::panic)]
#[allow(clippy::unwrap_used)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use ridge_ast::Span;

    /// Build a minimal token stream that represents:
    ///   INDENT  <ident0>  NEWLINE  <ident1>  NEWLINE  <ident2>  NEWLINE  DEDENT  EOF
    /// with distinct spans so we can verify `Block::span`.
    fn three_stmt_tokens() -> Vec<(Token, Span)> {
        // Assign non-overlapping spans so merge tests are meaningful.
        // indent: 0..1, a: 2..3, nl: 3..4, b: 5..6, nl: 6..7, c: 8..9, nl: 9..10, dedent: 10..11
        vec![
            (Token::Indent, Span::new(0, 1)),
            (Token::LowerIdent("a".to_string()), Span::new(2, 3)),
            (Token::Newline, Span::new(3, 4)),
            (Token::LowerIdent("b".to_string()), Span::new(5, 6)),
            (Token::Newline, Span::new(6, 7)),
            (Token::LowerIdent("c".to_string()), Span::new(8, 9)),
            (Token::Newline, Span::new(9, 10)),
            (Token::Dedent, Span::new(10, 11)),
            (Token::Eof, Span::new(11, 11)),
        ]
    }

    // ── T9-1: parse_block_three_stmts ────────────────────────────────────────
    //
    // Directly exercise parse_block (not via if/try) with 3 statements.
    // Verifies count, ordering, and that the function succeeds at all.

    #[test]
    fn parse_block_three_stmts() {
        let tokens = three_stmt_tokens();
        let mut cur = Cursor::new(&tokens);
        let block = parse_block(&mut cur).expect("expected Ok from parse_block");
        assert_eq!(
            block.stmts.len(),
            3,
            "expected 3 stmts, got {}",
            block.stmts.len()
        );
        assert!(matches!(&block.stmts[0], Expr::Ident(id) if id.text == "a"));
        assert!(matches!(&block.stmts[1], Expr::Ident(id) if id.text == "b"));
        assert!(matches!(&block.stmts[2], Expr::Ident(id) if id.text == "c"));
    }

    // ── T9-2: parse_block_span_covers_indent_to_dedent ───────────────────────
    //
    // Block::span must start at INDENT.start and end at DEDENT.end (convex hull).
    // Grammar §5.3 line 517: Block ::= INDENT { Expr NEWLINE } DEDENT

    #[test]
    fn parse_block_span_covers_indent_to_dedent() {
        let tokens = three_stmt_tokens();
        let indent_span = tokens[0].1; // Span::new(0, 1)
        let dedent_span = tokens[7].1; // Span::new(10, 11)

        let mut cur = Cursor::new(&tokens);
        let block = parse_block(&mut cur).expect("expected Ok");

        assert_eq!(
            block.span.start, indent_span.start,
            "Block::span.start should equal INDENT span start ({}) but got {}",
            indent_span.start, block.span.start
        );
        assert_eq!(
            block.span.end, dedent_span.end,
            "Block::span.end should equal DEDENT span end ({}) but got {}",
            dedent_span.end, block.span.end
        );
    }

    // ── T9-3: parse_block_empty_direct (P014) ────────────────────────────────
    //
    // Direct call to parse_block with INDENT immediately followed by DEDENT
    // (no if/try wrapper). Must return P014 EmptyBlock.

    #[test]
    fn parse_block_empty_direct() {
        let s = Span::point(0);
        let tokens: Vec<(Token, Span)> =
            vec![(Token::Indent, s), (Token::Dedent, s), (Token::Eof, s)];
        let mut cur = Cursor::new(&tokens);
        let result = parse_block(&mut cur);
        assert!(result.is_err(), "expected Err(P014), got Ok");
        assert_eq!(
            result.unwrap_err().code(),
            "P014",
            "expected error code P014"
        );
    }

    // ── T9-4: parse_block_trailing_newline_direct ─────────────────────────────
    //
    // A NEWLINE immediately before DEDENT must be consumed gracefully; the
    // resulting block contains only the stmts before the trailing NEWLINE.

    #[test]
    fn parse_block_trailing_newline_direct() {
        // Token stream: INDENT  x  NEWLINE  DEDENT  EOF
        // The NEWLINE comes right before DEDENT — parse_block must handle this.
        let s = Span::point(0);
        let tokens: Vec<(Token, Span)> = vec![
            (Token::Indent, s),
            (Token::LowerIdent("x".to_string()), s),
            (Token::Newline, s),
            (Token::Dedent, s),
            (Token::Eof, s),
        ];
        let mut cur = Cursor::new(&tokens);
        let block = parse_block(&mut cur).expect("expected Ok");
        assert_eq!(
            block.stmts.len(),
            1,
            "expected 1 stmt, got {}",
            block.stmts.len()
        );
        assert!(matches!(&block.stmts[0], Expr::Ident(id) if id.text == "x"));
    }
}
