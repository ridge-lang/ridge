//! Control-flow and binding expression parsers (T7, grammar §§6.1–6.7).
//!
//! This module implements:
//!
//! - [`parse_if`]        — `if <cond> then <branch> [else <branch>]`
//! - [`parse_match`]     — `match <scrutinee> INDENT arms DEDENT`
//! - [`parse_let`]       — `let <pat> [: <ty>] = <expr>`
//! - [`parse_var_decl`]  — `var <ident> [: <ty>] = <expr>`
//! - [`parse_try`]       — `try INDENT block DEDENT`
//! - [`parse_guard`]     — `guard <cond> else <else-branch>`
//! - [`parse_return`]    — `return [<expr>]`
//!
//! All functions call `parse_expr_pratt` (the internal Pratt core from
//! `expr.rs`) rather than the public `parse_expr` to avoid re-dispatching on
//! keywords inside nested expression contexts.
//!
//! # Block branches (`Expr::Block`)
//!
//! When `if`/`else`, match arm bodies, or `guard else` branches encounter an
//! `INDENT` token, they call `parse_block` and wrap the result in
//! `Expr::Block`.  This is a T7 plan extension: the plan §3.7 lists `Expr`
//! variants without a `Block` wrapper, but the branch fields are `Box<Expr>`;
//! multi-statement blocks must therefore be representable as an `Expr`.

#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use ridge_ast::{Block, Expr, Ident, MatchArm};
use ridge_lexer::Token;

use crate::{
    block::parse_block,
    cursor::Cursor,
    error::ParseError,
    expr::{parse_expr, parse_expr_pratt},
    pattern::{parse_match_pattern, parse_pattern},
    ty::parse_type,
};

// ── parse_if ─────────────────────────────────────────────────────────────────

/// Parse an `if` expression (grammar §6.3).
///
/// ```text
/// if <cond> then <branch> [else <branch>]
/// ```
///
/// Precondition: `cur.peek() == &Token::KwIf`.
///
/// Each branch is either a single-line `Expr` or an INDENT-delimited `Block`
/// (wrapped as `Expr::Block`).
pub(crate) fn parse_if(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `if`

    let cond = parse_expr_pratt(cur)?;

    // `then` may sit on the next line at the same indent as `if`, separated from
    // the condition by a layout Newline. Skip it, but only when `then` actually
    // follows so no other separator is disturbed.
    if cur.peek() == &Token::Newline && cur.peek_n(1) == Some(&Token::KwThen) {
        cur.bump();
    }

    cur.expect(&Token::KwThen)?;

    let then_branch = parse_branch_body(cur)?;

    // Skip a Newline between the then-branch and an `else`, but ONLY when an
    // `else` actually follows.  This occurs when the then-branch is single-line
    // and `else` is on the next line:
    //   if x then 1
    //   else 2
    // In the else-less form the Newline is the statement separator the enclosing
    // block relies on, so it must be left in place — consuming it unconditionally
    // fused an else-less `if` with the statement that followed it.
    if cur.peek() == &Token::Newline && cur.peek_n(1) == Some(&Token::KwElse) {
        cur.bump();
    }

    let else_branch = if cur.peek() == &Token::KwElse {
        cur.bump(); // consume `else`
        Some(Box::new(parse_branch_body(cur)?))
    } else {
        None
    };

    let span_end = else_branch
        .as_ref()
        .map_or_else(|| then_branch.span(), |e| e.span());
    let span = start.merge(span_end);

    Ok(Expr::If {
        cond: Box::new(cond),
        then_branch: Box::new(then_branch),
        else_branch,
        span,
    })
}

// ── parse_match ───────────────────────────────────────────────────────────────

/// Parse a `match` expression (grammar §6.4).
///
/// ```text
/// match <scrutinee>
///     <pattern> [when <guard>] -> <body>
///     …
/// ```
///
/// Precondition: `cur.peek() == &Token::KwMatch`.
///
/// ## Error recovery (T12, §4.7)
///
/// If a single match arm fails to parse, tokens are skipped to the next
/// `NEWLINE` or `DEDENT` and parsing continues with the next arm.
pub(crate) fn parse_match(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `match`

    // Detect bracket (no-layout) mode BEFORE parsing the scrutinee.
    // In layout mode an `Indent` token appears after the scrutinee.
    // In no-layout mode (bracket-suppressed) there is no `Indent`; instead
    // match-arm patterns follow immediately (distinguished by `->` at depth 0
    // with no `Indent` beforehand).
    //
    // We scan from the current position to find the first of:
    //   `Indent`   at depth 0  → layout mode
    //   `Arrow`    at depth 0  → no-layout mode (arm boundary marker)
    //   Eof / scope exit       → no-layout mode (safety)
    let no_layout = match_is_no_layout_mode(cur);

    // In no-layout mode the Pratt juxtaposition call (level 11) would greedily
    // consume the first arm's pattern-tuple `(pat1, pat2)` as a call argument.
    // Avoid this by parsing only an atom (level 12) as the scrutinee; match
    // scrutinees in bracket context are always atoms or paren-expressions.
    let scrutinee = if no_layout {
        crate::expr::parse_expr_atom12(cur)?
    } else {
        parse_expr_pratt(cur)?
    };

    // Consume optional NEWLINE before INDENT (layout: scrutinee on same line,
    // arms indented below).
    if cur.peek() == &Token::Newline {
        cur.bump();
    }

    if cur.peek() == &Token::Indent {
        // ── Layout mode: INDENT … DEDENT ─────────────────────────────────────
        cur.bump(); // consume `Indent`

        let mut arms: Vec<MatchArm> = Vec::new();
        let mut first_arm_error: Option<ParseError> = None;

        loop {
            match parse_match_arm(cur) {
                Ok(arm) => arms.push(arm),
                Err(e) => {
                    if first_arm_error.is_none() {
                        first_arm_error = Some(e);
                    }
                    // Recovery: skip to the next arm boundary.
                    sync_to_next_match_arm(cur);
                }
            }

            if cur.peek() == &Token::Newline {
                cur.bump(); // consume NEWLINE between arms
            }
            if cur.peek() == &Token::Dedent {
                break;
            }
            if cur.at_eof() {
                break;
            }
        }

        let dedent_span = cur.expect(&Token::Dedent)?;
        let span = start.merge(dedent_span);

        if let Some(e) = first_arm_error {
            return Err(e);
        }

        Ok(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
            span,
        })
    } else {
        // ── No-layout mode (bracket-suppressed) ──────────────────────────────
        //
        // The column-rule:
        //   MATCH_COL = column of the first arm's first significant token.
        //   Terminate the arm loop when the next significant token's column
        //   drops BELOW MATCH_COL — it belongs to the outer expression.
        //
        // This lets a nested `match` inside an arm body end naturally when the
        // outer match's next arm appears at a smaller column, without requiring
        // parentheses or an explicit `end` keyword.
        let mut arms: Vec<MatchArm> = Vec::new();
        let mut end_span = scrutinee.span();

        // Skip leading Newlines so MATCH_COL is captured on the first arm.
        cur.skip_newlines();
        let match_col = cur.peek_significant_column();

        loop {
            // (1) Hard-stops: bracket closers and EOF always end the match.
            match cur.peek() {
                Token::RParen | Token::RBrack | Token::RBrace | Token::Eof => break,
                _ => {}
            }

            // (2) Consume an inter-arm Newline if present, then apply the
            //     column rule.  Both sides must have a column for the rule to
            //     fire; if `peek_significant_column` returns `None` (no LineMap
            //     available), the rule is skipped — safe-fallback behaviour.
            cur.skip_newlines();
            if let (Some(arm_col), Some(min_col)) = (cur.peek_significant_column(), match_col) {
                if arm_col < min_col {
                    break; // dedented past MATCH_COL → arm belongs to outer
                }
            }

            // (3) After consuming Newlines, re-check hard-stops (a Newline
            //     just before `)` etc. leaves us on the closer).
            match cur.peek() {
                Token::RParen | Token::RBrack | Token::RBrace | Token::Eof => break,
                _ => {}
            }

            match parse_match_arm_inner(cur, true) {
                Ok(arm) => {
                    end_span = arm.span;
                    arms.push(arm);
                }
                Err(_) => break, // no more arms (or unparseable)
            }
        }

        let span = start.merge(end_span);
        Ok(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
            span,
        })
    }
}

/// Return `true` if the match expression at the current cursor position is
/// inside bracket context (no layout tokens — "no-layout mode").
///
/// Scans forward from the current position looking for the first decisive
/// token at bracket depth 0:
/// - `Indent`                → layout mode (`false`)
/// - `Arrow` (`->`)          → no-layout mode (`true`); this is the first
///   arm's arrow, reached without an `Indent`
/// - scope exit / `Eof`      → no-layout mode (`true`)
///
/// Bracket depth tracking prevents confusing `->` inside an annotated type
/// (e.g. `(x: Int -> Bool)`) with a top-level arm arrow.
fn match_is_no_layout_mode(cur: &Cursor<'_>) -> bool {
    const SCAN_LIMIT: usize = 200;
    let mut depth: i32 = 0;
    for i in 0..SCAN_LIMIT {
        match cur.peek_n(i) {
            Some(Token::LParen | Token::LBrack | Token::LBrace) => depth += 1,
            Some(Token::RParen | Token::RBrack | Token::RBrace) => {
                depth -= 1;
                if depth < 0 {
                    return true; // exited enclosing scope without Indent
                }
            }
            Some(Token::Indent) if depth == 0 => return false, // layout mode
            Some(Token::Arrow) if depth == 0 => return true,   // no-layout: first arm arrow
            Some(Token::Eof) | None => return true,
            _ => {}
        }
    }
    true // scan limit reached: assume no-layout
}

