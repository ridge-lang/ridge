//! Structural stdlib acceptance checks.
//!
//! This file asserts structural stdlib invariants. It does NOT
//! duplicate work already owned by:
//!
//! - `manifest_consistency.rs`   — bidirectional manifest / signature checks
//! - `test_ridge_smoke.rs`       — parse-clean `.test.ridge` files
//! - `crates/ridge-codegen-erl/tests/stdlib_map.rs::build_map_count_is_exactly_6`
//!   — path-A retired to exactly 6 `std.op.*` entries
//!
//! ## Checks and documented external commands
//!
//! | Test                        | Gate       |
//! |-----------------------------|------------|
//! | `required_modules_are_discovered_and_built` | Module coverage |
//! | `g5_path_b_dominance`       | Generated target coverage |
//! | `artefacts_count_matches_plan` | Source and suite counts |
//! | `dod_doc_link`              | Documents external lint/doc commands; runs neither |
//!
//! ## Generated-target architecture
//!
//! `crates/ridge-stdlib/build.rs` is the **canonical extractor**
//! for the path-B FFI lookup table.  The generated table is exposed via
//! `ridge_stdlib::ffi_targets::lookup`. `ridge-codegen-erl` now
//! depends on `ridge-stdlib` (regular dep) and adapts the returned
//! `StdlibFfiTarget` into `BridgeTarget::RidgeStdlibLocal` at the seam.
//!
//! The generated-target coverage assertion uses
//! `ridge_stdlib::codegen_ffi_targets::extract_all_stdlib_decls`, which is
//! the canonical reference implementation of the extraction logic and remains
//! the authoritative source for verifying path-B coverage.  Asserting that a
//! symbol appears in `extract_all_stdlib_decls` output is equivalent to
//! asserting it would be served by path B at runtime — the generated table IS
//! the output of that extraction.
//!
//! Note: `ridge-stdlib` intentionally has no dep on `ridge-codegen-erl`
//! (that direction would close a cycle).  The dep edge runs one way:
//! `ridge-codegen-erl → ridge-stdlib`.

// Integration tests are allowed to use expect/unwrap/panic freely.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::items_after_statements,
    clippy::assertions_on_constants,
    clippy::doc_markdown
)]

use std::path::Path;

use ridge_stdlib::build_driver::{build_all, discover};
use ridge_stdlib::codegen_ffi_targets::extract_all_stdlib_decls;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Locate the `stdlib/` directory relative to `CARGO_MANIFEST_DIR`.
fn stdlib_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib")
}

/// Recursively collect files matching a predicate under `dir`, in sorted order.
fn collect_files<F>(dir: &Path, pred: F) -> Vec<std::path::PathBuf>
where
    F: Fn(&Path) -> bool + Copy,
{
    let mut out = Vec::new();
    collect_files_impl(dir, pred, &mut out);
    out.sort();
    out
}

fn collect_files_impl<F>(dir: &Path, pred: F, out: &mut Vec<std::path::PathBuf>)
where
    F: Fn(&Path) -> bool + Copy,
{
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("dod.rs: could not read directory {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry
            .unwrap_or_else(|e| panic!("dod.rs: directory entry error in {}: {e}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            collect_files_impl(&path, pred, out);
        } else if pred(&path) {
            out.push(path);
        }
    }
}

// Module coverage is a property of build results, not a count of test names.

fn missing_modules<'a>(required: &[&'a str], observed: &[&str]) -> Vec<&'a str> {
    required
        .iter()
        .copied()
        .filter(|module| !observed.contains(module))
        .collect()
}

/// The modules covered by the module integration targets must be discovered
/// and pass the real build pipeline. Function behavior has separate Ridge tests.
#[test]
fn required_modules_are_discovered_and_built() {
    const REQUIRED: &[&str] = &[
        "std.actor",
        "std.bool",
        "std.cli",
        "std.crypto",
        "std.env",
        "std.float",
        "std.fs",
        "std.int",
        "std.io",
        "std.json",
        "std.list",
        "std.map",
        "std.net.http",
        "std.option",
        "std.proc",
        "std.random",
        "std.result",
        "std.set",
        "std.text",
        "std.time",
    ];
    let dir = stdlib_dir();
    let modules = discover(&dir);
    let discovered: Vec<&str> = modules.iter().map(|module| module.name.as_str()).collect();
    let missing = missing_modules(REQUIRED, &discovered);
    assert!(
        missing.is_empty(),
        "required modules not discovered: {missing:?}"
    );

    let summary = build_all(&dir).unwrap_or_else(|error| panic!("stdlib build failed: {error}"));
    let built: Vec<&str> = summary.modules_built.iter().map(String::as_str).collect();
    let missing = missing_modules(REQUIRED, &built);
    assert!(
        missing.is_empty(),
        "required modules not built: {missing:?}"
    );
}

#[test]
fn module_coverage_reports_missing_names() {
    let required = ["std.bool", "std.list"];
    assert!(missing_modules(&required, &["std.list", "std.bool", "std.extra"]).is_empty());
    assert_eq!(
        missing_modules(&required, &["std.bool", "std.extra"]),
        ["std.list"]
    );
    assert_eq!(missing_modules(&required, &[]), required);
}

// ── G5 / §11.2 — path-B dominance ────────────────────────────────────────────

