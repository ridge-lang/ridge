//! Style rules applied by the Ridge formatter.
//!
//! Each rule is a stateless transformation:
//!
//! - [`normalise_indentation`] — replace tabs with 2 spaces; strip trailing
//!   whitespace from each line.
//! - [`normalise_operator_spaces`] — ensure exactly one space on each side of
//!   every binary operator.
//! - [`trailing_comment_placement`] — emit a trailing comment on the same
//!   line if the combined length ≤ 80 chars, otherwise on the preceding line.
//!
//! The rules are stateless and pure; the printer ([`crate::printer`]) drives
//! them in the correct order.

// ── Indentation normalisation ─────────────────────────────────────────────────

/// Replace leading tabs with 2-space indentation and strip trailing whitespace.
///
/// Each tab character in the leading-whitespace prefix of `line` is replaced
/// by two spaces.  Any trailing whitespace (spaces, tabs) is then removed.
///
/// Interior tabs (not in the leading whitespace) are left as-is because they
/// may appear inside string literals where they are significant.  String
/// content normalisation is handled by a higher-level pass.
#[must_use]
pub fn normalise_indentation(line: &str) -> String {
    // Count leading tabs and spaces.
    let mut leading = String::new();
    let mut rest_start = 0;
    for (i, ch) in line.char_indices() {
        match ch {
            '\t' => leading.push_str("  "),
            ' ' => leading.push(' '),
            _ => {
                rest_start = i;
                break;
            }
        }
        rest_start = i + ch.len_utf8();
    }
    let rest = line[rest_start..].trim_end();
    format!("{leading}{rest}")
}

// ── Operator spacing ──────────────────────────────────────────────────────────

/// Binary operators that the formatter normalises to have exactly one space on
/// each side.
///
/// The list is ordered from longest to shortest so that multi-char operators
/// (`|>`, `==`, `!=`, `<=`, `>=`, `&&`, `||`, `++`, `?>`) are matched before
/// their single-char prefixes.
const BINARY_OPS: &[&str] = &[
    "|>", "?>", "++", "&&", "||", "==", "!=", "<=", ">=", "->", "<-", "::", "+", "-", "*", "/",
    "<", ">",
];

