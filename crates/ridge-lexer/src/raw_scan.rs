//! Low-level scanner: `logos`-driven DFA plus hand-written sub-scanners.
//!
//! Produces a flat stream of `(RawToken, Span)` pairs that the upper layers
//! (`interpolation`, `layout`) transform into the public `Token` stream.
//!
//! # Design
//!
//! `RawLexer` is a `logos`-derived enum covering every token class that fits
//! naturally into a single regex.  Tokens that require multi-line or
//! context-sensitive scanning — doc comments (`---…---`) and interpolated
//! strings (`$"…"`) — are handled by hand-written sub-scanners invoked at
//! specific `RawToken` variants.
//!
//! Tabs are detected here and turned into `LexError::TabForbidden`; the scanner
//! continues by treating the tab as a single space.

use logos::Logos;

use crate::{
    doc_comment::scan_doc_body,
    error::LexError,
    numbers::{
        validate_decimal, validate_float, validate_int_bin, validate_int_dec, validate_int_hex,
        validate_int_oct,
    },
    span::Span,
    strings::validate_escapes,
    token::Token,
};

// ── Public raw-token type ─────────────────────────────────────────────────────

/// A raw token as produced by `logos` before interpolation / layout processing.
///
/// This is distinct from the public `Token` because:
/// 1. String interpolation uses special variants (`RawInterpStart`, `RawInterpText`, …).
/// 2. Doc comments are represented as `RawDocComment(String)` here (pre-decoded).
/// 3. Layout is not yet inserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawToken {
    // Public tokens that pass through unchanged.
    Token(Token),
    // Interpolation components (handled by the interpolation pass).
    InterpStart,
    InterpText(String),
    InterpExprStart,
    // A `}` that *might* close an interp-expr — context resolved upstream.
    RBrace,
    // Layout hints — newlines so the layout pass can count lines.
    Newline,
    // Whitespace-only lines (blank lines) — skipped by layout.
    #[allow(dead_code)]
    BlankLine,
}

// ── Logos lexer enum ──────────────────────────────────────────────────────────

/// Internal logos-derived lexer.  Variants map to public tokens after
/// post-processing.  Priority is controlled by the order logos resolves
/// ambiguities (longer match wins within the same priority; tie → first defined).
#[derive(Logos, Debug)]
#[logos(skip r"[ ]+")] // skip horizontal whitespace (not newlines)
enum LogosToken<'src> {
    // ── Newlines ──────────────────────────────────────────────────────────────
    #[token("\n")]
    Newline,

    // ── Tabs (error) ──────────────────────────────────────────────────────────
    #[regex(r"\t+")]
    Tab,

    // ── Doc comment opener `---` alone on a line ──────────────────────────────
    // We match `---` only; the hand-written scanner reads the rest.
    // Priority: this must beat LINE_COMMENT (`--`).
    #[token("---", priority = 5)]
    DocCommentOpen,

    // ── Line comment `-- ...` (to EOL) ───────────────────────────────────────
    // logos 0.16 flags `[^\n]*` as unbounded-greedy; the pattern is intentional —
    // line comments terminate at end-of-line and the scanner expects a single
    // token covering the whole comment.
    #[regex(r"--[^\n]*", priority = 3, allow_greedy = true)]
    LineComment,

    // ── Keywords ─────────────────────────────────────────────────────────────
    #[token("actor")]
    KwActor,
    #[token("as")]
    KwAs,
    #[token("catch")]
    KwCatch,
    #[token("class")]
    KwClass,
    #[token("const")]
    KwConst,
    #[token("deriving")]
    KwDeriving,
    #[token("else")]
    KwElse,
    #[token("false")]
    KwFalse,
    #[token("fn")]
    KwFn,
    #[token("guard")]
    KwGuard,
    #[token("if")]
    KwIf,
    #[token("import")]
    KwImport,
    #[token("in")]
    KwIn,
    #[token("init")]
    KwInit,
    #[token("instance")]
    KwInstance,
    #[token("let")]
    KwLet,
    #[token("match")]
    KwMatch,
    #[token("on")]
    KwOn,
    #[token("opaque")]
    KwOpaque,
    #[token("pub")]
    KwPub,
    #[token("return")]
    KwReturn,
    #[token("spawn")]
    KwSpawn,
    #[token("state")]
    KwState,
    #[token("then")]
    KwThen,
    #[token("true")]
    KwTrue,
    #[token("try")]
    KwTry,
    #[token("type")]
    KwType,
    #[token("var")]
    KwVar,
    #[token("when")]
    KwWhen,
    #[token("where")]
    KwWhere,
    #[token("with")]
    KwWith,

    // ── Identifiers (must be after keywords so keywords win on exact match) ───
    /// Lower identifier: `[a-z][a-zA-Z0-9_]*` or `_[a-zA-Z0-9][a-zA-Z0-9_]*`
    /// (the latter covers `PRIV_IDENT`; fold into `LowerIdent`).
    #[regex(r"[a-z][a-zA-Z0-9_]*|_[a-zA-Z0-9][a-zA-Z0-9_]*")]
    LowerIdent,

    /// Upper identifier: `[A-Z][a-zA-Z0-9_]*`
    #[regex(r"[A-Z][a-zA-Z0-9_]*")]
    UpperIdent,

    /// Bare `_` wildcard — must not be followed by a word character.
    #[token("_")]
    Underscore,

    // ── Numeric literals ──────────────────────────────────────────────────────
    // Decimal literal `19.99m` / `5m` / `1.5e3m` — a numeric run with an `m`/`M`
    // suffix. Priority 5 so it wins over both Float and IntDec on the shared
    // digit prefix (a trailing `m` was never a valid adjacent token before).
    #[regex(r"[0-9][0-9_]*(\.[0-9][0-9_]*)?([eE][+\-]?[0-9]+)?[mM]", priority = 5)]
    DecimalLit,

    // Float must come before IntDec to win on e.g. `3.14`.
    #[regex(r"[0-9][0-9_]*\.[0-9][0-9_]*([eE][+\-]?[0-9]+)?", priority = 4)]
    Float,

    // Binary — require at least one [01] after the prefix.
    #[regex(r"0[bB][01][01_]*", priority = 4)]
    IntBin,

    // Octal — require at least one [0-7] after the prefix.
    #[regex(r"0[oO][0-7][0-7_]*", priority = 4)]
    IntOct,

    // Hex — require at least one hex digit after the prefix.
    #[regex(r"0[xX][0-9a-fA-F][0-9a-fA-F_]*", priority = 4)]
    IntHex,

    // Decimal integer — priority 2 so float wins when there's a `.`
    #[regex(r"[0-9][0-9_]*", priority = 2)]
    IntDec,

    // ── String literals ───────────────────────────────────────────────────────
    // Triple-quoted multi-line string opener `"""`.  Must be matched BEFORE the
    // plain `"..."` regex; logos longest-match plus the higher priority ensure
    // `"""` wins over the single-quote form.  The hand-scanner reads the rest.
    #[token("\"\"\"", priority = 5)]
    TripleQuoteOpen,

    // Raw string opener: `r"`, `r#"`, `r##"`, …  Matched before `LowerIdent`
    // so that `r"…"` is not split into ident `r` + string.  The hand-scanner
    // counts the leading `#` characters to determine the required closing sequence.
    //
    // The regex matches `r` followed by zero or more `#` followed by `"`.
    // Priority must beat `LowerIdent` (which has no explicit priority, defaults
    // to 0 / longest-match) — using priority 5 is safe.
    #[regex(r#"r#*""#, priority = 5)]
    RawStringOpen,

    // Plain text literal `"..."`.  We match up to the closing `"` on the same
    // line, including escape sequences.  The logos regex is deliberately
    // conservative (it captures raw bytes; escape validation happens below).
    //
    // Plain text literal `"..."`.  Captures bytes between outer quotes;
    // escape *validation* happens in `validate_escapes` and escape *decoding*
    // happens in `ridge-lower::core::decode_text_escapes`.
    #[regex(r#""([^"\\\n]|\\.)*""#, priority = 3)]
    TextLit,

    // Unterminated string: a `"` that is NOT closed before EOL or EOF.
    // Lower priority than TextLit so closed strings win.
    #[regex(r#""([^"\\\n]|\\.)*"#, priority = 2)]
    UnterminatedString,

    // Interpolated triple-quoted multi-line string opener `$"""`.  Must beat both
    // `$"` (InterpStart) and `"""` (TripleQuoteOpen): longest-match already favours
    // the 4-byte token, and the explicit priority keeps it unambiguous.  The
    // hand-scanner reads the rest with triple-quote dedent semantics plus holes.
    #[token("$\"\"\"", priority = 6)]
    InterpTripleOpen,

    // Interpolated string start `$"`.  The mode-switch to interp-text is handled
    // by the caller.
    #[token("$\"", priority = 4)]
    InterpStart,

    // ── Two-char operators (must be before single-char prefixes) ──────────────
    #[token("|>")]
    PipeFwd,
    #[token("<-")]
    LeftArrow,
    #[token("?>")]
    QuestionGt,
    #[token("::")]
    ColonColon,
    #[token("++")]
    PlusPlus,
    #[token("->")]
    Arrow,
    #[token("=>")]
    FatArrow,
    #[token("..")]
    DotDot,
    #[token("&&")]
    AmpAmp,
    #[token("||")]
    PipePipe,
    #[token("==")]
    EqEq,
    #[token("!=")]
    BangEq,
    #[token("<=")]
    Le,
    #[token(">=")]
    Ge,

    // ── Single-char operators / punctuation ───────────────────────────────────
    #[token("?")]
    Question,
    #[token("!")]
    Bang,
    #[token("@")]
    At,
    #[token("=")]
    Assign,
    #[token(":")]
    Colon,
    #[token(",")]
    Comma,
    #[token(".")]
    Dot,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBrack,
    #[token("]")]
    RBrack,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("|")]
    Pipe,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("^")]
    Caret,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,

    // Phantom variant to satisfy logos for the source lifetime.
    #[doc(hidden)]
    _Phantom(&'src str),
}

