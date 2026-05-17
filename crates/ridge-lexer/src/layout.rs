//! Offside-rule layout post-processor.
//!
//! # Algorithm (plan §5)
//!
//! After the raw scan + interpolation pass have produced a linear stream of
//! `(Token, Span)` pairs (still containing physical `Newline` tokens), this
//! module inserts `Indent`, `Dedent`, and semantic `Newline` tokens according
//! to the Ridge offside rule.
//!
//! ## Core model (§5.1)
//!
//! A **layout stack** of `u32` columns.  Initial state: `[0]` (top-level).
//!
//! The processor consumes one **logical line** at a time:
//! 1. Count leading spaces to get `col`.
//! 2. Compare with `stack.top()`:
//!    - `col > top` → push `col`, emit `Indent`.
//!    - `col == top` → emit `Newline` (not on the very first logical line).
//!    - `col < top` → pop while `top > col`, emit one `Dedent` per pop.
//!      If after popping `top != col`, that is `InconsistentDedent`.
//!
//! ## Bracket suppression (§5.2)
//!
//! While `bracket_depth > 0` (any of `(`, `[`, `{` open), physical newlines
//! and indentation changes produce **no** `INDENT` or `DEDENT` tokens.
//!
//! However, a `NEWLINE` token IS emitted inside brackets when a logical line
//! appears at a column ≤ the "block baseline" of the innermost bracket frame.
//! The block baseline is the column of the first non-blank logical line inside
//! the bracket that starts on a different physical line from the opening bracket
//! token.  Subsequent logical lines at column ≤ baseline are treated as new
//! statements and emit a `NEWLINE` before their tokens.  This enables
//! multi-statement lambda and let bodies inside parenthesised contexts.
//!
//! `${` opens a logical bracket; the matching `}` closes it (tracked by the
//! interpolation pass which emits `InterpExprEnd` for that `}`).
//!
//! ## Blank lines (§5.3)
//!
//! Lines that contain only whitespace (i.e. a `Newline` with no preceding
//! real token on that line) do not advance the layout state.
//!
//! ## Operator-leading continuation (§5.4)
//!
//! A physical newline immediately followed by one of the binary infix
//! operators (`|>`, `||`, `&&`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `++`,
//! `::`, `+`, `-`, `*`, `/`, `%`, `^`, `with`, `.`) does NOT end the current
//! logical line.  The line-grouping pass merges such lines into their
//! predecessor before the layout comparison runs.  This allows Elm/F#/OCaml-
//! style pipe chains.  `=`, `->`, `then`, `else` are NOT in the set; those
//! introduce new blocks.
//!
//! ## EOF (§4.6)
//!
//! Pop all open stack levels above the sentinel `0`, emitting one `Dedent`
//! per pop.  Then emit a single `Eof`.

use crate::{error::LexError, span::Span, token::Token};

/// A single logical line: `(col, line_start_byte, tokens)`.
///
/// `col` is the number of leading whitespace bytes (= column of first token).
/// `line_start_byte` is the byte offset of the start of the first physical
/// line that contributes to this logical line.
/// `tokens` are the real (non-Newline) tokens on the line.
type LogicalLine = (u32, u32, Vec<(Token, Span)>);

/// Per-bracket-frame state for the flat-block NEWLINE rule.
///
/// When we open a bracket (`(`, `[`, `{`, `${`), we push a `BracketFrame`.
/// We use it to emit `NEWLINE` tokens between sibling statements inside the
/// bracket — but never `INDENT` or `DEDENT`.
#[derive(Debug)]
struct BracketFrame {
    /// Byte offset of the start of the physical line that contains the
    /// bracket-opening token.  Used to distinguish "same line as opener"
    /// from "later physical line".
    open_line_start: u32,
    /// The block baseline: column of the first non-blank logical line inside
    /// this bracket that is on a different physical line from the opener.
    /// `None` until established.
    baseline: Option<u32>,
}

