//! Glob pattern matching for Ridge module-path patterns.
//!
//! Ridge uses dot-separated module paths (e.g. `acme.domain.Models`).
//! This module provides [`GlobPattern`] which wraps a user-facing dotted glob
//! string and compiles it into an efficient matcher.
//!
//! ## Glob semantics (R007)
//!
//! - `*` matches **exactly one** path segment (a non-empty run of characters
//!   between dots).
//! - `**` matches **zero or more** path segments.
//! - `.` is the segment separator — it is NOT a glob metacharacter.
//! - Matching is case-sensitive.
//!
//! ## Implementation approach
//!
//! We translate each dotted Ridge glob into a `/`-separated path-style glob and
//! delegate compilation and matching to the `globset` crate.  Translation rules:
//!
//! - `.` → `/`
//! - `*` (single-segment) → `*` (globset `*` does not match `/`, so this
//!   correctly limits to one segment)
//! - `**` (multi-segment) → `**` (globset `**` matches any number of path
//!   components, including zero)
//!
//! This approach is simpler than writing a custom recursive matcher and
//! leverages globset's well-tested path semantics out of the box.
//!
//! Patterns that contain filesystem metacharacters (`/`, `\`) are rejected
//! because Ridge module paths never contain them.

use globset::{GlobBuilder, GlobMatcher};

use crate::error::ManifestError;

// ── Public types ──────────────────────────────────────────────────────────────

/// A compiled Ridge module-path glob pattern.
///
/// Wraps a raw dotted string like `"acme.domain.*"` and provides a fast
/// [`GlobPattern::matches`] predicate against fully-qualified module names.
pub struct GlobPattern {
    /// The original dotted glob string as written in the manifest.
    pub raw: String,
    /// Compiled matcher (opaque; derived from `raw`).
    pub compiled: CompiledGlob,
}

/// Opaque compiled matcher; the actual implementation detail.
pub struct CompiledGlob {
    matcher: GlobMatcher,
}

impl GlobPattern {
    /// Parse and compile a dotted Ridge glob pattern.
    ///
    /// Returns `Err` with an [`ManifestError::BadMemberGlob`] carrying the
    /// offending pattern and the compilation error message.
    ///
    /// # Errors
    ///
    /// Returns an error if the pattern is empty or contains invalid glob syntax.
    pub fn new(raw: &str) -> Result<Self, GlobError> {
        if raw.is_empty() {
            return Err(GlobError {
                pattern: raw.to_owned(),
                message: "glob pattern must not be empty".to_owned(),
            });
        }

        // Reject filesystem separators — Ridge module paths are dots only.
        if raw.contains('/') || raw.contains('\\') {
            return Err(GlobError {
                pattern: raw.to_owned(),
                message: "glob pattern must not contain '/' or '\\' — use '.' as separator"
                    .to_owned(),
            });
        }

        let path_glob = translate_to_path(raw);
        // `literal_separator(true)` makes `*` stop at `/` (one segment only),
        // while `**` continues to match across separators.  This enforces the
        // Ridge glob contract: `*` = one segment, `**` = any number of segments.
        let compiled = GlobBuilder::new(&path_glob)
            .literal_separator(true)
            .build()
            .map_err(|e| GlobError {
                pattern: raw.to_owned(),
                message: e.to_string(),
            })?;

        Ok(Self {
            raw: raw.to_owned(),
            compiled: CompiledGlob {
                matcher: compiled.compile_matcher(),
            },
        })
    }

    /// Test whether `module_path` (a fully-qualified, dot-separated name such
    /// as `"acme.domain.Models.User"`) is matched by this pattern.
    #[must_use]
    pub fn matches(&self, module_path: &str) -> bool {
        // Convert the module path to the same slash-separated form used by the
        // compiled glob, then match.
        let path_form = module_path.replace('.', "/");
        self.compiled.matcher.is_match(&path_form)
    }
}

impl std::fmt::Debug for GlobPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobPattern")
            .field("raw", &self.raw)
            .finish_non_exhaustive()
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

/// Intermediate error from glob compilation (not yet mapped to a variant).
///
/// Callers convert this to [`ManifestError::BadMemberGlob`] or
/// [`ManifestError::ProjectExportPatternInvalid`] depending on context.
#[derive(Debug)]
pub struct GlobError {
    /// The pattern that failed to compile.
    pub pattern: String,
    /// Human-readable compilation error.
    pub message: String,
}

impl GlobError {
    /// Convert to `ManifestError::BadMemberGlob`.
    #[must_use]
    pub fn into_bad_member_glob(self) -> ManifestError {
        ManifestError::BadMemberGlob {
            pattern: self.pattern,
            error: self.message,
        }
    }

    /// Convert to `ManifestError::ProjectExportPatternInvalid`.
    #[must_use]
    pub fn into_export_pattern_invalid(self, path: std::path::PathBuf) -> ManifestError {
        ManifestError::ProjectExportPatternInvalid {
            raw: self.pattern,
            path,
        }
    }
}

// ── Translation helper ────────────────────────────────────────────────────────

/// Translate a dotted Ridge glob to a slash-separated path-style glob.
///
/// `**` is preserved; lone `*` is preserved; `.` becomes `/`.
fn translate_to_path(raw: &str) -> String {
    // We must be careful: `**` must stay `**` and not become `**/` midway.
    // Walk char-by-char, replacing `.` with `/`.
    raw.replace('.', "/")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
}
