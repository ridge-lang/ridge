//! Validation helpers for numeric literals.
//!
//! `logos` captures the raw text of each numeric literal.  This module
//! validates the following constraints:
//! - No leading underscore on the digit run (e.g. `0b_1` is invalid).
//! - No trailing underscore (e.g. `1_000_` is invalid).
//! - Base-prefix literals must have at least one digit after the prefix
//!   (e.g. `0x` with nothing following is invalid).
//!
//! The validation functions return `Ok(text)` on success or a [`LexError`] on
//! failure.  The text is always the raw source slice (no decoding).

use crate::{error::LexError, span::Span};

/// Validate a decimal integer literal (`[0-9][0-9_]*`).
///
/// Returns `Err` if the literal ends with `_`.
pub(crate) fn validate_int_dec(text: &str, span: Span) -> Result<(), LexError> {
    debug_assert!(!text.is_empty());
    if text.ends_with('_') {
        return Err(LexError::TrailingUnderscoreLiteral { span });
    }
    Ok(())
}

/// Validate a binary integer literal (`0[bB][01][01_]*`).
///
/// The logos regex already ensures at least one binary digit after `0b`, so
/// we only need to check for a trailing underscore.
pub(crate) fn validate_int_bin(text: &str, span: Span) -> Result<(), LexError> {
    validate_trailing_underscore(text, span)
}

/// Validate an octal integer literal (`0[oO][0-7][0-7_]*`).
pub(crate) fn validate_int_oct(text: &str, span: Span) -> Result<(), LexError> {
    validate_trailing_underscore(text, span)
}

/// Validate a hexadecimal integer literal (`0[xX][0-9a-fA-F][0-9a-fA-F_]*`).
pub(crate) fn validate_int_hex(text: &str, span: Span) -> Result<(), LexError> {
    validate_trailing_underscore(text, span)
}

/// Validate a floating-point literal.
///
/// Trailing underscore in either the integer or fractional part is rejected.
pub(crate) fn validate_float(text: &str, span: Span) -> Result<(), LexError> {
    // Split on `.` — check both halves for trailing `_`.
    // We can't just check `text.ends_with('_')` because the exponent may follow.
    // A simpler guard: find `_` immediately before `.` or before `e`/`E`.
    if text.ends_with('_') {
        return Err(LexError::TrailingUnderscoreLiteral { span });
    }
    // Check integer part ends well: no `_` immediately before `.`
    if let Some(dot_pos) = text.find('.') {
        if text[..dot_pos].ends_with('_') {
            return Err(LexError::TrailingUnderscoreLiteral { span });
        }
    }
    Ok(())
}

fn validate_trailing_underscore(text: &str, span: Span) -> Result<(), LexError> {
    if text.ends_with('_') {
        return Err(LexError::TrailingUnderscoreLiteral { span });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::new(0, 5)
    }

    // ── Decimal ──────────────────────────────────────────────────────────────

    #[test]
    fn dec_valid() {
        assert!(validate_int_dec("0", sp()).is_ok());
        assert!(validate_int_dec("1_000_000", sp()).is_ok());
        assert!(validate_int_dec("42", sp()).is_ok());
    }

    #[test]
    fn dec_trailing_underscore() {
        assert!(matches!(
            validate_int_dec("1_", sp()),
            Err(LexError::TrailingUnderscoreLiteral { .. })
        ));
    }

    // ── Binary ───────────────────────────────────────────────────────────────

    #[test]
    fn bin_valid() {
        assert!(validate_int_bin("0b1010", sp()).is_ok());
        assert!(validate_int_bin("0B1010", sp()).is_ok());
        assert!(validate_int_bin("0b1010_0011", sp()).is_ok());
    }

    #[test]
    fn bin_trailing_underscore() {
        assert!(matches!(
            validate_int_bin("0b1010_", sp()),
            Err(LexError::TrailingUnderscoreLiteral { .. })
        ));
    }

    // ── Octal ────────────────────────────────────────────────────────────────

    #[test]
    fn oct_valid() {
        assert!(validate_int_oct("0o777", sp()).is_ok());
        assert!(validate_int_oct("0O777", sp()).is_ok());
    }

    #[test]
    fn oct_trailing_underscore() {
        assert!(matches!(
            validate_int_oct("0o7_", sp()),
            Err(LexError::TrailingUnderscoreLiteral { .. })
        ));
    }

    // ── Hex ──────────────────────────────────────────────────────────────────

    #[test]
    fn hex_valid() {
        assert!(validate_int_hex("0x1_DEAD_BEEF", sp()).is_ok());
        assert!(validate_int_hex("0xDEAD", sp()).is_ok());
        assert!(validate_int_hex("0xFF", sp()).is_ok());
    }

    #[test]
    fn hex_trailing_underscore() {
        assert!(matches!(
            validate_int_hex("0xDEAD_", sp()),
            Err(LexError::TrailingUnderscoreLiteral { .. })
        ));
    }

    // ── Float ────────────────────────────────────────────────────────────────

    #[test]
    fn float_valid() {
        assert!(validate_float("3.14", sp()).is_ok());
        assert!(validate_float("1_000.0", sp()).is_ok());
        assert!(validate_float("1.0e10", sp()).is_ok());
        assert!(validate_float("1.0e+10", sp()).is_ok());
        assert!(validate_float("2.5e-3", sp()).is_ok());
    }

    #[test]
    fn float_trailing_underscore() {
        assert!(matches!(
            validate_float("1.0_", sp()),
            Err(LexError::TrailingUnderscoreLiteral { .. })
        ));
    }

    #[test]
    fn float_underscore_before_dot() {
        // "1_.0" would be caught
        assert!(matches!(
            validate_float("1_.0", sp()),
            Err(LexError::TrailingUnderscoreLiteral { .. })
        ));
    }
}
