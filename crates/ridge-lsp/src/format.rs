//! Helpers for range and on-type formatting.
//!
//! Both build on the same formatter the CLI's `ridge fmt` and the whole-document
//! `textDocument/formatting` handler use ([`ridge_fmt::format_source`]) so there
//! is one definition of "formatted Ridge", never a second that could drift.
//!
//! - Range formatting ([`range_format_edits`]) formats the whole buffer, diffs it
//!   against the original by line, and keeps only the change hunks that overlap
//!   the requested range — the selection is reformatted, the rest is left alone.
//! - On-type formatting ([`on_type_newline_edits`]) reacts to a newline by setting
//!   the fresh line's indentation from the offside structure of the preceding
//!   line. It is purely lexical, so it works on the half-written buffer a full
//!   parse would reject, and it mirrors the VS Code `increaseIndentPattern` so
//!   every client gets the same offside auto-indent, not only VS Code.

use ridge_lexer::LineIndex;
use tower_lsp::lsp_types::{Position, Range, TextEdit};

/// Fallback indentation step, in spaces, for when the client reports no
/// `tab_size`. Two spaces is the Ridge house style — the `ridge-fmt` printer's
/// own indent unit.
const INDENT: usize = 2;

// ── Line addressing ───────────────────────────────────────────────────────────

/// Byte offset where each line begins. The count equals the number of lines as
/// produced by `str::split('\n')`: a trailing newline yields a final empty line.
fn line_starts(s: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Content of line `i` with its trailing `\n`/`\r` stripped. Stripping the line
/// ending lets a CRLF buffer compare equal to the formatter's LF output, so a
/// Windows checkout does not diff as "every line changed".
fn line_content<'a>(s: &'a str, starts: &[usize], i: usize) -> &'a str {
    let start = starts[i];
    let end = starts.get(i + 1).copied().unwrap_or(s.len());
    let raw = &s[start..end];
    let line = raw.strip_suffix('\n').unwrap_or(raw);
    line.strip_suffix('\r').unwrap_or(line)
}

// ── Line diff (range formatting) ──────────────────────────────────────────────

/// A run of changed lines: original lines `orig` (a half-open `[start, end)`
/// range of line indices) are replaced by formatted lines `fmt`.
#[derive(Debug, PartialEq, Eq)]
struct Hunk {
    orig: (usize, usize),
    fmt: (usize, usize),
}