/// Path-B is active for example symbols — no path-A entry consulted
/// for any example symbol except `std.op.*`.
///
/// Asserts that a curated set of stdlib symbols used by the four canonical
/// examples (`log_analyzer`, `url_shortener`, `game_of_life`, `rate_limiter`)
/// appear in `extract_all_stdlib_decls` output — i.e., they ARE covered by
/// path-B.  This directly proves that the generated `ffi_targets` lookup table
/// would serve these symbols via `BridgeTarget::RidgeStdlibLocal`, not via the
/// path-A static map.
///
/// `std.op.*` entries are intentionally omitted from this list — they are the
/// only permanent path-A entries (emitted by `ridge-lower::operators`)
/// and have no `.ridge` body or `@ffi` annotation.
///
/// ## Architecture note
///
/// `ridge-codegen-erl` depends on `ridge-stdlib` (not the
/// reverse).  `extract_all_stdlib_decls` is the canonical reference extraction
/// that `crates/ridge-stdlib/build.rs` uses to generate the lookup table.
/// Asserting that a symbol appears here is equivalent to asserting it would be
/// served by path B at runtime via `ridge_stdlib::ffi_targets::lookup`.
///
/// The generated target table is shared by the stdlib and codegen crates.
#[test]
fn g5_path_b_dominance() {
    // Curated set of example symbols — ~10 representative ones per the plan.
    // These cover all four examples and all relevant stdlib modules.
    const EXAMPLE_SYMBOLS: &[(&str, &str)] = &[
        // log_analyzer uses std.list.map, std.text.split, std.io.println
        ("std.list", "map"),
        ("std.text", "split"),
        ("std.io", "println"),
        // url_shortener uses std.map.fromList, std.option.withDefault
        ("std.map", "fromList"),
        ("std.option", "withDefault"),
        // game_of_life uses std.list.filter, std.time.now
        ("std.list", "filter"),
        ("std.time", "now"),
        // rate_limiter uses std.fs.lines, std.cli.args, std.random.int
        ("std.fs", "lines"),
        ("std.cli", "args"),
        ("std.random", "int"),
    ];

    let stdlib = stdlib_dir();
    let decls = extract_all_stdlib_decls(&stdlib)
        .unwrap_or_else(|e| panic!("G5 §11.2: extract_all_stdlib_decls failed: {e}"));

    for &(module, fn_name) in EXAMPLE_SYMBOLS {
        let found = decls
            .iter()
            .any(|d| d.ridge_module == module && d.ridge_fn == fn_name);
        assert!(
            found,
            "G5 §11.2: example symbol ({module}, {fn_name}) not found in path-B \
             extraction (extract_all_stdlib_decls). Path-B must cover this symbol \
             so it is served by RidgeStdlibLocal, not path-A. \
             This means the symbol is missing from the stdlib .ridge source or its \
             pub fn declaration was not recognized."
        );
    }
}

// ── §11.4 — artefact counts match plan ───────────────────────────────────────

/// Module `.ridge` files and paired `.test.ridge` BEAM suites under
/// `stdlib/`.
///
/// `net/http.ridge` and `net/http.test.ridge` each count toward their total.
/// `crypto`, `sql`, `query`, `data`, and `migrate` ship no `.test.ridge` (their
/// behaviour is covered by Rust-side tests), so the `.ridge` count runs five
/// ahead.
#[test]
fn artefacts_count_matches_plan() {
    const EXPECTED_RG: usize = 34;
    const EXPECTED_TEST_RG: usize = 21;

    let stdlib = stdlib_dir();

    // Collect plain .ridge files (not .test.ridge).
    let ridge_files: Vec<std::path::PathBuf> = collect_files(&stdlib, |p| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "ridge" {
            return false;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        // `codec.ridge` is the canonical declaration of the built-in
        // Encode/Decode classes — not a compiled module, so it is not one of
        // the EXPECTED_RG stdlib source files.
        if stem == "codec" {
            return false;
        }
        // Exclude .test.ridge: stem ends with ".test".
        !Path::new(stem)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("test"))
    });

    // Collect .test.ridge files.
    let test_ridge_files: Vec<std::path::PathBuf> = collect_files(&stdlib, |p| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "ridge" {
            return false;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        Path::new(stem)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("test"))
    });

    assert_eq!(
        ridge_files.len(),
        EXPECTED_RG,
        "§11.4: expected exactly {EXPECTED_RG} .ridge source files under stdlib/, \
         found {}.\n  files: {:#?}",
        ridge_files.len(),
        ridge_files
    );

    assert_eq!(
        test_ridge_files.len(),
        EXPECTED_TEST_RG,
        "§11.4: expected exactly {EXPECTED_TEST_RG} .test.ridge files under stdlib/, \
         found {}.\n  files: {:#?}",
        test_ridge_files.len(),
        test_ridge_files
    );
}

// ── G7, G8 — CI gates placeholder ────────────────────────────────────────────

/// Clippy and cargo-doc checks run outside this test.
///
/// This test is a documentation placeholder and does not run either command.
/// Build tooling must execute them directly and report their results:
///
/// - `cargo clippy -p ridge-stdlib --all-targets -- -D warnings`
/// - `cargo doc -p ridge-stdlib --no-deps`
///
/// External check failures must be reported by the build pipeline.
#[test]
fn dod_doc_link() {
    // Clippy and rustdoc run outside this test process.
    // This test is a structural placeholder so that `dod.rs` owns the gate
    // documentation and a test runner reports it as "passed" alongside the
    // other structural gates.
    let () = (); // no-op body
}
