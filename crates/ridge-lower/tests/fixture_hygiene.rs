//! A test may not name its own fixture directory.
//!
//! A path built as `temp_dir().join("something_fixed")` is the same path in
//! every process that builds it, and a helper that creates it on the way in and
//! removes it on the way out will therefore delete a sibling's fixture. What
//! arrives is never a message about directories: it is "workspace graph must be
//! present", or an empty module list, or a lowered module that isn't there —
//! failures that read like a defect in the code under test and send the reader
//! into the wrong crate.
//!
//! The runner decides how bad it is. Under plain `cargo test` the tests in a
//! target are threads in one process and one target runs at a time; under
//! `cargo nextest`, which CI and `scripts/gate.sh` both use, every test is its
//! own process and targets overlap. Measured on the same machine, with the two
//! targets that shared an id: 0 failures in 30 runs under `cargo test`, and 27
//! in 30 under nextest. A local green is not evidence.
//!
//! # Why this lives here
//!
//! It reads every crate's `tests/` tree, not just this one — the shape had been
//! hand-rolled five times across three crates while `ridge-cli` and
//! `ridge-driver` had been taking the directory from the library all along. So
//! this was divergence from a convention rather than the absence of one, and a
//! check covering only `ridge-lower` would repeat the mistake at a different
//! scale. It lives in this crate because this is where the race was found.
//!
//! # What it does not cover
//!
//! The shared OS temp dir only. `CARGO_TARGET_TMPDIR` is Cargo's own
//! per-package scratch directory — cleaned by `cargo clean`, not shared outside
//! the package — so a fixed name under it is not the same hazard. One test file
//! uses it today, with a distinct name per test.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

/// Repo-root path resolved from `CARGO_MANIFEST_DIR` (= `crates/ridge-lower/`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every `.rs` file under `root`, recursively.
fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_sources(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out
}

/// Every `crates/*/tests` directory in the workspace.
fn test_trees() -> Vec<PathBuf> {
    let crates = repo_root().join("crates");
    let mut out = Vec::new();
    for entry in fs::read_dir(&crates).expect("read crates/").flatten() {
        let tests = entry.path().join("tests");
        if tests.is_dir() {
            out.push(tests);
        }
    }
    out.sort();
    out
}

#[test]
fn no_test_names_its_own_fixture_directory() {
    // Split so this file is not its own first offender. The alternative is to
    // exempt it by name, and an exemption is a hole that outlives the reason
    // for it.
    const NEEDLE: &str = concat!("temp_", "dir()");

    let mut offenders: Vec<String> = Vec::new();
    let mut files_read = 0_usize;
    let mut files_with_a_test = 0_usize;
    let mut files_using_tempfile = 0_usize;

    for tree in test_trees() {
        for file in rust_sources(&tree) {
            let body = fs::read_to_string(&file).expect("read test source");
            files_read += 1;
            if body.contains("#[test]") || body.contains("#[tokio::test]") {
                files_with_a_test += 1;
            }
            if body.contains("tempfile::") {
                files_using_tempfile += 1;
            }
            for (i, line) in body.lines().enumerate() {
                // Prose is allowed to say what the rule is; the doc comment
                // above says it twice.
                if !line.contains(NEEDLE) || line.trim_start().starts_with("//") {
                    continue;
                }
                let shown = file
                    .strip_prefix(repo_root())
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .replace('\\', "/");
                offenders.push(format!("{}:{}: {}", shown, i + 1, line.trim()));
            }
        }
    }

    // Three ways this test could report a clean tree without having looked at
    // one: no directories, no files, or a reader that never sees content. Each
    // gets its own floor, so a break in the walk fails here instead of passing
    // as a green.
    assert!(
        files_read > 0,
        "no test sources were read — the walk over crates/*/tests found nothing"
    );
    assert!(
        files_with_a_test > 0,
        "read {files_read} files and none declared a test — the walk is reaching \
         the wrong tree"
    );
    assert!(
        files_using_tempfile > 0,
        "read {files_read} files and none mentioned `tempfile::` — the reader is \
         not seeing file contents, so the check below cannot fail"
    );

    assert!(
        offenders.is_empty(),
        "a fixture directory named in source is shared with every other process \
         that names it, and the helpers here delete the directory they build. \
         Take the uniqueness from the library instead — the crate's \
         `TempWorkspace` helper, or `tempfile::Builder::new().prefix(..).tempdir()` \
         — and keep the descriptive part as a prefix:\n  {}",
        offenders.join("\n  ")
    );
}