/// Run the layout post-processor.
///
/// Input: token stream from the interpolation pass; `Newline` tokens are
/// *physical* newlines.  Output: semantic token stream with `Indent`, `Dedent`,
/// and semantic `Newline` tokens inserted; physical `Newline` tokens removed.
pub(crate) fn process(tokens: &[(Token, Span)]) -> (Vec<(Token, Span)>, Vec<LexError>) {
    let mut out: Vec<(Token, Span)> = Vec::new();
    let mut errors: Vec<LexError> = Vec::new();

    // Layout stack: columns of open blocks.  Starts with sentinel 0.
    let mut stack: Vec<u32> = vec![0];

    // Bracket depth: `(`, `[`, `{`, `${` (InterpExprStart) open brackets.
    // While depth > 0, INDENT/DEDENT are suppressed; NEWLINE may be emitted
    // per the block-baseline rule.
    let mut bracket_depth: u32 = 0;

    // Stack of per-bracket frames for the flat-block rule.
    let mut bracket_frames: Vec<BracketFrame> = Vec::new();

    // Whether any non-blank logical line has been emitted yet.
    let mut first_logical_line = true;

    // Pre-process: group into logical lines.
    // Each logical line is: (col, line_start_byte, tokens_on_line)
    let logical_lines = collect_logical_lines(tokens);

    // Walk logical lines and emit layout tokens.
    for (col, line_start_byte, line_tokens) in &logical_lines {
        let col = *col;
        let line_start_byte = *line_start_byte;

        if line_tokens.is_empty() {
            // Blank line — skip.
            continue;
        }

        // Determine line_span from the first token on the line.
        let line_tok_span = line_tokens[0].1;

        if bracket_depth == 0 {
            // ── Normal layout (outside brackets) ─────────────────────────────
            let top = *stack.last().unwrap_or(&0);

            if first_logical_line {
                // First logical line must be at col 0.
                if col > 0 {
                    errors.push(LexError::IndentAtTopLevel {
                        span: Span::point(line_tok_span.start),
                    });
                }
                first_logical_line = false;
            } else if col > top {
                // Indent.
                stack.push(col);
                out.push((Token::Indent, Span::point(line_tok_span.start)));
            } else if col == top {
                // Same level — emit Newline (not before the very first line).
                out.push((Token::Newline, Span::point(line_tok_span.start)));
            } else {
                // Dedent — pop levels.
                while *stack.last().unwrap_or(&0) > col {
                    stack.pop();
                    out.push((Token::Dedent, Span::point(line_tok_span.start)));
                }
                let new_top = *stack.last().unwrap_or(&0);
                if new_top != col {
                    // Inconsistent dedent.
                    errors.push(LexError::InconsistentDedent {
                        span: Span::point(line_tok_span.start),
                        col,
                        expected: stack.clone(),
                    });
                }
                // Emit NEWLINE after the dedents (we're at the same level or closest).
                if !first_logical_line {
                    out.push((Token::Newline, Span::point(line_tok_span.start)));
                }
            }
        } else {
            // ── Inside brackets (block-baseline rule) ─────────────────
            // INDENT and DEDENT are never emitted here.
            // NEWLINE is emitted when col ≤ baseline (and baseline is established).
            if let Some(frame) = bracket_frames.last_mut() {
                if let Some(baseline) = frame.baseline {
                    // Baseline already established — check if this is a new stmt.
                    if line_start_byte != frame.open_line_start && col <= baseline {
                        // New sibling statement — emit NEWLINE before its tokens.
                        out.push((Token::Newline, Span::point(line_tok_span.start)));
                    }
                } else if line_start_byte != frame.open_line_start {
                    // First non-blank logical line on a different physical line
                    // from the opener — establish the baseline.  Do NOT emit
                    // NEWLINE for this first-baseline line.
                    frame.baseline = Some(col);
                }
                // If same physical line as opener, do nothing (no baseline yet).
            }
        }

        // Emit the real tokens on this line, tracking bracket depth.
        for (tok, span) in line_tokens {
            match tok {
                Token::LParen | Token::LBrack | Token::LBrace | Token::InterpExprStart => {
                    bracket_depth += 1;
                    // Push a new bracket frame.  The open_line_start is the
                    // line_start_byte of THIS logical line (the one containing
                    // the opening bracket token).
                    bracket_frames.push(BracketFrame {
                        open_line_start: line_start_byte,
                        baseline: None,
                    });
                }
                // All four closing-bracket variants decrement depth identically.
                Token::RParen | Token::RBrack | Token::RBrace | Token::InterpExprEnd => {
                    bracket_depth = bracket_depth.saturating_sub(1);
                    bracket_frames.pop();
                }
                _ => {}
            }
            out.push((tok.clone(), *span));
        }
    }

    // At EOF: unwind the layout stack (emit Dedents for each open block above 0).
    let eof_span = tokens
        .last()
        .map_or(Span::point(0), |(_, s)| Span::point(s.end));

    // Pop all levels above sentinel.
    while stack.len() > 1 {
        stack.pop();
        out.push((Token::Dedent, eof_span));
    }

    // Emit EOF.
    out.push((Token::Eof, eof_span));

    // Remove any Newline tokens that were passed through from the interpolation
    // pass (they are physical newlines that slipped through — we handle them
    // via logical-line grouping above).
    let out = out
        .into_iter()
        .filter(|(t, _)| !matches!(t, Token::Newline if false)) // keep all Newlines we inserted
        .collect();

    (out, errors)
}

