//! The formatter's emission pass.
//!
//! [`print`] takes a [`ridge_parser::ParseResultWithTrivia`] and produces
//! a normalised Ridge source string.
//!
//! # Strategy (§2.3)
//!
//! The trivia-preserving round-trip works in three phases:
//!
//! 1. **Line normalisation** — apply [`crate::rules`] to every line of the
//!    CRLF-normalised source (tabs → 2 spaces, trailing whitespace stripped,
//!    operator spacing).
//! 2. **Blank-line normalisation** — use the AST's top-level item spans to
//!    find declaration boundaries; enforce exactly one blank line between
//!    consecutive non-import top-level declarations and zero blank lines
//!    between consecutive import statements at the file head.
//! 3. **Comment re-attachment** — line comments that were on the same line as
//!    code are re-attached per the trailing-comment placement rule
//!    (same-line if ≤ 80 chars, else preceding line).
//!
//! The printer does NOT rewrite expression syntax, does NOT reorder imports,
//! and does NOT reflow long lines.  See `README.md` for the transitional
//! notice.

use ridge_ast::Item;
use ridge_lexer::LineMap;
use ridge_parser::{ParseResultWithTrivia, Trivia};

use crate::rules::{normalise_indentation, trailing_comment_placement, TrailingCommentPlacement};

