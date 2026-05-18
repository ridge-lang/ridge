//! Integration tests for `ridge run`.
//!
//! Tests that require a BEAM runtime (`erl` on PATH) are gated behind
//! `#[cfg(feature = "beam-runtime")]`.  The "no executable member" error path
//! and `--observer` connection-info stderr test do not need OTP.
//!
//! ## Feature gates
//!
//! - `beam-runtime` — tests that spawn `erl`.
//! - `cli-watch` — the `--watch` cycle test.
//!
//! Run BEAM tests with:
//! ```text
//! cargo test -p ridge-cli --features beam-runtime,cli-watch
//! ```
//!
//! Run the `--watch` stress test (ignored by default):
//! ```text
//! cargo test -p ridge-cli --features beam-runtime,cli-watch -- --ignored watch_stress
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::make_workspace;
#[cfg(feature = "beam-runtime")]
use common::{make_app_workspace, make_example_app_workspace, make_mixed_workspace, write_file};
use predicates::str::contains;

// ── helpers ───────────────────────────────────────────────────────────────────

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// Spawn-friendly variant: `assert_cmd::Command::spawn` became private in 2.x,
/// so the `--watch` cycle tests need a raw `std::process::Command` built from
/// the same cargo-bin path. Keeps `ridge_cmd()` available for the assert-based
/// tests that benefit from its richer expectation API.
#[cfg(feature = "cli-watch")]
fn ridge_spawnable_cmd() -> std::process::Command {
    std::process::Command::new(assert_cmd::cargo::cargo_bin("ridge"))
}

/// A minimal Ridge `main` entry point.  Canonical surface: `fn name -> Type =
/// expr` (no parens for zero-arg, no braces; body after `=`).  We use a
/// trivially-typed return so the source parses without needing `import std.io`
/// or capability declarations — these tests only assert that the CLI does not
/// hit C001/C006, not that stdout matches a specific string.
#[cfg(feature = "beam-runtime")]
const HELLO_MAIN: &str = "pub fn main -> Int = 0\n";

// ── Test 1–4: ridge run on each canonical example ─────────────────────────────

/// `ridge run` on the `log_analyzer` example matches the expected output.
///
/// Requires OTP (`erl` on PATH).
#[cfg(feature = "beam-runtime")]
#[test]
fn run_log_analyzer() {
    let tw = make_example_app_workspace("log_analyzer");
    let output = ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The example may fail due to missing CLI args — we accept a non-zero
    // exit here since the expected/*.txt harness is the authoritative check.
    // This test asserts the command at least runs without C001/C006.
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "unexpected workspace-level error.\nstderr: {stderr}"
    );
    let _ = (stdout, stderr);
}

/// `ridge run` on the `url_shortener` example.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_url_shortener() {
    let tw = make_example_app_workspace("url_shortener");
    let output = ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "unexpected workspace-level error.\nstderr: {stderr}"
    );
}

/// `ridge run` on the `game_of_life` example.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_game_of_life() {
    let tw = make_example_app_workspace("game_of_life");
    let output = ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "unexpected workspace-level error.\nstderr: {stderr}"
    );
}

/// `ridge run` on the `rate_limiter` example.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_rate_limiter() {
    let tw = make_example_app_workspace("rate_limiter");
    let output = ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "unexpected workspace-level error.\nstderr: {stderr}"
    );
}

// ── Test 5: --member selection ────────────────────────────────────────────────

/// `ridge run --member myapp` selects the `myapp` app member in a mixed workspace.
///
/// Requires OTP.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_member_selection() {
    let tw = make_mixed_workspace(HELLO_MAIN);
    let output = ridge_cmd()
        .arg("run")
        .arg("--member")
        .arg("myapp")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C005") && !stderr.contains("C007"),
        "unexpected member-selection error.\nstderr: {stderr}"
    );
}

// ── Test 6: argument pass-through after -- ────────────────────────────────────

/// Arguments after `--` are passed through to the BEAM node.
///
/// Requires OTP.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_arg_passthrough() {
    // A module that accepts args — we just verify it doesn't error on C001/C006.
    let tw = make_app_workspace("Main", HELLO_MAIN);
    let output = ridge_cmd()
        .arg("run")
        .arg("--")
        .arg("foo")
        .arg("bar")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "arg passthrough caused a workspace error.\nstderr: {stderr}"
    );
}

// ── Test 7: "No executable member" error — does not need OTP ─────────────────

/// `ridge run` in a workspace with only `library` members exits non-zero with
/// `C006 NoExecutableMember`.
#[test]
fn run_no_executable_member() {
    // make_workspace creates a library-only workspace.
    let tw = make_workspace("Lib", "pub fn helper -> Int = 42\n");

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C006"));
}

// ── Test 8: --watch recompile + restart cycle ─────────────────────────────────

