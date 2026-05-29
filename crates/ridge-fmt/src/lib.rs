//! `ridge-fmt` — Source formatter for the Ridge language.
//!
//! # Entry point
//!
//! - [`format_source`] — format a Ridge source string; returns the formatted
//!   output or a [`FormatError`] if the input cannot be parsed.
//!
//! # Algorithm
//!
//! The formatter implements a *trivia-preserving round-trip* (§2.3):
//!
//! 1. Parse with trivia preserved via
//!    [`ridge_parser::parse_module_with_trivia`].
//! 2. Walk the AST in source order, emitting normalised whitespace while
//!    re-inserting the trivia (line comments, blank lines) at their original
//!    attached positions.
//! 3. Re-emit the result as a `String`.
//!
//! See `crates/ridge-fmt/README.md` for the transitional notice.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod printer;
pub mod rules;
pub mod trivia;

use thiserror::Error;

// ── FormatError ───────────────────────────────────────────────────────────────

/// Errors produced by the `ridge-fmt` formatter.
///
/// Error codes occupy the `C101`–`C199` namespace (§1.3 #3 of the Phase 8
/// plan).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FormatError {
    /// `C101` — the source could not be parsed.
    ///
    /// The formatter never silently corrupts a broken file.  When the parser
    /// reports errors, this variant is returned so the caller (the CLI layer,
    /// `ridge fmt`) can decide whether to emit a warning or exit non-zero.
    ///
    /// The contained string is a human-readable summary of the first parse
    /// error encountered.
    #[error("C101 FmtSourceUnparseable: {0}")]
    FmtSourceUnparseable(String),
}