/// Skip tokens to the next match-arm sync point: `NEWLINE` or `DEDENT`.
fn sync_to_next_match_arm(cur: &mut Cursor<'_>) {
    loop {
        match cur.peek() {
            Token::Newline | Token::Dedent | Token::Eof => return,
            _ => {
                cur.bump();
            }
        }
    }
}

/// Parse a single match arm: `<pattern> [when <guard>] -> <body>`.
///
/// When `no_layout` is `true` the arm body is parsed by
/// `parse_flat_block_arm_body` which can collect multiple sequential
/// statements without Indent/Dedent tokens (bracket-suppressed context).
fn parse_match_arm_inner(cur: &mut Cursor<'_>, no_layout: bool) -> Result<MatchArm, ParseError> {
    let start = cur.span();

    let pattern = parse_match_pattern(cur)?;

    let guard = if cur.peek() == &Token::KwWhen {
        cur.bump(); // consume `when`
        Some(parse_expr_pratt(cur)?)
    } else {
        None
    };

    cur.expect(&Token::Arrow)?;

    let body = if no_layout {
        parse_flat_block_arm_body(cur)?
    } else {
        parse_branch_body(cur)?
    };
    let span = start.merge(body.span());

    Ok(MatchArm {
        pattern,
        guard,
        body,
        span,
    })
}

/// Convenience wrapper for layout-mode match arms (existing call sites).
fn parse_match_arm(cur: &mut Cursor<'_>) -> Result<MatchArm, ParseError> {
    parse_match_arm_inner(cur, false)
}

/// Parse a no-layout match-arm body: collect sequential statements until a
/// new arm boundary is detected (by `is_match_arm_start`) or the token
/// stream can no longer start an expression.
///
/// Returns a single `Expr` if only one statement is parsed, or an
/// `Expr::Block` when multiple statements are collected.
///
/// This is needed for `match` expressions inside bracket context where the
/// lexer suppresses Indent/Dedent tokens so `parse_branch_body` would only
/// parse the very first statement.
fn parse_flat_block_arm_body(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    // Activate the no-layout arm barrier so that Pratt juxtaposition stops
    // before consuming the next arm's pattern as a call argument.
    let prev = cur.no_layout_arm;
    cur.no_layout_arm = true;

    let result = parse_flat_block_arm_body_inner(cur);

    cur.no_layout_arm = prev;
    result
}

fn parse_flat_block_arm_body_inner(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    // Capture the body column BEFORE parsing the first statement.
    // All subsequent statements in this body must start at column >= body_col.
    // When `peek_significant_column` returns `None` (no LineMap), the column
    // rule is disabled and we fall back to the pre-E4 single-statement behaviour.
    let body_col = cur.peek_significant_column();

    // Parse the first statement unconditionally (a body is required).
    let first = parse_expr(cur)?;

    let mut stmts = vec![first];

    // Collect additional statements separated by Newlines while:
    //   (a) A Newline is followed by an expression-starting token.
    //   (b) The column rule says the next statement is ≥ body_col
    //       (i.e. it has NOT dedented past the body's baseline).
    //   (c) The next token is NOT the start of a new match arm
    //       (`is_match_arm_start` heuristic — prevents eating the outer arm).
    loop {
        // Between statements the layout pass emits a Newline token.
        // Consume it if present.
        if cur.peek() != &Token::Newline {
            // No inter-statement Newline — also handle same-line continuations
            // where a second expression follows directly (no Newline, e.g. in
            // unit tests with hand-crafted token streams).
            if can_start_expr(cur) && !is_match_arm_start(cur) {
                match parse_expr(cur) {
                    Ok(s) => {
                        stmts.push(s);
                        continue;
                    }
                    Err(_) => break,
                }
            }
            break;
        }

        // Peek at the column of the next significant token (after the Newline).
        // If it is below body_col, this line belongs to the outer context.
        if let (Some(next_col), Some(min_col)) = (cur.peek_significant_column(), body_col) {
            if next_col < min_col {
                break; // dedented below body baseline → stop collecting
            }
        }

        // Column OK — consume the Newline and collect the next statement,
        // unless it looks like a sibling arm (in which case the outer arm
        // loop will handle it).
        if cur.peek_n(1).is_some_and(can_start_expr_token) && !is_match_arm_start_after_newline(cur)
        {
            cur.bump(); // consume Newline
            match parse_expr(cur) {
                Ok(s) => stmts.push(s),
                Err(_) => break,
            }
        } else {
            break;
        }
    }

    if stmts.len() == 1 {
        Ok(stmts.remove(0))
    } else {
        // Safety: stmts.len() >= 2, so first() and last() are always Some.
        let first_span = stmts[0].span();
        let last_span = stmts[stmts.len() - 1].span();
        let span = first_span.merge(last_span);
        Ok(Expr::Block(ridge_ast::Block { stmts, span }))
    }
}

/// Return `true` if the tokens at and after the current `Newline` look like
/// the start of a new match arm.
///
/// This is a variant of [`is_match_arm_start`] used from
/// `parse_flat_block_arm_body_inner` when the cursor is positioned ON a
/// `Newline` token.  It peeks at `pos+1` (the token after the Newline) and
/// delegates to the normal `is_match_arm_start` logic.
///
/// Returns `false` if the cursor is not at a `Newline`.
fn is_match_arm_start_after_newline(cur: &Cursor<'_>) -> bool {
    if cur.peek() != &Token::Newline {
        return false;
    }
    // Peek at the token after the Newline and apply the single-token check.
    // For bracket-group patterns we conservatively return false (the outer
    // arm loop's `is_match_arm_start` will handle those correctly).
    match cur.peek_n(1) {
        Some(tok) if !matches!(tok, Token::LParen | Token::LBrack | Token::LBrace) => {
            matches!(cur.peek_n(2), Some(Token::Arrow | Token::KwWhen))
        }
        _ => false,
    }
}

/// Return `true` if the tokens at the current cursor position look like the
/// start of a new match arm: `<pattern> [when] ->`.
///
/// We check two forms:
///   1. Bracket-group pattern: `(…) ->` or `(…) when`
///   2. Single-token pattern:  `token ->` or `token when`
///
/// This heuristic is intentionally conservative — it only fires when we are
/// confident we are seeing a new arm, never for ordinary expressions.
pub(crate) fn is_match_arm_start(cur: &Cursor<'_>) -> bool {
    let first = cur.peek();

    // Single-token pattern (ident, wildcard, constructor, literal): check
    // that peek_n(1) is `->` or `when`.
    if !matches!(first, Token::LParen | Token::LBrack | Token::LBrace) {
        return matches!(cur.peek_n(1), Some(Token::Arrow | Token::KwWhen));
    }

    // Bracket-group pattern: scan to depth-0 close, then check the next token.
    let mut depth: i32 = 0;
    for i in 0..200 {
        match cur.peek_n(i) {
            Some(Token::LParen | Token::LBrack | Token::LBrace) => depth += 1,
            Some(Token::RParen | Token::RBrack | Token::RBrace) => {
                depth -= 1;
                if depth < 0 {
                    return false; // exited enclosing scope — not an arm start
                }
                if depth == 0 {
                    return matches!(cur.peek_n(i + 1), Some(Token::Arrow | Token::KwWhen));
                }
            }
            None => return false,
            _ => {}
        }
    }
    false
}

// ── parse_let ─────────────────────────────────────────────────────────────────

/// Parse a `let` binding expression (grammar §6.1).
///
/// ```text
/// let <pat> [: <ty>] = <expr>
/// ```
///
/// The pattern can be any full `Pattern` (destructuring allowed).
///
/// Precondition: `cur.peek() == &Token::KwLet`.
pub(crate) fn parse_let(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `let`

    let pat = parse_pattern(cur)?;

    let ty = if cur.peek() == &Token::Colon {
        cur.bump(); // consume `:`
        Some(parse_type(cur)?)
    } else {
        None
    };

    cur.expect(&Token::Assign)?;

    let value = parse_branch_body(cur)?;
    let span = start.merge(value.span());

    Ok(Expr::Let {
        pat,
        ty,
        value: Box::new(value),
        span,
    })
}

// ── parse_var_decl ────────────────────────────────────────────────────────────

