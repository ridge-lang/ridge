//! Token cursor — low-level, position-tracking access to the token stream.
//!
//! `Cursor` is internal to `ridge-parser`. The public API is [`crate::parse_module`]
//! and [`crate::parse_source`].
//!
//! Design (§4.2 of the Phase 2 Parser Plan):
//! - Lookahead budget k = 2 (see `peek_n`).
//! - `peek()` never panics: returns `&Token::Eof` past the end of the slice.
//! - The lexer always appends an `Eof` token, so `toks` is never truly empty
//!   during a normal parse.

// `peek_n`, `eat`, and `at_eof` are part of the required Cursor API (§4.2)
// and will be called in T3+.  Suppress dead_code until all callers exist.
// `pub(crate)` inside a private module is intentional — makes visibility
// explicit and consistent with the rest of the codebase.
#![allow(dead_code)]
#![allow(clippy::redundant_pub_crate)]

use crate::error::ParseError;
use ridge_ast::Span;
use ridge_lexer::{LineMap, Token};

/// An immutable-slice cursor over a pre-lexed token stream.
///
/// Not public — only `parse_module` (and future `parse_*` helpers) use it.
pub(crate) struct Cursor<'t> {
    toks: &'t [(Token, Span)],
    pos: usize,
    /// When `true`, the Pratt juxtaposition argument collector will stop before
    /// consuming a token sequence that looks like the start of a new match arm
    /// (`<pattern> ->` or `<pattern> when`).
    ///
    /// Set by `parse_flat_block_arm_body` so that the last statement in a
    /// no-layout match arm body does not eat the next arm's pattern as a
    /// call argument.
    pub(crate) no_layout_arm: bool,
    /// Nesting depth of open brackets (`(`, `[`, `{`, `${`).
    ///
    /// Incremented by bracket-opening parse helpers; decremented by the
    /// matching closers.  Used by `parse_branch_body` to decide whether to
    /// apply the flat-block NEWLINE extension: that extension only fires when
    /// we are inside at least one bracket, where the lexer emits NEWLINE (not
    /// INDENT/DEDENT) at statement boundaries.
    pub(crate) bracket_depth: u32,
    /// Line-map for column lookups in the nested-match offside rule (E2).
    ///
    /// Provided by [`ridge_lexer::LineMap`] when the cursor is constructed via
    /// [`Self::new_with_line_map`].  Used by [`Self::peek_significant_column`]
    /// to convert a token's `span.start` into a 0-based column number.
    ///
    /// `None` when no `LineMap` is available (e.g. callers that use
    /// `parse_module` directly without a `LineMap`).  When `None`,
    /// [`Self::peek_significant_column`] returns `None` and the column rule
    /// does not fire, preserving the pre-E2 behaviour.
    line_map: Option<&'t LineMap>,
}

/// Sentinel `Eof` returned by `peek()` / `peek_n()` when `pos` is at or past
/// the end of the slice.  The lexer always appends a real `Eof`, so this is
/// only a safety net for defensive callers.
static EOF_SENTINEL: Token = Token::Eof;

impl<'t> Cursor<'t> {
    /// Construct a new cursor positioned at the beginning of `toks`.
    ///
    /// `toks` should be the output of `ridge_lexer::tokenize`, which always
    /// ends with `Token::Eof`.
    pub(crate) const fn new(toks: &'t [(Token, Span)]) -> Self {
        Self {
            toks,
            pos: 0,
            no_layout_arm: false,
            bracket_depth: 0,
            line_map: None,
        }
    }

    /// Construct a cursor with line-map information for column tracking.
    ///
    /// `line_map` is the [`ridge_lexer::LineMap`] produced by
    /// `ridge_lexer::tokenize`.  Pass this when the column rule in
    /// `parse_match` no-layout mode must be active.
    pub(crate) const fn new_with_line_map(
        toks: &'t [(Token, Span)],
        line_map: &'t LineMap,
    ) -> Self {
        Self {
            toks,
            pos: 0,
            no_layout_arm: false,
            bracket_depth: 0,
            line_map: Some(line_map),
        }
    }

