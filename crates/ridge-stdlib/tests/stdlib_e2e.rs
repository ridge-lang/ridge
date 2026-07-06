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
/// that the suite actually compiled and executed (the `failed == 0` assertion is
/// the real regression catch); kept well below the current count (~235) so
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
/// 4. Parse the summary line and assert `passed >= MIN_PASSING` and `failed == 0`.
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

    // ── 4. Parse the summary line and assert gate ─────────────────────────────
    // Expected format (per crates/ridge-cli/src/cmd/test.rs summary printer):
    // "Tests: <P> passed, <F> failed, <S> skipped (<T>ms)"
    let (passed, failed) = parse_summary(&stdout).unwrap_or_else(|| {
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
/// Looks for `Tests: <P> passed, <F> failed` anywhere in the output.
/// Returns `Some((passed, failed))` or `None` if no matching line is found.
fn parse_summary(output: &str) -> Option<(u64, u64)> {
    for line in output.lines() {
        let line = line.trim();
        // Match: "Tests: N passed, M failed"
        if let Some(rest) = line.strip_prefix("Tests: ") {
            // rest: "N passed, M failed, K skipped (Tms)"
            let parts: Vec<&str> = rest.split(',').collect();
            if parts.len() >= 2 {
                let passed = parts[0].split_whitespace().next()?.parse().ok()?;
                let failed = parts[1].split_whitespace().next()?.parse().ok()?;
                return Some((passed, failed));
            }
        }
    }
    None
}