impl FormatError {
    /// Return the stable error code string (e.g. `"C101"`).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::FmtSourceUnparseable(_) => "C101",
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Replace every tab character with two spaces throughout the source.
///
/// This runs before the lexer sees the source so that tab-indented files are
/// accepted.  The tab-expansion is uniform (every `\t` → `"  "`) rather than
/// tab-stop-aware, matching the plan's "tabs become two spaces each" rule.
fn expand_tabs(src: &str) -> String {
    if !src.contains('\t') {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len() * 2);
    for ch in src.chars() {
        if ch == '\t' {
            out.push_str("  ");
        } else {
            out.push(ch);
        }
    }
    out
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Rewrite legacy prefix-style test functions to the `@test` attribute form.
///
/// For each `pub fn test_*` function that does not already carry an
/// `@test` attribute, inserts `@test "<derived>"` on a new line immediately
/// above the `pub fn` line at the same indentation.  The derived name is the
/// function name with its `test_` prefix stripped.
///
/// The rewrite is **idempotent**: a function already carrying `@test` is left
/// untouched.  Everything else in the file — trivia, comments, other
/// declarations — is preserved verbatim.
///
/// # Errors
///
/// Returns [`FormatError::FmtSourceUnparseable`] (`C101`) when the source
/// fails to parse.
pub fn migrate_tests(src: &str) -> Result<String, FormatError> {
    use ridge_ast::{Attribute, Item, Visibility};

    // parse_module_with_trivia normalises CRLF internally and returns the
    // normalised source in `normalised_src`.  All span byte offsets produced
    // by the parser reference that normalised string, so we work against it
    // rather than the raw input.
    let parsed = ridge_parser::parse_module_with_trivia(src);

    if !parsed.result.errors.is_empty() {
        let msg = parsed
            .result
            .errors
            .first()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown parse error".to_string());
        return Err(FormatError::FmtSourceUnparseable(msg));
    }
    if !parsed.result.lex_errors.is_empty() {
        let msg = parsed
            .result
            .lex_errors
            .first()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown lex error".to_string());
        return Err(FormatError::FmtSourceUnparseable(msg));
    }

    // The normalised source is the base for all insertions.
    let normalised = &parsed.normalised_src;

    // Collect insertion points: (byte_offset_of_line_start, attribute_text).
    // Each entry describes inserting `@test "<name>"\n<indent>` at the
    // beginning of the line that holds `pub fn test_…`.
    let mut insertions: Vec<(usize, String)> = Vec::new();

    for item in &parsed.result.module.items {
        let Item::Fn(decl) = item else { continue };

        // Only `pub fn test_*` without an existing @test attribute.
        if decl.vis != Visibility::Pub {
            continue;
        }
        let fn_name = &decl.name.text;
        if !fn_name.starts_with("test_") {
            continue;
        }
        let already_has_test = decl
            .attrs
            .iter()
            .any(|a| matches!(a, Attribute::Test { .. }));
        if already_has_test {
            continue;
        }

        // `FnDecl.span.start` is the byte offset of the `fn` keyword in the
        // normalised source.  Walk backward to find the start of the line
        // that contains `pub fn …` — that is where we insert the attribute.
        let fn_offset = decl.span.start as usize;
        let line_start = find_line_start(normalised, fn_offset);

        // Capture the indentation of the `pub fn` line.
        let indent = leading_whitespace(&normalised[line_start..]).to_string();

        // Derive the test display name by stripping the `test_` prefix.
        let display_name = fn_name.strip_prefix("test_").unwrap_or(fn_name);

        let insertion = format!("@test \"{display_name}\"\n{indent}");
        insertions.push((line_start, insertion));
    }

    if insertions.is_empty() {
        return Ok(normalised.to_string());
    }

    // Apply insertions from last to first so earlier byte offsets stay valid.
    insertions.sort_by_key(|(offset, _)| *offset);
    insertions.reverse();

    let mut result = normalised.to_string();
    for (offset, text) in insertions {
        result.insert_str(offset, &text);
    }

    Ok(result)
}

/// Return the byte offset of the start of the line containing `offset`.
///
/// Scans backward from `offset` to find the preceding newline (or the
/// beginning of the string).
fn find_line_start(src: &str, offset: usize) -> usize {
    let bytes = src.as_bytes();
    // Start scanning from `offset - 1` to skip the character at `offset`
    // itself (which may be `fn`, not a newline).
    if offset == 0 {
        return 0;
    }
    let mut i = offset - 1;
    loop {
        if bytes[i] == b'\n' {
            return i + 1;
        }
        if i == 0 {
            return 0;
        }
        i -= 1;
    }
}

/// Return the leading whitespace prefix of a line (spaces and tabs).
fn leading_whitespace(line: &str) -> &str {
    let trimmed = line.trim_start_matches([' ', '\t']);
    &line[..line.len() - trimmed.len()]
}

/// Format a Ridge source string according to the standard style rules.
///
/// # Algorithm
///
/// 1. Pre-normalise tabs (→ 2 spaces per tab) so the parser does not reject
///    tab-indented input (§3.2 edge-case: "Mixed tabs / spaces in input:
///    re-emitted with two-space indentation").
/// 2. Parse with trivia via [`ridge_parser::parse_module_with_trivia`].
/// 3. If parse errors are present, return
///    [`FormatError::FmtSourceUnparseable`] — the formatter never silently
///    corrupts a broken file.
/// 4. Walk the AST in source order, emitting normalised whitespace and
///    re-inserting trivia at attached positions.
/// 5. Return the formatted `String`.
///
/// # Errors
///
/// Returns [`FormatError::FmtSourceUnparseable`] (`C101`) when the source
/// fails to parse (either lex errors or parse errors).
pub fn format_source(src: &str) -> Result<String, FormatError> {
    // Pre-normalise tabs before the lexer sees the source so that tab-
    // indented files are not rejected with a `LexError::TabForbidden`.
    // Each tab in leading whitespace becomes two spaces (§3.2 algorithm).
    let src = &expand_tabs(src);

    let parsed = ridge_parser::parse_module_with_trivia(src);

    // Fail closed on unparseable input.
    if !parsed.result.errors.is_empty() {
        let msg = parsed
            .result
            .errors
            .first()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown parse error".to_string());
        return Err(FormatError::FmtSourceUnparseable(msg));
    }
    if !parsed.result.lex_errors.is_empty() {
        let msg = parsed
            .result
            .lex_errors
            .first()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown lex error".to_string());
        return Err(FormatError::FmtSourceUnparseable(msg));
    }

    let formatted = printer::print(&parsed);
    Ok(formatted)
}