/// Parse a `var` declaration expression (grammar §6.1).
///
/// ```text
/// var <ident> [: <ty>] = <expr>
/// ```
///
/// Unlike `let`, the left-hand side is a single lower-case identifier.
///
/// Precondition: `cur.peek() == &Token::KwVar`.
pub(crate) fn parse_var_decl(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `var`

    let name_span = cur.span();
    let name_text = match cur.peek().clone() {
        Token::LowerIdent(s) => {
            cur.bump();
            s
        }
        _ => {
            return Err(ParseError::Expected {
                span: cur.span(),
                expected: "<identifier>",
                found: cur.peek().to_string(),
            });
        }
    };
    let name = Ident::new(name_text, name_span);

    let ty = if cur.peek() == &Token::Colon {
        cur.bump(); // consume `:`
        Some(parse_type(cur)?)
    } else {
        None
    };

    cur.expect(&Token::Assign)?;

    let value = parse_branch_body(cur)?;
    let span = start.merge(value.span());

    Ok(Expr::Var {
        name,
        ty,
        value: Box::new(value),
        span,
    })
}

// ── parse_try ─────────────────────────────────────────────────────────────────

/// Parse a `try` expression (grammar §6.5).
///
/// ```text
/// try
///     <block>
/// ```
///
/// `try` introduces a do-block.  If no `INDENT` follows, the next
/// single expression is wrapped in a one-statement `Block`.
///
/// Precondition: `cur.peek() == &Token::KwTry`.
pub(crate) fn parse_try(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `try`

    // Consume optional NEWLINE between `try` and the INDENT.
    if cur.peek() == &Token::Newline {
        cur.bump();
    }

    let block = if cur.peek() == &Token::Indent {
        parse_block(cur)?
    } else {
        // Single-stmt try: wrap in a Block.
        let stmt = parse_expr_pratt(cur)?;
        let stmt_span = stmt.span();
        Block {
            stmts: vec![stmt],
            span: stmt_span,
        }
    };

    let span = start.merge(block.span);
    Ok(Expr::Try { block, span })
}

// ── parse_guard ───────────────────────────────────────────────────────────────

/// Parse a `guard` expression (grammar §6.6).
///
/// ```text
/// guard <cond> else <else-branch>
/// ```
///
/// The `else-branch` is EITHER:
/// - A single-line `Expr` → wrapped in `Block { stmts: [expr], span }`.
/// - A multi-statement INDENT-delimited `Block`.
///
/// `Guard::else_branch` is always a `Block`.  No divergence check is
/// performed at parse time (the type checker enforces it).
///
/// Precondition: `cur.peek() == &Token::KwGuard`.
pub(crate) fn parse_guard(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `guard`

    let cond = parse_expr_pratt(cur)?;

    // Skip Newline tokens between the condition and `else`/`Indent`.
    // DO NOT skip Dedent — that signals the guard exited scope with no `else`.
    while cur.peek() == &Token::Newline {
        cur.bump();
    }

    // If the `else` is at a deeper indent level (e.g. `guard cond\n    else …`),
    // the lexer emits an Indent before KwElse.  Consume it and remember that we
    // must consume the matching Dedent after the else body to keep the enclosing
    // block's Dedent count balanced.
    let saw_indent = if cur.peek() == &Token::Indent {
        cur.bump();
        true
    } else {
        false
    };

    cur.expect(&Token::KwElse)?;

    // Consume optional NEWLINE between `else` and the INDENT.
    if cur.peek() == &Token::Newline {
        cur.bump();
    }

    let else_branch = if cur.peek() == &Token::Indent {
        // Multi-statement block form.
        parse_block(cur)?
    } else {
        // Single-expression form: use full parse_expr so that keywords like
        // `return` are dispatched correctly (e.g. `guard cond else return err`).
        let stmt = crate::expr::parse_expr(cur)?;
        let stmt_span = stmt.span();
        Block {
            stmts: vec![stmt],
            span: stmt_span,
        }
    };

    // If we consumed an Indent before KwElse, the `else` and its body form an
    // "attached continuation" of the guard form.  Consume the matching Dedent
    // now so that the enclosing parse_block does not see a stray Dedent and
    // terminate the surrounding block prematurely (Bug B, T13).
    if saw_indent {
        // Skip any trailing Newline inside the continuation before the Dedent.
        while cur.peek() == &Token::Newline {
            cur.bump();
        }
        // Expect the closing Dedent of the continuation.
        if cur.peek() == &Token::Dedent {
            cur.bump();
        } else {
            return Err(ParseError::LayoutMismatch {
                span: cur.span(),
                hint: "expected `dedent` to close `guard … else` continuation (P006)",
            });
        }
    }

    let span = start.merge(else_branch.span);
    Ok(Expr::Guard {
        cond: Box::new(cond),
        else_branch,
        span,
    })
}

// ── parse_return ──────────────────────────────────────────────────────────────

/// Parse a `return` expression (grammar §6.7).
///
/// ```text
/// return [<expr>]
/// ```
///
/// If the token after `return` cannot start an expression (e.g. `NEWLINE`,
/// `DEDENT`, `EOF`), the return value is synthesised as `Expr::Unit` at the
/// span immediately after `return`.
///
/// Precondition: `cur.peek() == &Token::KwReturn`.
pub(crate) fn parse_return(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    let start = cur.span();
    cur.bump(); // consume `return`

    let value = if can_start_expr(cur) {
        parse_expr_pratt(cur)?
    } else {
        // No following expression — synthesise `()` at the current position.
        Expr::Unit(cur.span())
    };

    let span = start.merge(value.span());
    Ok(Expr::Return {
        value: Box::new(value),
        span,
    })
}

// ── Branch body helper ────────────────────────────────────────────────────────

/// Parse a branch body: either a multi-statement `Block` (INDENT form) or a
/// single expression.
///
/// INDENT form → `Expr::Block(block)`
/// Single expression → that expression directly.
///
/// This variant does NOT apply the flat-block NEWLINE extension (R014).
/// Use it for `if`/`then`/`else`, `let` values, `var` values, `guard` bodies,
/// and any other position where the value is a sub-expression within a larger
/// context — the NEWLINE in that position belongs to the enclosing block.
pub(crate) fn parse_branch_body(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    // Consume optional NEWLINE before INDENT.
    if cur.peek() == &Token::Newline {
        // Only skip if it's immediately followed by INDENT.
        if cur.peek_n(1) == Some(&Token::Indent) {
            cur.bump(); // consume NEWLINE
        }
    }

    if cur.peek() == &Token::Indent {
        let block = parse_block(cur)?;
        Ok(Expr::Block(block))
    } else {
        parse_expr(cur)
    }
}

/// Parse a lambda or match-arm body, applying the flat-block NEWLINE extension
/// (R014) when inside a bracket context.
///
/// INDENT form  → `Expr::Block(block)`
/// Flat form    → `Expr::Block(block)` when ≥2 stmts; single `Expr` when 1.
/// Single expr  → that expression directly.
///
/// The flat form fires only when `bracket_depth > 0` — i.e. the lexer is
/// emitting `NEWLINE` tokens between sibling statements inside a bracket
/// (instead of `INDENT`/`DEDENT`).  This is the context where `lambda` bodies
/// and no-layout match arm bodies can have multiple statements separated only
/// by `NEWLINE`.
///
/// Callers: `parse_lambda` and (for no-layout match arms) the match arm body
/// parser.  All other `parse_branch_body` callers use the non-flat variant.
pub(crate) fn parse_branch_body_flat(cur: &mut Cursor<'_>) -> Result<Expr, ParseError> {
    // Consume optional NEWLINE before INDENT.
    if cur.peek() == &Token::Newline {
        // Only skip if it's immediately followed by INDENT.
        if cur.peek_n(1) == Some(&Token::Indent) {
            cur.bump(); // consume NEWLINE
        }
    }

    if cur.peek() == &Token::Indent {
        let block = parse_block(cur)?;
        return Ok(Expr::Block(block));
    }

    // Single-expression form, with optional flat-NEWLINE-block continuation.
    let first = parse_expr(cur)?;

    // Only collect a flat block if we are inside brackets (bracket_depth > 0).
    // Outside brackets the INDENT/DEDENT tokens delimit blocks, and a NEWLINE
    // here belongs to the enclosing block, not to this sub-expression.
    if cur.bracket_depth == 0 {
        return Ok(first);
    }

    let mut statements: Vec<Expr> = vec![first];

    // Collect additional statements while a NEWLINE is followed by an
    // expression-starting token.  Operator-leading continuation lines are
    // already merged by the continuation rule, so a NEWLINE here is a genuine statement
    // boundary.
    while cur.peek() == &Token::Newline && cur.peek_n(1).is_some_and(can_start_expr_token) {
        cur.bump(); // consume NEWLINE
        statements.push(parse_expr(cur)?);
    }

    if statements.len() == 1 {
        Ok(statements.remove(0))
    } else {
        // Safety: len >= 2, so first() and last() are always Some.
        let first_span = statements[0].span();
        let last_span = statements[statements.len() - 1].span();
        let span = first_span.merge(last_span);
        Ok(Expr::Block(ridge_ast::Block {
            stmts: statements,
            span,
        }))
    }
}