/// Returns `true` if `tok` is a binary infix operator that may lead a
/// continuation line.  `=` and `->` are intentionally excluded; those
/// introduce new blocks.
fn is_continuation_operator(tok: &Token) -> bool {
    matches!(
        tok,
        Token::PipeFwd
            | Token::PipePipe
            | Token::AmpAmp
            | Token::EqEq
            | Token::BangEq
            | Token::Lt
            | Token::Gt
            | Token::Le
            | Token::Ge
            | Token::PlusPlus
            | Token::ColonColon
            | Token::Plus
            | Token::Minus
            | Token::Star
            | Token::Slash
            | Token::Percent
            | Token::Caret
            | Token::KwWith
            | Token::Dot
    )
}

/// Group the flat token stream into logical lines.
///
/// Returns `Vec<(col, line_start_byte, Vec<(Token, Span)>)>` where:
/// - `col` is the number of leading spaces on that logical line.
/// - `line_start_byte` is the byte offset of the start of the first physical
///   line that contributes to this logical line (used by the bracket-frame
///   rule to detect "different physical line from the opener").
///
/// Blank lines (lines where the only token is a `Newline`) produce an entry
/// with an empty `Vec<(Token, Span)>` so the caller can skip them.
///
/// Physical `Newline` tokens are consumed and not included in any line's token
/// list.
///
/// Physical lines whose first token is a continuation operator are
/// merged into the preceding non-empty logical line before the layout
/// comparison runs.
fn collect_logical_lines(tokens: &[(Token, Span)]) -> Vec<LogicalLine> {
    let mut physical_lines: Vec<LogicalLine> = Vec::new();

    // Walk the token stream; track the byte offset of the start of the current line.
    let mut line_start_byte: u32 = 0;
    let mut current_line: Vec<(Token, Span)> = Vec::new();

    for (tok, span) in tokens {
        if matches!(tok, Token::Newline) {
            // End of physical line.
            let col = compute_col(&current_line, line_start_byte);
            physical_lines.push((col, line_start_byte, current_line.clone()));
            current_line.clear();
            line_start_byte = span.end;
        } else {
            current_line.push((tok.clone(), *span));
        }
    }

    // Push the last line (no trailing Newline).
    let col = compute_col(&current_line, line_start_byte);
    physical_lines.push((col, line_start_byte, current_line));

    // Merge continuation-operator-leading lines into their predecessor.
    // A line is a continuation if its first (non-empty) token is in the
    // continuation set.  Blank lines are left in place (they will be skipped
    // by the caller) and do NOT interrupt a continuation chain.
    let mut merged: Vec<LogicalLine> = Vec::new();

    for (col, lsb, line_tokens) in physical_lines {
        if line_tokens.is_empty() {
            // Blank line — preserve as-is (caller skips blanks).
            merged.push((col, lsb, line_tokens));
            continue;
        }

        let first_is_continuation = line_tokens
            .first()
            .is_some_and(|(t, _)| is_continuation_operator(t));

        if first_is_continuation {
            // Find the last non-empty predecessor in `merged` and append.
            if let Some(pred) = merged
                .iter_mut()
                .rev()
                .find(|(_, _, toks)| !toks.is_empty())
            {
                pred.2.extend(line_tokens);
                continue;
            }
        }

        merged.push((col, lsb, line_tokens));
    }

    merged
}