/// Emit a formatted version of the source described by `parsed`.
///
/// Called by [`crate::format_source`] after validating that `parsed` has no
/// errors.
#[must_use]
pub fn print(parsed: &ParseResultWithTrivia) -> String {
    let src = &parsed.normalised_src;
    let line_map = LineMap::new(src);

    // Collect raw lines from the normalised source.
    let raw_lines: Vec<&str> = src.split('\n').collect();
    let line_count = raw_lines.len();

    // Phase 1a: apply indentation normalisation (tabs → 2 spaces, strip
    // trailing whitespace) to every line.
    let mut lines: Vec<String> = raw_lines.iter().map(|l| normalise_indentation(l)).collect();

    // Phase 1b: for each line that has a trailing comment, extract the comment
    // and leave only the code part in `lines[i]`.
    //
    // `trailing_comments[i]` holds the raw comment text (e.g. `"-- note"`) if
    // line `i` has a trailing comment.
    let mut trailing_comments: Vec<Option<String>> = vec![None; line_count];

    for trivia_item in &parsed.trivia {
        if let Trivia::LineComment {
            line, col, text, ..
        } = trivia_item
        {
            let li = *line as usize;
            // A "trailing" comment is one that has CODE before the `--` on
            // the same source line.  `col > 0` alone misclassifies indented
            // full-line comments (`        -- ...`) as trailing — the printer
            // would then strip the comment from the line and store it as
            // trailing-text, leaving the line blank, which the blank-line
            // normaliser later removes, silently dropping the comment.
            //
            // Determine "code present before `--`" by checking that the
            // raw source line has any non-whitespace character in positions
            // [0, col).
            if li >= line_count {
                continue;
            }
            let has_code_before = raw_lines[li]
                .as_bytes()
                .iter()
                .take(*col as usize)
                .any(|b| !b.is_ascii_whitespace());
            if has_code_before {
                // Trailing comment: strip it from the normalised line.
                let normalised_line = &lines[li];
                if let Some(comment_start) = find_line_comment_in(normalised_line) {
                    let code_part = normalised_line[..comment_start].trim_end().to_string();
                    trailing_comments[li] = Some(text.clone());
                    lines[li] = code_part;
                }
            }
        }
    }

    // Phase 1c: apply operator spacing to non-full-comment, non-blank lines.
    // Full-comment lines (those where the line is `<ws>*--...`) and doc-
    // comment lines (`---` markers and their block bodies) are left untouched.
    //
    // A "full comment" is any `--`-comment whose `col` equals the leading
    // whitespace count of the source line (i.e. nothing but whitespace
    // precedes the `--`).  The previous heuristic (`col == 0`) misclassified
    // indented full-line comments as trailing — and trailing-attachment then
    // mutated their source line, sometimes dropping the comment entirely.
    let full_comment_lines: std::collections::HashSet<usize> = parsed
        .trivia
        .iter()
        .filter_map(|t| {
            if let Trivia::LineComment { line, col, .. } = t {
                let li = *line as usize;
                if li >= line_count {
                    return None;
                }
                let has_code_before = raw_lines[li]
                    .as_bytes()
                    .iter()
                    .take(*col as usize)
                    .any(|b| !b.is_ascii_whitespace());
                if has_code_before {
                    None
                } else {
                    Some(li)
                }
            } else {
                None
            }
        })
        .collect();

    // Mark lines that lie inside a `---…---` doc-comment block.  Operator
    // spacing must not run on the body of a doc block, otherwise prose like
    // `Token-bucket` becomes `Token - bucket` and the file is not idempotent
    // under repeated `ridge fmt` passes.
    let mut in_doc_block: Vec<bool> = vec![false; line_count];
    {
        let mut depth = 0u32;
        for (i, line) in raw_lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let is_doc_marker = trimmed == "---" || trimmed.starts_with("--- ");
            if is_doc_marker {
                // The marker line itself is excluded; toggle depth around it.
                in_doc_block[i] = false;
                if depth == 0 {
                    depth = 1;
                } else {
                    depth = 0;
                }
            } else if depth > 0 {
                in_doc_block[i] = true;
            }
        }
    }

    for (i, line) in lines.iter_mut().enumerate() {
        let is_full_comment = full_comment_lines.contains(&i);
        let is_blank = line.trim().is_empty();
        let is_doc = line.trim_start().starts_with("---");
        if !is_full_comment && !is_blank && !is_doc && !in_doc_block[i] {
            *line = crate::rules::normalise_operator_spaces(line);
        }
    }

    // Phase 2: blank-line normalisation around top-level declarations.
    //
    // Strategy: for each pair of consecutive items, the "inter-item region"
    // is every line whose index is strictly between the last line of item `i`
    // and the first line of item `i+1`.
    //
    // We compute item_start_lines via the AST span.start (reliable).
    // We compute item_end_lines as `item_start_lines[i+1] - 1` rather than
    // from span.end (which can point past trailing newlines into the gap).
    //
    // `line_col` returns 1-based numbers; convert to 0-based by subtracting 1.

    let item_start_lines: Vec<usize> = parsed
        .result
        .module
        .items
        .iter()
        .map(|item| {
            let span = item_span(item);
            let (line1, _) = line_map.line_col(span.start);
            (line1 as usize).saturating_sub(1) // 0-based
        })
        .collect();

    let item_is_import: Vec<bool> = parsed
        .result
        .module
        .items
        .iter()
        .map(|item| matches!(item, Item::Import(_)))
        .collect();

    // Mark lines to remove or where to inject a blank.
    let mut line_removed: Vec<bool> = vec![false; line_count];
    let mut inject_blank_after: Vec<bool> = vec![false; line_count];

    let n_items = item_start_lines.len();
    for i in 0..n_items.saturating_sub(1) {
        let next_start = item_start_lines[i + 1]; // 0-based

        // The inter-item region: all lines strictly between item i's first
        // line and item i+1's first line.  Item i's last code line is
        // somewhere in [item_start_lines[i], next_start - 1].  We look at
        // all lines in [item_start_lines[i] + 1, next_start - 1] to find
        // blank lines in the gap.  (Lines in item i's body are non-blank
        // so they won't be touched.)
        let gap_start = item_start_lines[i] + 1;
        let gap_end = next_start; // exclusive

        // When the gap between two top-level items contains ANY line-comment,
        // the printer is hands-off about blank lines in that gap.  The user's
        // blank-line layout around a standalone or leading comment is
        // intentional and the "keep only the first blank" rule was silently
        // eating the blank between a comment and the following declaration
        // (or, for leading-doc comments above an item, the blank between
        // the previous item and the comment block).  Doc-comment
        // markers (`---`) are NOT line-comments and don't trigger this
        // branch.
        let has_comment_in_gap = (gap_start..gap_end).any(|l| {
            if l >= line_count {
                return false;
            }
            let t = lines[l].trim_start();
            t.starts_with("--") && !t.starts_with("---")
        });

        if has_comment_in_gap {
            continue;
        }

        let both_imports = item_is_import[i] && item_is_import[i + 1];

        if both_imports {
            // Remove ALL blank lines in the gap.
            for line_no in gap_start..gap_end {
                if line_no < line_count && lines[line_no].trim().is_empty() {
                    line_removed[line_no] = true;
                }
            }
        } else {
            // The separator is the run of blank lines immediately before item
            // i+1 — the lines after item i's last content line. Blank lines
            // interior to item i's body come before that content line and are
            // left untouched: the printer is hands-off about intra-body layout,
            // the same way it is for a gap that contains a comment. (An earlier
            // rule collected every blank line between the two item *starts* and
            // kept only the first; a body's own blank lines then counted as gap,
            // so it ate the real separator and jammed the next declaration
            // against the body.)
            let last_content = (item_start_lines[i]..next_start)
                .rev()
                .find(|&l| l < line_count && !lines[l].trim().is_empty())
                .unwrap_or(item_start_lines[i]);
            let separator_blanks: Vec<usize> = ((last_content + 1)..next_start)
                .filter(|&l| l < line_count && lines[l].trim().is_empty())
                .collect();

            if separator_blanks.is_empty() {
                // No blank line separating the two — inject exactly one.
                if last_content < line_count {
                    inject_blank_after[last_content] = true;
                }
            } else {
                // Keep the first separator blank, remove any extras.
                for &line_no in &separator_blanks[1..] {
                    line_removed[line_no] = true;
                }
            }
        }
    }

    // Phase 3: emit — assemble the output string.
    let mut out = String::with_capacity(src.len() + src.len() / 8);

    for i in 0..line_count {
        if line_removed[i] {
            continue;
        }

        emit_line(&mut out, i, &lines[i], &trailing_comments);

        if inject_blank_after[i] {
            out.push('\n');
        }
    }

    // Ensure the file ends with exactly one newline.
    let trimmed = out.trim_end_matches('\n');
    let mut result = trimmed.to_string();
    result.push('\n');
    result
}

