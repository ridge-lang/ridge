//! Slow-CI lane: compiles stdlib `.rg` and `.test.rg` files together as a
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
use std::path::Path;

// ── Thresholds (updated per plan phase) ─────────────────────────��────────────

/// Minimum passing-test count for G-C (Phase C gate — G4 final gate, >= 151 tests).
/// Phase C added capability-bearing tests for io, fs, env, cli, time, random, proc, json.
const MIN_PASSING: u64 = 151;

// ── Harness ─────────────────────────────────────────────────────────────��─────

/// Slow-CI lane: compile and execute all stdlib `.test.rg` functions via
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
    // ── 1. Locate the stdlib source directory ──────────────────────────────��─
    let stdlib_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib");
    assert!(
        stdlib_dir.is_dir(),
        "stdlib directory not found at {}",
        stdlib_dir.display()
    );

    // Normalise path separators for embedding in TOML (TOML is text; on Windows
    // backslashes would be treated as escape sequences inside a quoted string).
    let stdlib_str = stdlib_dir.to_string_lossy().replace('\\', "/");

    // ── 2. Create a per-run tempdir holding only the two TOML manifests ──────
    // The `.rg` and `.test.rg` source files stay in their canonical on-disk
    // location; the driver reads them directly via the absolute `src_root`.
    // Per OQ-C024 / D171 / D175: absolute-path indirection, no tempdir copy.
    let td = tempfile::TempDir::new().expect("create tempdir for stdlib-e2e workspace");
    let ws_root = td.path();

    // Workspace manifest.
    let ws_toml = "[workspace]\nname = \"stdlib-e2e\"\nversion = \"0.1.0\"\nmembers = [\"std\"]\n";
    std::fs::write(ws_root.join("ridge.toml"), ws_toml).expect("write workspace ridge.toml");

    // Project manifest: src_root points at the real stdlib directory.
    std::fs::create_dir_all(ws_root.join("std")).expect("create std/ project dir");
    let proj_toml = format!(
        "[project]\nname = \"std\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.src]\nroot = \"{stdlib_str}\"\n\n[project.exports]\npublic = [\"std.**\"]\n"
    );
    std::fs::write(ws_root.join("std").join("ridge.toml"), &proj_toml)
        .expect("write project ridge.toml");

    // ── 3. Invoke `ridge test` against the fixture workspace ─────────────────
    let output = Command::cargo_bin("ridge")
        .expect("ridge binary not found — run `cargo build -p ridge-cli` first")
        .arg("test")
        .current_dir(ws_root)
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
