//! Track-B `.test.ridge` smoke test.
//!
//! Asserts that every `<module>.test.ridge` file under `crates/ridge-stdlib/stdlib/`
//! exists (exactly 18 of them — one per stdlib module, per §10 file-count audit)
//! and that each file **parses without errors**.
//!
//! Full execution (compile + run via BEAM child process) is the job of
//! `ridge test` (Phase 8 T9, shipped); this smoke test only guards the
//! parse-clean invariant via the public single-file `ridge_parser::parse_source`
//! entry point.
//!
//! ## T-codes used
//!
//! `T201 TestRgCountMismatch` — file-count assertion.
//! `T201 TestRgParseFailed`   — per-file parse assertion (matches manifest test style).

// Integration tests are allowed to use expect/unwrap/panic freely.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use std::path::Path;

use ridge_parser::parse_source;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Expected number of `.test.ridge` files — one per stdlib module.
///
/// From the §10 file-count audit (plan line 949):
/// "18 module `.ridge` files + 18 `.test.ridge` files = 36 source files".
const EXPECTED_TEST_RG_COUNT: usize = 18;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Locate the `stdlib/` directory relative to `CARGO_MANIFEST_DIR`.
fn stdlib_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib")
}

/// Recursively collect every `*.test.ridge` file under `dir`, in lexicographic order.
///
/// Returns absolute paths.  Panics if the directory cannot be read.
fn collect_test_rg_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    collect_recursive(dir, &mut paths);
    paths.sort();
    paths
}

fn collect_recursive(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "T201 TestRgParseFailed: could not read directory {}: {e}",
            dir.display()
        )
    });
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| {
            panic!(
                "T201 TestRgParseFailed: directory entry error in {}: {e}",
                dir.display()
            )
        });
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, out);
        } else if path.extension().is_some_and(|e| e == "ridge") {
            // Only include files whose stem ends with ".test"
            // (i.e. the full filename is "<module>.test.ridge").
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if std::path::Path::new(stem)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("test"))
            {
                out.push(path);
            }
        }
    }
}

// ── Test 1: file count ────────────────────────────────────────────────────────

/// Asserts that exactly `EXPECTED_TEST_RG_COUNT` `.test.ridge` files exist under
/// `stdlib/`.
///
/// A count mismatch means a module was added without its companion `.test.ridge`
/// (or vice versa), failing the §10 audit-table invariant.
///
/// Failure message: `T201 TestRgCountMismatch`.
#[test]
fn test_rg_file_count_is_18() {
    let stdlib = stdlib_dir();
    let files = collect_test_rg_files(&stdlib);

    assert_eq!(
        files.len(),
        EXPECTED_TEST_RG_COUNT,
        "T201 TestRgCountMismatch {{ expected: {}, found: {} }}\n  files: {:#?}",
        EXPECTED_TEST_RG_COUNT,
        files.len(),
        files
    );
}

// ── Test 2: parse — all 18 files parse without errors ────────────────────────

/// For each `.test.ridge` file: read it and call `ridge_parser::parse_source`.
/// Asserts that both `errors` and `lex_errors` are empty.
///
/// Failure message: `T201 TestRgParseFailed`.
#[test]
fn test_rg_files_parse_cleanly() {
    let stdlib = stdlib_dir();
    let files = collect_test_rg_files(&stdlib);

    // Sanity: if collect returns zero files, something is wrong with the test
    // setup rather than the source files.  Surface it early.
    assert!(
        !files.is_empty(),
        "T201 TestRgParseFailed {{ reason: \"no .test.ridge files found under {}\" }}",
        stdlib.display()
    );

    for path in &files {
        let rel = path
            .strip_prefix(&stdlib)
            .unwrap_or(path)
            .display()
            .to_string();

        let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("T201 TestRgParseFailed {{ file: {rel:?}, reason: \"could not read: {e}\" }}")
        });

        let result = parse_source(&src);

        assert!(
            result.lex_errors.is_empty(),
            "T201 TestRgParseFailed {{ file: {:?}, reason: \"lex errors: {:?}\" }}",
            rel,
            result.lex_errors
        );

        assert!(
            result.errors.is_empty(),
            "T201 TestRgParseFailed {{ file: {:?}, reason: \"parse errors: {:?}\" }}",
            rel,
            result.errors
        );
    }
}