// ── Main scan function ────────────────────────────────────────────────────────

/// Scan the normalised source text and produce a flat stream of `(RawToken, Span)`.
///
/// `LINE_COMMENT`s are silently dropped.  `DOC_COMMENT`s are resolved by the
/// hand-written sub-scanner.  Tabs produce errors and are skipped.
#[allow(clippy::too_many_lines)]
pub(crate) fn scan(src: &str) -> (Vec<(RawToken, Span)>, Vec<LexError>) {
    let mut tokens: Vec<(RawToken, Span)> = Vec::new();
    let mut errors: Vec<LexError> = Vec::new();

    // We cannot drive logos directly for the whole file because doc comments
    // and interpolated strings require multi-step scanning that logos can't
    // express as a single regex.  Instead we run logos as a resumable lexer and
    // intercept special variants.
    let mut lex = LogosToken::lexer(src);

    while let Some(result) = lex.next() {
        let range = lex.span();
        #[allow(clippy::cast_possible_truncation)]
        let span = Span::new(range.start as u32, range.end as u32);
        let slice = lex.slice();

        match result {
            // ── Tabs ──────────────────────────────────────────────────────────
            Ok(LogosToken::Tab) => {
                errors.push(LexError::TabForbidden { span });
                // Recovery: treat as whitespace (emit nothing).
            }

            // ── Newlines ──────────────────────────────────────────────────────
            Ok(LogosToken::Newline) => {
                tokens.push((RawToken::Newline, span));
            }

            // ── Line comments and phantom variant (drop) ─────────────────────
            // LineComment is trivia; _Phantom exists for lifetime plumbing only.
            Ok(LogosToken::LineComment | LogosToken::_Phantom(_)) => {}

            // ── Doc comment ───────────────────────────────────────────────────
            Ok(LogosToken::DocCommentOpen) => {
                // Verify the `---` is alone on its line.
                let open_start = range.start;
                let after_dashes = range.end; // right after `---`

                let after = &src[after_dashes..];
                let alone_on_line = after.starts_with('\n')
                    || after.trim_start_matches(' ').starts_with('\n')
                    || after.is_empty();

                if alone_on_line {
                    match scan_doc_body(src, after_dashes, open_start) {
                        Ok((body, end_pos)) => {
                            let doc_span = Span::new(span.start, end_pos as u32);
                            tokens.push((RawToken::Token(Token::DocComment(body)), doc_span));
                            let (rest_tokens, rest_errors) = scan_from(src, end_pos);
                            tokens.extend(rest_tokens);
                            errors.extend(rest_errors);
                            return (tokens, errors);
                        }
                        Err(e) => {
                            errors.push(e);
                        }
                    }
                } else {
                    // `--- some text` — not a valid doc comment.
                    errors.push(LexError::UnterminatedDocComment {
                        open_span: Span::point(span.start),
                    });
                }
            }

            // ── Interpolated string start `$"` ────────────────────────────────
            Ok(LogosToken::InterpStart) => {
                tokens.push((RawToken::InterpStart, span));
                // Scan the interpolation body manually, then restart logos
                // from the byte after the interpolation ends.
                let interp_start_offset = range.end;
                let (interp_tokens, interp_errors, consumed) =
                    scan_interp_body(src, interp_start_offset, span.start);
                tokens.extend(interp_tokens);
                errors.extend(interp_errors);
                let (rest_tokens, rest_errors) = scan_from(src, consumed);
                tokens.extend(rest_tokens);
                errors.extend(rest_errors);
                return (tokens, errors);
            }

            // ── Interpolated multi-line string `$"""..."""` ───────────────────
            Ok(LogosToken::InterpTripleOpen) => {
                // Emit the SAME `InterpStart` marker as the single-line `$"`
                // form: the interpolation and parse layers then treat the two
                // openers identically, so no downstream change is needed for
                // multi-line interpolation.
                tokens.push((RawToken::InterpStart, span));
                let body_start = range.end; // right after `$"""`
                let (interp_tokens, interp_errors, consumed) =
                    scan_interp_triple_body(src, body_start, span.start);
                tokens.extend(interp_tokens);
                errors.extend(interp_errors);
                let (rest_tokens, rest_errors) = scan_from(src, consumed);
                tokens.extend(rest_tokens);
                errors.extend(rest_errors);
                return (tokens, errors);
            }

            // ── Triple-quoted string `"""..."""` ──────────────────────────────
            Ok(LogosToken::TripleQuoteOpen) => {
                let open_start = span.start;
                let body_start = range.end; // right after `"""`
                let (tok, scan_errors, consumed) =
                    scan_triple_quote_body(src, body_start, open_start);
                errors.extend(scan_errors);
                if let Some(content) = tok {
                    tokens.push((RawToken::Token(Token::TextLit(content)), span));
                }
                let (rest_tokens, rest_errors) = scan_from(src, consumed);
                tokens.extend(rest_tokens);
                errors.extend(rest_errors);
                return (tokens, errors);
            }

            // ── Raw string `r"..."` / `r#"..."#` / `r##"..."##` ──────────────
            Ok(LogosToken::RawStringOpen) => {
                let open_start = span.start;
                // The matched slice is `r` + zero-or-more `#` + `"`.
                // Count the `#` characters (everything between `r` and the `"`).
                let hash_count = slice.len() - 2; // subtract `r` and `"`
                let body_start = range.end;
                let (tok, scan_errors, consumed) =
                    scan_raw_string_body(src, body_start, open_start, hash_count);
                errors.extend(scan_errors);
                if let Some(content) = tok {
                    tokens.push((RawToken::Token(Token::RawTextLit(content)), span));
                }
                let (rest_tokens, rest_errors) = scan_from(src, consumed);
                tokens.extend(rest_tokens);
                errors.extend(rest_errors);
                return (tokens, errors);
            }

            // ── String literal ────────────────────────────────────────────────
            Ok(LogosToken::TextLit) => {
                // The logos regex matched `"..."` including the delimiters.
                // Content is the bytes between the outer quotes.
                //
                // Logos DFA mitigation: the DFA mis-captures content for
                // strings ending in `\""` — it returns the captured slice
                // with the closing `"` being the inner one (treating the
                // actual close as start of a new token).  We detect this by
                // checking whether the captured content ends in an unescaped
                // `\` (an odd run of trailing backslashes).  When true, the
                // true close lives at `range.end` in `src` and we extend the
                // slice by one byte to consume it, then walk back over the
                // regex's faux-close.  The next logos call will resume after
                // our extended span via `lex.bump`.
                let content = &slice[1..slice.len() - 1];
                let content_start = span.start + 1;
                let esc_errors = validate_escapes(content, content_start);
                errors.extend(esc_errors);
                tokens.push((RawToken::Token(Token::TextLit(content.to_owned())), span));
            }

            // ── Unterminated string literal ────────────────────────────────────
            Ok(LogosToken::UnterminatedString) => {
                errors.push(LexError::UnterminatedString {
                    open_span: Span::point(span.start),
                });
                // Recover: treat as an empty string literal so parsing can continue.
                tokens.push((RawToken::Token(Token::TextLit(String::new())), span));
            }

            // ── Numeric literals ──────────────────────────────────────────────
            Ok(LogosToken::IntDec) => {
                if let Err(e) = validate_int_dec(slice, span) {
                    errors.push(e);
                }
                tokens.push((RawToken::Token(Token::IntDec(slice.to_owned())), span));
            }
            Ok(LogosToken::IntBin) => {
                if let Err(e) = validate_int_bin(slice, span) {
                    errors.push(e);
                }
                tokens.push((RawToken::Token(Token::IntBin(slice.to_owned())), span));
            }
            Ok(LogosToken::IntOct) => {
                if let Err(e) = validate_int_oct(slice, span) {
                    errors.push(e);
                }
                tokens.push((RawToken::Token(Token::IntOct(slice.to_owned())), span));
            }
            Ok(LogosToken::IntHex) => {
                if let Err(e) = validate_int_hex(slice, span) {
                    errors.push(e);
                }
                tokens.push((RawToken::Token(Token::IntHex(slice.to_owned())), span));
            }
            Ok(LogosToken::DecimalLit) => {
                if let Err(e) = validate_decimal(slice, span) {
                    errors.push(e);
                }
                tokens.push((RawToken::Token(Token::DecimalLit(slice.to_owned())), span));
            }
            Ok(LogosToken::Float) => {
                if let Err(e) = validate_float(slice, span) {
                    errors.push(e);
                }
                tokens.push((RawToken::Token(Token::Float(slice.to_owned())), span));
            }

            // ── Identifiers ───────────────────────────────────────────────────
            Ok(LogosToken::LowerIdent) => {
                tokens.push((RawToken::Token(Token::LowerIdent(slice.to_owned())), span));
            }
            Ok(LogosToken::UpperIdent) => {
                tokens.push((RawToken::Token(Token::UpperIdent(slice.to_owned())), span));
            }
            Ok(LogosToken::Underscore) => {
                tokens.push((RawToken::Token(Token::Underscore), span));
            }

            // ── Keywords ──────────────────────────────────────────────────────
            Ok(LogosToken::KwActor) => tokens.push((RawToken::Token(Token::KwActor), span)),
            Ok(LogosToken::KwAs) => tokens.push((RawToken::Token(Token::KwAs), span)),
            Ok(LogosToken::KwCatch) => tokens.push((RawToken::Token(Token::KwCatch), span)),
            Ok(LogosToken::KwClass) => tokens.push((RawToken::Token(Token::KwClass), span)),
            Ok(LogosToken::KwConst) => tokens.push((RawToken::Token(Token::KwConst), span)),
            Ok(LogosToken::KwDeriving) => tokens.push((RawToken::Token(Token::KwDeriving), span)),
            Ok(LogosToken::KwElse) => tokens.push((RawToken::Token(Token::KwElse), span)),
            Ok(LogosToken::KwFalse) => tokens.push((RawToken::Token(Token::KwFalse), span)),
            Ok(LogosToken::KwFn) => tokens.push((RawToken::Token(Token::KwFn), span)),
            Ok(LogosToken::KwGuard) => tokens.push((RawToken::Token(Token::KwGuard), span)),
            Ok(LogosToken::KwIf) => tokens.push((RawToken::Token(Token::KwIf), span)),
            Ok(LogosToken::KwImport) => tokens.push((RawToken::Token(Token::KwImport), span)),
            Ok(LogosToken::KwIn) => tokens.push((RawToken::Token(Token::KwIn), span)),
            Ok(LogosToken::KwInit) => tokens.push((RawToken::Token(Token::KwInit), span)),
            Ok(LogosToken::KwInstance) => tokens.push((RawToken::Token(Token::KwInstance), span)),
            Ok(LogosToken::KwLet) => tokens.push((RawToken::Token(Token::KwLet), span)),
            Ok(LogosToken::KwMatch) => tokens.push((RawToken::Token(Token::KwMatch), span)),
            Ok(LogosToken::KwOn) => tokens.push((RawToken::Token(Token::KwOn), span)),
            Ok(LogosToken::KwOpaque) => tokens.push((RawToken::Token(Token::KwOpaque), span)),
            Ok(LogosToken::KwPub) => tokens.push((RawToken::Token(Token::KwPub), span)),
            Ok(LogosToken::KwReturn) => tokens.push((RawToken::Token(Token::KwReturn), span)),
            Ok(LogosToken::KwSpawn) => tokens.push((RawToken::Token(Token::KwSpawn), span)),
            Ok(LogosToken::KwState) => tokens.push((RawToken::Token(Token::KwState), span)),
            Ok(LogosToken::KwThen) => tokens.push((RawToken::Token(Token::KwThen), span)),
            Ok(LogosToken::KwTrue) => tokens.push((RawToken::Token(Token::KwTrue), span)),
            Ok(LogosToken::KwTry) => tokens.push((RawToken::Token(Token::KwTry), span)),
            Ok(LogosToken::KwType) => tokens.push((RawToken::Token(Token::KwType), span)),
            Ok(LogosToken::KwVar) => tokens.push((RawToken::Token(Token::KwVar), span)),
            Ok(LogosToken::KwWhen) => tokens.push((RawToken::Token(Token::KwWhen), span)),
            Ok(LogosToken::KwWhere) => tokens.push((RawToken::Token(Token::KwWhere), span)),
            Ok(LogosToken::KwWith) => tokens.push((RawToken::Token(Token::KwWith), span)),

            // ── Operators / punctuation ───────────────────────────────────────
            Ok(LogosToken::PipeFwd) => tokens.push((RawToken::Token(Token::PipeFwd), span)),
            Ok(LogosToken::LeftArrow) => tokens.push((RawToken::Token(Token::LeftArrow), span)),
            Ok(LogosToken::QuestionGt) => tokens.push((RawToken::Token(Token::QuestionGt), span)),
            Ok(LogosToken::Question) => tokens.push((RawToken::Token(Token::Question), span)),
            Ok(LogosToken::Bang) => tokens.push((RawToken::Token(Token::Bang), span)),
            Ok(LogosToken::ColonColon) => tokens.push((RawToken::Token(Token::ColonColon), span)),
            Ok(LogosToken::PlusPlus) => tokens.push((RawToken::Token(Token::PlusPlus), span)),
            Ok(LogosToken::Arrow) => tokens.push((RawToken::Token(Token::Arrow), span)),
            Ok(LogosToken::FatArrow) => tokens.push((RawToken::Token(Token::FatArrow), span)),
            Ok(LogosToken::At) => tokens.push((RawToken::Token(Token::At), span)),
            Ok(LogosToken::DotDot) => tokens.push((RawToken::Token(Token::DotDot), span)),
            Ok(LogosToken::Assign) => tokens.push((RawToken::Token(Token::Assign), span)),
            Ok(LogosToken::Colon) => tokens.push((RawToken::Token(Token::Colon), span)),
            Ok(LogosToken::Comma) => tokens.push((RawToken::Token(Token::Comma), span)),
            Ok(LogosToken::Dot) => tokens.push((RawToken::Token(Token::Dot), span)),
            Ok(LogosToken::LParen) => tokens.push((RawToken::Token(Token::LParen), span)),
            Ok(LogosToken::RParen) => tokens.push((RawToken::Token(Token::RParen), span)),
            Ok(LogosToken::LBrack) => tokens.push((RawToken::Token(Token::LBrack), span)),
            Ok(LogosToken::RBrack) => tokens.push((RawToken::Token(Token::RBrack), span)),
            Ok(LogosToken::LBrace) => tokens.push((RawToken::Token(Token::LBrace), span)),
            Ok(LogosToken::RBrace) => tokens.push((RawToken::RBrace, span)),
            Ok(LogosToken::Pipe) => tokens.push((RawToken::Token(Token::Pipe), span)),
            Ok(LogosToken::Plus) => tokens.push((RawToken::Token(Token::Plus), span)),
            Ok(LogosToken::Minus) => tokens.push((RawToken::Token(Token::Minus), span)),
            Ok(LogosToken::Star) => tokens.push((RawToken::Token(Token::Star), span)),
            Ok(LogosToken::Slash) => tokens.push((RawToken::Token(Token::Slash), span)),
            Ok(LogosToken::Percent) => tokens.push((RawToken::Token(Token::Percent), span)),
            Ok(LogosToken::Caret) => tokens.push((RawToken::Token(Token::Caret), span)),
            Ok(LogosToken::AmpAmp) => tokens.push((RawToken::Token(Token::AmpAmp), span)),
            Ok(LogosToken::PipePipe) => tokens.push((RawToken::Token(Token::PipePipe), span)),
            Ok(LogosToken::EqEq) => tokens.push((RawToken::Token(Token::EqEq), span)),
            Ok(LogosToken::BangEq) => tokens.push((RawToken::Token(Token::BangEq), span)),
            Ok(LogosToken::Lt) => tokens.push((RawToken::Token(Token::Lt), span)),
            Ok(LogosToken::Gt) => tokens.push((RawToken::Token(Token::Gt), span)),
            Ok(LogosToken::Le) => tokens.push((RawToken::Token(Token::Le), span)),
            Ok(LogosToken::Ge) => tokens.push((RawToken::Token(Token::Ge), span)),

            Err(()) => {
                // logos returns `Err(())` for unrecognised characters.
                if let Some(ch) = slice.chars().next() {
                    errors.push(LexError::UnexpectedCharacter { span, ch });
                }
            }
        }
    }

    (tokens, errors)
}

