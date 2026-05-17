//! The public `Token` enum and the internal `RawToken` enum used by `logos`.

/// Every token that the Ridge lexer can emit.
///
/// Literal values are stored as their raw source text (e.g. `"1_000"` for an
/// integer literal).  Decoding to a concrete value happens in the parser /
/// type-checker phase.
///
/// Layout tokens (`Newline`, `Indent`, `Dedent`, `Eof`) are synthesised by
/// the layout post-processor and carry zero-width spans.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Token {
    // ── Keywords (grammar §1.2, 30 total) ────────────────────────────────────
    KwActor,
    KwAs,
    KwCatch,
    KwClass,
    KwConst,
    KwDeriving,
    KwElse,
    KwFalse,
    KwFn,
    KwGuard,
    KwIf,
    KwImport,
    KwIn,
    KwInit,
    KwInstance,
    KwLet,
    KwMatch,
    KwOn,
    KwPub,
    KwReturn,
    KwSpawn,
    KwState,
    KwThen,
    KwTrue,
    KwTry,
    KwType,
    KwVar,
    KwWhen,
    KwWhere,
    KwWith,

    // ── Identifiers (grammar §1.4) ───────────────────────────
    /// Lower-case identifier.  Also covers `PRIV_IDENT` (leading `_`);
    /// the parser detects the leading underscore from the text.
    LowerIdent(String),
    /// Upper-case identifier: type names, constructors, module roots.
    UpperIdent(String),
    /// The bare `_` wildcard token.  `_foo` and `_Bar` are `LowerIdent` /
    /// `UpperIdent`, not `Underscore`.
    Underscore,

    // ── Numeric literals (grammar §1.5) ────────────────────────────────
    /// Decimal integer literal (raw source text, e.g. `"1_000"`).
    IntDec(String),
    /// Binary integer literal (raw source text, e.g. `"0b1010"`).
    IntBin(String),
    /// Octal integer literal (raw source text, e.g. `"0o777"`).
    IntOct(String),
    /// Hexadecimal integer literal (raw source text, e.g. `"0x1_DEAD"`).
    IntHex(String),
    /// Floating-point literal (raw source text, e.g. `"3.14"`).
    Float(String),

    // ── String literals ──────────────────────────────────────────────────────
    /// A plain `"..."` string literal.  Raw bytes between the outer quotes,
    /// escapes left un-decoded.
    TextLit(String),

    // ── String interpolation tokens (grammar lines 228–241) ────────────
    /// `$"` — opens an interpolated string.
    InterpStart,
    /// Literal text segment inside an interpolated string.
    InterpText(String),
    /// `${` — opens an expression hole inside an interpolated string.
    InterpExprStart,
    /// `}` that closes a `${...}` expression hole.
    InterpExprEnd,
    /// `"` that closes an interpolated string.
    InterpEnd,

    // ── Operators and punctuation (grammar §1.7) ─────────────────────────────
    /// `|>`
    PipeFwd,
    /// `<-`
    LeftArrow,
    /// `?>`
    QuestionGt,
    /// `?`
    Question,
    /// `!`
    Bang,
    /// `::`
    ColonColon,
    /// `++`
    PlusPlus,
    /// `->`
    Arrow,
    /// `=>` (reserved, unused in 0.1.0)
    FatArrow,
    /// `@`
    At,
    /// `..` (reserved)
    DotDot,
    /// `=`
    Assign,
    /// `:`
    Colon,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBrack,
    /// `]`
    RBrack,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `|`
    Pipe,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `^`
    Caret,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `==`
    EqEq,
    /// `!=`
    BangEq,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `>=`
    Ge,

    // ── Trivia we preserve in the stream ─────────────────────────────────────
    /// A block doc-comment `---\n...\n---`.  Content is the raw text between
    /// the opening and closing `---` lines, including newlines.
    /// This is a real token (not stripped); the parser decides attachment.
    DocComment(String),

    // ── Synthesised layout tokens ─────────────────────────────────────────────
    /// Emitted when a new logical line begins at the same indentation level.
    Newline,
    /// Emitted when a new logical line is indented deeper than the current block.
    Indent,
    /// Emitted (possibly multiple times) when a logical line is dedented.
    Dedent,
    /// End of file; always the last token.  Preceded by any outstanding `Dedent`s.
    Eof,
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KwActor => write!(f, "actor"),
            Self::KwAs => write!(f, "as"),
            Self::KwCatch => write!(f, "catch"),
            Self::KwClass => write!(f, "class"),
            Self::KwConst => write!(f, "const"),
            Self::KwDeriving => write!(f, "deriving"),
            Self::KwElse => write!(f, "else"),
            Self::KwFalse => write!(f, "false"),
            Self::KwFn => write!(f, "fn"),
            Self::KwGuard => write!(f, "guard"),
            Self::KwIf => write!(f, "if"),
            Self::KwImport => write!(f, "import"),
            Self::KwIn => write!(f, "in"),
            Self::KwInit => write!(f, "init"),
            Self::KwInstance => write!(f, "instance"),
            Self::KwLet => write!(f, "let"),
            Self::KwMatch => write!(f, "match"),
            Self::KwOn => write!(f, "on"),
            Self::KwPub => write!(f, "pub"),
            Self::KwReturn => write!(f, "return"),
            Self::KwSpawn => write!(f, "spawn"),
            Self::KwState => write!(f, "state"),
            Self::KwThen => write!(f, "then"),
            Self::KwTrue => write!(f, "true"),
            Self::KwTry => write!(f, "try"),
            Self::KwType => write!(f, "type"),
            Self::KwVar => write!(f, "var"),
            Self::KwWhen => write!(f, "when"),
            Self::KwWhere => write!(f, "where"),
            Self::KwWith => write!(f, "with"),

            Self::LowerIdent(s)
            | Self::UpperIdent(s)
            | Self::IntDec(s)
            | Self::IntBin(s)
            | Self::IntOct(s)
            | Self::IntHex(s)
            | Self::Float(s)
            | Self::InterpText(s) => write!(f, "{s}"),

            Self::Underscore => write!(f, "_"),

            Self::TextLit(s) => write!(f, "\"{s}\""),

            Self::InterpStart => write!(f, "$\""),
            Self::InterpExprStart => write!(f, "${{"),
            Self::InterpEnd => write!(f, "\""),
            // Both InterpExprEnd and RBrace render as `}`.
            Self::InterpExprEnd | Self::RBrace => write!(f, "}}"),

            Self::PipeFwd => write!(f, "|>"),
            Self::LeftArrow => write!(f, "<-"),
            Self::QuestionGt => write!(f, "?>"),
            Self::Question => write!(f, "?"),
            Self::Bang => write!(f, "!"),
            Self::ColonColon => write!(f, "::"),
            Self::PlusPlus => write!(f, "++"),
            Self::Arrow => write!(f, "->"),
            Self::FatArrow => write!(f, "=>"),
            Self::At => write!(f, "@"),
            Self::DotDot => write!(f, ".."),
            Self::Assign => write!(f, "="),
            Self::Colon => write!(f, ":"),
            Self::Comma => write!(f, ","),
            Self::Dot => write!(f, "."),
            Self::LParen => write!(f, "("),
            Self::RParen => write!(f, ")"),
            Self::LBrack => write!(f, "["),
            Self::RBrack => write!(f, "]"),
            Self::LBrace => write!(f, "{{"),
            Self::Pipe => write!(f, "|"),
            Self::Plus => write!(f, "+"),
            Self::Minus => write!(f, "-"),
            Self::Star => write!(f, "*"),
            Self::Slash => write!(f, "/"),
            Self::Percent => write!(f, "%"),
            Self::Caret => write!(f, "^"),
            Self::AmpAmp => write!(f, "&&"),
            Self::PipePipe => write!(f, "||"),
            Self::EqEq => write!(f, "=="),
            Self::BangEq => write!(f, "!="),
            Self::Lt => write!(f, "<"),
            Self::Gt => write!(f, ">"),
            Self::Le => write!(f, "<="),
            Self::Ge => write!(f, ">="),

            Self::DocComment(s) => write!(f, "---\n{s}\n---"),

            Self::Newline => write!(f, "<NEWLINE>"),
            Self::Indent => write!(f, "<INDENT>"),
            Self::Dedent => write!(f, "<DEDENT>"),
            Self::Eof => write!(f, "<EOF>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_display_roundtrip() {
        // Every keyword's Display output matches its source spelling.
        let pairs: &[(&str, Token)] = &[
            ("actor", Token::KwActor),
            ("as", Token::KwAs),
            ("catch", Token::KwCatch),
            ("class", Token::KwClass),
            ("const", Token::KwConst),
            ("deriving", Token::KwDeriving),
            ("else", Token::KwElse),
            ("false", Token::KwFalse),
            ("fn", Token::KwFn),
            ("guard", Token::KwGuard),
            ("if", Token::KwIf),
            ("import", Token::KwImport),
            ("in", Token::KwIn),
            ("init", Token::KwInit),
            ("instance", Token::KwInstance),
            ("let", Token::KwLet),
            ("match", Token::KwMatch),
            ("on", Token::KwOn),
            ("pub", Token::KwPub),
            ("return", Token::KwReturn),
            ("spawn", Token::KwSpawn),
            ("state", Token::KwState),
            ("then", Token::KwThen),
            ("true", Token::KwTrue),
            ("try", Token::KwTry),
            ("type", Token::KwType),
            ("var", Token::KwVar),
            ("when", Token::KwWhen),
            ("where", Token::KwWhere),
            ("with", Token::KwWith),
        ];
        for (src, tok) in pairs {
            assert_eq!(&tok.to_string(), src, "keyword mismatch for `{src}`");
        }
    }

    #[test]
    fn punctuation_display_roundtrip() {
        let pairs: &[(&str, Token)] = &[
            ("|>", Token::PipeFwd),
            ("<-", Token::LeftArrow),
            ("?>", Token::QuestionGt),
            ("?", Token::Question),
            ("!", Token::Bang),
            ("::", Token::ColonColon),
            ("++", Token::PlusPlus),
            ("->", Token::Arrow),
            ("=>", Token::FatArrow),
            ("@", Token::At),
            ("..", Token::DotDot),
            ("=", Token::Assign),
            (":", Token::Colon),
            (",", Token::Comma),
            (".", Token::Dot),
            ("+", Token::Plus),
            ("-", Token::Minus),
            ("*", Token::Star),
            ("/", Token::Slash),
            ("%", Token::Percent),
            ("^", Token::Caret),
            ("&&", Token::AmpAmp),
            ("||", Token::PipePipe),
            ("==", Token::EqEq),
            ("!=", Token::BangEq),
            ("<", Token::Lt),
            (">", Token::Gt),
            ("<=", Token::Le),
            (">=", Token::Ge),
        ];
        for (src, tok) in pairs {
            assert_eq!(&tok.to_string(), src, "punctuation mismatch for `{src}`");
        }
    }
}