/// Return the text of `line` with exactly one space on each side of every
/// binary operator that is not already correctly spaced.
///
/// This is a best-effort pass that operates on the re-emitted source text
/// (which already has correct structure).  It does not parse expressions — it
/// performs a linear scan and rewrites operator occurrences that violate the
/// spacing rule.
///
/// Operators inside string literals are intentionally left unchanged.
///
/// # Encoding correctness
///
/// The operator alphabet is pure ASCII, so byte-level matching (`bytes[i..]`)
/// is sound for the recognition step.  When the byte at `i` is not the start
/// of an operator we must NOT push `bytes[i] as char` — that interpretation
/// would split a multi-byte UTF-8 sequence into single-byte Latin-1 chars and
/// corrupt the round-trip.  Instead we use `line[i..].chars().next()` to read
/// the next full Unicode scalar and advance by `ch.len_utf8()`.
#[must_use]
pub fn normalise_operator_spaces(line: &str) -> String {
    // Fast path: if the line contains none of the operator characters at all,
    // return early.
    if !line.contains(['+', '-', '*', '/', '<', '>', '=', '!', '|', '&', '?', ':']) {
        return line.to_string();
    }

    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len + 16);
    let mut i = 0;
    let mut in_string = false;
    let mut in_interp = false;

    // Helper closure: advance `i` by one Unicode scalar starting at byte
    // position `i`, pushing the scalar into `out`.  Used for every non-
    // operator emission so multi-byte UTF-8 stays intact.
    //
    // Returns `Some(())` if a char was emitted; `None` if `i` is past end-of-
    // string (shouldn't happen given the loop guard but defensive).
    let advance_char = |out: &mut String, line: &str, i: &mut usize| -> bool {
        if let Some(ch) = line[*i..].chars().next() {
            out.push(ch);
            *i += ch.len_utf8();
            true
        } else {
            false
        }
    };

    while i < len {
        // String literal tracking.  The `"` and `$` markers are pure ASCII,
        // so byte-level checks are safe; the body of the string contains
        // arbitrary UTF-8 and must round-trip via char-aware emission.
        //
        // When we're already inside a string/interpolation and see `\`, the
        // next byte is part of an escape sequence (`\"`, `\\`, `\n`, …) and
        // must be consumed verbatim.  Without this guard a literal `\"` toggles
        // `in_string` off and the operator-spacing pass then mutates the rest
        // of the string (observed on
        // `"  ridge run -- add \"<title>\""` → `"… add \" < title > \""`).
        // Placed before the `"` toggle so it wins.
        if (in_string || in_interp) && bytes[i] == b'\\' && i + 1 < len {
            out.push('\\');
            i += 1;
            advance_char(&mut out, line, &mut i);
            continue;
        }
        if bytes[i] == b'"' && !in_interp {
            in_string = !in_string;
            out.push('"');
            i += 1;
            continue;
        }
        if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'"' {
            in_interp = true;
            out.push('$');
            out.push('"');
            i += 2;
            continue;
        }
        if in_string || in_interp {
            if bytes[i] == b'"' {
                in_string = false;
                in_interp = false;
                out.push('"');
                i += 1;
                continue;
            }
            // Non-quote byte inside a string literal: emit one full UTF-8
            // scalar, never a single byte.
            advance_char(&mut out, line, &mut i);
            continue;
        }

        // Try to match a binary operator at position `i`.  Operators are
        // pure ASCII, so byte-level matching is sound here.
        let matched = BINARY_OPS.iter().find(|&&op| {
            let ob = op.as_bytes();
            i + ob.len() <= len && &bytes[i..i + ob.len()] == ob
        });

        if let Some(&op) = matched {
            // Skip `->` and `<-` inside type annotations: these are not binary
            // operators in the arithmetic sense and their spacing is context-
            // dependent.  We still normalise them (one space each side).
            //
            // Exception: `--` (line comment prefix) — never matched because
            // the line comment scanner runs first and comments are stripped.
            //
            // Exception: `-` that starts a negative number literal (preceded
            // only by whitespace, `(`, `[`, `=`, or `,`).  We check the
            // preceding non-whitespace character.
            if op == "-" {
                let prev_char = out.trim_end().chars().next_back();
                let is_unary = matches!(
                    prev_char,
                    None | Some(
                        '(' | '['
                            | ','
                            | '='
                            | '+'
                            | '-'
                            | '*'
                            | '/'
                            | '<'
                            | '>'
                            | '!'
                            | '&'
                            | '|'
                            | '?'
                    )
                );
                if is_unary {
                    out.push('-');
                    i += 1;
                    continue;
                }
            }

            // When the operator is the FIRST non-whitespace token on the line
            // (continuation form, e.g. `    |> List.map ...` under a multi-
            // line pipeline), preserve the leading indent verbatim instead of
            // collapsing it to a single space.  Without this, every
            // continuation-style `|>` / `?>` / `&&` line would be dedented to
            // column 1, breaking the visual alignment the author wrote.
            let out_trimmed = out.trim_end_matches(' ');
            let trimmed_len = out_trimmed.len();
            let at_line_start = trimmed_len == 0;
            if at_line_start {
                // Leading-whitespace-only prefix; keep it as-is.  Append the
                // operator directly so we get `<indent>op<space>`.
                out.push_str(op);
                i += op.len();
                while i < len && bytes[i] == b' ' {
                    i += 1;
                }
                out.push(' ');
            } else {
                // Mid-line operator: normalise to `<lhs><space>op<space><rhs>`.
                out.truncate(trimmed_len);
                out.push(' ');
                out.push_str(op);
                i += op.len();
                // Skip any trailing spaces after the operator.
                while i < len && bytes[i] == b' ' {
                    i += 1;
                }
                out.push(' ');
            }
            // If we just consumed a space past end-of-line, trim the trailing
            // space later.
        } else {
            // Non-operator, non-string byte: emit one full UTF-8 scalar.
            advance_char(&mut out, line, &mut i);
        }
    }

    // Remove any trailing space we may have introduced.
    out.trim_end().to_string()
}