/// Return `true` if `tok` can begin an expression.
///
/// Token-level variant of [`can_start_expr`] — used where we have a `&Token`
/// from `cursor.peek_n()` rather than a cursor reference.
const fn can_start_expr_token(tok: &Token) -> bool {
    matches!(
        tok,
        Token::IntDec(_)
            | Token::IntBin(_)
            | Token::IntOct(_)
            | Token::IntHex(_)
            | Token::Float(_)
            | Token::TextLit(_)
            | Token::KwTrue
            | Token::KwFalse
            | Token::InterpStart
            | Token::LowerIdent(_)
            | Token::UpperIdent(_)
            | Token::LParen
            | Token::LBrack
            | Token::Minus      // unary minus
            | Token::KwIf
            | Token::KwMatch
            | Token::KwLet
            | Token::KwVar
            | Token::KwTry
            | Token::KwGuard
            | Token::KwReturn
            | Token::KwFn      // lambda
            | Token::KwSpawn // spawn expression
    )
}

// ── Expression-start predicate ────────────────────────────────────────────────

/// Return `true` if the current token can begin an expression.
///
/// Used by `parse_return` to determine whether a value follows.
///
/// Excludes layout tokens (`Newline`, `Indent`, `Dedent`, `Eof`) and tokens
/// that cannot appear at the start of an expression (`Arrow`, `Assign`, etc.).
pub(crate) fn can_start_expr(cur: &Cursor<'_>) -> bool {
    matches!(
        cur.peek(),
        Token::IntDec(_)
            | Token::IntBin(_)
            | Token::IntOct(_)
            | Token::IntHex(_)
            | Token::Float(_)
            | Token::TextLit(_)
            | Token::KwTrue
            | Token::KwFalse
            | Token::InterpStart
            | Token::LowerIdent(_)
            | Token::UpperIdent(_)
            | Token::LParen
            | Token::LBrack
            | Token::Minus      // unary minus
            | Token::KwIf
            | Token::KwMatch
            | Token::KwLet
            | Token::KwVar
            | Token::KwTry
            | Token::KwGuard
            | Token::KwReturn
            | Token::KwFn      // lambda
            | Token::KwSpawn // spawn expression
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::panic)]
#[allow(clippy::unwrap_used)]
#[allow(clippy::expect_used)]
#[allow(clippy::match_wildcard_for_single_variants)]
mod tests {
    use super::*;
    use ridge_ast::{Block, Expr, Literal, Pattern, Span};
    use ridge_lexer::tokenize;

    fn lex(src: &str) -> Vec<(ridge_lexer::Token, Span)> {
        tokenize(src).tokens
    }

    /// Parse `src` as a full expression using the keyword-dispatch entry.
    fn parse_e(src: &str) -> Result<Expr, ParseError> {
        let toks = lex(src);
        let mut cur = Cursor::new(&toks);
        crate::expr::parse_expr(&mut cur)
    }

    fn ok(src: &str) -> Expr {
        parse_e(src).unwrap_or_else(|e| panic!("parse_expr({src:?}) failed: {e:?}"))
    }

    fn err_e(src: &str) -> ParseError {
        parse_e(src)
            .err()
            .unwrap_or_else(|| panic!("parse_expr({src:?}) expected Err, got Ok"))
    }

    /// Helper: extract the match arms from the first paren-arg of a Call expression.
    ///
    /// Handles the common E4-test shape: `f (match ...) `→
    ///   `Call { callee: Ident("f"), args: [Paren { inner: Match { arms } }] }`
    /// Returns `None` if the shape doesn't match.
    fn extract_match_arms_from_call_arg(expr: &Expr) -> Option<&Vec<ridge_ast::MatchArm>> {
        if let Expr::Call { args, .. } = expr {
            if let Some(arg) = args.first() {
                // Unwrap a Paren wrapper if present (parser emits Paren for `(...)`).
                let inner = if let Expr::Paren { inner, .. } = arg {
                    inner.as_ref()
                } else {
                    arg
                };
                if let Expr::Match { arms, .. } = inner {
                    return Some(arms);
                }
            }
        }
        None
    }

    // ── parse_if_single_line ────────────────────────────────────────────

