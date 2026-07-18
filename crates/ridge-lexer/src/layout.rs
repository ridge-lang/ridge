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
//! ## Operator-trailing continuation (§5.6)
//!
//! The mirror of §5.4: a physical line that ENDS with one of those binary infix
//! operators does not end the logical line either, because the operator still
//! needs a right-hand side.  The following line is merged into it, so
//!
//! ```text
//! let total = subtotal +
//!     shipping
//! ```
//!
//! reads as one expression.  Since a binary operator at end of line always
//! demands a continuation, no previously valid program ended a logical line
//! there, so enabling the merge changes no valid program's shape.
//!
//! ## Bracket-leading argument continuation (§5.5)
//!
//! A logical line that is more indented than the current block and opens with
//! `[`, `(`, or `{` is treated as a *continuation argument* of the preceding
//! line rather than a new INDENT block, so multi-line calls such as
//!
//! ```text
//! users
//!     |> Repo.setWhere
//!         [ Repo.set (fn (u) -> u.age) 40 ]
//!         (fn (u) -> u.id == 1)
//! ```
//!
//! parse the bracketed atoms as further arguments (the parser's juxtaposition
//! rule does the rest).  The suppression fires only when the previous line did
//! NOT end with a block introducer (`=`, `->`, `then`, `else`, `try`) and the
//! line is not itself a `match` arm (no top-level `->`).  Those guards mean a
//! suppressed INDENT could only ever have been a layout error before, so no
//! previously valid program changes shape.
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

    // Last real token of the previous non-blank logical line.  Drives the
    // bracket-leading argument-continuation rule (§5.5): it separates a
    // continued call argument from a block body opened by `=`/`->`/`then`/…
    let mut prev_last_tok: Option<Token> = None;

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
                // A more-indented line normally opens an INDENT block — unless
                // it is a bracket-leading argument continuation of the previous
                // logical line (§5.5).  Suppress the INDENT in that case so the
                // parser folds the bracketed atom in as another call argument;
                // the real tokens below flow straight onto the previous line.
                if !is_bracket_arg_continuation(line_tokens, prev_last_tok.as_ref()) {
                    stack.push(col);
                    out.push((Token::Indent, Span::point(line_tok_span.start)));
                }
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
                } else if line_start_byte != frame.open_line_start
                    && !is_bracket_arg_continuation(line_tokens, prev_last_tok.as_ref())
                {
                    // First non-blank logical line on a different physical line
                    // from the opener — establish the baseline.  Do NOT emit
                    // NEWLINE for this first-baseline line.
                    //
                    // Skip a line that is itself a bracket-leading continuation
                    // argument of the opener (a list element whose nested
                    // `[ … ]`/`( … )` argument opens on the next indented line).
                    // Such a line sits deeper than the true sibling column, so
                    // adopting it as the baseline would make every following
                    // element's own nested bracket read as a new statement and
                    // split the element in two (the `expected ] but found [`
                    // papercut).  Defer to the first real sibling — a
                    // leading-comma element, or a same-column statement — to fix
                    // the baseline instead.
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

        // Record the last real token so the next line's continuation check
        // (§5.5) can tell a continued argument from a block-introducing line.
        if let Some((last, _)) = line_tokens.last() {
            prev_last_tok = Some(last.clone());
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

/// Returns `true` when `line_tokens` is a bracket-leading argument continuation
/// of the preceding logical line (§5.5) — it should fold onto that line as
/// another call argument instead of opening a fresh INDENT block.
///
/// Three conditions must hold:
/// - the line leads with an opening bracket (`[`, `(`, `{`);
/// - the previous line did not end with a block introducer (`=`, `->`, `then`,
///   `else`, `try`), which would make this line a block body; and
/// - the line is not a `match` arm (no `->` at bracket depth 0).
///
/// Together these mean a suppressed INDENT here could only have been a layout
/// error before, so enabling the continuation regresses no valid program.
fn is_bracket_arg_continuation(
    line_tokens: &[(Token, Span)],
    prev_last_tok: Option<&Token>,
) -> bool {
    let leads_with_bracket = matches!(
        line_tokens.first(),
        Some((Token::LParen | Token::LBrack | Token::LBrace, _))
    );
    if !leads_with_bracket {
        return false;
    }

    if matches!(
        prev_last_tok,
        Some(Token::Assign | Token::Arrow | Token::KwThen | Token::KwElse | Token::KwTry)
    ) {
        return false;
    }

    !line_has_top_level_arrow(line_tokens)
}

/// Returns `true` if `line_tokens` contains an `Arrow` (`->`) at bracket depth 0
/// (outside every `(`/`[`/`{`/`${`).  This marks a `match` arm pattern such as
/// `[] -> …` or `(a, b) -> …`, distinguishing it from a bracketed call argument
/// whose own `->` (e.g. a lambda) is always nested inside a bracket.
fn line_has_top_level_arrow(line_tokens: &[(Token, Span)]) -> bool {
    let mut depth: i32 = 0;
    for (tok, _) in line_tokens {
        match tok {
            Token::LParen | Token::LBrack | Token::LBrace | Token::InterpExprStart => depth += 1,
            Token::RParen | Token::RBrack | Token::RBrace | Token::InterpExprEnd => depth -= 1,
            Token::Arrow if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// Returns `true` if `tok` may lead a continuation line but must NOT continue
/// a line when it *trails* one.  This is the leading-only set.
///
/// The one member is `->` (`Arrow`).  A line that *starts* with `->` is always
/// completing the previous line — a function signature whose return type wraps
/// (`fn f (a) (b)\n    -> Ret =`) or a lambda header split across lines.  It is
/// never the start of a fresh construct, since a `match` arm carries its `->`
/// *after* the pattern, never at the head of a line.  In trailing position the
/// opposite holds: `pat ->` and `fn x ->` open a block that the following
/// indented line belongs to, so `->` stays out of [`is_continuation_operator`]
/// (which drives the trailing-merge in §5.6).
fn is_leading_continuation_operator(tok: &Token) -> bool {
    is_continuation_operator(tok) || matches!(tok, Token::Arrow)
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
            .is_some_and(|(t, _)| is_leading_continuation_operator(t));

        // §5.6: merge when the previous non-empty line ENDS with a continuation
        // operator — a trailing binary infix operator still needs its right-hand
        // side, so this line completes it.
        let pred_trails_operator = merged
            .iter()
            .rev()
            .find(|(_, _, toks)| !toks.is_empty())
            .and_then(|(_, _, toks)| toks.last())
            .is_some_and(|(t, _)| is_continuation_operator(t));

        if first_is_continuation || pred_trails_operator {
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
        // Matches game_of_life.ridge: `[(-1,-1), (-1,0), ...]` multi-line list.
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

    /// A line ending with a binary operator (§5.6) continues on the next line;
    /// no NEWLINE/INDENT separates the operator from its right-hand side.
    #[test]
    fn trailing_operator_joins_next_line() {
        let toks = tokens("let x = 1 +\n    2");
        assert!(
            !toks.contains(&Token::Indent) && !toks.contains(&Token::Newline),
            "unexpected layout token in trailing-op continuation: {toks:?}"
        );
        let after_plus = toks
            .iter()
            .position(|t| *t == Token::Plus)
            .and_then(|i| toks.get(i + 1));
        assert!(
            matches!(after_plus, Some(Token::IntDec(s)) if s == "2"),
            "expected `2` directly after `+`: {toks:?}"
        );
    }

    /// The trailing-operator and leading-operator continuation forms tokenize
    /// to the same stream.
    #[test]
    fn trailing_and_leading_operator_agree() {
        let trailing = tokens("let x = a +\n    b");
        let leading = tokens("let x = a\n    + b");
        assert_eq!(
            trailing, leading,
            "trailing- and leading-operator continuations should tokenize alike"
        );
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
        // Simulates game_of_life.ridge lines 59-63:
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

    // ── §5.5 bracket-leading argument continuation ───────────────────────────

    fn indents(src: &str) -> usize {
        tokens(src).iter().filter(|t| **t == Token::Indent).count()
    }

    /// A `[`-leading, more-indented line after a non-block-introducer folds onto
    /// the previous line as a call argument — no extra INDENT.
    #[test]
    fn bracket_arg_continuation_suppresses_indent() {
        // fn body is the only block; `[ a ]` continues `users |> setWhere`.
        let src = "fn f =\n    users\n        |> setWhere\n            [ a ]";
        assert_eq!(
            indents(src),
            1,
            "only the fn body should INDENT: {:?}",
            tokens(src)
        );
    }

    /// `(`-leading and `[`-leading continuation lines chain onto the same call.
    #[test]
    fn chained_bracket_and_paren_args_suppress_indent() {
        let src = "fn f =\n    users\n        |> setWhere\n            [ a ]\n            (b)";
        let toks = tokens(src);
        assert_eq!(
            toks.iter().filter(|t| **t == Token::Indent).count(),
            1,
            "only the fn body should INDENT: {toks:?}"
        );
        // No INDENT/DEDENT separates the call from its bracketed args.
        assert_eq!(
            toks.iter().filter(|t| **t == Token::Dedent).count(),
            1,
            "exactly one DEDENT (fn body close at EOF): {toks:?}"
        );
    }

    /// A `{`-leading record-literal argument is also a continuation.
    #[test]
    fn brace_arg_continuation_suppresses_indent() {
        let src = "fn f =\n    update row\n        { a = 1 }";
        assert_eq!(indents(src), 1, "{:?}", tokens(src));
    }

    /// An arrow nested inside the bracketed argument (a lambda) must NOT be read
    /// as a `match` arm — the continuation still fires.
    #[test]
    fn lambda_arrow_inside_bracket_arg_still_continues() {
        let src = "fn f =\n    users\n        |> setWhere\n            (fn u -> u)";
        assert_eq!(indents(src), 1, "{:?}", tokens(src));
    }

    /// A `match` arm whose pattern is `[]` keeps its INDENT — the top-level `->`
    /// marks it as an arm, not a continuation.
    #[test]
    fn match_empty_list_arm_keeps_indent() {
        let src = "fn f =\n    match xs\n        [] -> 0\n        h :: t -> 1";
        // fn body + match arms = 2 INDENTs.
        assert_eq!(indents(src), 2, "{:?}", tokens(src));
        let arrows = tokens(src).iter().filter(|t| **t == Token::Arrow).count();
        assert_eq!(arrows, 2, "both arm arrows present: {:?}", tokens(src));
    }

    /// A `match` arm with a tuple pattern likewise keeps its INDENT.
    #[test]
    fn match_tuple_arm_keeps_indent() {
        let src = "fn f =\n    match p\n        (a, b) -> 0\n        _ -> 1";
        assert_eq!(indents(src), 2, "{:?}", tokens(src));
    }

    /// A bracket-leading `let` body (previous line ends in `=`) keeps its INDENT.
    #[test]
    fn let_body_bracket_keeps_indent() {
        let src = "fn f =\n    let x =\n        [ 1 ]\n    x";
        // fn body + let value block = 2 INDENTs.
        assert_eq!(indents(src), 2, "{:?}", tokens(src));
    }

    /// A bracket-leading match-arm body (previous line ends in `->`) keeps its
    /// INDENT.
    #[test]
    fn arm_body_bracket_keeps_indent() {
        let src = "fn f =\n    match xs\n        A ->\n            [ 1 ]";
        // fn body + match arms + arm body = 3 INDENTs.
        assert_eq!(indents(src), 3, "{:?}", tokens(src));
    }

    /// A `match` nested in a `let` value still indents its arms — the arm shape
    /// (top-level `->`), not the head keyword, is what protects it.
    #[test]
    fn nested_match_in_let_keeps_indent() {
        let src = "fn f =\n    let r = match xs\n        [] -> 0\n        a :: b -> 1\n    r";
        // fn body + match arms = 2 INDENTs.
        assert_eq!(indents(src), 2, "{:?}", tokens(src));
    }

    /// A bracket-leading line at the SAME column as its sibling is a separate
    /// statement, not a continuation (the rule only fires for `col > top`).
    #[test]
    fn sibling_bracket_at_same_col_not_continuation() {
        let src = "fn f =\n    compute\n    [ 1 ]";
        let toks = tokens(src);
        assert_eq!(
            toks.iter().filter(|t| **t == Token::Indent).count(),
            1,
            "only the fn body INDENTs: {toks:?}"
        );
        assert!(
            toks.contains(&Token::Newline),
            "sibling statements separated by NEWLINE: {toks:?}"
        );
    }

    // ── continuation-line layout (wrapped signatures, nested lists) ──────────

    /// A `fn` signature whose return type wraps onto a `->`-leading line joins
    /// that line to the header instead of opening a stray INDENT block that the
    /// body then dedents out of inconsistently.
    #[test]
    fn wrapped_signature_return_type_joins_header() {
        let src = "fn seed (a: Int) (b: Int)\n        -> Result Unit Error =\n    let x = a\n    x";
        let (toks, errs) = tokens_with_errors(src);
        assert!(
            errs.is_empty(),
            "wrapped signature must not raise a layout error: {errs:?}"
        );
        // Only the fn body opens a block; the `->` line no longer INDENTs.
        assert_eq!(
            toks.iter().filter(|t| **t == Token::Indent).count(),
            1,
            "only the fn body should INDENT: {toks:?}"
        );
        // The `->` follows the parameter list with no intervening layout token.
        assert!(
            toks.windows(2)
                .any(|w| w[0] == Token::RParen && w[1] == Token::Arrow),
            "`->` should sit right after the last param's `)`: {toks:?}"
        );
    }

    /// A `->`-leading line joins its predecessor, but a `->` that *trails* a
    /// line (a `match` arm, a lambda header) still opens a block — the leading
    /// and trailing roles of `->` stay distinct.
    #[test]
    fn trailing_arrow_still_opens_a_block() {
        let src = "fn f =\n    match xs\n        A ->\n            body";
        // fn body + match arms + arm body = 3 INDENTs; trailing `->` must not
        // have merged the arm body onto the arm line.
        assert_eq!(indents(src), 3, "{:?}", tokens(src));
    }

    /// A list whose elements carry a nested `[ … ]` argument that opens on the
    /// next indented line stays a flat, comma-separated stream: no spurious
    /// NEWLINE splits an element from its bracketed argument.
    #[test]
    fn nested_list_arg_on_continuation_line_emits_no_newline() {
        let src = "fn migrations () -> List Int =\n    [ mig \"0001\"\n        [ createSchema ]\n    , mig \"0002\"\n        [ createIndex ] ]";
        let toks = tokens(src);
        assert_eq!(
            toks.iter().filter(|t| **t == Token::Newline).count(),
            0,
            "a nested-bracket continuation must not inject a NEWLINE inside the list: {toks:?}"
        );
        assert_eq!(
            toks.iter().filter(|t| **t == Token::Indent).count(),
            1,
            "only the fn body may INDENT: {toks:?}"
        );
    }
}
