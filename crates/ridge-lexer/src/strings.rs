//! Escape validation for string literals and interpolated text segments (D047).
//!
//! The valid escape set per spec line 497 / D047:
//! `\n`, `\t`, `\"`, `\\`, `\r`, `\0`, `\u{HHHH}`
//!
//! The lexer validates escapes **eagerly** (OQ-L005 default).  The raw source
//! bytes are preserved in the token; decoding to a concrete character value
//! is a parser/type-checker concern.

use crate::{
    error::{LexError, UnicodeEscapeError},
    span::Span,
};

/// Validate all escape sequences inside the content slice of a string literal
/// or interpolated text segment.
///
/// `content` is the raw bytes **between** the string delimiters (not including
/// the surrounding `"` characters).  `content_start` is the byte offset of the
/// first content byte in the original source (used to produce accurate spans).
///
/// Returns a vector of [`LexError`]s; empty means all escapes are valid.
pub(crate) fn validate_escapes(content: &str, content_start: u32) -> Vec<LexError> {
    let mut errors = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        // Found a backslash — look at the next byte.
        let escape_start = content_start + i as u32;
        i += 1;
        if i >= bytes.len() {
            // Trailing backslash at end-of-content; the unterminated-string
            // error is raised by the caller.
            break;
        }
        match bytes[i] {
            b'n' | b't' | b'"' | b'\\' | b'r' | b'0' => {
                i += 1; // valid single-char escape
            }
            b'u' => {
                // \u{HHHH} — validate the `{...}` part.
                i += 1;
                if i >= bytes.len() || bytes[i] != b'{' {
                    let span = Span::new(escape_start, content_start + i as u32);
                    errors.push(LexError::InvalidUnicodeEscape {
                        span,
                        reason: UnicodeEscapeError::Unterminated,
                    });
                    continue;
                }
                i += 1; // skip `{`
                let hex_start = i;
                while i < bytes.len() && bytes[i] != b'}' && bytes[i] != b'"' {
                    i += 1;
                }
                if i >= bytes.len() || bytes[i] != b'}' {
                    let span = Span::new(escape_start, content_start + i as u32);
                    errors.push(LexError::InvalidUnicodeEscape {
                        span,
                        reason: UnicodeEscapeError::Unterminated,
                    });
                    continue;
                }
                // We have `{...}` — now validate hex content.
                let hex_bytes = &bytes[hex_start..i];
                i += 1; // skip `}`
                if hex_bytes.is_empty() || !hex_bytes.iter().all(u8::is_ascii_hexdigit) {
                    let span = Span::new(escape_start, content_start + i as u32);
                    errors.push(LexError::InvalidUnicodeEscape {
                        span,
                        reason: UnicodeEscapeError::InvalidHex,
                    });
                    continue;
                }
                // Validate scalar value range (≤ 0x10FFFF, not a surrogate).
                let hex_str = std::str::from_utf8(hex_bytes).unwrap_or(""); // hex digits are ASCII
                if let Ok(val) = u32::from_str_radix(hex_str, 16) {
                    if val > 0x0010_FFFF || (0xD800..=0xDFFF).contains(&val) {
                        let span = Span::new(escape_start, content_start + i as u32);
                        errors.push(LexError::InvalidUnicodeEscape {
                            span,
                            reason: UnicodeEscapeError::OutOfRange,
                        });
                    }
                } else {
                    // Too many digits for u32 — definitely out of range.
                    let span = Span::new(escape_start, content_start + i as u32);
                    errors.push(LexError::InvalidUnicodeEscape {
                        span,
                        reason: UnicodeEscapeError::OutOfRange,
                    });
                }
            }
            other => {
                // Unknown escape character.
                let ch = other as char;
                let span = Span::new(escape_start, content_start + i as u32 + 1);
                errors.push(LexError::InvalidEscape {
                    span,
                    got: format!("\\{ch}"),
                });
                i += 1;
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_escapes() {
        let errors = validate_escapes(r#"\n\t\"\\\r\0"#, 0);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn valid_unicode_escape() {
        let errors = validate_escapes(r"\u{1F600}", 0);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn valid_unicode_ascii() {
        let errors = validate_escapes(r"\u{41}", 0);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn invalid_escape_x() {
        let errors = validate_escapes(r"\x00", 0);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            LexError::InvalidEscape { got, .. } if got == r"\x"
        ));
    }

    #[test]
    fn invalid_unicode_surrogate() {
        let errors = validate_escapes(r"\u{D800}", 0);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            LexError::InvalidUnicodeEscape {
                reason: UnicodeEscapeError::OutOfRange,
                ..
            }
        ));
    }

    #[test]
    fn invalid_unicode_too_large() {
        let errors = validate_escapes(r"\u{110000}", 0);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            LexError::InvalidUnicodeEscape {
                reason: UnicodeEscapeError::OutOfRange,
                ..
            }
        ));
    }

    #[test]
    fn unterminated_unicode_no_brace() {
        // \u without { is unterminated
        let errors = validate_escapes(r"\u41", 0);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            LexError::InvalidUnicodeEscape {
                reason: UnicodeEscapeError::Unterminated,
                ..
            }
        ));
    }

    #[test]
    fn unterminated_unicode_no_close_brace() {
        let errors = validate_escapes(r"\u{41", 0);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            LexError::InvalidUnicodeEscape {
                reason: UnicodeEscapeError::Unterminated,
                ..
            }
        ));
    }

    #[test]
    fn unicode_empty_braces() {
        let errors = validate_escapes(r"\u{}", 0);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            LexError::InvalidUnicodeEscape {
                reason: UnicodeEscapeError::InvalidHex,
                ..
            }
        ));
    }

    #[test]
    fn unicode_non_hex() {
        let errors = validate_escapes(r"\u{GG}", 0);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            LexError::InvalidUnicodeEscape {
                reason: UnicodeEscapeError::InvalidHex,
                ..
            }
        ));
    }

    #[test]
    fn multiple_errors() {
        let errors = validate_escapes(r"\x\j", 0);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn no_escapes_plain_text() {
        let errors = validate_escapes("hello world", 0);
        assert!(errors.is_empty());
    }

    #[test]
    fn offset_propagation() {
        // Content starts at byte 5 in the source.
        // The `\x` is at offset 0 within the content, so absolute offset is 5.
        let errors = validate_escapes(r"\x", 5);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].span().start, 5);
    }
}