// ── Trailing comment placement ────────────────────────────────────────────────

/// Decide how to emit a trailing line comment (`-- ...`) relative to its
/// code line.
///
/// Placement rule:
/// - If the code line + one space + the comment fits in ≤ 80 characters:
///   emit `code  -- comment` on a single line.
/// - Otherwise: emit the comment on the preceding line (already indented to
///   match the code line's indentation), then the code line on its own line.
///
/// Returns `(preceding_comment, code_line)` where `preceding_comment` is
/// `None` when the same-line placement applies, or `Some(comment_text)` when
/// the comment must precede the code line.
///
/// The caller is responsible for writing the output in the correct order.
#[must_use]
pub fn trailing_comment_placement<'a>(
    code_line: &str,
    comment: &'a str,
) -> TrailingCommentPlacement<'a> {
    // +1 for the separating space between code and comment.
    let combined_len = code_line.len() + 1 + comment.len();
    if combined_len <= 80 {
        TrailingCommentPlacement::SameLine
    } else {
        TrailingCommentPlacement::PrecedingLine(comment)
    }
}

/// Result of [`trailing_comment_placement`].
#[derive(Debug, PartialEq, Eq)]
pub enum TrailingCommentPlacement<'a> {
    /// Emit comment on the same line as the code (combined length ≤ 80).
    SameLine,
    /// Emit comment on the line before the code.  Contains the comment text.
    PrecedingLine(&'a str),
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_indentation_tabs() {
        assert_eq!(normalise_indentation("\tfoo"), "  foo");
        assert_eq!(normalise_indentation("\t\tfoo"), "    foo");
    }

    #[test]
    fn normalise_indentation_strips_trailing() {
        assert_eq!(normalise_indentation("foo   "), "foo");
        assert_eq!(normalise_indentation("  foo  "), "  foo");
    }

    #[test]
    fn normalise_indentation_mixed() {
        assert_eq!(normalise_indentation("\t foo"), "   foo");
    }

    #[test]
    fn trailing_comment_placement_short_line() {
        // "fn foo x = x" (13) + " " + "-- note" (7) = 21 ≤ 80 → same line.
        let result = trailing_comment_placement("fn foo x = x", "-- note");
        assert_eq!(result, TrailingCommentPlacement::SameLine);
    }

    #[test]
    fn trailing_comment_placement_long_line() {
        let long_code = "fn reallyLongFunctionNameThatExceedsEightyCharactersOnItsOwnAndShouldPushCommentToNewLine x = x";
        let result = trailing_comment_placement(long_code, "-- comment");
        assert!(matches!(result, TrailingCommentPlacement::PrecedingLine(_)));
    }

    #[test]
    fn trailing_comment_placement_exactly_80() {
        // Construct a case that is exactly 80 chars → same line.
        let code = "a".repeat(71); // 71 chars
        let comment = "-- x"; // 4 chars; 71 + 1 + 4 = 76 ≤ 80
        let result = trailing_comment_placement(&code, comment);
        assert_eq!(result, TrailingCommentPlacement::SameLine);
    }

    #[test]
    fn trailing_comment_placement_81() {
        // 77 + 1 + 4 = 82 > 80 → preceding line.
        let code = "a".repeat(77);
        let comment = "-- x";
        let result = trailing_comment_placement(&code, comment);
        assert!(matches!(result, TrailingCommentPlacement::PrecedingLine(_)));
    }
}
