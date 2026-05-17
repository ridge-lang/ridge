//! Hand-written block doc-comment scanner (grammar §1.6).
//!
//! A doc-comment is:
//! ```text
//! "---" NEWLINE { (* any line not solely "---" *) NEWLINE } "---"
//! ```
//!
//! Both the opening and closing `---` must be **alone on their line**
//! (strictly alone, per grammar lines 260–262).
//!
//! This scanner is called from `raw_scan` when it detects a line whose entire
//! non-whitespace content is `---`, after already consuming the three dashes.
//! It returns the collected body text and the end-byte offset of the closing
//! `---`, or a `LexError::UnterminatedDocComment` if EOF was reached first.

use crate::{error::LexError, span::Span};

/// Scan the body of a doc-comment, starting from the byte immediately after
/// the opening `---` has been consumed.
///
/// `src` is the **full** normalised source; `pos` is the current byte position
/// (pointing at the `\n` after the opening `---`, or at EOF).
/// `open_start` is the byte offset of the very first `-` in `---`.
///
/// Returns `(body_text, end_pos)` on success where `end_pos` is the byte
/// immediately after the closing `---` line's newline (or EOF), or an error.
pub(crate) fn scan_doc_body(
    src: &str,
    pos: usize,
    open_start: usize,
) -> Result<(String, usize), LexError> {
    let mut body = String::new();
    let mut i = pos;
    let bytes = src.as_bytes();

    // Skip the newline that follows the opening `---`.
    if i < bytes.len() && bytes[i] == b'\n' {
        i += 1;
    }

    loop {
        if i >= bytes.len() {
            // EOF before closing `---`.
            return Err(LexError::UnterminatedDocComment {
                open_span: Span::point(u32::try_from(open_start).unwrap_or(u32::MAX)),
            });
        }

        // Find end of this line.
        let line_start = i;
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        let line = &src[line_start..i];

        // Is this the closing `---` line?
        if line.trim() == "---" {
            // Consume the trailing newline if present.
            if i < bytes.len() && bytes[i] == b'\n' {
                i += 1;
            }
            // Trim a single trailing newline from body for cleanliness.
            if body.ends_with('\n') {
                body.pop();
            }
            return Ok((body, i));
        }

        // Not a closing line — accumulate it.
        body.push_str(line);
        // Consume the `\n`.
        if i < bytes.len() && bytes[i] == b'\n' {
            body.push('\n');
            i += 1;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may use unwrap
mod tests {
    use super::*;

    fn scan(src: &str) -> Result<(String, usize), LexError> {
        // Find the `---` opener and call scan_doc_body after it.
        let opener = src.find("---").unwrap_or(0);
        let after_dashes = opener + 3;
        scan_doc_body(src, after_dashes, opener)
    }

    #[test]
    fn simple_body() {
        let src = "---\nHello doc\n---\n";
        let (body, _) = scan(src).unwrap();
        assert_eq!(body, "Hello doc");
    }

    #[test]
    fn multiline_body() {
        let src = "---\nLine one\nLine two\n---\n";
        let (body, _) = scan(src).unwrap();
        assert_eq!(body, "Line one\nLine two");
    }

    #[test]
    fn empty_body() {
        let src = "---\n---\n";
        let (body, _) = scan(src).unwrap();
        assert_eq!(body, "");
    }

    #[test]
    fn body_with_dashes() {
        // `--` inside the body should NOT close it — only `---` alone on a line.
        let src = "---\n-- a comment inside\n---\n";
        let (body, _) = scan(src).unwrap();
        assert_eq!(body, "-- a comment inside");
    }

    #[test]
    fn body_with_single_dash() {
        let src = "---\n- item\n---\n";
        let (body, _) = scan(src).unwrap();
        assert_eq!(body, "- item");
    }

    #[test]
    fn unterminated() {
        let src = "---\nhello\n";
        let result = scan(src);
        assert!(matches!(
            result,
            Err(LexError::UnterminatedDocComment { .. })
        ));
    }

    #[test]
    fn unterminated_eof_no_newline() {
        let src = "---\nhello";
        let result = scan(src);
        assert!(matches!(
            result,
            Err(LexError::UnterminatedDocComment { .. })
        ));
    }

    #[test]
    fn end_position_after_close() {
        // After the closing `---\n` the position should point past it.
        // "---\nfoo\n---\n" = 3+1+3+1+3+1 = 12 bytes.
        // scan() calls scan_doc_body with opener=0, after_dashes=3.
        // scan_doc_body returns the position after the closing `---\n`.
        // The `scan` helper adds the 3 dashes offset already; the raw function
        // is called with pos=3 (after `---`), open_start=0.
        // End should be 12 (0-based, past the trailing \n of `---\n`).
        let src = "---\nfoo\n---\nafter";
        let (_, end) = scan(src).unwrap();
        // "---\nfoo\n---\n" is 12 bytes.
        assert_eq!(end, 12);
    }

    #[test]
    fn dashes_not_alone_on_line() {
        // `--- title` on a line should not close the doc comment.
        let src = "---\n--- not a closer\n---\n";
        let (body, _) = scan(src).unwrap();
        assert_eq!(body, "--- not a closer");
    }
}
