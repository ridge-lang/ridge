//! Snapshot tests for the four example Ridge programs (`DoD §12`).
#![allow(clippy::missing_const_for_fn)] // token_kind_name cannot be const due to match on &Token
//!
//! Each test asserts `errors.is_empty()` then locks the token stream in an
//! `insta` snapshot.  Run `cargo insta review` to accept new/changed snapshots.

use std::fmt::Write as _;

use ridge_lexer::{tokenize, Token};

const LOG_ANALYZER: &str = include_str!("../../../examples/log_analyzer.rg");
const URL_SHORTENER: &str = include_str!("../../../examples/url_shortener.rg");
const GAME_OF_LIFE: &str = include_str!("../../../examples/game_of_life.rg");
const RATE_LIMITER: &str = include_str!("../../../examples/rate_limiter.rg");

/// Render the token stream as a compact, human-readable string for snapshots.
///
/// Format: one token per line — `SPAN TOKEN_KIND payload`.
fn render(src: &str) -> String {
    let out = tokenize(src);
    assert!(
        out.errors.is_empty(),
        "lexer produced errors on example file:\n{:#?}",
        out.errors
    );
    let mut buf = String::new();
    for (tok, span) in &out.tokens {
        let kind = token_kind_name(tok);
        let payload = token_payload(tok);
        if payload.is_empty() {
            let _ = writeln!(buf, "{span} {kind}");
        } else {
            let _ = writeln!(buf, "{span} {kind} {payload}");
        }
    }
    buf
}

fn token_kind_name(t: &Token) -> &'static str {
    match t {
        Token::KwActor => "KW_ACTOR",
        Token::KwAs => "KW_AS",
        Token::KwCatch => "KW_CATCH",
        Token::KwClass => "KW_CLASS",
        Token::KwConst => "KW_CONST",
        Token::KwDeriving => "KW_DERIVING",
        Token::KwElse => "KW_ELSE",
        Token::KwFalse => "KW_FALSE",
        Token::KwFn => "KW_FN",
        Token::KwGuard => "KW_GUARD",
        Token::KwIf => "KW_IF",
        Token::KwImport => "KW_IMPORT",
        Token::KwIn => "KW_IN",
        Token::KwInit => "KW_INIT",
        Token::KwInstance => "KW_INSTANCE",
        Token::KwLet => "KW_LET",
        Token::KwMatch => "KW_MATCH",
        Token::KwOn => "KW_ON",
        Token::KwPub => "KW_PUB",
        Token::KwReturn => "KW_RETURN",
        Token::KwSpawn => "KW_SPAWN",
        Token::KwState => "KW_STATE",
        Token::KwThen => "KW_THEN",
        Token::KwTrue => "KW_TRUE",
        Token::KwTry => "KW_TRY",
        Token::KwType => "KW_TYPE",
        Token::KwVar => "KW_VAR",
        Token::KwWhen => "KW_WHEN",
        Token::KwWhere => "KW_WHERE",
        Token::KwWith => "KW_WITH",
        Token::LowerIdent(_) => "LOWER_IDENT",
        Token::UpperIdent(_) => "UPPER_IDENT",
        Token::Underscore => "UNDERSCORE",
        Token::IntDec(_) => "INT_DEC",
        Token::IntBin(_) => "INT_BIN",
        Token::IntOct(_) => "INT_OCT",
        Token::IntHex(_) => "INT_HEX",
        Token::Float(_) => "FLOAT",
        Token::TextLit(_) => "TEXT_LIT",
        Token::InterpStart => "INTERP_START",
        Token::InterpText(_) => "INTERP_TEXT",
        Token::InterpExprStart => "INTERP_EXPR_START",
        Token::InterpExprEnd => "INTERP_EXPR_END",
        Token::InterpEnd => "INTERP_END",
        Token::PipeFwd => "PIPE_FWD",
        Token::LeftArrow => "LEFT_ARROW",
        Token::QuestionGt => "QUESTION_GT",
        Token::Question => "QUESTION",
        Token::Bang => "BANG",
        Token::ColonColon => "COLON_COLON",
        Token::PlusPlus => "PLUS_PLUS",
        Token::Arrow => "ARROW",
        Token::FatArrow => "FAT_ARROW",
        Token::At => "AT",
        Token::DotDot => "DOT_DOT",
        Token::Assign => "ASSIGN",
        Token::Colon => "COLON",
        Token::Comma => "COMMA",
        Token::Dot => "DOT",
        Token::LParen => "LPAREN",
        Token::RParen => "RPAREN",
        Token::LBrack => "LBRACK",
        Token::RBrack => "RBRACK",
        Token::LBrace => "LBRACE",
        Token::RBrace => "RBRACE",
        Token::Pipe => "PIPE",
        Token::Plus => "PLUS",
        Token::Minus => "MINUS",
        Token::Star => "STAR",
        Token::Slash => "SLASH",
        Token::Percent => "PERCENT",
        Token::Caret => "CARET",
        Token::AmpAmp => "AMP_AMP",
        Token::PipePipe => "PIPE_PIPE",
        Token::EqEq => "EQ_EQ",
        Token::BangEq => "BANG_EQ",
        Token::Lt => "LT",
        Token::Gt => "GT",
        Token::Le => "LE",
        Token::Ge => "GE",
        Token::DocComment(_) => "DOC_COMMENT",
        Token::Newline => "NEWLINE",
        Token::Indent => "INDENT",
        Token::Dedent => "DEDENT",
        Token::Eof => "EOF",
    }
}

fn token_payload(t: &Token) -> String {
    match t {
        Token::LowerIdent(s)
        | Token::UpperIdent(s)
        | Token::IntDec(s)
        | Token::IntBin(s)
        | Token::IntOct(s)
        | Token::IntHex(s)
        | Token::Float(s)
        | Token::TextLit(s)
        | Token::InterpText(s)
        | Token::DocComment(s) => format!("{s:?}"),
        _ => String::new(),
    }
}

#[test]
fn tokenize_log_analyzer() {
    insta::assert_snapshot!(render(LOG_ANALYZER));
}

#[test]
fn tokenize_url_shortener() {
    insta::assert_snapshot!(render(URL_SHORTENER));
}

#[test]
fn tokenize_game_of_life() {
    insta::assert_snapshot!(render(GAME_OF_LIFE));
}

#[test]
fn tokenize_rate_limiter() {
    insta::assert_snapshot!(render(RATE_LIMITER));
}
