//! Slow-CI lane: compiles stdlib `.ridge` and `.test.ridge` files together as a
//! single workspace member, invokes `ridge test`, parses the summary line,
//! and asserts the test count meets G4 (≥ 151 in slow-CI lane).
//!
//! Gated behind `#[cfg(feature = "stdlib-e2e")]` so per-PR `cargo test --all`
//! does not pay the multi-second BEAM compile + execute cost.
//!
//! ## First run
//!
//! `assert_cmd::Command::cargo_bin("ridge")` will trigger a rebuild of the
//! `ridge` binary on first run.  Allow up to 60 seconds for this.

#![cfg(feature = "stdlib-e2e")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

use assert_cmd::Command;

// ── Thresholds ────────────────────────────────────────────────────────────────

/// Floor for the number of stdlib tests that must run and pass. A sanity check
/// that the suite actually compiled and executed; kept below the suite size so
/// legitimate churn does not trip it, but a wholesale failure to compile does.
const MIN_PASSING: u64 = 200;

// ── Harness ─────────────────────────────────────────────────────────────��─────

/// Slow-CI lane: compile and execute all stdlib `.test.ridge` functions via
/// `ridge test`, assert the summary line satisfies the gate.
///
/// Algorithm:
///
/// 1. Locate the canonical stdlib directory via `env!("CARGO_MANIFEST_DIR")`.
/// 2. Materialise a temporary workspace holding only two TOML manifests;
///    the stdlib sources are read in-place via the absolute `src_root`.
/// 3. Invoke `ridge test` as a subprocess against the temp workspace.
/// 4. Require a successful exit and a complete summary with no failures or rejections.
#[test]
fn stdlib_e2e_runs_all_tests() {
    // ── 1. Compile + run the embedded stdlib's own test suite on BEAM ─────────
    // `ridge test --stdlib` unpacks the sources the compiler carries and compiles
    // them AS the standard library (permitting `@ffi`, taking the reconciled
    // types and base codec instances from source), then runs every `.test.ridge`
    // function in a fresh BEAM process. It is self-contained, so no on-disk
    // workspace fixture is needed. This exercises the whole stdlib self-compile
    // path — the modern reconciled data layer included.
    let output = Command::cargo_bin("ridge")
        .expect("ridge binary not found — run `cargo build -p ridge-cli` first")
        .arg("test")
        .arg("--stdlib")
        .output()
        .expect("failed to spawn ridge process");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert!(
        output.status.success(),
        "stdlib-e2e: ridge test exited with {}.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
        output.status
    );

    // ── 4. Parse the summary line and assert gate ─────────────────────────────
    // Expected format (per crates/ridge-cli/src/cmd/test.rs summary printer):
    // "Tests: <P> passed, <F> failed, <R> rejected (<T>ms)"
    let (passed, failed, rejected) = parse_summary(&stdout).unwrap_or_else(|| {
        panic!(
            "stdlib-e2e: could not parse summary line from ridge test output.\n\
             --- stdout ---\n{stdout}\n\
             --- stderr ---\n{stderr}"
        )
    });

    assert_eq!(
        failed, 0,
        "stdlib-e2e: {failed} test(s) failed.\n\
         --- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}"
    );

    assert_eq!(
        rejected, 0,
        "stdlib-e2e: {rejected} test(s) rejected.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );

    assert!(
        passed >= MIN_PASSING,
        "stdlib-e2e: expected >= {MIN_PASSING} passing tests, got {passed}.\n\
         --- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}"
    );
}

// ── Helper ────────────────────────────────────────────────────────────────────

/// Parse the `ridge test` summary line.
///
/// Requires the complete passed/failed/rejected partition and elapsed time.
fn parse_summary(output: &str) -> Option<(u64, u64, u64)> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Tests: ") {
            let (passed, rest) = rest.split_once(" passed, ")?;
            let (failed, rest) = rest.split_once(" failed, ")?;
            let (rejected, elapsed) = rest.split_once(" rejected (")?;
            elapsed.strip_suffix("ms)")?.parse::<u128>().ok()?;
            return Some((
                passed.parse().ok()?,
                failed.parse().ok()?,
                rejected.parse().ok()?,
            ));
        }
    }
    None
}

#[test]
fn summary_parser_preserves_rejection_counts() {
    assert_eq!(
        parse_summary("progress\nTests: 200 passed, 1 failed, 3 rejected (42ms)\n"),
        Some((200, 1, 3))
    );
}

#[test]
fn summary_parser_requires_the_complete_partition() {
    for output in [
        "Tests: 200 passed, 0 failed, 0 skipped (42ms)",
        "Tests: 200 passed, 0 failed",
        "Tests: 200 passed, 0 failed, 0 rejected (unknown)",
        "notice: no tests discovered",
    ] {
        assert_eq!(parse_summary(output), None, "accepted {output}");
    }
}