    #[test]
    fn parse_if_single_line() {
        let e = ok("if x then 1 else 2");
        if let Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } = e
        {
            assert!(matches!(*cond, Expr::Ident(ref id) if id.text == "x"));
            assert!(matches!(
                *then_branch,
                Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "1"
            ));
            let eb = else_branch.expect("expected else branch");
            assert!(matches!(
                *eb,
                Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "2"
            ));
        } else {
            panic!("expected If, got {e:?}");
        }
    }

    // ── parse_if_no_else ────────────────────────────────────────────────

    #[test]
    fn parse_if_no_else() {
        let e = ok("if x then 1");
        if let Expr::If {
            else_branch: None, ..
        } = e
        {
            // pass
        } else {
            panic!("expected If with no else, got {e:?}");
        }
    }

    // ── parse_if_multiline ──────────────────────────────────────────────
    //
    // Source:
    //   if x then
    //       1
    //   else
    //       2
    //
    // The lexer emits: KwIf LowerIdent("x") KwThen Newline
    //                  Indent IntDec("1") Newline Dedent
    //                  KwElse Newline
    //                  Indent IntDec("2") Newline Dedent Eof

    #[test]
    fn parse_if_multiline_then() {
        let src = "if x then\n    1\nelse\n    2";
        let e = ok(src);
        if let Expr::If {
            then_branch,
            else_branch,
            ..
        } = e
        {
            // Both branches should be Expr::Block wrapping a single IntDec.
            assert!(
                matches!(*then_branch, Expr::Block(ref b) if b.stmts.len() == 1),
                "expected Expr::Block as then_branch, got {then_branch:?}"
            );
            let eb = else_branch.expect("expected else branch");
            assert!(
                matches!(*eb, Expr::Block(ref b) if b.stmts.len() == 1),
                "expected Expr::Block as else_branch, got {eb:?}"
            );
        } else {
            panic!("expected If, got {e:?}");
        }
    }

    // ── parse_if_no_else_preserves_statement_separator ──────────────────
    //
    // Regression: an else-less `if` must not swallow the NEWLINE that
    // separates it from the next statement in the enclosing block.  The
    // then-branch parser previously consumed a trailing NEWLINE
    // unconditionally while probing for `else`, so this source:
    //   if outer then
    //       if inner then a   ← else-less
    //       b                 ← sibling statement
    // collapsed the inner `if` and `b` into a single statement.

    #[test]
    fn parse_if_no_else_preserves_statement_separator() {
        let src = "if outer then\n    if inner then a\n    b";
        let e = ok(src);
        if let Expr::If { then_branch, .. } = e {
            if let Expr::Block(Block { stmts, .. }) = *then_branch {
                assert_eq!(
                    stmts.len(),
                    2,
                    "expected inner-if + b as two stmts, got {}: {stmts:?}",
                    stmts.len()
                );
                assert!(
                    matches!(
                        &stmts[0],
                        Expr::If {
                            else_branch: None,
                            ..
                        }
                    ),
                    "expected first stmt to be else-less If, got {:?}",
                    stmts[0]
                );
                assert!(
                    matches!(&stmts[1], Expr::Ident(id) if id.text == "b"),
                    "expected second stmt to be Ident(b), got {:?}",
                    stmts[1]
                );
            } else {
                panic!("expected Expr::Block as then_branch, got {then_branch:?}");
            }
        } else {
            panic!("expected If, got {e:?}");
        }
    }

    // ── parse_if_newline_before_else_still_binds ────────────────────────
    //
    // Companion to the regression above: when the then-branch is single-line
    // and `else` sits on the next line, the NEWLINE before `else` must still
    // be consumed so the `else` binds to this `if`.
    //   if x then 1
    //   else 2

    #[test]
    fn parse_if_newline_before_else_still_binds() {
        let src = "if x then 1\nelse 2";
        let e = ok(src);
        assert!(
            matches!(
                e,
                Expr::If {
                    else_branch: Some(_),
                    ..
                }
            ),
            "expected If with else branch, got {e:?}"
        );
    }

    #[test]
    fn parse_if_then_on_next_line() {
        // `then` wrapped onto the next line at the same indent as `if`, separated
        // from the condition by a layout Newline — used to fail with
        // "expected then, found <NEWLINE>".
        let src = "if x\nthen 1\nelse 2";
        let e = ok(src);
        assert!(
            matches!(
                e,
                Expr::If {
                    else_branch: Some(_),
                    ..
                }
            ),
            "expected If with else branch, got {e:?}"
        );
    }

    // ── parse_match_two_arms ────────────────────────────────────────────

    #[test]
    fn parse_match_two_arms() {
        // match x
        //     A -> 1
        //     B -> 2
        let src = "match x\n    A -> 1\n    B -> 2";
        let e = ok(src);
        if let Expr::Match {
            scrutinee, arms, ..
        } = e
        {
            assert!(matches!(*scrutinee, Expr::Ident(ref id) if id.text == "x"));
            assert_eq!(arms.len(), 2, "expected 2 arms, got {}", arms.len());
            assert!(
                matches!(arms[0].pattern, Pattern::Constructor { ref name, .. } if name.text == "A")
            );
            assert!(
                matches!(arms[1].pattern, Pattern::Constructor { ref name, .. } if name.text == "B")
            );
        } else {
            panic!("expected Match, got {e:?}");
        }
    }

    // ── parse_match_or_pattern ─────────────────────────────────────────

    #[test]
    fn parse_match_or_pattern_alternatives() {
        // match n
        //     1 | 2 | 3 -> "few"
        //     _ -> "many"
        let src = "match n\n    1 | 2 | 3 -> \"few\"\n    _ -> \"many\"";
        let e = ok(src);
        if let Expr::Match { arms, .. } = e {
            assert_eq!(arms.len(), 2, "expected 2 arms, got {}", arms.len());
            if let Pattern::Or { alts, .. } = &arms[0].pattern {
                assert_eq!(alts.len(), 3, "expected 3 alternatives, got {}", alts.len());
                assert!(
                    alts.iter().all(|a| matches!(a, Pattern::Literal { .. })),
                    "expected all-literal alternatives, got {alts:?}"
                );
            } else {
                panic!(
                    "expected first arm to be Pattern::Or, got {:?}",
                    arms[0].pattern
                );
            }
            // The trailing `_` arm is a plain wildcard, not wrapped in Or.
            assert!(matches!(arms[1].pattern, Pattern::Wildcard { .. }));
        } else {
            panic!("expected Match, got {e:?}");
        }
    }

    #[test]
    fn parse_match_single_alternative_is_not_or() {
        // A lone pattern with no `|` stays unwrapped.
        let src = "match x\n    Some y -> y\n    _ -> 0";
        let e = ok(src);
        if let Expr::Match { arms, .. } = e {
            assert!(
                !matches!(arms[0].pattern, Pattern::Or { .. }),
                "a single alternative must not be wrapped in Or, got {:?}",
                arms[0].pattern
            );
        } else {
            panic!("expected Match, got {e:?}");
        }
    }

    #[test]
    fn parse_match_nested_or_is_rejected() {
        // `|` is an or-separator only at the arm root; inside `( )` the pattern
        // parser does not consume it, so the arm fails to parse.
        let src = "match x\n    Some (1 | 2) -> 1\n    _ -> 0";
        assert!(
            parse_e(src).is_err(),
            "nested or-pattern `Some (1 | 2)` should not parse, got {:?}",
            parse_e(src)
        );
    }

    // ── parse_match_arm_with_guard ─────────────────────────────────────

    #[test]
    fn parse_match_arm_with_guard() {
        // match x
        //     N when n > 0 -> n
        //     _ -> 0
        let src = "match x\n    N when n > 0 -> n\n    _ -> 0";
        let e = ok(src);
        if let Expr::Match { arms, .. } = e {
            assert_eq!(arms.len(), 2);
            // First arm should have a guard.
            assert!(
                arms[0].guard.is_some(),
                "expected guard on first arm, got {:?}",
                arms[0].guard
            );
        } else {
            panic!("expected Match, got {e:?}");
        }
    }

    // ── parse_let_simple ────────────────────────────────────────────────

    #[test]
    fn parse_let_simple() {
        let e = ok("let x = 1");
        if let Expr::Let { pat, ty, value, .. } = e {
            assert!(matches!(pat, Pattern::Var { ref name, .. } if name.text == "x"));
            assert!(ty.is_none());
            assert!(matches!(*value, Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "1"));
        } else {
            panic!("expected Let, got {e:?}");
        }
    }

    // ── parse_let_with_type_annotation ─────────────────────────────────

    #[test]
    fn parse_let_with_type_annotation() {
        use ridge_ast::{PrimitiveType, Type};
        let e = ok("let x: Int = 1");
        if let Expr::Let { pat, ty, value, .. } = e {
            assert!(matches!(pat, Pattern::Var { ref name, .. } if name.text == "x"));
            assert!(
                matches!(
                    ty,
                    Some(Type::Primitive {
                        name: PrimitiveType::Int,
                        ..
                    })
                ),
                "expected Some(Int), got {ty:?}"
            );
            assert!(matches!(*value, Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "1"));
        } else {
            panic!("expected Let, got {e:?}");
        }
    }

    // ── parse_let_destructuring_tuple ──────────────────────────────────

    #[test]
    fn parse_let_destructuring_tuple() {
        // `let (x, y) = p` — destructuring pattern
        let e = ok("let (x, y) = p");
        if let Expr::Let { pat, .. } = e {
            if let Pattern::Tuple { elems, .. } = pat {
                assert_eq!(elems.len(), 2);
                assert!(matches!(&elems[0], Pattern::Var { name, .. } if name.text == "x"));
                assert!(matches!(&elems[1], Pattern::Var { name, .. } if name.text == "y"));
            } else {
                panic!("expected Tuple pattern, got {pat:?}");
            }
        } else {
            panic!("expected Let, got {e:?}");
        }
    }

    // ── parse_let_destructuring_constructor ─────────────────────────────

    #[test]
    fn parse_let_destructuring_constructor() {
        // `let Some x = opt`
        let e = ok("let Some x = opt");
        if let Expr::Let { pat, .. } = e {
            if let Pattern::Constructor { name, args, .. } = pat {
                assert_eq!(name.text, "Some");
                assert_eq!(args.len(), 1);
                assert!(matches!(&args[0], Pattern::Var { name, .. } if name.text == "x"));
            } else {
                panic!("expected Constructor pattern, got {pat:?}");
            }
        } else {
            panic!("expected Let, got {e:?}");
        }
    }

    // ── parse_var_decl ─────────────────────────────────────────────────

    #[test]
    fn parse_var_decl() {
        use ridge_ast::{PrimitiveType, Type};
        let e = ok("var counter: Int = 0");
        if let Expr::Var {
            name, ty, value, ..
        } = e
        {
            assert_eq!(name.text, "counter");
            assert!(
                matches!(
                    ty,
                    Some(Type::Primitive {
                        name: PrimitiveType::Int,
                        ..
                    })
                ),
                "expected Some(Int), got {ty:?}"
            );
            assert!(matches!(*value, Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "0"));
        } else {
            panic!("expected Var, got {e:?}");
        }
    }

    // ── parse_assign ───────────────────────────────────────────────────

    #[test]
    fn parse_assign() {
        // `counter <- counter + 1`
        let e = ok("counter <- counter + 1");
        if let Expr::Assign { target, value, .. } = e {
            assert!(matches!(*target, Expr::Ident(ref id) if id.text == "counter"));
            assert!(
                matches!(
                    *value,
                    Expr::Binary {
                        op: ridge_ast::BinOp::Add,
                        ..
                    }
                ),
                "expected Add, got {value:?}"
            );
        } else {
            panic!("expected Assign, got {e:?}");
        }
    }

    // ── parse_try_single_stmt ─────────────────────────────────────────

    #[test]
    fn parse_try_single_stmt() {
        // try
        //     doSomething
        let src = "try\n    doSomething";
        let e = ok(src);
        if let Expr::Try { block, .. } = e {
            assert_eq!(block.stmts.len(), 1, "expected 1 stmt in try block");
            assert!(
                matches!(&block.stmts[0], Expr::Ident(id) if id.text == "doSomething"),
                "expected Ident(doSomething), got {:?}",
                block.stmts[0]
            );
        } else {
            panic!("expected Try, got {e:?}");
        }
    }

    // ── parse_guard_single_line ───────────────────────────────────────

    #[test]
    fn parse_guard_single_line() {
        // `guard cond else return err`
        let e = ok("guard cond else return err");
        if let Expr::Guard {
            cond, else_branch, ..
        } = e
        {
            assert!(matches!(*cond, Expr::Ident(ref id) if id.text == "cond"));
            assert_eq!(
                else_branch.stmts.len(),
                1,
                "expected 1 stmt in guard else, got {}",
                else_branch.stmts.len()
            );
            // The single stmt should be `return err`.
            assert!(
                matches!(&else_branch.stmts[0], Expr::Return { .. }),
                "expected Return in guard else, got {:?}",
                else_branch.stmts[0]
            );
        } else {
            panic!("expected Guard, got {e:?}");
        }
    }

    // ── parse_guard_multi_stmt (block form) ───────────────────────────
    //
    // Multi-statement guard else block (mirrors log_analyzer.ridge:89–91):
    //   guard (len >= 2) else
    //       Io.eprintln "usage"
    //       return Err e
    //
    // The else branch is a multi-statement Block whose final stmt is Return.

    #[test]
    fn parse_guard_multi_stmt() {
        let src = "guard (len >= 2) else\n    Io.eprintln \"usage\"\n    return err";
        let e = ok(src);
        if let Expr::Guard { else_branch, .. } = e {
            assert!(
                else_branch.stmts.len() >= 2,
                "expected >= 2 stmts, got {}",
                else_branch.stmts.len()
            );
            let last = else_branch.stmts.last().expect("non-empty");
            assert!(
                matches!(last, Expr::Return { .. }),
                "expected last stmt to be Return, got {last:?}"
            );
        } else {
            panic!("expected Guard, got {e:?}");
        }
    }

    // ── parse_return_with_value ───────────────────────────────────────

    #[test]
    fn parse_return_with_value() {
        let e = ok("return x");
        if let Expr::Return { value, .. } = e {
            assert!(matches!(*value, Expr::Ident(ref id) if id.text == "x"));
        } else {
            panic!("expected Return, got {e:?}");
        }
    }

    // ── parse_return_without_value ────────────────────────────────────

    #[test]
    fn parse_return_without_value() {
        // `return` alone — value should be synthesised Unit.
        // We need a token stream where `return` is immediately followed by EOF.
        let toks = lex("return");
        let mut cur = Cursor::new(&toks);
        let e = crate::expr::parse_expr(&mut cur).unwrap();
        if let Expr::Return { value, .. } = e {
            assert!(
                matches!(*value, Expr::Unit(_)),
                "expected Unit, got {value:?}"
            );
        } else {
            panic!("expected Return, got {e:?}");
        }
    }

    // ── parse_block_single_stmt ───────────────────────────────────────
    //
    // A one-statement block inside an `if then` branch.

    #[test]
    fn parse_block_single_stmt() {
        // if flag then
        //     x
        let src = "if flag then\n    x";
        let e = ok(src);
        if let Expr::If { then_branch, .. } = e {
            if let Expr::Block(Block { stmts, .. }) = *then_branch {
                assert_eq!(stmts.len(), 1);
                assert!(matches!(&stmts[0], Expr::Ident(id) if id.text == "x"));
            } else {
                panic!("expected Expr::Block as then_branch, got {then_branch:?}");
            }
        } else {
            panic!("expected If, got {e:?}");
        }
    }

    // ── parse_block_multi_stmt ────────────────────────────────────────
    //
    // Block with 3 statements — verify count and ordering.

    #[test]
    fn parse_block_multi_stmt() {
        // if flag then
        //     a
        //     b
        //     c
        let src = "if flag then\n    a\n    b\n    c";
        let e = ok(src);
        if let Expr::If { then_branch, .. } = e {
            if let Expr::Block(Block { stmts, .. }) = *then_branch {
                assert_eq!(stmts.len(), 3, "expected 3 stmts, got {}", stmts.len());
                assert!(matches!(&stmts[0], Expr::Ident(id) if id.text == "a"));
                assert!(matches!(&stmts[1], Expr::Ident(id) if id.text == "b"));
                assert!(matches!(&stmts[2], Expr::Ident(id) if id.text == "c"));
            } else {
                panic!("expected Expr::Block as then_branch, got {then_branch:?}");
            }
        } else {
            panic!("expected If, got {e:?}");
        }
    }

    // ── parse_block_empty_rejects (P014) ──────────────────────────────
    //
    // `parse_block` must reject an INDENT immediately followed by DEDENT.
    // We test via `parse_try` which calls `parse_block`.

    #[test]
    fn parse_block_empty_rejects() {
        // Build a token stream for: try INDENT DEDENT EOF
        // The lexer won't produce that naturally, so we construct it manually.
        use ridge_ast::Span;
        use ridge_lexer::Token;
        let s = Span::point(0);
        let tokens: Vec<(Token, Span)> = vec![
            (Token::KwTry, s),
            (Token::Newline, s),
            (Token::Indent, s),
            (Token::Dedent, s),
            (Token::Eof, s),
        ];
        let mut cur = Cursor::new(&tokens);
        let result = crate::expr::parse_expr(&mut cur);
        assert!(result.is_err(), "expected Err(P014), got {result:?}");
        assert_eq!(
            result.unwrap_err().code(),
            "P014",
            "expected P014 EmptyBlock"
        );
    }

    // ── parse_block_trailing_newline ──────────────────────────────────
    //
    // Block with a trailing NEWLINE immediately before DEDENT: should still
    // parse cleanly and produce the correct statement count.

    #[test]
    fn parse_block_trailing_newline() {
        // if flag then
        //     x
        //     y
        //
        // (trailing blank line — lexer emits an extra Newline before Dedent
        // in some layouts; parse_block's while-loop breaks on peek==DEDENT.)
        let src = "if flag then\n    x\n    y\n";
        let e = ok(src);
        if let Expr::If { then_branch, .. } = e {
            if let Expr::Block(Block { stmts, .. }) = *then_branch {
                assert_eq!(stmts.len(), 2, "expected 2 stmts, got {}", stmts.len());
            } else {
                panic!("expected Expr::Block, got {then_branch:?}");
            }
        } else {
            panic!("expected If, got {e:?}");
        }
    }

    // ── parse_guard_else_on_next_line_indented ───────────────────────────────
    //
    // Mirrors game_of_life.ridge:25-27:
    //   guard (r >= 0 && r < grid.rows)
    //       else return false
    //
    // The `else` is on a new line at a deeper indent than `guard`.

    #[test]
    fn parse_guard_else_on_next_line_indented() {
        // Simulate: guard cond NEWLINE INDENT else return false
        // The lexer for "guard cond\n    else return false" emits:
        //   KwGuard LowerIdent("cond") Newline Indent KwElse KwReturn KwFalse Dedent Eof
        use ridge_ast::Span;
        use ridge_lexer::Token;
        let s = Span::point(0);
        let tokens: Vec<(Token, Span)> = vec![
            (Token::KwGuard, s),
            (Token::LowerIdent("cond".to_string()), s),
            (Token::Newline, s),
            (Token::Indent, s),
            (Token::KwElse, s),
            (Token::KwReturn, s),
            (Token::KwFalse, s),
            (Token::Dedent, s),
            (Token::Eof, s),
        ];
        let mut cur = Cursor::new(&tokens);
        let e =
            crate::expr::parse_expr(&mut cur).unwrap_or_else(|err| panic!("parse failed: {err:?}"));
        if let Expr::Guard {
            cond, else_branch, ..
        } = e
        {
            assert!(
                matches!(*cond, Expr::Ident(ref id) if id.text == "cond"),
                "expected cond to be Ident(cond), got {cond:?}"
            );
            assert_eq!(
                else_branch.stmts.len(),
                1,
                "expected 1 stmt in else branch, got {}",
                else_branch.stmts.len()
            );
            assert!(
                matches!(&else_branch.stmts[0], Expr::Return { .. }),
                "expected Return in else branch, got {:?}",
                else_branch.stmts[0]
            );
        } else {
            panic!("expected Guard expression");
        }
    }

    // ── T13-guard-1: parse_guard_with_indent_else_followed_by_more_stmts ────
    //
    // Critical regression test (Bug B).  Mirrors game_of_life.ridge:25-29:
    //   fn cellAt ... =
    //       guard (cond)
    //           else return false
    //       let row = ...
    //
    // The token stream for the function body:
    //   Indent
    //     KwGuard LowerIdent("cond") Newline
    //     Indent KwElse KwReturn KwFalse Dedent
    //     Newline
    //     KwLet LowerIdent("row") Assign LowerIdent("x")
    //     Newline
    //   Dedent
    //
    // Before the fix, parse_guard left the inner Dedent on the stream;
    // parse_block interpreted it as end-of-block, so `let row = x` landed at
    // module level, producing "expected top-level declaration, found 'let'".
    // After the fix, parse_guard consumes that Dedent, and parse_block sees
    // the guard + let as two sibling stmts in the same block.

    #[test]
    fn parse_guard_with_indent_else_followed_by_more_stmts() {
        use ridge_ast::Span;
        use ridge_lexer::Token;
        let s = Span::point(0);
        // Token stream for the body of `fn cellAt ... =`:
        //   Indent
        //     guard cond \n  <Indent> else return false <Dedent>
        //     \n
        //     let row = x
        //     \n
        //   Dedent
        let tokens: Vec<(Token, Span)> = vec![
            (Token::Indent, s),
            // guard stmt
            (Token::KwGuard, s),
            (Token::LowerIdent("cond".to_string()), s),
            (Token::Newline, s),
            (Token::Indent, s), // deeper indent before `else`
            (Token::KwElse, s),
            (Token::KwReturn, s),
            (Token::KwFalse, s),
            (Token::Dedent, s), // close the deeper indent
            (Token::Newline, s),
            // let stmt
            (Token::KwLet, s),
            (Token::LowerIdent("row".to_string()), s),
            (Token::Assign, s),
            (Token::LowerIdent("x".to_string()), s),
            (Token::Newline, s),
            // end of block
            (Token::Dedent, s),
            (Token::Eof, s),
        ];
        let mut cur = Cursor::new(&tokens);
        let block = crate::block::parse_block(&mut cur)
            .unwrap_or_else(|e| panic!("parse_block failed: {e:?}"));
        assert_eq!(
            block.stmts.len(),
            2,
            "expected Guard + Let as sibling stmts, got {} stmts: {:?}",
            block.stmts.len(),
            block.stmts
        );
        assert!(
            matches!(&block.stmts[0], Expr::Guard { .. }),
            "expected first stmt to be Guard, got {:?}",
            block.stmts[0]
        );
        assert!(
            matches!(&block.stmts[1], Expr::Let { .. }),
            "expected second stmt to be Let, got {:?}",
            block.stmts[1]
        );
    }

    // ── parse_if_missing_then → P001 ──────────────────────────────────

    #[test]
    fn parse_if_missing_then() {
        // `if x 1` — missing `then`
        let e = err_e("if x 1");
        assert_eq!(e.code(), "P001", "expected P001, got {e:?}");
    }

    // ── parse_let_missing_assign → P001 ───────────────────────────────

    #[test]
    fn parse_let_missing_assign() {
        // `let x 1` — missing `=`
        let e = err_e("let x 1");
        assert_eq!(e.code(), "P001", "expected P001, got {e:?}");
    }

    // ── parse_match_arm_missing_arrow → P001 ───────────────────────────

    #[test]
    fn parse_match_arm_missing_arrow() {
        // match x
        //     A 1
        let src = "match x\n    A 1";
        let e = err_e(src);
        assert_eq!(e.code(), "P001", "expected P001, got {e:?}");
    }

    // ── T13-1: parse_let_with_if_value_single_line ────────────────────────────
    //
    // `let x = if c then 1 else 2` — value is Expr::If (keyword-starting).

    #[test]
    fn parse_let_with_if_value_single_line() {
        let e = ok("let x = if c then 1 else 2");
        if let Expr::Let { pat, ty, value, .. } = e {
            assert!(matches!(pat, Pattern::Var { ref name, .. } if name.text == "x"));
            assert!(ty.is_none());
            assert!(
                matches!(*value, Expr::If { .. }),
                "expected Expr::If as value, got {value:?}"
            );
        } else {
            panic!("expected Let, got {e:?}");
        }
    }

    // ── T13-2: parse_let_with_match_value ────────────────────────────────────
    //
    // `let x = match y\n    A -> 1\n    B -> 2` — value is Expr::Match.

    #[test]
    fn parse_let_with_match_value() {
        let src = "let x = match y\n    A -> 1\n    B -> 2";
        let e = ok(src);
        if let Expr::Let { value, .. } = e {
            assert!(
                matches!(*value, Expr::Match { .. }),
                "expected Expr::Match as value, got {value:?}"
            );
            if let Expr::Match { arms, .. } = *value {
                assert_eq!(arms.len(), 2, "expected 2 arms, got {}", arms.len());
            }
        } else {
            panic!("expected Let, got {e:?}");
        }
    }

    // ── T13-3: parse_let_with_block_value ────────────────────────────────────
    //
    // `let x =\n    let y = 1\n    y` — value is Expr::Block.
    // Verifies INDENT-delimited block is accepted as the let value.

    #[test]
    fn parse_let_with_block_value() {
        let src = "let x =\n    let y = 1\n    y";
        let e = ok(src);
        if let Expr::Let { value, .. } = e {
            assert!(
                matches!(*value, Expr::Block(_)),
                "expected Expr::Block as value, got {value:?}"
            );
            if let Expr::Block(block) = *value {
                assert_eq!(
                    block.stmts.len(),
                    2,
                    "expected 2 stmts, got {}",
                    block.stmts.len()
                );
            }
        } else {
            panic!("expected Let, got {e:?}");
        }
    }

    // ── T13-4: parse_var_with_if_value ───────────────────────────────────────
    //
    // `var counter = if reset then 0 else current` — value is Expr::If.

    #[test]
    fn parse_var_with_if_value() {
        let e = ok("var counter = if reset then 0 else current");
        if let Expr::Var { name, value, .. } = e {
            assert_eq!(name.text, "counter");
            assert!(
                matches!(*value, Expr::If { .. }),
                "expected Expr::If as value, got {value:?}"
            );
        } else {
            panic!("expected Var, got {e:?}");
        }
    }

    // ── E4-1: multi-statement arm body terminates on sibling-arm dedent ─────────
    //
    // Source (no-layout match inside a paren):
    //
    //   f (match x
    //       A ->
    //           let a = 1
    //           let b = 2
    //       B -> 3)
    //
    // In no-layout mode, R014 only emits a NEWLINE when col ≤ baseline (col 4).
    // Both `let` statements are at col 8 > 4 — no Newline between them.
    // The first `let` returns after parsing `= 1`, then `let b = 2` is the next
    // token. `KwLet` is NOT in `can_start_arg_atom`, so juxtaposition stops;
    // the outer `can_start_expr` loop picks up `let b = 2` as a second statement.
    // The `B` arm at col 4 = MATCH_COL terminates the outer match arm loop.
    #[test]
    fn e4_multi_stmt_body_terminates_at_sibling_arm() {
        // `let a = 1` returns with value=1, then `let b = 2` is collected as a
        // sibling statement (KwLet is not a juxtaposition argument atom).
        // `B -> 3` triggers the column rule (col 4 < MATCH_COL or = MATCH_COL).
        let src = "fn g = f (match x\n    A ->\n        let a = 1\n        let b = 2\n    B -> 3)";
        let result = crate::parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        let ridge_ast::Item::Fn(fn_decl) = &result.module.items[0] else {
            panic!("expected Item::Fn");
        };
        let body = match &fn_decl.body {
            ridge_ast::Body::Expr(e) => e,
            other => panic!("expected Body::Expr, got {other:?}"),
        };
        let arms = extract_match_arms_from_call_arg(body)
            .unwrap_or_else(|| panic!("could not find nested match in fn body: {body:?}"));
        assert_eq!(
            arms.len(),
            2,
            "expected 2 arms (A-body and B), got {} arms: {:?}",
            arms.len(),
            arms
        );
        // A arm body: 2-stmt Block (let a, let b).
        assert!(
            matches!(&arms[0].body, Expr::Block(b) if b.stmts.len() == 2),
            "expected A arm to have a 2-stmt Block body (let a + let b), got {:?}",
            arms[0].body
        );
    }

    // ── E4-2: single-statement arm body unchanged ─────────────────────────────
    //
    // Regression: a one-statement no-layout arm body must still produce a
    // single Expr (not wrapped in Expr::Block).
    #[test]
    fn e4_single_stmt_arm_body_not_wrapped() {
        let src = "fn g = f (match x\n    A -> 1\n    B -> 2)";
        let result = crate::parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        let ridge_ast::Item::Fn(fn_decl) = &result.module.items[0] else {
            panic!("expected Item::Fn");
        };
        let body = match &fn_decl.body {
            ridge_ast::Body::Expr(e) => e,
            other => panic!("expected Body::Expr, got {other:?}"),
        };
        let arms = extract_match_arms_from_call_arg(body)
            .unwrap_or_else(|| panic!("could not find nested match in fn body: {body:?}"));
        assert_eq!(arms.len(), 2, "expected 2 arms, got {}", arms.len());
        // Both arm bodies should be plain IntDec literals, NOT blocks.
        assert!(
            !matches!(&arms[0].body, Expr::Block(_)),
            "single-stmt arm body must NOT be Block, got {:?}",
            arms[0].body
        );
        assert!(
            !matches!(&arms[1].body, Expr::Block(_)),
            "single-stmt arm body must NOT be Block, got {:?}",
            arms[1].body
        );
    }

    // ── E4-3: nested match inside body terminates body via is_match_arm_start ──
    //
    // An arm body that is itself a `match` expression must parse correctly:
    // the inner match grabs its own arms, and the outer arm ends after the
    // inner match terminates (by column rule).
    //
    //   f (match outer
    //       A ->
    //           match inner
    //               X -> 1
    //               Y -> 2
    //       B -> 3)
    //
    // Outer match: 2 arms. Inner match inside A: 2 arms.
    #[test]
    fn e4_nested_match_in_arm_body_terminates_correctly() {
        let src = "fn g = f (match outer\n    A ->\n        match inner\n            X -> 1\n            Y -> 2\n    B -> 3)";
        let result = crate::parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected errors: {:?}",
            result.errors
        );
        let ridge_ast::Item::Fn(fn_decl) = &result.module.items[0] else {
            panic!("expected Item::Fn");
        };
        let body = match &fn_decl.body {
            ridge_ast::Body::Expr(e) => e,
            other => panic!("expected Body::Expr, got {other:?}"),
        };
        let arms = extract_match_arms_from_call_arg(body)
            .unwrap_or_else(|| panic!("could not find nested match in fn body: {body:?}"));
        assert_eq!(
            arms.len(),
            2,
            "outer match should have 2 arms (A and B), got {} arms: {:?}",
            arms.len(),
            arms
        );
        // A's body is the inner match.
        if let Expr::Match {
            arms: inner_arms, ..
        } = &arms[0].body
        {
            assert_eq!(
                inner_arms.len(),
                2,
                "inner match should have 2 arms (X and Y), got {} arms: {:?}",
                inner_arms.len(),
                inner_arms
            );
        } else {
            panic!("expected A arm body to be a Match, got {:?}", arms[0].body);
        }
    }

    // ── T13-5: parse_let_simple_not_wrapped ──────────────────────────────────
    //
    // Key invariant: single-line `let x = 1` must produce
    // `Let { value: Literal(IntDec(1)) }` — NOT wrapped in Expr::Block.

    #[test]
    fn parse_let_simple_not_wrapped() {
        let e = ok("let x = 1");
        if let Expr::Let { value, .. } = e {
            assert!(
                matches!(*value, Expr::Literal(Literal::IntDec { ref raw, .. }) if raw == "1"),
                "expected bare IntDec(1), got {value:?}"
            );
            assert!(
                !matches!(*value, Expr::Block(_)),
                "single-line value must NOT be wrapped in Expr::Block"
            );
        } else {
            panic!("expected Let, got {e:?}");
        }
    }
}

