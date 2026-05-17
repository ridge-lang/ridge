//! `ridge-fmt` — Source formatter for the Ridge language.
//!
//! # Entry point
//!
//! - [`format_source`] — format a Ridge source string; returns the formatted
//!   output or a [`FormatError`] if the input cannot be parsed.
//!
//! # Algorithm
//!
//! The formatter implements a *trivia-preserving round-trip* (§2.3, OQ-C007):
//!
//! 1. Parse with trivia preserved via
//!    [`ridge_parser::parse_module_with_trivia`].
//! 2. Walk the AST in source order, emitting normalised whitespace while
//!    re-inserting the trivia (line comments, blank lines) at their original
//!    attached positions.
//! 3. Re-emit the result as a `String`.
//!
//! See `crates/ridge-fmt/README.md` for the transitional notice (OQ-C007).

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