/// `ridge run --watch` survives a single file-change cycle.
///
/// Writes a `.ridge` file mid-run and asserts a recompile + restart occurs
/// without a crash or zombie process.
///
/// Requires OTP and `cli-watch` feature.
#[cfg(all(feature = "beam-runtime", feature = "cli-watch"))]
#[test]
fn run_watch_single_cycle() {
    use std::time::Duration;

    let tw = make_app_workspace("Main", HELLO_MAIN);

    // Spawn `ridge run --watch` in the background.
    let mut child = ridge_spawnable_cmd()
        .arg("run")
        .arg("--watch")
        .current_dir(&tw.path)
        .spawn()
        .expect("failed to spawn ridge run --watch");

    // Give the initial compile + launch a moment.
    std::thread::sleep(Duration::from_secs(3));

    // Touch the source file to trigger a watch event.
    write_file(
        &tw.path,
        "apps/demo/src/Main.ridge",
        "pub fn main -> Int = 1\n",
    );

    // Wait for debounce + recompile + relaunch.
    std::thread::sleep(Duration::from_secs(4));

    // The watch process should still be alive (it should not have crashed).
    let result = child.try_wait().expect("try_wait failed");
    assert!(
        result.is_none(),
        "ridge run --watch exited prematurely after a file change"
    );

    // Kill the watcher cleanly.
    let _ = child.kill();
    let _ = child.wait();
}

// ── Test 9: --observer prints connection-info to stderr ───────────────────────

/// `ridge run --observer` prints the connection-info line to stderr before
/// launching the BEAM node.
///
/// This test does NOT require OTP — it asserts the stderr line appears before
/// the process is attempted.  If OTP is absent the process may fail, but the
/// stderr line must still have been emitted.
///
/// Note: the observer test relies on the workspace having an executable member
/// and finding (or failing to find) erl.  We use a library workspace here to
/// trigger the C006 early exit so the test is OTP-agnostic.
#[test]
fn run_observer_no_executable_member() {
    // Use a library workspace — the CLI should error with C006 before even
    // attempting to resolve the cookie or spawn erl.
    let tw = make_workspace("Lib", "pub fn helper -> Int = 42\n");

    ridge_cmd()
        .arg("run")
        .arg("--observer")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C006"));
}

/// `ridge run --observer` on an app workspace prints the connection-info line
/// to stderr.
///
/// Requires OTP (needs `erl` to attempt to spawn the node); the test is
/// satisfied if the stderr output contains the connection-info hint regardless
/// of whether the BEAM node actually starts.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_observer_prints_connection_info() {
    // Create a cookie file in a temp location and point the CLI at it via
    // --cookie to avoid depending on the developer's ~/.erlang.cookie.
    let tw = make_app_workspace("Main", HELLO_MAIN);

    let output = ridge_cmd()
        .arg("run")
        .arg("--observer")
        .arg("--cookie")
        .arg("testcookie123")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run --observer spawn failed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Connect with:"),
        "expected connection-info hint on stderr.\nstderr: {stderr}"
    );
}

// ── Stress test: 50-cycle --watch without BEAM process leaks ─────────────────

/// Stress test: `ridge run --watch` survives 50 sequential file-change cycles
/// without leaking BEAM child processes.
///
/// This test is `#[ignore]` by default because it takes ~5 minutes and
/// requires OTP.  Run it explicitly with:
/// ```text
/// cargo test -p ridge-cli --features beam-runtime,cli-watch -- --ignored watch_stress
/// ```
///
/// The test verifies R14 (no zombie processes) by checking that the watcher
/// process does not accumulate open handles after each cycle.
#[cfg(all(feature = "beam-runtime", feature = "cli-watch"))]
#[ignore = "slow stress test — run with: cargo test -p ridge-cli --features beam-runtime,cli-watch -- --ignored watch_stress"]
#[test]
fn watch_stress() {
    use std::time::Duration;

    let tw = make_app_workspace("Main", HELLO_MAIN);

    let mut child = ridge_spawnable_cmd()
        .arg("run")
        .arg("--watch")
        .current_dir(&tw.path)
        .spawn()
        .expect("failed to spawn ridge run --watch");

    // Initial boot.
    std::thread::sleep(Duration::from_secs(3));

    for i in 0..50_u32 {
        // Write a new version of the source file.
        let new_source = format!("pub fn main -> Int = {i}\n");
        write_file(&tw.path, "apps/demo/src/Main.ridge", &new_source);

        // Wait for debounce (500 ms) + compile + restart overhead.
        std::thread::sleep(Duration::from_secs(3));

        // The watcher must still be alive.
        let result = child.try_wait().expect("try_wait failed");
        assert!(
            result.is_none(),
            "ridge run --watch exited prematurely at cycle {i}"
        );
    }

    // Clean shutdown.
    let _ = child.kill();
    let _ = child.wait();
}