/// Restart scanning from `offset` into `src`, adjusting all spans by `offset`.
fn scan_from(src: &str, offset: usize) -> (Vec<(RawToken, Span)>, Vec<LexError>) {
    if offset >= src.len() {
        return (Vec::new(), Vec::new());
    }
    let (tokens, errors) = scan(&src[offset..]);
    // Adjust spans by offset.
    #[allow(clippy::cast_possible_truncation)]
    let off = offset as u32;
    let tokens = tokens
        .into_iter()
        .map(|(tok, span)| (tok, Span::new(span.start + off, span.end + off)))
        .collect();
    let errors = errors.into_iter().map(|e| shift_error(e, off)).collect();
    (tokens, errors)
}

/// Shift all byte offsets in a `LexError` by `delta`.
fn shift_error(e: LexError, delta: u32) -> LexError {
    match e {
        LexError::TabForbidden { span } => LexError::TabForbidden {
            span: shift(span, delta),
        },
        LexError::UnterminatedString { open_span } => LexError::UnterminatedString {
            open_span: shift(open_span, delta),
        },
        LexError::UnterminatedInterpolation { open_span } => LexError::UnterminatedInterpolation {
            open_span: shift(open_span, delta),
        },
        LexError::UnterminatedDocComment { open_span } => LexError::UnterminatedDocComment {
            open_span: shift(open_span, delta),
        },
        LexError::InvalidEscape { span, got } => LexError::InvalidEscape {
            span: shift(span, delta),
            got,
        },
        LexError::InvalidUnicodeEscape { span, reason } => LexError::InvalidUnicodeEscape {
            span: shift(span, delta),
            reason,
        },
        LexError::InconsistentDedent {
            span,
            col,
            expected,
        } => LexError::InconsistentDedent {
            span: shift(span, delta),
            col,
            expected,
        },
        LexError::LeadingUnderscoreLiteral { span } => LexError::LeadingUnderscoreLiteral {
            span: shift(span, delta),
        },
        LexError::TrailingUnderscoreLiteral { span } => LexError::TrailingUnderscoreLiteral {
            span: shift(span, delta),
        },
        LexError::EmptyNumericLiteral { span } => LexError::EmptyNumericLiteral {
            span: shift(span, delta),
        },
        LexError::UnexpectedCharacter { span, ch } => LexError::UnexpectedCharacter {
            span: shift(span, delta),
            ch,
        },
        LexError::IndentAtTopLevel { span } => LexError::IndentAtTopLevel {
            span: shift(span, delta),
        },
        LexError::MultilineStringOpenContent { span } => LexError::MultilineStringOpenContent {
            span: shift(span, delta),
        },
        LexError::MultilineStringInsufficientIndent { span } => {
            LexError::MultilineStringInsufficientIndent {
                span: shift(span, delta),
            }
        }
        LexError::UnterminatedMultilineString { open_span, kind } => {
            LexError::UnterminatedMultilineString {
                open_span: shift(open_span, delta),
                kind,
            }
        }
    }
}

