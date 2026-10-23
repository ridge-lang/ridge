//! The parser reports malformed upstream layout without rejecting valid newlines.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use ridge_ast::Span;
use ridge_lexer::{tokenize, Token};
use ridge_parser::{parse_module, parse_source, ParseError};

#[test]
fn bracket_layout_invariant_rejects_indentation_tokens() {
    let source = "pub fn identity (x: Int) -> Int = x\n";
    for token in [Token::Indent, Token::Dedent] {
        let mut lexed = tokenize(source);
        let at = lexed
            .tokens
            .iter()
            .position(|(t, _)| *t == Token::LParen)
            .unwrap()
            + 1;
        let span = Span::point(lexed.tokens[at].1.start);
        lexed.tokens.insert(at, (token, span));
        let result = parse_module(&lexed.tokens);
        assert!(
            result.errors.iter().any(|error| matches!(
                error,
                ParseError::InternalLayoutInvariantViolated { span: found } if *found == span
            )),
            "{:?}",
            result.errors
        );
    }
    let valid = parse_source("pub fn pair () = (1,\n    2)\n");
    assert!(valid.lex_errors.is_empty(), "{:?}", valid.lex_errors);
    assert!(valid.errors.is_empty(), "{:?}", valid.errors);
    let unclosed = parse_source("pub fn value () =\n    (1\n");
    assert!(!unclosed.errors.is_empty());
    assert!(!unclosed
        .errors
        .iter()
        .any(|error| matches!(error, ParseError::InternalLayoutInvariantViolated { .. })));
}
