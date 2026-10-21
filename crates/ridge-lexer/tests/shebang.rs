//! An interpreter directive is trivia only at the beginning of the source.
#![allow(clippy::unwrap_used)]
use ridge_lexer::{tokenize, Token};

#[test]
fn leading_shebang_keeps_source_offsets_and_line_numbers() {
    for prefix in ["#!/usr/bin/env ridge\n", "#!/ruta/ñ/ridge --flag\n"] {
        let source = format!("{prefix}pub fn answer -> Int = 42\n");
        let lexed = tokenize(&source);
        assert!(lexed.errors.is_empty(), "{:?}", lexed.errors);
        let span = lexed
            .tokens
            .iter()
            .find_map(|(token, span)| matches!(token, Token::KwPub).then_some(*span))
            .unwrap();
        assert_eq!(span.start as usize, prefix.len());
        assert_eq!(lexed.line_map.line_col(span.start), (2, 1));
    }
}

#[test]
fn crlf_shebang_uses_normalized_offsets() {
    let lexed = tokenize("#!/usr/bin/env ridge\r\npub fn answer -> Int = 42\r\n");
    assert!(lexed.errors.is_empty(), "{:?}", lexed.errors);
    let (_, span) = lexed
        .tokens
        .iter()
        .find(|(token, _)| matches!(token, Token::KwPub))
        .unwrap();
    assert_eq!(span.start as usize, "#!/usr/bin/env ridge\n".len());
    assert_eq!(lexed.line_map.line_col(span.start), (2, 1));
}

#[test]
fn shebang_without_a_following_line_is_valid_trivia() {
    let lexed = tokenize("#!/usr/bin/env ridge");
    assert!(lexed.errors.is_empty(), "{:?}", lexed.errors);
    assert!(lexed
        .tokens
        .iter()
        .all(|(token, _)| matches!(token, Token::Eof | Token::Newline)));
}

#[test]
fn shebang_is_not_a_comment_after_the_first_byte() {
    for source in [
        "\n#!/usr/bin/env ridge\n",
        " #!/usr/bin/env ridge\n",
        "---\ndoc\n---\n#!/usr/bin/env ridge\n",
    ] {
        assert!(!tokenize(source).errors.is_empty(), "{source:?}");
    }
}