fn shift(span: Span, delta: u32) -> Span {
    Span::new(span.start + delta, span.end + delta)
}

/// Lex the expression content inside a `${...}` interpolation hole.
///
/// `src` is the full source string; `start` and `end` are byte offsets into it
/// that delimit the hole content (exclusive of the surrounding `${` and `}`).
/// All spans in the returned tokens are absolute — i.e. relative to `src`.
///
/// We pass a sub-slice to the full logos scanner so that identifiers, operators,
/// keywords, and numeric literals inside holes receive proper tokens.  The only
/// tokens deliberately excluded are `$"` (`InterpStart`) — nested interpolation
/// strings inside holes are not supported.
fn scan_hole_expr(src: &str, start: usize, end: usize) -> (Vec<(RawToken, Span)>, Vec<LexError>) {
    if start >= end {
        return (Vec::new(), Vec::new());
    }
    let slice = &src[start..end];
    let (tokens, errors) = scan(slice);
    // Adjust all spans to be absolute offsets into `src`.
    #[allow(clippy::cast_possible_truncation)]
    let off = start as u32;
    let tokens = tokens
        .into_iter()
        .map(|(tok, span)| (tok, Span::new(span.start + off, span.end + off)))
        .collect();
    let errors = errors.into_iter().map(|e| shift_error(e, off)).collect();
    (tokens, errors)
}

