//! Glob-pattern matching for member and export patterns.
//!
//! Moved from the `ridge-resolve` copy of this module, which carried the only
//! tests either copy had. The pattern code itself was byte-identical between
//! the two; only the tests were not.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_manifest::GlobPattern;
// ── positive matches ──────────────────────────────────────────────────────

#[test]
fn single_star_matches_one_segment() {
    let pat = GlobPattern::new("acme.domain.*").unwrap();
    assert!(pat.matches("acme.domain.Foo"));
}

#[test]
fn double_star_matches_multiple_segments() {
    let pat = GlobPattern::new("acme.domain.**").unwrap();
    assert!(pat.matches("acme.domain.Sub.Foo"));
}

#[test]
fn double_star_matches_single_segment_too() {
    // ** = zero or more segments; must match the direct child too.
    let pat = GlobPattern::new("acme.domain.**").unwrap();
    assert!(pat.matches("acme.domain.Foo"));
}

#[test]
fn double_star_matches_zero_segments() {
    // ** matches zero segments — the prefix itself.
    let pat = GlobPattern::new("acme.**").unwrap();
    assert!(pat.matches("acme.Foo"));
    assert!(pat.matches("acme.foo.bar.baz"));
}

#[test]
fn literal_match_exact() {
    let pat = GlobPattern::new("Exact.Name").unwrap();
    assert!(pat.matches("Exact.Name"));
}

#[test]
fn mixed_star_and_double_star() {
    // "acme.*.models.**" should match "acme.domain.models.User.Extra"
    let pat = GlobPattern::new("acme.*.models.**").unwrap();
    assert!(pat.matches("acme.domain.models.User"));
    assert!(pat.matches("acme.domain.models.User.Extra"));
}

// ── negative matches ──────────────────────────────────────────────────────

#[test]
fn single_star_does_not_match_two_segments() {
    let pat = GlobPattern::new("acme.domain.*").unwrap();
    // Sub.Foo is two segments beyond "acme.domain" — must not match.
    assert!(!pat.matches("acme.domain.Sub.Foo"));
}

#[test]
fn single_star_does_not_match_different_prefix() {
    let pat = GlobPattern::new("acme.domain.*").unwrap();
    assert!(!pat.matches("acme.infra.Foo"));
}

#[test]
fn literal_does_not_match_child() {
    let pat = GlobPattern::new("Exact.Name").unwrap();
    assert!(!pat.matches("Exact.Name.Sub"));
}

#[test]
fn case_sensitive_mismatch() {
    let pat = GlobPattern::new("Models.*").unwrap();
    // lowercase 'models' must not match upper-case 'Models'.
    assert!(!pat.matches("models.foo"));
}

// ── error cases ───────────────────────────────────────────────────────────

#[test]
fn empty_pattern_is_err() {
    assert!(GlobPattern::new("").is_err());
}

#[test]
fn unclosed_character_class_is_err() {
    // "[abc" is invalid glob syntax — no closing ']'.
    assert!(GlobPattern::new("libs/[abc").is_err());
}

#[test]
fn slash_in_pattern_is_err() {
    // Ridge patterns must not contain filesystem separators.
    assert!(GlobPattern::new("libs/apps").is_err());
}

// ── GlobError helper conversions ──────────────────────────────────────────

#[test]
fn glob_error_into_bad_member_glob() {
    let err = GlobPattern::new("").unwrap_err();
    let m_err = err.into_bad_member_glob();
    assert_eq!(m_err.code(), "M005");
}

#[test]
fn glob_error_into_export_pattern_invalid() {
    let err = GlobPattern::new("").unwrap_err();
    let m_err = err.into_export_pattern_invalid("/tmp/ridge.toml".into());
    assert_eq!(m_err.code(), "M014");
}
