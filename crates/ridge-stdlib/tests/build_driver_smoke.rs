//! T4/T5 smoke test — stdlib build pipeline produces a clean summary.
//!
//! This test exercises `build_all` against the real `stdlib/` directory.
//! With T5 landed, the directory contains five tier-1 `.rg` modules; the
//! test asserts `Ok(_)` and delegates module-specific assertions to the
//! per-module test files (`int_test.rs`, `float_test.rs`, etc.).

// Integration tests are allowed to use expect/unwrap/panic freely.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ridge_stdlib::build_driver::build_all;

/// Stdlib build completes without error.
///
/// After T5, the `stdlib/` directory contains five tier-1 modules, so
/// `modules_built` will be non-empty.  We assert only that `build_all`
/// returns `Ok` — per-module content checks live in the dedicated test files.
#[test]
fn smoke_stdlib_builds_cleanly() {
    let stdlib_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib");

    let result = build_all(&stdlib_dir);

    // If the build fails, surface the full error message so it is visible
    // in `cargo test` output.
    result.unwrap_or_else(|e| panic!("build_all failed: {e}"));
}
