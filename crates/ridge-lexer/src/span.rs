//! Byte-offset span and line-map for diagnostics.
//!
//! `Span` is the canonical source location type used throughout `ridge-lexer`.
//! It uses `u32` offsets (sufficient for files up to 4 GiB, which is a safe
//! upper bound for Ridge source files in 0.1.0).

/// A half-open byte range `[start, end)` into a source file.
///
/// Both offsets are **byte** offsets (not char or codepoint offsets).
/// `ariadne` and `logos` both work in byte offsets natively, so no conversion
/// is required.  Line/column numbers are a derived quantity — use
/// [`LineMap::line_col`] when you need them for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

impl Span {
    /// Construct a new span.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// A zero-width span at a single byte position (used for synthesised tokens).
    #[must_use]
    pub const fn point(offset: u32) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    /// Return the number of bytes covered by this span.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// True for zero-width spans (typically synthesised layout tokens).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Extend this span to cover `other` as well (convex hull).
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

impl std::fmt::Display for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// Maps byte offsets to `(line, column)` pairs for human-readable diagnostics.
///
/// Built once per source file; cheap to query thereafter.  Lines are 0-indexed
/// internally but the [`line_col`](LineMap::line_col) method returns 1-based
/// values matching editor convention.
pub struct LineMap {
    /// `line_starts[i]` is the byte offset of the first character on line `i`.
    line_starts: Vec<u32>,
}

impl LineMap {
    /// Build a `LineMap` from the normalised source text.
    ///
    /// Both `\n` and (after normalisation) `\r\n` / `\r` map to single `\n`
    /// boundaries, so the caller should pass the already-normalised source.
    #[must_use]
    pub fn new(src: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                // Safety: source files are limited to 4 GiB.
                #[allow(clippy::cast_possible_truncation)]
                line_starts.push((i + 1) as u32);
            }
        }
        Self { line_starts }
    }

    /// Convert a byte offset to a `(line, column)` pair (both **1-based**).
    ///
    /// Returns `(1, 1)` for any offset into an empty file.
    #[must_use]
    pub fn line_col(&self, byte_offset: u32) -> (u32, u32) {
        let line_idx = self
            .line_starts
            .partition_point(|&start| start <= byte_offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line_idx];
        let col = byte_offset - line_start;
        (u32::try_from(line_idx).unwrap_or(u32::MAX) + 1, col + 1)
    }

    /// Total number of lines recorded (at least 1 for any input).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }
}

/// A non-ASCII character recorded for UTF-16 ↔ byte conversion.
///
/// Only characters whose UTF-8 length is greater than one are recorded; on an
/// ASCII-only line the per-line list is empty and conversion is a simple offset
/// add.
#[derive(Debug, Clone, Copy)]
struct WideChar {
    /// Byte offset of this character within its line.
    byte_in_line: u32,
    /// Number of UTF-8 bytes the character occupies (2–4).
    utf8_len: u8,
    /// Number of UTF-16 code units the character occupies (1 for the basic
    /// multilingual plane, 2 for a surrogate pair).
    utf16_len: u8,
}

/// Maps between LSP UTF-16 positions and byte offsets.
///
/// LSP `Position.character` counts UTF-16 code units, while the compiler works
/// in byte offsets. [`LineMap`] only converts byte → (line, byte-column), which
/// is wrong on any line containing non-ASCII text. `LineIndex` records the
/// non-ASCII characters per line so the two encodings can be converted exactly,
/// while staying allocation-light: an ASCII-only line carries an empty list and
/// hits a fast offset-arithmetic path.
#[derive(Debug)]
pub struct LineIndex {
    /// `line_starts[i]` is the byte offset of the first byte on line `i`.
    line_starts: Vec<u32>,
    /// Per line, the non-ASCII characters in source order. Empty for ASCII-only
    /// lines (the common case for Ridge identifiers and keywords).
    wide_chars: Vec<Vec<WideChar>>,
    /// Total byte length of the source, used to clamp out-of-range positions.
    len: u32,
}