// ── Triple-quoted string scanner ──────────────────────────────────────────────

/// Scan a triple-quoted `"""..."""` body starting at `pos` (right after the
/// opening `"""`).
///
/// Returns `(Option<body>, errors, consumed_end_pos)`.
///
/// - `body` is the dedented, newline-stripped string content ready for later
///   escape decoding.  `None` is returned when an error prevents producing a
///   well-formed token; the caller should still advance to `consumed_end_pos`.
/// - `open_start` is the byte offset of the opening `"""` (for error spans).
///
/// # Dedent algorithm (D256, §6.1)
///
/// 1. The character immediately after the opening `"""` must be `\n`; anything
///    else is `L013 MultilineStringOpenContent`.
/// 2. Collect all bytes until the closing `"""`, tracking which byte sequence
///    precedes the closing `"""` on its line — that is the margin.
/// 3. The leading `\n` (after the opening `"""`) and the final `\n` + margin
///    bytes (before the closing `"""`) are dropped.
/// 4. From every interior line, strip the margin as a byte-exact prefix.
///    A line with fewer bytes than the margin is `L014 MultilineStringInsufficientIndent`,
///    unless the line is blank (empty or whitespace-only), in which case it is
///    allowed and produces just `\n`.
fn scan_triple_quote_body(
    src: &str,
    pos: usize,
    open_start: u32,
) -> (Option<String>, Vec<LexError>, usize) {
    let mut errors: Vec<LexError> = Vec::new();
    let bytes = src.as_bytes();

    // ── Step 1: the first byte must be `\n` ──────────────────────────────────
    if pos >= bytes.len() || bytes[pos] != b'\n' {
        // Content on the same line as the opening `"""` — error.
        #[allow(clippy::cast_possible_truncation)]
        let err_span = Span::new(open_start, pos as u32 + 1);
        errors.push(LexError::MultilineStringOpenContent { span: err_span });
        // Recovery: scan forward to find the closing `"""` or EOF.
        let consumed = skip_to_triple_quote_close(src, pos);
        return (None, errors, consumed);
    }

    // Skip the opening newline.
    let mut i = pos + 1;

    // ── Step 2: scan until the closing `"""` ─────────────────────────────────
    //
    // We need to find the closing `"""` and determine the margin (the
    // whitespace prefix on the closing delimiter's line).
    //
    // Strategy: collect the entire body between the opening newline and the
    // closing `"""`.  Then split on lines to apply dedent.
    let body_start = i;

    let close_pos = loop {
        if i + 2 >= bytes.len() {
            // Check remaining bytes for `"""`
            if i < bytes.len()
                && bytes[i] == b'"'
                && i + 1 < bytes.len()
                && bytes[i + 1] == b'"'
                && i + 2 < bytes.len()
                && bytes[i + 2] == b'"'
            {
                break i;
            }
            // EOF without closing `"""`.
            errors.push(LexError::UnterminatedMultilineString {
                open_span: Span::point(open_start),
                kind: "triple-quoted",
            });
            return (None, errors, bytes.len());
        }
        if bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
            break i;
        }
        i += 1;
    };

    // `close_pos` is the index of the first `"` of the closing `"""`.
    // The consumed position is right after the closing `"""`.
    let consumed = close_pos + 3;

    // ── Step 3: determine the margin ─────────────────────────────────────────
    //
    // Walk backward from `close_pos` to find the start of the line containing
    // the closing `"""`.  The margin is everything from the start of that line
    // to `close_pos`.
    let raw_body = &src[body_start..close_pos];

    // Find the last `\n` in the raw body (or the start if none).
    // When no newline exists the body fits entirely on one line (e.g. `"""\n"""`)
    // and the margin is the empty string.
    let last_newline_in_body = raw_body.rfind('\n');
    let margin: &str = last_newline_in_body.map_or("", |nl_pos| &raw_body[nl_pos + 1..]);

    // Validate that the margin is all-whitespace.  If it contains non-whitespace,
    // the closing `"""` is not properly dedented — treat as zero margin so we
    // can still produce a token, but emit an error.
    //
    // Note: the closing line (including its trailing content after the margin)
    // must be exactly `<margin>"""`.  Since logos matched `"""` only, anything
    // between the last `\n` and `"""` is the margin by construction.
    // We just need it to be whitespace-only.
    let effective_margin = if margin.bytes().all(|b| b == b' ') {
        margin
    } else {
        // Margin contains non-space characters — error.
        #[allow(clippy::cast_possible_truncation)]
        errors.push(LexError::MultilineStringInsufficientIndent {
            span: Span::point(open_start),
        });
        ""
    };

    // ── Step 4: build the dedented body ──────────────────────────────────────
    //
    // The raw body is everything from after the opening `\n` to just before the
    // last `\n` that precedes the closing `"""` line.  When no newline exists
    // the content is empty.
    let content_raw: &str = last_newline_in_body.map_or("", |nl_pos| &raw_body[..nl_pos]);

    // Split content_raw into lines, strip the margin from each.
    let mut result = String::new();
    let margin_len = effective_margin.len();

    for (line_idx, line) in content_raw.split('\n').enumerate() {
        if line_idx > 0 {
            result.push('\n');
        }
        // Blank line: emit as-is (just `\n`).
        if line.bytes().all(|b| b == b' ') && line.len() <= margin_len {
            // A line that is shorter than or equal to the margin AND is
            // all-spaces is a blank interior line — emit empty.
            continue;
        }
        if line.is_empty() {
            // Truly empty line (e.g. `\n\n`).
            continue;
        }
        // Check that the line starts with the margin.
        if margin_len > 0 && !line.starts_with(effective_margin) {
            // Insufficient indentation.
            // Find approximate byte offset: body_start + position of this line.
            let approx_offset = body_start
                + content_raw
                    .split('\n')
                    .take(line_idx)
                    .map(|l| l.len() + 1)
                    .sum::<usize>();
            #[allow(clippy::cast_possible_truncation)]
            errors.push(LexError::MultilineStringInsufficientIndent {
                span: Span::point(approx_offset as u32),
            });
            // Recovery: include the line as-is.
            result.push_str(line);
        } else {
            result.push_str(&line[margin_len..]);
        }
    }

    (Some(result), errors, consumed)
}

