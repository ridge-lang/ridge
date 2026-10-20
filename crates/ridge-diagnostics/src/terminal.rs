//! Visible rendering of untrusted text inside terminal messages.

use std::fmt::{self, Write};

/// Render a value with C0, C1 and DEL controls replaced by visible Rust escapes.
///
/// Printable Unicode, quotes and backslashes are preserved. This streams into
/// the destination without allocating a complete intermediate message, and does
/// not truncate. Apply it to message content, leaving trusted layout such as
/// diagnostic line endings outside the wrapper.
pub fn terminal_text(value: impl fmt::Display) -> impl fmt::Display {
    TerminalText(value)
}

struct TerminalText<T>(T);

impl<T: fmt::Display> fmt::Display for TerminalText<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(EscapingWriter(f), "{}", self.0)
    }
}

struct EscapingWriter<'a, 'b>(&'a mut fmt::Formatter<'b>);

impl Write for EscapingWriter<'_, '_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut start = 0;
        for (index, ch) in text.char_indices() {
            if ch.is_control() {
                self.0.write_str(&text[start..index])?;
                write!(self.0, "{}", ch.escape_default())?;
                start = index + ch.len_utf8();
            }
        }
        self.0.write_str(&text[start..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_text_escapes_control_families_and_preserves_printable_text() {
        assert_eq!(
            terminal_text("café 'C:\\src'\0\t\n\r\u{1b}[2J\u{7}\u{7f}\u{9b}2J").to_string(),
            "café 'C:\\src'\\u{0}\\t\\n\\r\\u{1b}[2J\\u{7}\\u{7f}\\u{9b}2J"
        );
        for ch in ('\0'..='\u{1f}').chain('\u{7f}'..='\u{9f}') {
            assert!(terminal_text(ch)
                .to_string()
                .chars()
                .all(|c| !c.is_control()));
        }
    }

    #[test]
    fn terminal_text_streams_chunks_without_truncating_or_double_escaping() {
        struct Chunks;
        impl fmt::Display for Chunks {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("é\u{1b}")?;
                f.write_str("]0;title\u{7}")
            }
        }
        assert_eq!(terminal_text(Chunks).to_string(), "é\\u{1b}]0;title\\u{7}");
        let long = format!("{}\u{1b}END", "é".repeat(16384));
        let rendered = terminal_text(&long).to_string();
        assert_eq!(rendered, format!("{}\\u{{1b}}END", "é".repeat(16384)));
        assert_eq!(terminal_text(&rendered).to_string(), rendered);
    }

    #[test]
    fn terminal_text_propagates_destination_errors() {
        struct Failing;
        impl Write for Failing {
            fn write_str(&mut self, _: &str) -> fmt::Result {
                Err(fmt::Error)
            }
        }
        assert!(write!(Failing, "{}", terminal_text("\u{1b}")).is_err());
    }
}
