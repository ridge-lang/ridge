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
}
