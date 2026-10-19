// Integration tests are allowed to use expect/panic (test-only code).
#![allow(clippy::expect_used, clippy::panic)]

//! Capability erasure audit for emitted Core Erlang snapshots.
//!
//!  Model B: capabilities are erased at the Core Erlang emission boundary.
//! The only place capability metadata appears in emitted output is as a
//! `%% Caps: …` comment on each function — never as runtime code.
//!
//! This file enforces that contract at the emission boundary by grepping all
//! `*.snap` files under `tests/snapshots/` for banned capability tokens.
//!
//! ## Coverage when snapshots are absent
//!
//! When `tests/snapshots/` is empty, the audit passes vacuously. Once the
//! four example `.core` snapshots are present, the
//! `no_capability_token_in_emitted_core_erl` test covers them automatically
//! (no changes to this file required).
//!
//! ## Banned tokens
//!
//! `Capability::`, `CapabilitySet::`, `capability_check`, `fs_capability`,
//! `io_capability`, `net_capability`, `time_capability`, `random_capability`,
//! `env_capability`, `proc_capability`, `spawn_capability`, `ffi_capability`.
//!
//! ## Whitelist
//!
//! Lines whose `trim_start()` starts with `"%% Caps:"` are comment lines and
//! are explicitly excluded from the ban.

use std::fs;
use std::path::{Path, PathBuf};

/// Tokens that must never appear in emitted Core Erlang (outside `%% Caps:` comments).
///
/// These are Ridge compiler-internal capability vocabulary terms.  None of them
/// should survive codegen erasure (Model B). Note that BEAM-stdlib atoms
/// like `'io'`, `'file'`, `'inet'` share substrings but are distinct; the audit
/// does not flag them because the banned tokens here are Ridge-specific compound
/// forms (e.g. `io_capability`, `Capability::Io`) not BEAM stdlib atoms.
const BANNED_TOKENS: &[&str] = &[
    "Capability::",
    "CapabilitySet::",
    "capability_check",
    "fs_capability",
    "io_capability",
    "net_capability",
    "time_capability",
    "random_capability",
    "env_capability",
    "proc_capability",
    "spawn_capability",
    "ffi_capability",
];

/// Return all `.snap` files under the given directory tree.
///
/// If the directory does not exist, returns an empty `Vec` — the audit passes vacuously.
fn find_snap_files(snapshots_dir: &Path) -> Vec<PathBuf> {
    if !snapshots_dir.exists() {
        return Vec::new();
    }
    let mut result = Vec::new();
    collect_snaps(snapshots_dir, &mut result);
    result.sort(); // deterministic order for failure messages
    result
}

fn collect_snaps(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_snaps(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("snap") {
            out.push(path);
        }
    }
}

/// Resolve the `tests/snapshots/` directory relative to this crate root.
///
/// When running under `cargo test`, the working directory is the workspace root.
/// We resolve via `CARGO_MANIFEST_DIR` (set by Cargo for integration test
/// binaries) so the path is always absolute and correct regardless of where
/// `cargo test` is invoked from.
fn snapshots_dir() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set when running under cargo test");
    PathBuf::from(manifest).join("tests").join("snapshots")
}

// ── DoD test 1 — Main audit ───────────────────────────────────────────────────

/// Scan all `*.snap` files under `tests/snapshots/` for banned capability tokens.
///
/// This enforces capability erasure: after codegen
/// erasure, no capability vocabulary token may appear in emitted Core Erlang
/// outside of `%% Caps:` comment lines.
///
/// **Passes vacuously** until the four example `.core` snapshots are present.
/// Once those snapshots are committed, this test automatically covers all of them.
#[test]
fn no_capability_token_in_emitted_core_erl() {
    let dir = snapshots_dir();
    let snaps = find_snap_files(&dir);

    // If no snapshots are present, pass vacuously.
    // Adding the four example snapshots requires no changes to this test.
    for snap in &snaps {
        let content = fs::read_to_string(snap)
            .unwrap_or_else(|e| panic!("cannot read snap {}: {e}", snap.display()));

        for (lineno, line) in content.lines().enumerate() {
            // `%% Caps: …` comment lines are explicitly whitelisted (§6).
            if line.trim_start().starts_with("%% Caps:") {
                continue;
            }
            for banned in BANNED_TOKENS {
                assert!(
                    !line.contains(banned),
                    "banned capability token `{banned}` in non-comment line of {snap}:{lineno}\n  line: {line}",
                    snap = snap.display(),
                );
            }
        }
    }
}

// ── DoD test 2 — Whitelist correctness ───────────────────────────────────────

/// Verify that `%% Caps:` comment lines are correctly whitelisted.
///
/// Uses synthetic snap content:
/// - A `%% Caps: io, fs` line with no banned tokens in other lines → must pass.
/// - A `%% Caps: io` line followed by a line containing a banned token → must
///   detect the banned token on the non-comment line.
///
/// This test does not write to disk; it exercises the audit logic inline.
#[test]
fn audit_ignores_caps_comment_lines() {
    // Synthetic snap content: caps comment is whitelisted, no banned tokens elsewhere.
    let content_ok = "\
'main'/1 =
    %% Caps: io, fs
    fun (Args) -> ... end
";

    for (lineno, line) in content_ok.lines().enumerate() {
        if line.trim_start().starts_with("%% Caps:") {
            continue;
        }
        for banned in BANNED_TOKENS {
            assert!(
                !line.contains(banned),
                "false positive: banned token `{banned}` flagged on line {lineno}: {line}"
            );
        }
    }

    // Synthetic snap content with a banned token on a non-comment line.
    // The audit must catch it.
    let content_bad = "\
'main'/1 =
    %% Caps: io, fs
    fun (Args) -> io_capability:check(Args) end
";

    let mut found_violation = false;
    for line in content_bad.lines() {
        if line.trim_start().starts_with("%% Caps:") {
            continue;
        }
        for banned in BANNED_TOKENS {
            if line.contains(banned) {
                found_violation = true;
            }
        }
    }
    assert!(
        found_violation,
        "audit must detect banned token on non-comment line"
    );
}
