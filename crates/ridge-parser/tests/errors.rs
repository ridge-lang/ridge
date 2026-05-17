//! Error-recovery fixture tests (T12, §6 T12 `DoD`).
//!
//! Each `.rg` file under `tests/fixtures/errors/` starts with one or more
//! `-- expect: PXXX` header lines.  This harness:
//!
//! 1. Enumerates all `.rg` files in the fixture directory.
//! 2. Parses the expected codes from `-- expect:` header lines.
//! 3. Calls `parse_source` on the file content.
//! 4. Asserts `errors` is non-empty.
//! 5. Asserts every expected code appears at least once in `errors`.
//! 6. Asserts that every error's span is NOT `Span::point(0)` — recovery
//!    must not collapse spans to the sentinel position.
//!
//! ## Multi-error regression
//!
//! `multi_three_errors.rg` has three `-- expect: P001` lines.  This harness
//! asserts that `errors.len() == 3` for that file specifically — confirming
//! that error recovery does not produce fewer (abort-early) or more (duplication)
//! errors than expected.
//!
//! ## Variants intentionally not covered by fixtures
//!
//! - `P014 EmptyBlock`: unreachable from real Ridge source.  The error requires
//!   an `INDENT` token immediately followed by a `DEDENT` token, but the Ridge
//!   lexer never emits this token sequence for any valid source input (blank
//!   lines inside a block produce `Newline`, not `Indent+Dedent`).  `P014` is
//!   only reachable through manually-constructed token streams (as in the unit
//!   tests in `block.rs`).
//!
//! - `P999 InternalLayoutInvariantViolated`: by design unreachable from user
//!   source.  `P999` signals a lexer bug, not a user-authored error.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::path::PathBuf;

use ridge_parser::parse_source;

/// Fixture directory relative to the `ridge-parser` crate root.
const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/errors");

/// The filename prefix for the multi-error regression fixture.
const MULTI_ERROR_FIXTURE: &str = "multi_three_errors";

/// Expected error count for the multi-error regression fixture.
const MULTI_ERROR_COUNT: usize = 3;

#[test]
fn all_error_fixtures_pass() {
    let dir = PathBuf::from(FIXTURE_DIR);
    assert!(
        dir.is_dir(),
        "fixture directory does not exist: {}",
        dir.display()
    );

    let mut fixture_count = 0usize;
    let mut failures: Vec<String> = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("failed to read fixture directory")
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rg"))
        .collect();

    // Sort for deterministic order in failure messages.
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let file_name = path
            .file_name()
            .expect("fixture path has no filename")
            .to_string_lossy()
            .to_string();
        let stem = path
            .file_stem()
            .expect("fixture path has no stem")
            .to_string_lossy()
            .to_string();

        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));

        let expected_codes = parse_expect_headers(&src);
        if expected_codes.is_empty() {
            failures.push(format!(
                "{file_name}: no `-- expect: PXXX` header lines found"
            ));
            continue;
        }

        fixture_count += 1;

        let result = parse_source(&src);

        // ── 1. At least one error ───────────────────────────────────────────
        if result.errors.is_empty() {
            failures.push(format!(
                "{file_name}: expected errors but got none (expected codes: {expected_codes:?})"
            ));
            continue;
        }

        // ── 2. Every expected code appears ──────────────────────────────────
        for code in &expected_codes {
            let found = result.errors.iter().any(|e| e.code() == code.as_str());
            if !found {
                let actual: Vec<_> = result
                    .errors
                    .iter()
                    .map(ridge_parser::ParseError::code)
                    .collect();
                failures.push(format!(
                    "{file_name}: expected code {code} not found; actual codes: {actual:?}"
                ));
            }
        }

        // ── 3. No span collapsed to Span::point(0) ──────────────────────────
        for err in &result.errors {
            let span = err.span();
            // The plan's assertion: NOT a point at position 0.
            // Span::point(0) has start=0 AND end=0.
            if span.start == 0 && span.end == 0 {
                failures.push(format!(
                    "{file_name}: error {:?} has Span::point(0) — recovery collapsed span",
                    err.code()
                ));
            }
        }

        // ── 4. Multi-error regression ────────────────────────────────────────
        if stem == MULTI_ERROR_FIXTURE {
            let count = result.errors.len();
            if count != MULTI_ERROR_COUNT {
                failures.push(format!(
                    "{file_name}: expected exactly {MULTI_ERROR_COUNT} errors, got {count} ({:?})",
                    result
                        .errors
                        .iter()
                        .map(ridge_parser::ParseError::code)
                        .collect::<Vec<_>>()
                ));
            }
        }
    }

    // ── Minimum fixture count check ──────────────────────────────────────────
    assert!(
        fixture_count >= 15,
        "DoD requires ≥ 15 fixture files; found {fixture_count}"
    );

    // ── Report all failures at once ──────────────────────────────────────────
    if !failures.is_empty() {
        let msg = failures.join("\n  ");
        panic!("fixture test failures:\n  {msg}");
    }
}

/// Parse `-- expect: PXXX` header lines from the top of a fixture source file.
///
/// Lines are scanned from the start; scanning stops at the first line that is
/// not a line comment (i.e. does not start with `--`).
///
/// Returns a `Vec` of code strings like `["P001", "P002"]`.
fn parse_expect_headers(src: &str) -> Vec<String> {
    let mut codes = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("--") {
            // Non-comment line: stop scanning headers.
            break;
        }
        // Look for `-- expect: PXXX`
        let after_dashes = trimmed.trim_start_matches('-').trim();
        if let Some(rest) = after_dashes.strip_prefix("expect:") {
            let code = rest.trim().to_uppercase();
            if !code.is_empty() {
                codes.push(code);
            }
        }
    }
    codes
}

#[cfg(test)]
mod unit {
    use super::parse_expect_headers;

    #[test]
    fn parse_expect_single() {
        let src = "-- expect: P001\nfn f = 1\n";
        let codes = parse_expect_headers(src);
        assert_eq!(codes, vec!["P001"]);
    }

    #[test]
    fn parse_expect_multiple() {
        let src = "-- expect: P001\n-- expect: P002\nfn f = 1\n";
        let codes = parse_expect_headers(src);
        assert_eq!(codes, vec!["P001", "P002"]);
    }

    #[test]
    fn parse_expect_stops_at_non_comment() {
        let src = "-- expect: P001\nfn f = 1\n-- expect: P002 (not seen)\n";
        let codes = parse_expect_headers(src);
        assert_eq!(codes, vec!["P001"]);
    }

    #[test]
    fn parse_expect_skips_non_expect_comments() {
        let src = "-- expect: P001\n-- This is a regular comment\n-- expect: P005\nfn f = 1\n";
        let codes = parse_expect_headers(src);
        assert_eq!(codes, vec!["P001", "P005"]);
    }

    #[test]
    fn parse_expect_empty() {
        let src = "fn f = 1\n";
        let codes = parse_expect_headers(src);
        assert!(codes.is_empty());
    }
}