/// Matched line-index pairs of the longest common subsequence of `a` and `b`,
/// ascending. `O(n*m)` time and space, which is fine because the caller trims the
/// shared prefix and suffix first, leaving only the genuinely changed band.
fn lcs_matches(lhs: &[&str], rhs: &[&str]) -> Vec<(usize, usize)> {
    let rows = lhs.len();
    let cols = rhs.len();
    let mut dp = vec![vec![0u32; cols + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            dp[i][j] = if lhs[i] == rhs[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < rows && j < cols {
        if lhs[i] == rhs[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// Split the two line sequences into change hunks. Unchanged lines belong to no
/// hunk; each hunk is a maximal run where the original and formatted lines differ.
fn line_hunks(orig_lines: &[&str], fmt_lines: &[&str]) -> Vec<Hunk> {
    // Shared prefix.
    let mut p = 0;
    while p < orig_lines.len() && p < fmt_lines.len() && orig_lines[p] == fmt_lines[p] {
        p += 1;
    }
    // Shared suffix, not reaching into the prefix.
    let mut s = 0;
    while s < orig_lines.len() - p
        && s < fmt_lines.len() - p
        && orig_lines[orig_lines.len() - 1 - s] == fmt_lines[fmt_lines.len() - 1 - s]
    {
        s += 1;
    }
    let o_mid = &orig_lines[p..orig_lines.len() - s];
    let f_mid = &fmt_lines[p..fmt_lines.len() - s];

    let mut hunks = Vec::new();
    let mut oi = 0usize;
    let mut fi = 0usize;
    let push_gap = |oi: usize, mo: usize, fi: usize, mf: usize, out: &mut Vec<Hunk>| {
        if oi < mo || fi < mf {
            out.push(Hunk {
                orig: (p + oi, p + mo),
                fmt: (p + fi, p + mf),
            });
        }
    };
    for (mo, mf) in lcs_matches(o_mid, f_mid) {
        push_gap(oi, mo, fi, mf, &mut hunks);
        oi = mo + 1;
        fi = mf + 1;
    }
    push_gap(oi, o_mid.len(), fi, f_mid.len(), &mut hunks);
    hunks
}

/// Reformat only the lines the requested range touches.
///
/// `formatted` is the whole-document output of [`ridge_fmt::format_source`]; the
/// diff against `text` is restricted to hunks overlapping `sel` (expanded to whole
/// lines), so a "format selection" leaves the rest of the file untouched. Returns
/// no edits when the selection lands entirely on already-formatted lines.
#[must_use]
pub fn range_format_edits(
    text: &str,
    formatted: &str,
    sel: Range,
    line_index: &LineIndex,
) -> Vec<TextEdit> {
    let o_starts = line_starts(text);
    let f_starts = line_starts(formatted);
    let orig_lines: Vec<&str> = (0..o_starts.len())
        .map(|i| line_content(text, &o_starts, i))
        .collect();
    let fmt_lines: Vec<&str> = (0..f_starts.len())
        .map(|i| line_content(formatted, &f_starts, i))
        .collect();

    // The selection touches every line from its first to its last, inclusive.
    let sel_lo = sel.start.line as usize;
    let sel_hi = sel.end.line as usize + 1;

    let mut edits = Vec::new();
    for hunk in line_hunks(&orig_lines, &fmt_lines) {
        let (a, b) = hunk.orig;
        // Intersect the hunk's original lines `[a, b)` with the selected band.
        // A pure insertion (`a == b`) counts as overlapping when it sits strictly
        // inside the selection.
        let overlaps = if a == b {
            sel_lo < a && a < sel_hi
        } else {
            a < sel_hi && sel_lo < b
        };
        if !overlaps {
            continue;
        }
        let byte_start = o_starts[a];
        let byte_end = o_starts.get(b).copied().unwrap_or(text.len());
        let (start_line, start_col) = line_index.byte_to_utf16(byte_start_u32(byte_start));
        let (end_line, end_col) = line_index.byte_to_utf16(byte_start_u32(byte_end));
        let f_byte_start = f_starts[hunk.fmt.0];
        let f_byte_end = f_starts.get(hunk.fmt.1).copied().unwrap_or(formatted.len());
        edits.push(TextEdit {
            range: Range {
                start: Position::new(start_line, start_col),
                end: Position::new(end_line, end_col),
            },
            new_text: formatted[f_byte_start..f_byte_end].to_owned(),
        });
    }
    edits
}

/// Source files are bounded well under 4 GiB, so a byte offset always fits a
/// `u32`; clamp defensively rather than panic on the impossible overflow.
fn byte_start_u32(byte: usize) -> u32 {
    u32::try_from(byte).unwrap_or(u32::MAX)
}

// ── On-type indentation ───────────────────────────────────────────────────────

/// Number of leading spaces on a line (its indentation, in columns).
fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

/// Drop a trailing `--` line comment, matching the VS Code indentation rules
/// (whose `(--.*)?$` does the same). Like the editor's regex, this does not parse
/// strings, so a `--` inside a string literal is treated as a comment — an
/// accepted approximation already shipped client-side.
fn strip_trailing_comment(line: &str) -> &str {
    line.find("--").map_or(line, |idx| &line[..idx])
}

/// Whether `kw` ends `code` as a whole word (not as the tail of a longer word
/// such as `orelse`).
fn ends_with_keyword(code: &str, kw: &str) -> bool {
    code.strip_suffix(kw).is_some_and(|prefix| {
        prefix
            .chars()
            .last()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_')
    })
}

/// Whether `code` is the head of a `match` expression — `match <scrutinee>` whose
/// arms continue on the following indented lines. Mirrors the grammar's match-head
/// rule: `match` in value position (line start or after `=`) with no `>` after it
/// (an inline `=>` arm would keep the body on the same line).
fn is_match_head(code: &str) -> bool {
    for (idx, _) in code.match_indices("match") {
        let before = &code[..idx];
        let after = &code[idx + "match".len()..];
        let left_boundary = before
            .chars()
            .last()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let right_boundary = after
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let value_position = {
            let t = before.trim();
            t.is_empty() || t.ends_with('=')
        };
        if left_boundary && right_boundary && value_position && !after.contains('>') {
            return true;
        }
    }
    false
}

/// Whether `line` opens an indented block — the next line should sit one step in.
/// Ports the VS Code `increaseIndentPattern`: a line ending in `=` (a real
/// binding, not `==`/`!=`/`<=`/`>=`), `->`, `<-`, the keyword `then`/`else`/`try`,
/// or a `match` head.
fn opens_block(line: &str) -> bool {
    let code = strip_trailing_comment(line).trim_end();
    if code.is_empty() {
        return false;
    }
    if code.ends_with("->") || code.ends_with("<-") {
        return true;
    }
    if let Some(prefix) = code.strip_suffix('=') {
        // A bare `=` opens a body; a comparison operator does not.
        if !matches!(prefix.chars().last(), Some('=' | '!' | '<' | '>')) {
            return true;
        }
    }
    if ["then", "else", "try"]
        .iter()
        .any(|kw| ends_with_keyword(code, kw))
    {
        return true;
    }
    is_match_head(code)
}

/// Indent the fresh line a newline just created.
///
/// `position.line` is the line the cursor landed on after the newline. Its
/// indentation is set from the nearest preceding non-blank line: that line's own
/// indent, plus one step when it [`opens_block`]. Returns no edit when the line is
/// already indented correctly, so a client that pre-applied its own auto-indent
/// (VS Code) sees a no-op while one that did not (Neovim) gets the indentation.
///
/// `step` is the indentation width — the client's `FormattingOptions.tab_size`, so
/// the result agrees with the same client's own offside auto-indent. The edit is
/// always spaces: Ridge source forbids tabs.
#[must_use]
pub fn on_type_newline_edits(text: &str, position: Position, step: usize) -> Vec<TextEdit> {
    let step = if step == 0 { INDENT } else { step };
    let starts = line_starts(text);
    let cur = position.line as usize;
    if cur >= starts.len() {
        return Vec::new();
    }

    let mut desired = 0usize;
    for i in (0..cur).rev() {
        let prev = line_content(text, &starts, i);
        if prev.trim().is_empty() {
            continue;
        }
        desired = leading_spaces(prev) + if opens_block(prev) { step } else { 0 };
        break;
    }

    let cur_line = line_content(text, &starts, cur);
    let existing_ws = cur_line
        .bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count();
    // A run of spaces (no tabs) already at the target width needs no edit.
    if existing_ws == desired
        && cur_line.as_bytes()[..existing_ws]
            .iter()
            .all(|b| *b == b' ')
    {
        return Vec::new();
    }

    // The existing whitespace is ASCII (spaces/tabs), so its UTF-16 length equals
    // its byte length.
    let end_col = u32::try_from(existing_ws).unwrap_or(u32::MAX);
    vec![TextEdit {
        range: Range {
            start: Position::new(position.line, 0),
            end: Position::new(position.line, end_col),
        },
        new_text: " ".repeat(desired),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, col: u32) -> Position {
        Position::new(line, col)
    }

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range {
            start: pos(sl, sc),
            end: pos(el, ec),
        }
    }

    // ── line diff ──────────────────────────────────────────────────────────

    #[test]
    fn hunks_isolate_two_separate_changes() {
        let orig = ["a", "X", "c", "d", "Y", "f"];
        let fmt = ["a", "x", "c", "d", "y", "f"];
        let hunks = line_hunks(&orig, &fmt);
        assert_eq!(
            hunks,
            vec![
                Hunk {
                    orig: (1, 2),
                    fmt: (1, 2)
                },
                Hunk {
                    orig: (4, 5),
                    fmt: (4, 5)
                },
            ]
        );
    }

    #[test]
    fn hunks_handle_insertion_and_deletion() {
        // One line removed, one added, at different places.
        let orig = ["keep", "remove", "tail"];
        let fmt = ["keep", "tail", "added"];
        let hunks = line_hunks(&orig, &fmt);
        // "remove" deleted (orig 1..2 -> fmt 1..1), "added" inserted at end
        // (orig 3..3 -> fmt 2..3).
        assert_eq!(hunks[0].orig, (1, 2));
        assert_eq!(hunks[0].fmt, (1, 1));
        assert_eq!(hunks.last().unwrap().orig, (3, 3));
    }

    #[test]
    fn range_edit_touches_only_the_selected_change() {
        // Two badly-indented lines; format fixes both, but the selection covers
        // only the first.
        let text = "fn a =\n        1\nfn b =\n        2\n";
        let formatted = "fn a =\n  1\nfn b =\n  2\n";
        let idx = LineIndex::new(text);
        // Select line 1 only (the body of `a`).
        let edits = range_format_edits(text, formatted, range(1, 0, 1, 0), &idx);
        assert_eq!(edits.len(), 1, "only the selected line is reformatted");
        assert_eq!(edits[0].new_text, "  1\n");
        assert_eq!(edits[0].range, range(1, 0, 2, 0));
    }

    #[test]
    fn range_edit_empty_when_selection_already_formatted() {
        let text = "fn a =\n  1\nfn b =\n        2\n";
        let formatted = "fn a =\n  1\nfn b =\n  2\n";
        let idx = LineIndex::new(text);
        // Select lines 0..1, which are already formatted.
        let edits = range_format_edits(text, formatted, range(0, 0, 1, 3), &idx);
        assert!(edits.is_empty());
    }

    #[test]
    fn range_edit_diffs_crlf_against_lf() {
        let text = "fn a =\r\n        1\r\n";
        let formatted = "fn a =\n  1\n";
        let idx = LineIndex::new(text);
        let edits = range_format_edits(text, formatted, range(1, 0, 1, 0), &idx);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "  1\n");
    }

    // ── opens_block ─────────────────────────────────────────────────────────

    #[test]
    fn opens_block_recognises_offside_openers() {
        assert!(opens_block("fn add x y ="));
        assert!(opens_block("  result <-"));
        assert!(opens_block("count ->"));
        assert!(opens_block("  if cond then"));
        assert!(opens_block("  else"));
        assert!(opens_block("try"));
        assert!(opens_block("let x = match y"));
        assert!(opens_block("fn add x y =   -- a comment"));
    }

    #[test]
    fn opens_block_ignores_non_openers() {
        assert!(!opens_block("x == y"));
        assert!(!opens_block("a != b"));
        assert!(!opens_block("a <= b"));
        assert!(!opens_block("a >= b"));
        assert!(!opens_block("  x + y"));
        assert!(!opens_block("orelse")); // not the keyword `else`
        assert!(!opens_block("result = match y => 1")); // inline arm, body on this line
        assert!(!opens_block(""));
    }

    // ── on_type_newline_edits ────────────────────────────────────────────────

    #[test]
    fn newline_after_opener_indents_one_step() {
        // Cursor on the blank line 1, just created under `fn add x y =`.
        let text = "fn add x y =\n\n";
        let edits = on_type_newline_edits(text, pos(1, 0), 2);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "  ");
        assert_eq!(edits[0].range, range(1, 0, 1, 0));
    }

    #[test]
    fn newline_keeps_previous_indent_without_opener() {
        // Under an indented statement that does not open a block, the new line
        // keeps the same indent.
        let text = "fn f =\n  doThing\n\n";
        let edits = on_type_newline_edits(text, pos(2, 0), 2);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "  ");
    }

    #[test]
    fn newline_nested_opener_adds_to_running_indent() {
        let text = "fn f =\n  if c then\n\n";
        let edits = on_type_newline_edits(text, pos(2, 0), 2);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "    "); // 2 (running) + 2 (then)
    }

    #[test]
    fn newline_noop_when_already_indented() {
        // A client that pre-applied the same indent gets no edit back.
        let text = "fn add x y =\n  \n";
        let edits = on_type_newline_edits(text, pos(1, 0), 2);
        assert!(edits.is_empty());
    }

    #[test]
    fn newline_replaces_wrong_indent() {
        // The blank line already has 4 spaces, but only 2 are wanted.
        let text = "fn f =\n  doThing\n    \n";
        let edits = on_type_newline_edits(text, pos(2, 0), 2);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "  ");
        assert_eq!(edits[0].range, range(2, 0, 2, 4));
    }

    #[test]
    fn newline_at_top_of_file_has_no_indent() {
        let text = "\nfn f =\n";
        let edits = on_type_newline_edits(text, pos(0, 0), 2);
        assert!(edits.is_empty()); // desired 0, already 0
    }
}