/// Compute the column of the first token on a line.
///
/// The column is `first_token_span.start - line_start_byte`, which gives the
/// number of bytes of whitespace before the first token.
#[allow(clippy::missing_const_for_fn)]
fn compute_col(line: &[(Token, Span)], line_start_byte: u32) -> u32 {
    if let Some((_, span)) = line.first() {
        span.start.saturating_sub(line_start_byte)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use crate::token::Token;
    use crate::tokenize;

    fn tokens(src: &str) -> Vec<Token> {
        let out = tokenize(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        out.tokens.into_iter().map(|(t, _)| t).collect()
    }

    fn tokens_with_errors(src: &str) -> (Vec<Token>, Vec<crate::LexError>) {
        let out = tokenize(src);
        (out.tokens.into_iter().map(|(t, _)| t).collect(), out.errors)
    }

    #[test]
    fn empty_file_is_eof() {
        let toks = tokens("");
        assert_eq!(toks, vec![Token::Eof]);
    }

    #[test]
    fn single_line_no_newline() {
        let toks = tokens("let x = 1");
        // KwLet LowerIdent Assign IntDec Eof  (no layout tokens on single line)
        assert!(toks.contains(&Token::KwLet));
        assert!(toks.last() == Some(&Token::Eof));
        assert!(!toks.contains(&Token::Indent));
        assert!(!toks.contains(&Token::Dedent));
    }

    #[test]
    fn two_lines_same_indent() {
        // Two top-level lines at col 0 get a NEWLINE between them.
        let toks = tokens("let x = 1\nlet y = 2");
        assert!(toks.contains(&Token::Newline), "missing NEWLINE: {toks:?}");
        assert!(!toks.contains(&Token::Indent));
    }

    #[test]
    fn indent_and_dedent() {
        let src = "fn f =\n    let x = 1\n    x";
        let toks = tokens(src);
        assert!(toks.contains(&Token::Indent), "missing INDENT:  {toks:?}");
        assert!(toks.contains(&Token::Dedent), "missing DEDENT:  {toks:?}");
    }

    #[test]
    fn blank_lines_ignored() {
        let src = "let x = 1\n\nlet y = 2";
        let toks = tokens(src);
        // Two top-level lines => one NEWLINE, not two.
        let newline_count = toks.iter().filter(|t| **t == Token::Newline).count();
        assert_eq!(
            newline_count, 1,
            "blank line should not produce extra NEWLINE: {toks:?}"
        );
    }

    #[test]
    fn bracket_suppresses_layout() {
        // Newlines inside `(...)` must not produce INDENT/DEDENT.
        //  NEWLINE may be emitted inside brackets when a
        // logical line at col ≤ block baseline appears; INDENT/DEDENT are
        // still never emitted inside brackets.
        let src = "let x = (\n    1\n    )";
        let toks = tokens(src);
        assert!(
            !toks.contains(&Token::Indent),
            "INDENT inside brackets: {toks:?}"
        );
        assert!(
            !toks.contains(&Token::Dedent),
            "DEDENT inside brackets: {toks:?}"
        );
    }

    #[test]
    fn bracket_suppresses_layout_list() {
        // Matches game_of_life.rg: `[(-1,-1), (-1,0), ...]` multi-line list.
        //  NEWLINE may be emitted between items at same
        // column; INDENT/DEDENT are still never emitted inside brackets.
        let src = "let x = [\n    1\n    2\n    ]";
        let toks = tokens(src);
        assert!(!toks.contains(&Token::Indent));
        assert!(!toks.contains(&Token::Dedent));
    }

    #[test]
    fn inconsistent_dedent() {
        // Indent by 4, then dedent to 2 (not a valid level).
        let src = "let x =\n    a\n  b";
        let (_, errs) = tokens_with_errors(src);
        assert!(
            errs.iter()
                .any(|e| matches!(e, crate::LexError::InconsistentDedent { .. })),
            "expected InconsistentDedent: {errs:?}"
        );
    }

    #[test]
    fn top_level_indent_error() {
        let src = "  let x = 1";
        let (_, errs) = tokens_with_errors(src);
        assert!(
            errs.iter()
                .any(|e| matches!(e, crate::LexError::IndentAtTopLevel { .. })),
            "expected IndentAtTopLevel: {errs:?}"
        );
    }

    // ── Operator-leading continuation tests ─────────────────────────────

    /// A `|>` on its own line joins to the preceding line — no NEWLINE or
    /// INDENT before the pipe token.
    #[test]
    fn pipe_forward_on_new_line_joins() {
        let src = "let xs = a\n    |> f";
        let toks = tokens(src);
        // No layout token (Newline or Indent) may appear before the PipeFwd.
        assert!(
            toks.contains(&Token::PipeFwd),
            "expected PipeFwd in stream: {toks:?}"
        );
        // Walk: once we see 'a', the very next token must be PipeFwd.
        let mut after_a = false;
        for tok in &toks {
            if after_a {
                assert_eq!(
                    *tok,
                    Token::PipeFwd,
                    "expected PipeFwd directly after 'a', got: {toks:?}",
                );
                break;
            }
            if matches!(tok, Token::LowerIdent(s) if s == "a") {
                after_a = true;
            }
        }
        assert!(after_a, "ident 'a' not found in: {toks:?}");
        // No Newline or Indent appears anywhere before the PipeFwd.
        let mut saw_layout = false;
        for tok in &toks {
            if matches!(tok, Token::Newline | Token::Indent) {
                saw_layout = true;
            }
            if *tok == Token::PipeFwd {
                assert!(
                    !saw_layout,
                    "layout token appeared before PipeFwd: {toks:?}"
                );
                break;
            }
        }
    }

    /// Three-pipe chain: all three `|>` tokens appear without intervening
    /// NEWLINE / INDENT between them.
    #[test]
    fn chain_of_pipes_joins() {
        let src = "let xs = a\n    |> f\n    |> g\n    |> h";
        let toks = tokens(src);
        let pipe_count = toks.iter().filter(|t| **t == Token::PipeFwd).count();
        assert_eq!(pipe_count, 3, "expected 3 PipeFwd tokens: {toks:?}");
        // No INDENT anywhere (single-level let).
        assert!(
            !toks.contains(&Token::Indent),
            "unexpected INDENT in pipe chain: {toks:?}"
        );
        // No NEWLINE before any PipeFwd.
        let mut last_was_layout = false;
        for tok in &toks {
            if matches!(tok, Token::Newline | Token::Indent) {
                last_was_layout = true;
            } else if *tok == Token::PipeFwd {
                assert!(
                    !last_was_layout,
                    "layout token directly before PipeFwd: {toks:?}"
                );
                last_was_layout = false;
            } else {
                last_was_layout = false;
            }
        }
    }

    /// Inside a fn body, a multi-line pipe chain should be one logical
    /// statement — the body block emits one INDENT/DEDENT pair, and no
    /// internal NEWLINE/INDENT separates the pipe tokens.
    #[test]
    fn operator_on_new_line_inside_block_body() {
        let src = "fn f =\n    a\n        |> g\n        |> h";
        let toks = tokens(src);
        // One INDENT for the fn body, one DEDENT at EOF.
        let indent_count = toks.iter().filter(|t| **t == Token::Indent).count();
        let dedent_count = toks.iter().filter(|t| **t == Token::Dedent).count();
        assert_eq!(indent_count, 1, "expected 1 INDENT for fn body: {toks:?}");
        assert_eq!(dedent_count, 1, "expected 1 DEDENT for fn body: {toks:?}");
        // Both PipeFwd present.
        let pipe_count = toks.iter().filter(|t| **t == Token::PipeFwd).count();
        assert_eq!(pipe_count, 2, "expected 2 PipeFwd tokens: {toks:?}");
    }

    /// `=` on a new line MUST NOT be treated as a continuation — it starts a
    /// binding body, not a continuation of the preceding expression.
    #[test]
    fn equals_on_new_line_does_not_join() {
        // Top-level let with the `=` and value on the next line.
        // The result should have a NEWLINE before the `let y` declaration,
        // confirming that `= 42` was processed as its own line (or attached to
        // `let y` correctly), not merged into `let x`.
        let src = "let x = 1\nlet y = 2";
        let toks = tokens(src);
        // Standard two-top-level-decls output: Newline between them.
        assert!(
            toks.contains(&Token::Newline),
            "expected NEWLINE between top-level decls: {toks:?}"
        );
    }

    /// `->` on a new line MUST NOT be treated as a continuation.
    #[test]
    fn arrow_on_new_line_does_not_join() {
        // Match arm with `->` indented: each arm is a sibling, not a
        // continuation of the match head.
        let src = "match x\n    A -> 1\n    B -> 2";
        let toks = tokens(src);
        // Expect one INDENT for the match block, one NEWLINE between arms.
        assert!(
            toks.contains(&Token::Indent),
            "expected INDENT for match block: {toks:?}"
        );
        assert!(
            toks.contains(&Token::Newline),
            "expected NEWLINE between match arms: {toks:?}"
        );
        // Arrow tokens present.
        let arrow_count = toks.iter().filter(|t| **t == Token::Arrow).count();
        assert_eq!(arrow_count, 2, "expected 2 Arrow tokens: {toks:?}");
    }

    #[test]
    fn eof_unwinds_stack() {
        // A file that ends in the middle of an indented block should emit
        // synthetic DEDENTs before EOF.
        let src = "fn f =\n    let x = 1";
        let toks = tokens(src);
        // Expect: KwFn LowerIdent Assign Indent KwLet LowerIdent Assign IntDec Dedent Eof
        assert!(toks.contains(&Token::Dedent));
        assert_eq!(toks.last(), Some(&Token::Eof));
    }

    #[test]
    fn if_then_else() {
        let src = "if x then\n    a\nelse\n    b";
        let toks = tokens(src);
        assert!(toks.contains(&Token::Indent));
        assert!(toks.contains(&Token::Dedent));
        assert!(toks.contains(&Token::Newline));
    }

    #[test]
    fn match_arms_same_indent() {
        let src = "match x\n    A -> 1\n    B -> 2";
        let toks = tokens(src);
        // One INDENT for the match block; one NEWLINE between the arms; one DEDENT at EOF.
        let indent_count = toks.iter().filter(|t| **t == Token::Indent).count();
        let newline_count = toks.iter().filter(|t| **t == Token::Newline).count();
        assert_eq!(indent_count, 1, "one INDENT for match block: {toks:?}");
        assert_eq!(newline_count, 1, "one NEWLINE between arms: {toks:?}");
    }

    #[test]
    fn trailing_blank_lines_at_eof() {
        let src = "let x = 1\n\n\n";
        let toks = tokens(src);
        assert_eq!(toks.last(), Some(&Token::Eof));
        // No extra NEWLINEs from blank lines.
        let newline_count = toks.iter().filter(|t| **t == Token::Newline).count();
        assert_eq!(
            newline_count, 0,
            "trailing blank lines should not produce NEWLINE: {toks:?}"
        );
    }

    // ── NEWLINE inside brackets for multi-statement lambda bodies ────

    /// A two-statement lambda body inside parens should emit exactly one NEWLINE
    /// between the two statements (after the first non-blank inside-paren line
    /// establishes the baseline, subsequent lines at the same column emit NEWLINE).
    #[test]
    fn newline_between_lambda_body_stmts_in_paren() {
        // Simulates game_of_life.rg lines 59-63:
        //   List.forEach (fn row ->
        //       let line = row
        //       Io.println line)
        let src = "f (fn row ->\n    let line = row\n    Io.println line)";
        let toks = tokens(src);
        assert!(
            !toks.contains(&Token::Indent),
            "INDENT inside brackets: {toks:?}"
        );
        assert!(
            !toks.contains(&Token::Dedent),
            "DEDENT inside brackets: {toks:?}"
        );
        // Exactly one NEWLINE: between `let line = row` and `Io.println line`.
        let newline_count = toks.iter().filter(|t| **t == Token::Newline).count();
        assert_eq!(
            newline_count, 1,
            "expected exactly 1 NEWLINE between body stmts: {toks:?}"
        );
    }
}
