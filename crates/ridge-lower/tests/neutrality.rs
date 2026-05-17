//! Target-neutrality audit — spec N12.
//!
//! Walks every `*.rs` source file under `crates/ridge-ir/src/` and
//! `crates/ridge-lower/src/` and asserts that no banned token appears in
//! non-comment code.
//!
//! Also walks every `*.snap` file under `crates/ridge-lower/tests/snapshots/`
//! and asserts banned tokens are absent entirely (snapshots are IR output and
//! must be unconditionally target-neutral).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

const BANNED: &[&str] = &[
    "erlang",
    "beam",
    "wasm",
    "core_erl",
    "gen_server",
    "binary_to_list",
];

/// Collect all `*.rs` files recursively under `dir`.
fn collect_rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_rs_files(&path));
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out
}

/// Collect all `*.snap` files recursively under `dir`.
fn collect_snap_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(collect_snap_files(&path));
        } else if path.extension().and_then(|s| s.to_str()) == Some("snap") {
            out.push(path);
        }
    }
    out
}

/// Strip `//`-prefixed line comments and `/* … */` block comments from Rust source.
///
/// This is a conservative approximation: it removes lines that start with `//`
/// (after trimming) and removes `/* … */` blocks.  Explanatory comments that
/// discuss the target-neutrality boundary may legally mention banned tokens.
fn strip_comments(source: &str) -> String {
    // Remove block comments first.
    let mut without_block = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            // Skip to end of block comment.
            i += 2;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                if bytes[i] == b'\n' {
                    without_block.push('\n');
                }
                i += 1;
            }
        } else {
            without_block.push(bytes[i] as char);
            i += 1;
        }
    }

    // Remove line comments (`//` to end of line).
    let mut result = String::with_capacity(without_block.len());
    for line in without_block.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            // Drop line comment lines entirely.
        } else if let Some(comment_pos) = line.find("//") {
            // Inline comment: keep the code part.
            result.push_str(&line[..comment_pos]);
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    result
}

#[test]
fn neutrality_audit_source_files() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    // `crates/ridge-lower` is at `<manifest_dir>`.
    // `crates/ridge-ir`    is at `<manifest_dir>/../ridge-ir`.
    let lower_src = PathBuf::from(manifest_dir).join("src");
    let ir_src = PathBuf::from(manifest_dir)
        .parent()
        .expect("parent of ridge-lower")
        .join("ridge-ir/src");

    let mut failures: Vec<String> = Vec::new();

    for dir in &[&lower_src, &ir_src] {
        let files = collect_rs_files(dir);
        for path in &files {
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    failures.push(format!("could not read {}: {e}", path.display()));
                    continue;
                }
            };
            let stripped = strip_comments(&content);
            for &token in BANNED {
                // Case-insensitive search to catch e.g. "BEAM" or "Erlang".
                if stripped.to_lowercase().contains(token) {
                    failures.push(format!(
                        "found banned token `{token}` in {}",
                        path.display()
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "target-neutrality violations:\n{}",
        failures.join("\n")
    );
}

#[test]
fn neutrality_audit_snapshots() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let snap_dir = PathBuf::from(manifest_dir).join("tests/snapshots");

    let files = collect_snap_files(&snap_dir);
    let mut failures: Vec<String> = Vec::new();

    for path in &files {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("could not read {}: {e}", path.display()));
                continue;
            }
        };
        for &token in BANNED {
            if content.to_lowercase().contains(token) {
                failures.push(format!(
                    "found banned token `{token}` in {}",
                    path.display()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "snapshot neutrality violations:\n{}",
        failures.join("\n")
    );
}