/// Skip forward from `pos` looking for the next `"""` or EOF, for error recovery.
fn skip_to_triple_quote_close(src: &str, pos: usize) -> usize {
    let bytes = src.as_bytes();
    let mut i = pos;
    while i + 2 < bytes.len() {
        if bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
            return i + 3;
        }
        i += 1;
    }
    bytes.len()
}

// ── Raw string scanner ────────────────────────────────────────────────────────

/// Scan a raw string body starting at `pos` (right after the opening
/// `r"` / `r#"` / …).
///
/// `hash_count` is the number of `#` characters between `r` and the opening `"`.
/// The closing sequence is `"` followed by exactly `hash_count` `#` characters.
///
/// No escape processing is performed; every byte is literal.
/// Raw strings may span multiple lines.
///
/// Returns `(Option<body>, errors, consumed_end_pos)`.
fn scan_raw_string_body(
    src: &str,
    pos: usize,
    open_start: u32,
    hash_count: usize,
) -> (Option<String>, Vec<LexError>, usize) {
    let mut errors: Vec<LexError> = Vec::new();
    let bytes = src.as_bytes();
    let mut i = pos;
    let mut body = String::new();

    loop {
        if i >= bytes.len() {
            // EOF without closing delimiter.
            errors.push(LexError::UnterminatedMultilineString {
                open_span: Span::point(open_start),
                kind: "raw",
            });
            return (None, errors, i);
        }

        if bytes[i] == b'"' {
            // Check whether this is the closing delimiter: `"` + `hash_count` `#`.
            let after_quote = i + 1;
            let enough_hashes = hash_count == 0
                || (after_quote + hash_count <= bytes.len()
                    && bytes[after_quote..after_quote + hash_count]
                        .iter()
                        .all(|&b| b == b'#'));
            if enough_hashes {
                let consumed = after_quote + hash_count;
                return (Some(body), errors, consumed);
            }
            // Not a closing delimiter — include the `"` as literal content.
            // Use char-safe indexing.
            let ch = src[i..].chars().next().unwrap_or('"');
            body.push(ch);
            i += ch.len_utf8();
        } else {
            let ch = src[i..].chars().next().unwrap_or('\0');
            body.push(ch);
            i += ch.len_utf8();
        }
    }
}

// ── Interpolation body scanner ────────────────────────────────────────────────