// ── E5: nested-match no-layout fixtures ──────────────────────────────────────
//
// These 5 tests lock in the fix from E3/E4.  All use `parse_source` (which
// supplies the LineMap) so the column rule is active.

#[cfg(test)]
#[allow(clippy::panic)]
#[allow(clippy::unwrap_used)]
#[allow(clippy::expect_used)]
#[allow(clippy::match_wildcard_for_single_variants)]
mod nested_match_no_layout {
    use ridge_ast::Expr;

    /// Helper: parse `src` with `LineMap` and assert no errors.
    fn ok_source(src: &str) -> ridge_ast::Module {
        let result = crate::parse_source(src);
        assert!(
            result.errors.is_empty(),
            "unexpected parse errors in {:?}: {:?}",
            src,
            result.errors
        );
        assert!(
            result.lex_errors.is_empty(),
            "unexpected lex errors in {:?}: {:?}",
            src,
            result.lex_errors
        );
        result.module
    }

    /// Drill into a module with a single fn whose body is a Call whose first
    /// arg (unwrapped through Paren) is a Match.  Returns the arms.
    fn arms_from_fn_call_match(module: &ridge_ast::Module) -> &Vec<ridge_ast::MatchArm> {
        let ridge_ast::Item::Fn(fn_decl) = &module.items[0] else {
            panic!("expected Item::Fn");
        };
        let body = match &fn_decl.body {
            ridge_ast::Body::Expr(e) => e,
            other => panic!("expected Body::Expr, got {other:?}"),
        };
        if let Expr::Call { args, .. } = body {
            if let Some(arg) = args.first() {
                let inner = if let Expr::Paren { inner, .. } = arg {
                    inner.as_ref()
                } else {
                    arg
                };
                if let Expr::Match { arms, .. } = inner {
                    return arms;
                }
            }
        }
        panic!("expected Call{{ Paren{{ Match }} }} in fn body, got: {body:?}");
    }