impl LineIndex {
    /// Build a `LineIndex` from source text.
    ///
    /// `\n` ends a line; a preceding `\r` stays as the last byte of the line it
    /// terminates, matching [`LineMap::new`].
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "source files are bounded to 4 GiB; UTF-8/UTF-16 char lengths are 1–4"
    )]
    pub fn new(src: &str) -> Self {
        let mut line_starts = vec![0u32];
        let mut wide_chars: Vec<Vec<WideChar>> = vec![Vec::new()];
        let mut line_start = 0u32;
        for (byte_idx, ch) in src.char_indices() {
            let byte_idx = byte_idx as u32;
            if ch == '\n' {
                line_start = byte_idx + 1;
                line_starts.push(line_start);
                wide_chars.push(Vec::new());
                continue;
            }
            let utf8_len = ch.len_utf8() as u8;
            if utf8_len > 1 {
                // `wide_chars` always has an entry for the current line.
                if let Some(line) = wide_chars.last_mut() {
                    line.push(WideChar {
                        byte_in_line: byte_idx - line_start,
                        utf8_len,
                        utf16_len: ch.len_utf16() as u8,
                    });
                }
            }
        }
        Self {
            line_starts,
            wide_chars,
            len: src.len() as u32,
        }
    }

    /// Exclusive byte offset of a line's content end (the newline byte, or the
    /// end of the source for the last line).
    fn line_end(&self, line: usize) -> u32 {
        if line + 1 < self.line_starts.len() {
            self.line_starts[line + 1].saturating_sub(1)
        } else {
            self.len
        }
    }

    /// Convert an LSP `(line, utf16_col)` position (both 0-based) to a byte
    /// offset.
    ///
    /// Out-of-range lines clamp to the last line; a column past end-of-line
    /// clamps to the line's content end. Never panics.
    #[must_use]
    pub fn utf16_to_byte(&self, line: u32, utf16_col: u32) -> u32 {
        let last = self.line_starts.len() - 1;
        let line = (line as usize).min(last);
        let line_start = self.line_starts[line];
        let line_end = self.line_end(line);

        let byte = if self.wide_chars[line].is_empty() {
            line_start + utf16_col
        } else {
            let mut u16_seen = 0u32;
            let mut byte_in_line = 0u32;
            let mut resolved = None;
            for wc in &self.wide_chars[line] {
                let ascii_run = wc.byte_in_line - byte_in_line;
                if u16_seen + ascii_run >= utf16_col {
                    resolved = Some(line_start + byte_in_line + (utf16_col - u16_seen));
                    break;
                }
                u16_seen += ascii_run;
                byte_in_line = wc.byte_in_line;
                if u16_seen + u32::from(wc.utf16_len) > utf16_col {
                    // The column lands inside a surrogate pair; clamp to the
                    // character's start.
                    resolved = Some(line_start + byte_in_line);
                    break;
                }
                u16_seen += u32::from(wc.utf16_len);
                byte_in_line += u32::from(wc.utf8_len);
            }
            resolved.unwrap_or(line_start + byte_in_line + (utf16_col - u16_seen))
        };

        byte.min(line_end)
    }

    /// Convert a byte offset to an LSP `(line, utf16_col)` position (both
    /// 0-based). Offsets past end-of-source clamp to the end. Never panics.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "line index is bounded by the 4 GiB source-size limit"
    )]
    pub fn byte_to_utf16(&self, byte_offset: u32) -> (u32, u32) {
        let byte_offset = byte_offset.min(self.len);
        let line = self
            .line_starts
            .partition_point(|&start| start <= byte_offset)
            .saturating_sub(1);
        let line_start = self.line_starts[line];
        let target = byte_offset - line_start;
        let line_u32 = line as u32;

        if self.wide_chars[line].is_empty() {
            return (line_u32, target);
        }

        let mut u16_seen = 0u32;
        let mut byte_in_line = 0u32;
        for wc in &self.wide_chars[line] {
            if wc.byte_in_line >= target {
                return (line_u32, u16_seen + (target - byte_in_line));
            }
            u16_seen += wc.byte_in_line - byte_in_line;
            byte_in_line = wc.byte_in_line;
            if byte_in_line + u32::from(wc.utf8_len) > target {
                // Offset falls inside a multi-byte character; clamp to its start.
                return (line_u32, u16_seen);
            }
            u16_seen += u32::from(wc.utf16_len);
            byte_in_line += u32::from(wc.utf8_len);
        }
        (line_u32, u16_seen + (target - byte_in_line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_basics() {
        let s = Span::new(4, 9);
        assert_eq!(s.len(), 5);
        assert!(!s.is_empty());
        assert_eq!(s.to_string(), "4..9");
    }

    #[test]
    fn span_point() {
        let p = Span::point(7);
        assert!(p.is_empty());
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn span_merge() {
        let a = Span::new(2, 5);
        let b = Span::new(4, 10);
        assert_eq!(a.merge(b), Span::new(2, 10));
    }

    #[test]
    fn linemap_single_line() {
        let lm = LineMap::new("hello");
        assert_eq!(lm.line_col(0), (1, 1));
        assert_eq!(lm.line_col(4), (1, 5));
    }

    #[test]
    fn linemap_multiline() {
        // "abc\ndef\nghi"
        //  0123 456 7890
        let src = "abc\ndef\nghi";
        let lm = LineMap::new(src);
        assert_eq!(lm.line_col(0), (1, 1)); // 'a'
        assert_eq!(lm.line_col(3), (1, 4)); // '\n'
        assert_eq!(lm.line_col(4), (2, 1)); // 'd'
        assert_eq!(lm.line_col(7), (2, 4)); // '\n'
        assert_eq!(lm.line_col(8), (3, 1)); // 'g'
    }

    #[test]
    fn linemap_empty() {
        let lm = LineMap::new("");
        assert_eq!(lm.line_col(0), (1, 1));
    }

    #[test]
    fn linemap_line_count() {
        let lm = LineMap::new("a\nb\nc");
        assert_eq!(lm.line_count(), 3);
    }

    // ── LineIndex (UTF-16 ↔ byte) ──────────────────────────────────────────────

    #[test]
    fn line_index_ascii_identity() {
        let li = LineIndex::new("fn foo = 42");
        assert_eq!(li.utf16_to_byte(0, 0), 0);
        assert_eq!(li.utf16_to_byte(0, 3), 3);
        assert_eq!(li.byte_to_utf16(3), (0, 3));
    }

    #[test]
    fn line_index_two_byte_char() {
        // "café": c,a,f are 1 byte/1 unit; é is 2 bytes/1 unit.
        let li = LineIndex::new("café");
        assert_eq!(li.utf16_to_byte(0, 3), 3, "start of é");
        assert_eq!(li.utf16_to_byte(0, 4), 5, "one past é = end");
        assert_eq!(li.byte_to_utf16(3), (0, 3));
        assert_eq!(li.byte_to_utf16(5), (0, 4));
    }

    #[test]
    fn line_index_emoji_surrogate_pair() {
        // "😀ab": U+1F600 is 4 bytes / 2 UTF-16 units.
        let li = LineIndex::new("😀ab");
        assert_eq!(li.utf16_to_byte(0, 0), 0);
        assert_eq!(li.utf16_to_byte(0, 2), 4, "past the emoji");
        assert_eq!(li.utf16_to_byte(0, 3), 5, "the 'b'");
        assert_eq!(li.byte_to_utf16(0), (0, 0));
        assert_eq!(li.byte_to_utf16(4), (0, 2));
    }

    #[test]
    fn line_index_mid_surrogate_clamps() {
        // A column landing inside the surrogate pair clamps to the char start.
        let li = LineIndex::new("😀");
        assert_eq!(li.utf16_to_byte(0, 1), 0);
    }

    #[test]
    fn line_index_crlf_second_line() {
        // "hello\r\nworld": '\n' at byte 6, so line 1 begins at byte 7.
        let li = LineIndex::new("hello\r\nworld");
        assert_eq!(li.utf16_to_byte(1, 0), 7);
        assert_eq!(li.byte_to_utf16(7), (1, 0));
    }

    #[test]
    fn line_index_past_eol_clamps() {
        let li = LineIndex::new("hello");
        assert_eq!(li.utf16_to_byte(0, 9999), 5, "clamp to line end");
    }

    #[test]
    fn line_index_past_eof_line_clamps() {
        let li = LineIndex::new("a\nb\nc");
        // Line far past the end clamps to the last line; no panic.
        let _ = li.utf16_to_byte(9999, 0);
        assert_eq!(li.byte_to_utf16(9999), (2, 1), "clamps to end of source");
    }

    #[test]
    fn line_index_empty() {
        let li = LineIndex::new("");
        assert_eq!(li.utf16_to_byte(0, 0), 0);
        assert_eq!(li.byte_to_utf16(0), (0, 0));
    }

    #[test]
    fn line_index_cjk_column() {
        // Three CJK chars (3 bytes each, 1 UTF-16 unit each) then 'x'.
        let li = LineIndex::new("日本語x");
        assert_eq!(li.utf16_to_byte(0, 3), 9, "after 3 CJK chars = byte 9");
        assert_eq!(li.utf16_to_byte(0, 4), 10, "the 'x'");
        assert_eq!(li.byte_to_utf16(9), (0, 3));
    }
}