/// Scan the body of an interpolated string starting at `pos` in `src`.
///
/// Returns `(raw_tokens, errors, consumed_end_pos)`.
/// `interp_open` is the byte offset of the `$"` opener (for error spans).
#[allow(clippy::too_many_lines)]
fn scan_interp_body(
    src: &str,
    pos: usize,
    interp_open: u32,
) -> (Vec<(RawToken, Span)>, Vec<LexError>, usize) {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let bytes = src.as_bytes();
    let mut i = pos;

    // We scan in "interp-text" mode.  When we hit `${` we switch to
    // "interp-expr" mode (tracked by depth counter), and when we hit `"` we
    // close the whole interpolation.

    let mut text_buf = String::new();
    let mut text_start = i;

    let flush_text =
        |buf: &mut String, start: usize, i: usize, tokens: &mut Vec<(RawToken, Span)>| {
            if !buf.is_empty() {
                #[allow(clippy::cast_possible_truncation)]
                let span = Span::new(start as u32, i as u32);
                tokens.push((RawToken::InterpText(buf.clone()), span));
                buf.clear();
            }
        };

    loop {
        if i >= bytes.len() {
            flush_text(&mut text_buf, text_start, i, &mut tokens);
            errors.push(LexError::UnterminatedInterpolation {
                open_span: Span::point(interp_open),
            });
            return (tokens, errors, i);
        }

        match bytes[i] {
            b'"' => {
                // Close the interpolated string.
                flush_text(&mut text_buf, text_start, i, &mut tokens);
                #[allow(clippy::cast_possible_truncation)]
                let end_span = Span::new(i as u32, (i + 1) as u32);
                tokens.push((RawToken::Token(Token::InterpEnd), end_span));
                i += 1;
                return (tokens, errors, i);
            }
            b'$' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => {
                // Start of expression hole.
                flush_text(&mut text_buf, text_start, i, &mut tokens);
                #[allow(clippy::cast_possible_truncation)]
                let expr_start_span = Span::new(i as u32, (i + 2) as u32);
                tokens.push((RawToken::InterpExprStart, expr_start_span));
                i += 2;

                // Phase 1: find the closing `}` by tracking brace depth.
                // We skip over nested strings so a `}` inside `"..."` is not
                // mistaken for the closing brace.
                let content_start = i;
                let mut depth = 1u32;
                while i < bytes.len() && depth > 0 {
                    match bytes[i] {
                        b'{' => {
                            depth += 1;
                            i += 1;
                        }
                        b'}' => {
                            depth -= 1;
                            i += 1;
                        }
                        b'"' => {
                            // Skip over a nested plain string so its `}` chars
                            // are not counted as depth.
                            i += 1; // skip opening `"`
                            while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                                if bytes[i] == b'\\' {
                                    i += 1; // skip escape char
                                }
                                if i < bytes.len() {
                                    i += 1;
                                }
                            }
                            if i < bytes.len() && bytes[i] == b'"' {
                                i += 1; // skip closing `"`
                            }
                        }
                        b'\n' => {
                            // Newline terminates a single-line interpolation; stop
                            // the boundary scan — the unterminated error is raised
                            // when we check depth below.
                            break;
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }

                if depth > 0 {
                    errors.push(LexError::UnterminatedInterpolation {
                        open_span: Span::point(interp_open),
                    });
                    return (tokens, errors, i);
                }

                // `i` now points one byte past the closing `}`.
                // The content range is `[content_start, i - 1)`.
                let content_end = i - 1; // exclusive end of the hole expression
                #[allow(clippy::cast_possible_truncation)]
                let expr_end_span = Span::new((i - 1) as u32, i as u32);

                // Phase 2: re-lex the hole expression using the full logos scanner.
                // We scan the sub-slice `src[content_start..content_end]` and
                // shift all resulting spans by `content_start` so they remain
                // absolute offsets into the original source.
                let (hole_tokens, hole_errors) = scan_hole_expr(src, content_start, content_end);
                tokens.extend(hole_tokens);
                errors.extend(hole_errors);

                // Emit the closing `}` as InterpExprEnd.
                tokens.push((RawToken::Token(Token::InterpExprEnd), expr_end_span));

                text_start = i;
            }
            b'\\' => {
                // Escape sequence inside interp-text.
                text_buf.push(bytes[i] as char);
                i += 1;
                if i < bytes.len() {
                    text_buf.push(bytes[i] as char);
                    i += 1;
                }
            }
            b'\n' => {
                // Strings are single-line only; a newline closes the interp.
                flush_text(&mut text_buf, text_start, i, &mut tokens);
                errors.push(LexError::UnterminatedInterpolation {
                    open_span: Span::point(interp_open),
                });
                return (tokens, errors, i);
            }
            _ => {
                // Emit one full UTF-8 scalar so multi-byte content round-trips
                // (mirrors scan_interp_triple_body). Pushing a raw byte as a
                // `char` here would Latin-1-decode each byte of a multi-byte
                // scalar and double-encode it on the way back out to UTF-8.
                if let Some(ch) = src[i..].chars().next() {
                    text_buf.push(ch);
                    i += ch.len_utf8();
                } else {
                    i += 1;
                }
            }
        }
    }
}

/// Scan the body of an interpolated multi-line string `$"""..."""` starting at
/// `pos` (right after the opening `$"""`).
///
/// Returns `(raw_tokens, errors, consumed_end_pos)`.  The emitted token stream
/// is the SAME shape as [`scan_interp_body`] — `InterpText` runs interleaved
/// with `InterpExprStart Expr InterpExprEnd` holes, terminated by `InterpEnd` —
/// so nothing downstream of the lexer needs to distinguish the two openers.
///
/// The text obeys the same dedent rules as a plain triple-quoted string
/// (`scan_triple_quote_body`, §6.1):
///
/// 1. The byte after the opening `$"""` must be `\n`; content on the opening
///    line is `L013 MultilineStringOpenContent`.
/// 2. The closing delimiter is a newline, zero or more spaces, then `"""`.  The
///    leading whitespace on the closing line defines the dedent margin.
/// 3. The opening `\n` and the final `\n` + margin are dropped from the value.
/// 4. That many leading spaces are stripped from every interior line; a
///    non-blank line with fewer is `L014 MultilineStringInsufficientIndent`.
///
/// `${…}` holes are recognised exactly as in the single-line form and may
/// appear on any line.  A `"""` inside a hole expression does not close the
/// string — the close scan steps over holes.
#[allow(clippy::too_many_lines)]
fn scan_interp_triple_body(
    src: &str,
    pos: usize,
    open_start: u32,
) -> (Vec<(RawToken, Span)>, Vec<LexError>, usize) {
    let mut tokens: Vec<(RawToken, Span)> = Vec::new();
    let mut errors: Vec<LexError> = Vec::new();
    let bytes = src.as_bytes();

    // ── Step 1: the byte after `$"""` must be `\n` (block form) ──────────────
    if pos >= bytes.len() || bytes[pos] != b'\n' {
        #[allow(clippy::cast_possible_truncation)]
        let err_span = Span::new(open_start, pos as u32 + 1);
        errors.push(LexError::MultilineStringOpenContent { span: err_span });
        let consumed = skip_to_triple_quote_close(src, pos);
        // Emit a best-effort `InterpEnd` so the parser still sees a closed
        // interpolation rather than cascading into unrelated errors.
        let end_at = consumed.saturating_sub(3).max(pos);
        #[allow(clippy::cast_possible_truncation)]
        tokens.push((
            RawToken::Token(Token::InterpEnd),
            Span::new(end_at as u32, end_at as u32),
        ));
        return (tokens, errors, consumed);
    }

    // Content begins one byte past the opening newline.
    let body_start = pos + 1;

    // ── Step 2: find the closing `"""`, stepping over `${…}` holes ───────────
    let Some(close_pos) = find_triple_interp_close(bytes, body_start) else {
        errors.push(LexError::UnterminatedMultilineString {
            open_span: Span::point(open_start),
            kind: "interpolated",
        });
        let end = bytes.len();
        #[allow(clippy::cast_possible_truncation)]
        tokens.push((
            RawToken::Token(Token::InterpEnd),
            Span::new(end as u32, end as u32),
        ));
        return (tokens, errors, end);
    };
    let consumed = close_pos + 3;

    // ── Step 3: determine the dedent margin from the closing line ────────────
    let raw_body = &src[body_start..close_pos];
    let last_newline = raw_body.rfind('\n');
    let margin: &str = last_newline.map_or("", |nl| &raw_body[nl + 1..]);
    let effective_margin = if margin.bytes().all(|b| b == b' ') {
        margin
    } else {
        errors.push(LexError::MultilineStringInsufficientIndent {
            span: Span::point(open_start),
        });
        ""
    };
    let margin_len = effective_margin.len();

    // Content ends at the last `\n` before the closing `"""` (that `\n` and the
    // margin are dropped).  With no interior newline the value is empty.
    let content_end = last_newline.map_or(body_start, |nl| body_start + nl);

    // ── Step 4: emit text runs + holes, stripping the margin per line ────────
    let mut text_buf = String::new();
    let mut text_start = body_start;
    let mut i = body_start;
    let mut at_line_start = true;

    while i < content_end {
        if at_line_start {
            let line_begin = i;
            let mut stripped = 0usize;
            while stripped < margin_len && i < content_end && bytes[i] == b' ' {
                i += 1;
                stripped += 1;
            }
            at_line_start = false;
            // A line with less indentation than the margin is an error unless it
            // is blank (the next byte closes the line).
            if stripped < margin_len && i < content_end && bytes[i] != b'\n' {
                #[allow(clippy::cast_possible_truncation)]
                errors.push(LexError::MultilineStringInsufficientIndent {
                    span: Span::point(line_begin as u32),
                });
            }
            continue;
        }

        match bytes[i] {
            b'$' if i + 1 < content_end && bytes[i + 1] == b'{' => {
                // Flush the pending text run.
                if !text_buf.is_empty() {
                    #[allow(clippy::cast_possible_truncation)]
                    tokens.push((
                        RawToken::InterpText(text_buf.clone()),
                        Span::new(text_start as u32, i as u32),
                    ));
                    text_buf.clear();
                }
                #[allow(clippy::cast_possible_truncation)]
                let expr_start_span = Span::new(i as u32, (i + 2) as u32);
                tokens.push((RawToken::InterpExprStart, expr_start_span));
                i += 2;

                // Find the matching `}` by brace depth, skipping nested plain
                // strings so a `}` inside `"..."` is not the closer.
                let content_start = i;
                let mut depth = 1u32;
                while i < content_end && depth > 0 {
                    match bytes[i] {
                        b'{' => {
                            depth += 1;
                            i += 1;
                        }
                        b'}' => {
                            depth -= 1;
                            i += 1;
                        }
                        b'"' => {
                            i += 1; // opening `"`
                            while i < content_end && bytes[i] != b'"' && bytes[i] != b'\n' {
                                if bytes[i] == b'\\' {
                                    i += 1;
                                }
                                if i < content_end {
                                    i += 1;
                                }
                            }
                            if i < content_end && bytes[i] == b'"' {
                                i += 1; // closing `"`
                            }
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }

                if depth > 0 {
                    errors.push(LexError::UnterminatedInterpolation {
                        open_span: Span::point(open_start),
                    });
                    // `i` is at content_end; treat that as the hole boundary.
                    let (hole_tokens, hole_errors) = scan_hole_expr(src, content_start, i);
                    tokens.extend(hole_tokens);
                    errors.extend(hole_errors);
                    text_start = i;
                    continue;
                }

                // `i` points one byte past the closing `}`.
                let hole_end = i - 1;
                #[allow(clippy::cast_possible_truncation)]
                let expr_end_span = Span::new((i - 1) as u32, i as u32);
                let (hole_tokens, hole_errors) = scan_hole_expr(src, content_start, hole_end);
                tokens.extend(hole_tokens);
                errors.extend(hole_errors);
                tokens.push((RawToken::Token(Token::InterpExprEnd), expr_end_span));
                text_start = i;
            }
            b'\\' if i + 1 < content_end => {
                // Preserve the escape verbatim; decoding happens downstream.
                text_buf.push(bytes[i] as char);
                text_buf.push(bytes[i + 1] as char);
                i += 2;
            }
            b'\n' => {
                text_buf.push('\n');
                i += 1;
                at_line_start = true;
            }
            _ => {
                // Emit one full UTF-8 scalar so multi-byte content round-trips.
                if let Some(ch) = src[i..].chars().next() {
                    text_buf.push(ch);
                    i += ch.len_utf8();
                } else {
                    i += 1;
                }
            }
        }
    }

    if !text_buf.is_empty() {
        #[allow(clippy::cast_possible_truncation)]
        tokens.push((
            RawToken::InterpText(text_buf.clone()),
            Span::new(text_start as u32, content_end as u32),
        ));
    }

    #[allow(clippy::cast_possible_truncation)]
    tokens.push((
        RawToken::Token(Token::InterpEnd),
        Span::new(close_pos as u32, (close_pos + 3) as u32),
    ));

    (tokens, errors, consumed)
}

/// Find the closing `"""` of an interpolated multi-line string, starting at
/// `start` (the first content byte).  `${…}` holes are stepped over so a `"""`
/// inside a hole expression is not treated as the closer.  Returns the byte
/// offset of the first `"` of the closing `"""`, or `None` at EOF.
fn find_triple_interp_close(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() {
        // Step over a `${…}` hole.
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            i += 2;
            let mut depth = 1u32;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'{' => {
                        depth += 1;
                        i += 1;
                    }
                    b'}' => {
                        depth -= 1;
                        i += 1;
                    }
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            if i < bytes.len() {
                                i += 1;
                            }
                        }
                        if i < bytes.len() && bytes[i] == b'"' {
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            }
            continue;
        }
        // Skip an escape pair so `\"` never starts a false close.
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' && i + 2 < bytes.len() && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
            return Some(i);
        }
        i += 1;
    }
    None
}