    // ── E5-1: inner match in arm body with dedented sibling — url_shortener shape ──
    //
    // Canonical reproducer from §1.1:
    //
    //   f (match (method, path)
    //       ("POST", "/shorten") -> postResult
    //       ("GET", path) when path != "/" ->
    //           let target = lookup ()
    //           match target
    //               Some url -> redirect url
    //               None     -> notFound "not found"
    //       _ -> notFound "Not found")
    //
    // Outer match: 3 arms.  Inner match: 2 arms.
    #[test]
    fn e5_1_inner_match_with_dedented_sibling_outer_arm() {
        let src = concat!(
            "fn h = f (match (method, path)\n",
            "    (\"POST\", \"/shorten\") -> postResult\n",
            "    (\"GET\", path) when path != \"/\" ->\n",
            "        let target = lookup ()\n",
            "        match target\n",
            "            Some url -> redirect url\n",
            "            None     -> notFound \"not found\"\n",
            "    _ -> notFound \"Not found\")"
        );
        let module = ok_source(src);
        let outer_arms = arms_from_fn_call_match(&module);
        assert_eq!(
            outer_arms.len(),
            3,
            "outer match should have 3 arms, got {}: {:?}",
            outer_arms.len(),
            outer_arms
        );
        // Middle arm body contains the inner match.
        if let Expr::Match { arms: inner, .. } = &outer_arms[1].body {
            assert_eq!(
                inner.len(),
                2,
                "inner match should have 2 arms (Some, None), got {}: {:?}",
                inner.len(),
                inner
            );
        } else {
            // The middle arm body may be wrapped in a Block if there are multiple
            // statements.  Unwrap one level.
            if let Expr::Block(block) = &outer_arms[1].body {
                let last = block.stmts.last().expect("block must be non-empty");
                if let Expr::Match { arms: inner, .. } = last {
                    assert_eq!(
                        inner.len(),
                        2,
                        "inner match should have 2 arms (Some, None), got {}: {:?}",
                        inner.len(),
                        inner
                    );
                    return;
                }
            }
            panic!(
                "expected inner match in middle arm body, got {:?}",
                outer_arms[1].body
            );
        }
    }