/// Emit a single source line to the output buffer.
///
/// Applies trailing comment re-attachment: if the line has a trailing
/// comment, decide whether to put it on the same line or the preceding line.
fn emit_line(out: &mut String, idx: usize, line: &str, trailing_comments: &[Option<String>]) {
    let comment = trailing_comments.get(idx).and_then(|c| c.as_deref());

    if let Some(comment_text) = comment {
        match trailing_comment_placement(line, comment_text) {
            TrailingCommentPlacement::SameLine => {
                out.push_str(line);
                out.push_str("  ");
                out.push_str(comment_text);
                out.push('\n');
            }
            TrailingCommentPlacement::PrecedingLine(c) => {
                let indent = leading_spaces(line);
                out.push_str(indent);
                out.push_str(c);
                out.push('\n');
                out.push_str(line);
                out.push('\n');
            }
        }
    } else {
        out.push_str(line);
        out.push('\n');
    }
}

/// Return the leading whitespace prefix of `line` (spaces only, after
/// tab-expansion has already occurred).
fn leading_spaces(line: &str) -> &str {
    let trimmed = line.trim_start();
    &line[..line.len() - trimmed.len()]
}

/// Find the byte position of a `--` line comment in a normalised (tab-
/// expanded) line, returning `None` if no line comment is present.
///
/// Skips `---` doc-comment markers and `--` inside string literals.
fn find_line_comment_in(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_string = false;

    while i < len {
        match bytes[i] {
            b'"' => {
                in_string = !in_string;
                i += 1;
            }
            b'-' if !in_string && i + 1 < len && bytes[i + 1] == b'-' => {
                if i + 2 < len && bytes[i + 2] == b'-' {
                    return None; // `---` doc marker
                }
                return Some(i);
            }
            b'\\' if in_string => {
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

/// Return the [`ridge_ast::Span`] of a top-level `Item`.
fn item_span(item: &Item) -> ridge_ast::Span {
    match item {
        Item::Import(d) => d.span,
        Item::Const(d) => d.span,
        Item::Type(d) => d.span,
        Item::Fn(d) => d.span,
        Item::Actor(d) => d.span,
        // Typeclass items are not formatted in this release; return their span
        // for correct blank-line injection in the surrounding module layout.
        Item::ClassDecl(d) => d.span,
        Item::InstanceDecl(d) => d.span,
    }
}