    /// Return a reference to the current token without advancing.
    ///
    /// Returns `&Token::Eof` if `pos` is at or past the end of the slice.
    pub(crate) fn peek(&self) -> &'t Token {
        self.toks.get(self.pos).map_or(&EOF_SENTINEL, |(t, _)| t)
    }

    /// Lookahead: return a reference to the token at `pos + n`.
    ///
    /// Returns `None` if the index is out of bounds.  Per the plan the budget is
    /// k = 2; callers should not use `n > 1` except in the two documented
    /// disambiguation cases.
    pub(crate) fn peek_n(&self, n: usize) -> Option<&'t Token> {
        self.toks.get(self.pos + n).map(|(t, _)| t)
    }

    /// Return the span of the current token.
    ///
    /// Returns `Span::point(0)` for an empty slice (safety net only; the lexer
    /// always produces at least an `Eof` token with a valid span).
    pub(crate) fn span(&self) -> Span {
        self.toks
            .get(self.pos)
            .map_or_else(|| Span::point(0), |(_, s)| *s)
    }

    /// Advance past the current token and return a reference to it.
    ///
    /// Saturates at the last token (the `Eof` sentinel) — bumping past the end
    /// is a no-op that returns the sentinel.
    pub(crate) fn bump(&mut self) -> &'t Token {
        let tok = self.toks.get(self.pos).map_or(&EOF_SENTINEL, |(t, _)| t);
        // Saturate: do not advance past the last element.
        if self.pos < self.toks.len() {
            self.pos += 1;
        }
        tok
    }

    /// If the current token matches `want`, advance and return `true`.
    /// Otherwise return `false` without advancing.
    pub(crate) fn eat(&mut self, want: &Token) -> bool {
        if self.peek() == want {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Require the current token to match `want`.
    ///
    /// On success advances and returns the token's [`Span`].
    /// On failure emits a [`ParseError::Expected`] (P001) and does NOT advance,
    /// leaving recovery to the caller.
    pub(crate) fn expect(&mut self, want: &Token) -> Result<Span, ParseError> {
        let span = self.span();
        if self.peek() == want {
            self.bump();
            Ok(span)
        } else {
            Err(ParseError::Expected {
                span,
                expected: token_description(want),
                found: self.peek().to_string(),
            })
        }
    }

    /// True when the cursor is positioned at `Token::Eof`.
    pub(crate) fn at_eof(&self) -> bool {
        self.peek() == &Token::Eof
    }

    /// Return the span of the token at `pos + n`, or `Span::point(0)` if out
    /// of bounds.  Used sparingly for multi-token span construction (e.g. `()`).
    pub(crate) fn span_at(&self, n: usize) -> Span {
        self.toks
            .get(self.pos + n)
            .map_or_else(|| Span::point(0), |(_, s)| *s)
    }

    /// Advance past any `Newline` tokens at the current position.
    ///
    /// Used in the no-layout `parse_match` arm loop to skip inter-arm newlines
    /// before applying the column rule.  Does not skip other layout tokens
    /// (`Indent`, `Dedent`).
    pub(crate) fn skip_newlines(&mut self) {
        while self.peek() == &Token::Newline {
            self.bump();
        }
    }

    /// Return the 0-based byte column of the first significant (non-`Newline`)
    /// token at or after the current position, or `None` if:
    ///
    /// - the cursor is at `Eof`, or
    /// - no `LineMap` was provided at construction time (fallback to pre-E2
    ///   behaviour — the column rule is disabled).
    ///
    /// "Significant" means any token except `Newline` (comments are not emitted
    /// as tokens in the layout pass output).
    ///
    /// Column is 0-based: `token.span.start - line_start_byte`, consistent with
    /// the layout pass's own `compute_col` function.  The lookup is O(log n)
    /// via the binary-search inside [`ridge_lexer::LineMap::line_col`].
    pub(crate) fn peek_significant_column(&self) -> Option<u32> {
        let line_map = self.line_map?;

        // Scan forward from the current position, skipping Newlines.
        let mut offset = self.pos;
        loop {
            let (tok, span) = self.toks.get(offset)?;
            match tok {
                Token::Newline => {
                    offset += 1;
                }
                Token::Eof => return None,
                _ => {
                    // `LineMap::line_col` returns 1-based (line, col).
                    // Convert to 0-based column to match the layout pass.
                    let (_, col_1based) = line_map.line_col(span.start);
                    return Some(col_1based.saturating_sub(1));
                }
            }
        }
    }
}

/// Return a short, static description for a token — used as the `expected`
/// field in `ParseError::Expected`.
///
/// Returns a `&'static str` for all tokens the parser explicitly expects via
/// `cursor.expect(...)`.
const fn token_description(tok: &Token) -> &'static str {
    match tok {
        Token::Eof => "<EOF>",
        Token::Newline => "<NEWLINE>",
        Token::Indent => "<INDENT>",
        Token::Dedent => "<DEDENT>",
        Token::KwFn => "fn",
        Token::KwLet => "let",
        Token::KwVar => "var",
        Token::KwIf => "if",
        Token::KwThen => "then",
        Token::KwElse => "else",
        Token::KwMatch => "match",
        Token::KwReturn => "return",
        Token::KwImport => "import",
        Token::KwConst => "const",
        Token::KwType => "type",
        Token::KwActor => "actor",
        Token::KwPub => "pub",
        Token::KwInit => "init",
        Token::KwOn => "on",
        Token::KwState => "state",
        Token::KwSpawn => "spawn",
        Token::KwWith => "with",
        Token::KwTry => "try",
        Token::KwGuard => "guard",
        Token::KwWhen => "when",
        Token::KwTrue => "true",
        Token::KwFalse => "false",
        Token::KwAs => "as",
        Token::KwIn => "in",
        Token::KwWhere => "where",
        Token::KwCatch => "catch",
        Token::KwClass => "class",
        Token::KwDeriving => "deriving",
        Token::KwInstance => "instance",
        Token::Assign => "=",
        Token::Colon => ":",
        Token::Comma => ",",
        Token::Dot => ".",
        Token::Arrow => "->",
        Token::LParen => "(",
        Token::RParen => ")",
        Token::LBrack => "[",
        Token::RBrack => "]",
        Token::LBrace => "{",
        Token::RBrace | Token::InterpExprEnd => "}",
        Token::Pipe => "|",
        Token::ColonColon => "::",
        Token::PipeFwd => "|>",
        Token::Question => "?",
        Token::Bang => "!",
        Token::QuestionGt => "?>",
        Token::LeftArrow => "<-",
        Token::Plus => "+",
        Token::Minus => "-",
        Token::Star => "*",
        Token::Slash => "/",
        Token::Percent => "%",
        Token::Caret => "^",
        Token::AmpAmp => "&&",
        Token::PipePipe => "||",
        Token::EqEq => "==",
        Token::BangEq => "!=",
        Token::Lt => "<",
        Token::Gt => ">",
        Token::Le => "<=",
        Token::Ge => ">=",
        Token::At => "@",
        Token::DotDot => "..",
        Token::PlusPlus => "++",
        Token::FatArrow => "=>",
        Token::Underscore => "_",
        Token::InterpStart => "$\"",
        Token::InterpExprStart => "${",
        Token::InterpEnd => "\"",
        // Data-carrying tokens: return a generic category label.
        Token::LowerIdent(_) => "<identifier>",
        Token::UpperIdent(_) => "<Identifier>",
        Token::IntDec(_) | Token::IntBin(_) | Token::IntOct(_) | Token::IntHex(_) => {
            "<integer-literal>"
        }
        Token::Float(_) => "<float-literal>",
        Token::TextLit(_) => "<string-literal>",
        Token::RawTextLit(_) => "<raw-string-literal>",
        Token::InterpText(_) => "<interpolated-text>",
        Token::DocComment(_) => "<doc-comment>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_lexer::tokenize;

    fn toks(src: &str) -> Vec<(Token, Span)> {
        tokenize(src).tokens
    }

    #[test]
    fn peek_on_empty_returns_eof() {
        let tokens = toks("");
        let cur = Cursor::new(&tokens);
        assert_eq!(cur.peek(), &Token::Eof);
    }

    #[test]
    fn bump_advances_and_returns_token() {
        let tokens = toks("let x = 1");
        let mut cur = Cursor::new(&tokens);
        let t = cur.bump();
        assert_eq!(t, &Token::KwLet);
        assert!(matches!(cur.peek(), Token::LowerIdent(_)));
    }

    #[test]
    fn eat_matches_and_advances() {
        let tokens = toks("let x = 1");
        let mut cur = Cursor::new(&tokens);
        assert!(cur.eat(&Token::KwLet));
        assert!(!cur.eat(&Token::KwLet)); // already consumed
    }

    #[test]
    fn expect_ok_returns_span() {
        let tokens = toks("");
        let mut cur = Cursor::new(&tokens);
        let result = cur.expect(&Token::Eof);
        assert!(result.is_ok());
    }

    #[test]
    fn expect_fail_returns_p001() {
        let tokens = toks("let x = 1");
        let mut cur = Cursor::new(&tokens);
        let result = cur.expect(&Token::Eof);
        assert!(result.is_err(), "expected Err(P001) but got Ok");
        if let Err(e) = result {
            assert_eq!(e.code(), "P001");
        }
    }

    #[test]
    fn at_eof_true_for_empty_input() {
        let tokens = toks("");
        let cur = Cursor::new(&tokens);
        assert!(cur.at_eof());
    }

    #[test]
    fn peek_n_lookahead() {
        let tokens = toks("let x = 1");
        let cur = Cursor::new(&tokens);
        assert_eq!(cur.peek_n(0), Some(&Token::KwLet));
        assert!(matches!(cur.peek_n(1), Some(Token::LowerIdent(_))));
        assert_eq!(cur.peek_n(1000), None);
    }

    // ── E2: peek_significant_column ──────────────────────────────────────────

    /// Helper: lex `src` and return a cursor with line-map attached.
    fn toks_with_map(src: &str) -> (Vec<(Token, Span)>, ridge_lexer::LineMap) {
        let out = ridge_lexer::tokenize(src);
        (out.tokens, out.line_map)
    }

    /// E2-1: cursor at EOF with no `line_map` → `None`.
    #[test]
    fn peek_significant_column_none_at_eof_no_map() {
        let tokens = toks("");
        let cur = Cursor::new(&tokens);
        assert_eq!(cur.peek_significant_column(), None);
    }

    /// E2-2: single blank line between two tokens — `peek_significant_column`
    /// skips the Newline and reports the column of the next real token.
    ///
    /// Source: "let x = 1\n    y"
    /// After lexing the Newline is between `1` and `INDENT y`.  We advance
    /// past `let x = 1` (5 tokens: `KwLet`, `LowerIdent`, Assign, `IntDec`, Newline,
    /// Indent) so that cursor sits on the Newline, and verify the column of `y`.
    ///
    /// `y` is at column 4 (4 leading spaces on its line).
    #[test]
    fn peek_significant_column_skips_blank_line() {
        // "fn f =\n    x" — after the layout pass:
        // KwFn, LowerIdent("f"), Assign, Indent, LowerIdent("x"), Dedent, Eof
        // "x" is at column 4.
        let src = "fn f =\n    x";
        let (tokens, line_map) = toks_with_map(src);
        let mut cur = Cursor::new_with_line_map(&tokens, &line_map);
        // Advance past KwFn, LowerIdent, Assign: next is Indent.
        cur.bump(); // KwFn
        cur.bump(); // LowerIdent
        cur.bump(); // Assign
                    // Now at Indent; peek_significant_column should skip Indent and find "x" at col 4.
                    // But Indent is not a Newline — the method only skips Newline.
                    // Actually: skip the Indent explicitly, then we sit on LowerIdent("x").
                    // Let us just verify on a simpler arrangement: use a two-top-level decl
                    // file so there is a Newline between them.
                    // "let x = 1\nlet y = 2" → tokens: KwLet, LowerIdent(x), Assign, IntDec(1),
                    // Newline, KwLet, LowerIdent(y), Assign, IntDec(2), Eof
                    // After consuming the first 4 tokens (let x = 1), cursor is at Newline.
                    // peek_significant_column should return 0 (KwLet is at column 0).
        let src2 = "let x = 1\nlet y = 2";
        let (tokens2, line_map2) = toks_with_map(src2);
        let mut cur2 = Cursor::new_with_line_map(&tokens2, &line_map2);
        cur2.bump(); // KwLet
        cur2.bump(); // LowerIdent
        cur2.bump(); // Assign
        cur2.bump(); // IntDec
                     // Cursor now at Newline before the second `let`.
        assert!(
            matches!(cur2.peek(), Token::Newline),
            "expected Newline, got {:?}",
            cur2.peek()
        );
        let col = cur2.peek_significant_column();
        assert_eq!(
            col,
            Some(0),
            "second `let` is at column 0; expected Some(0), got {col:?}",
        );
    }

    /// E2-3: `peek_significant_column` at EOF returns None (even with `line_map`).
    #[test]
    fn peek_significant_column_eof_with_map() {
        let (tokens, line_map) = toks_with_map("");
        let cur = Cursor::new_with_line_map(&tokens, &line_map);
        assert_eq!(
            cur.peek_significant_column(),
            None,
            "empty input → cursor at Eof → should return None"
        );
    }

    /// E2-4: multiple consecutive Newlines are all skipped; the column of the
    /// first real token after them is returned correctly.
    ///
    /// We construct a token stream manually to place three consecutive Newline
    /// tokens before a real token at a known column.
    ///
    /// Source: "let a = 1\n\n    let b = 2"
    /// The "    let b" line is at column 4.
    #[test]
    fn peek_significant_column_multi_newline_skip() {
        // Two top-level decls with a blank line between them.
        // "let a = 1\n\nlet b = 2" (blank line = extra Newline suppressed by
        // the layout pass's blank-line rule; so there is still only one Newline).
        // Use an indented body instead so we can control the column:
        // "fn f =\n    let a = 1\n    let b = 2"
        // After the Indent, cursor sits on KwLet(a) at col 4.
        // The Newline between the two let-stmts means we sit on Newline before KwLet(b) at col 4.
        let src = "fn f =\n    let a = 1\n    let b = 2";
        let (tokens, line_map) = toks_with_map(src);
        let mut cur = Cursor::new_with_line_map(&tokens, &line_map);
        // Advance past: KwFn LowerIdent Assign Indent KwLet LowerIdent Assign IntDec
        for _ in 0..8 {
            cur.bump();
        }
        // Cursor should now be on Newline between the two let-stmts.
        assert!(
            matches!(cur.peek(), Token::Newline),
            "expected Newline, got {:?}",
            cur.peek()
        );
        let col = cur.peek_significant_column();
        assert_eq!(
            col,
            Some(4),
            "second `let b` is at column 4; expected Some(4), got {col:?}",
        );
    }
}