    // ── E5-2: two siblings each containing a nested match ───────────────────────
    //
    //   f (match x
    //       A ->
    //           match inner1
    //               X -> 1
    //               Y -> 2
    //       B ->
    //           match inner2
    //               P -> 3
    //               Q -> 4)
    //
    // Outer: 2 arms.  Each arm body: a 2-arm inner match.
    #[test]
    fn e5_2_two_siblings_each_with_nested_match() {
        let src = concat!(
            "fn h = f (match x\n",
            "    A ->\n",
            "        match inner1\n",
            "            X -> 1\n",
            "            Y -> 2\n",
            "    B ->\n",
            "        match inner2\n",
            "            P -> 3\n",
            "            Q -> 4)"
        );
        let module = ok_source(src);
        let outer_arms = arms_from_fn_call_match(&module);
        assert_eq!(
            outer_arms.len(),
            2,
            "outer match should have 2 arms (A, B), got {}: {:?}",
            outer_arms.len(),
            outer_arms
        );
        for (i, arm) in outer_arms.iter().enumerate() {
            if let Expr::Match { arms: inner, .. } = &arm.body {
                assert_eq!(
                    inner.len(),
                    2,
                    "arm {} inner match should have 2 arms, got {}: {:?}",
                    i,
                    inner.len(),
                    inner
                );
            } else {
                panic!("arm {} body should be a Match, got {:?}", i, arm.body);
            }
        }
    }

    // ── E5-3: same-line match: `match x ("a") -> 1, ("b") -> 2` ────────────────
    //
    // Single-line no-layout match inside a paren — all arms are on the same line
    // as the match keyword.  MATCH_COL applies but all arms have the same column
    // so they are all admitted.
    //
    // "f (match x ("a") -> 1 ("b") -> 2)" — comma-separated isn't valid Ridge;
    // instead use separate arms on the SAME line (same column → all admitted).
    #[test]
    fn e5_3_same_line_match_all_arms_admitted() {
        // Arms on the same line as `match x`: same column as MATCH_COL.
        // In no-layout mode, all tokens on the same line have no Newline between them.
        // The column rule: arm_col == match_col → still admitted (< is the terminator).
        let src = "fn h = f (match x (\"a\") -> 1)";
        let module = ok_source(src);
        let outer_arms = arms_from_fn_call_match(&module);
        assert_eq!(
            outer_arms.len(),
            1,
            "single-arm same-line match should have 1 arm, got {}: {:?}",
            outer_arms.len(),
            outer_arms
        );
    }

    // ── E5-4: match inside lambda inside call inside arm ────────────────────────
    //
    //   f (match x
    //       A -> List.map (fn y ->
    //           match y
    //               P -> 1
    //               Q -> 2) items
    //       B -> 3)
    //
    // The inner match is inside a lambda that is inside a call that is the arm body.
    // Outer match: 2 arms.  Inner match (inside lambda inside call): 2 arms.
    #[test]
    fn e5_4_match_inside_lambda_inside_call_inside_arm() {
        let src = concat!(
            "fn h = f (match x\n",
            "    A -> List.map (fn y ->\n",
            "        match y\n",
            "            P -> 1\n",
            "            Q -> 2) items\n",
            "    B -> 3)"
        );
        let module = ok_source(src);
        let outer_arms = arms_from_fn_call_match(&module);
        assert_eq!(
            outer_arms.len(),
            2,
            "outer match should have 2 arms (A, B), got {}: {:?}",
            outer_arms.len(),
            outer_arms
        );
        // B arm is a simple literal — regression check.
        assert!(
            matches!(&outer_arms[1].body, Expr::Literal(_)),
            "B arm body should be a literal, got {:?}",
            outer_arms[1].body
        );
    }

    // ── E5-5: single-arm nested match (degenerate) ──────────────────────────────
    //
    //   f (match x
    //       A ->
    //           match inner
    //               Only -> 1
    //       B -> 2)
    //
    // Inner match has a single arm.  Outer match has 2 arms.
    #[test]
    fn e5_5_single_arm_nested_match() {
        let src = concat!(
            "fn h = f (match x\n",
            "    A ->\n",
            "        match inner\n",
            "            Only -> 1\n",
            "    B -> 2)"
        );
        let module = ok_source(src);
        let outer_arms = arms_from_fn_call_match(&module);
        assert_eq!(
            outer_arms.len(),
            2,
            "outer match should have 2 arms (A, B), got {}: {:?}",
            outer_arms.len(),
            outer_arms
        );
        if let Expr::Match { arms: inner, .. } = &outer_arms[0].body {
            assert_eq!(
                inner.len(),
                1,
                "inner match should have 1 arm (Only), got {}: {:?}",
                inner.len(),
                inner
            );
        } else {
            panic!(
                "A arm body should be inner Match, got {:?}",
                outer_arms[0].body
            );
        }
    }
}
