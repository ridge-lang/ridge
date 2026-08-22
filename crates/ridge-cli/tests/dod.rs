// Test code: relax doc-quoting lint so DoD/G6/G2/etc abbreviations and
// pipeline stage names ("BuildTestMatrix") read as prose, not back-ticked
// identifiers.  Mirrors `crates/ridge-parser/tests/errors.rs`.
#![allow(clippy::doc_markdown)]

//! DoD acceptance — local-loop guard.
//!
//! These tests assert the M5 / G2 / G3 / G6 deliverables are in place.
//! They run as part of `cargo test --workspace` so a developer can validate
//! DoD locally without waiting for the full Azure DevOps pipeline (Stage 3
//! `BuildTestMatrix`), which remains the authoritative G2 attestation.
//!
//! These tests are NOT a substitute for the pipeline:
//! - They run in-process against the four canonical examples; they do NOT
//!   exercise the `install.sh` / `install.ps1` from-zero flow on a clean VM.
//! - They do NOT execute BEAM-side `erl` (which the test agent may not have).
//!
//! Run with:
//! ```text
//! cargo test -p ridge-cli --test dod
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{make_example_workspace, TempWorkspace};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build an `assert_cmd` Command for the `ridge` binary.
fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// The four canonical Phase 8 examples (DoD scope #2 / G6 / §3.16).
const CANONICAL_EXAMPLES: &[&str] = &[
    "log_analyzer",
    "url_shortener",
    "game_of_life",
    "rate_limiter",
];

/// Repo-root path resolved from `CARGO_MANIFEST_DIR` (= `crates/ridge-cli/`).
fn repo_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir).join("..").join("..")
}

// ── Test 1: build / check / fmt --check on each canonical example ────────────

/// `ridge build`, `ridge check`, `ridge fmt`, and `ridge fmt --check` (after
/// one fmt pass) must each succeed on every one of the four canonical examples
/// (DoD scope #2 — G3 + G6).
///
/// `ridge run` is intentionally NOT covered here because it requires `erl` on
/// PATH; the BuildTestMatrix pipeline stage owns that branch.  `ridge build`
/// is permissive of `C004 ErlangNotFound` (matching the pattern in
/// `tests/build.rs`) so the test is robust on a Rust-only CI agent.
///
/// G3 idempotency contract (post-2026-05-07 fmt fix): for any input where `fmt --check`
/// fails, exactly one `fmt` pass produces output where `fmt --check`
/// succeeds.  The four canonical examples now reach a fixed point in one
/// pass; this test asserts that.
#[test]
fn dod_examples_build_run_check_fmt_check() {
    for name in CANONICAL_EXAMPLES {
        let tw = make_example_workspace(name);

        // ── ridge build ─────────────────────────────────────────────────────
        let output = ridge_cmd()
            .arg("build")
            .current_dir(&tw.path)
            .output()
            .unwrap_or_else(|e| panic!("ridge build {name} spawn failed: {e}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            // Allow C004 (no OTP) per the established pattern in tests/build.rs.
            assert!(
                stderr.contains("C004") || stderr.contains("erlang") || stderr.contains("erl"),
                "ridge build {name} failed unexpectedly.\nstdout: {stdout}\nstderr: {stderr}"
            );
        }

        // ── ridge check ─────────────────────────────────────────────────────
        ridge_cmd()
            .arg("check")
            .current_dir(&tw.path)
            .assert()
            .success();

        // ── ridge fmt (apply once) — must succeed and reach a fixed point.
        ridge_cmd()
            .arg("fmt")
            .current_dir(&tw.path)
            .assert()
            .success();

        // ── ridge fmt --check (after one apply pass) — must report no
        //    further changes needed.  G3 idempotency contract.
        ridge_cmd()
            .arg("fmt")
            .arg("--check")
            .current_dir(&tw.path)
            .assert()
            .success();
    }
}

// ── Test 2: ridge new smoke test ─────────────────────────────────────────────

/// `ridge new my-app && cd my-app && ridge build` succeeds in a fresh
/// tempdir (DoD scope #1 / G2 — minus the BEAM execution leg, since `ridge
/// run` requires `erl`).
#[test]
fn dod_ridge_new_smoke() {
    let tw = TempWorkspace::new();

    // ── ridge new my-app ────────────────────────────────────────────────────
    ridge_cmd()
        .arg("new")
        .arg("my-app")
        .current_dir(&tw.path)
        .assert()
        .success();

    let app_dir = tw.path.join("my-app");
    assert!(
        app_dir.is_dir(),
        "ridge new must create my-app/ subdirectory; missing at {}",
        app_dir.display()
    );

    // ── ridge build inside the new project ─────────────────────────────────
    let output = ridge_cmd()
        .arg("build")
        .current_dir(&app_dir)
        .output()
        .expect("ridge build spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        // Allow C004 (no OTP) per the established pattern.
        assert!(
            stderr.contains("C004") || stderr.contains("erlang") || stderr.contains("erl"),
            "ridge build in new project failed unexpectedly: {stderr}"
        );
    }
}

// ── Test 3: fmt is idempotent on every canonical example ────────────────────

/// `ridge fmt` reaches a byte-identical fixed point in one pass on every one
/// of the four canonical Phase 8 examples (G3 — full idempotency).
///
/// Method: copy each `examples/<name>.ridge` into a tempdir-backed workspace,
/// run `ridge fmt` once, then run `ridge fmt --check` and assert success.
/// Repeating `fmt` a third time and diffing the output against the second
/// pass is also asserted to give a stronger guarantee than `--check` alone.
#[test]
fn dod_fmt_idempotent() {
    for name in CANONICAL_EXAMPLES {
        let tw = make_example_workspace(name);

        // First pass — apply the formatter.
        ridge_cmd()
            .arg("fmt")
            .current_dir(&tw.path)
            .assert()
            .success();

        // Snapshot the formatted source.
        let src_path = tw
            .path
            .join("apps")
            .join("demo")
            .join("src")
            .join(format!("{name}.ridge"));
        let after_first =
            fs::read(&src_path).unwrap_or_else(|e| panic!("read {}: {e}", src_path.display()));

        // --check must now succeed (no more reformatting needed).
        ridge_cmd()
            .arg("fmt")
            .arg("--check")
            .current_dir(&tw.path)
            .assert()
            .success();

        // Second apply pass — must produce byte-identical output.
        ridge_cmd()
            .arg("fmt")
            .current_dir(&tw.path)
            .assert()
            .success();
        let after_second =
            fs::read(&src_path).unwrap_or_else(|e| panic!("read {}: {e}", src_path.display()));
        assert_eq!(
            after_first, after_second,
            "fixture '{name}': second `ridge fmt` pass produced different bytes (idempotency violated)"
        );
    }
}

// ── Test 4: docs/hot-reload-design.md exists with the four documented OQs ────

/// `docs/hot-reload-design.md` exists and enumerates the four open-questions
/// of the hot-reload design.
#[test]
fn dod_hot_reload_doc_exists() {
    let doc_path = repo_root().join("docs").join("hot-reload-design.md");
    assert!(
        doc_path.is_file(),
        "expected docs/hot-reload-design.md at {}",
        doc_path.display()
    );

    let body = fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", doc_path.display()));

    // Match on stable header words rather than full sentences so minor copy
    // edits don't flake the test.
    let required_markers = &[
        "State migration",
        "Capability re-checking",
        "Type-compatibility",
        "stdlib hot-reload",
    ];
    for marker in required_markers {
        assert!(
            body.contains(marker),
            "docs/hot-reload-design.md missing required section marker '{marker}'\n\
             — required section must be present."
        );
    }
}

// ── Codes and messages cannot drift apart ────────────────────────────────────

/// Every `CliError` that carries a code prints it first, and every `CliWarning`
/// does the same after its `warning:` prefix.
///
/// The code and the message used to be written out separately in each `Display`
/// arm, which is how `C004` ended up with two spellings in one crate — "erlang
/// toolchain not found" in one enum and "erlang runtime not found" in another.
/// Asserting the two agree is cheaper than trusting they will.
#[test]
fn a_code_carrying_error_prints_its_code_first() {
    use ridge_cli::error::{CliError, CliWarning};

    // Exhaustive coverage of `CliError` lives in the crate's own unit tests,
    // where `#[non_exhaustive]` does not block a wildcard-free match. What is
    // worth re-checking from outside is that the codes survive the public API.
    let cases: Vec<CliError> = vec![
        CliError::no_workspace_root(Path::new("ws")),
        CliError::UnknownMember { name: "x".into() },
        CliError::NoExecutableMember,
        CliError::WatchAmbiguousMember,
        CliError::MigrateCompileFailed,
        CliError::WatchStateCorrupted,
    ];
    for e in cases {
        let code = e.code().expect("these variants all carry a code");
        let text = e.to_string();
        assert!(
            text.starts_with(code),
            "`{code}` must open its own message, got: {text}"
        );
    }

    // The two sentinels answer `None` rather than inventing a code.
    assert!(CliError::AlreadyReported.code().is_none());

    for w in [
        CliWarning::BoolTestDeprecated {
            qualified_name: "Main.test_x".into(),
        },
        CliWarning::PrefixTestDeprecated {
            module: "Main".into(),
            name: "test_x".into(),
        },
    ] {
        let text = w.to_string();
        assert!(
            text.starts_with(&format!("warning: {}", w.code())),
            "an advisory opens with `warning: <code>`, got: {text}"
        );
    }
}

// ── C001 means one thing ─────────────────────────────────────────────────────

/// `C001` is reported only where a workspace root was actually looked for.
///
/// It used to be the enum's junk drawer: seventeen call sites returned it when
/// the file watcher would not start, a child would not spawn, a lock was
/// poisoned, or `erl` was missing. Most printed the real cause first, so
/// `ridge repl` on a machine without OTP said `C004 ErlangNotFound` and then
/// `C001 NoWorkspaceRoot` about a workspace that was sitting right there.
///
/// The failure was never that anyone wanted to report `C001`. It was that a
/// closure needed *some* `CliError` and that variant took no fields. It now
/// carries the directory it searched, so building one means saying what was
/// looked for and where — but a constructor is still cheaper to call than to
/// think about, and the type system has nothing to say about that.
///
/// This used to look three lines back from the construction for the lookup
/// that justifies it, which encoded the shape those sites happened to have
/// rather than the rule. Every subcommand now goes through one helper, where
/// the lookup and the `NotFound` arm sit five lines apart, and a window wide
/// enough for that would stop catching anything. So the check reads the
/// enclosing function instead, and adds what the refactor made true: exactly
/// one place in the crate builds a `C001`.
#[test]
fn c001_is_only_constructed_where_a_workspace_root_was_sought() {
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

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    let mut found = 0_usize;

    for file in rust_sources(&src) {
        // `error.rs` declares the enum and samples every variant in its own
        // unit tests. Counting those as call sites is how a check like this
        // reports its own fixtures as the problem.
        if file.file_name().and_then(|n| n.to_str()) == Some("error.rs") {
            continue;
        }
        let body = fs::read_to_string(&file).expect("read source");
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("no_workspace_root") || line.trim_start().starts_with("//") {
                continue;
            }
            found += 1;
            // Scan back to the start of the enclosing function. A `C001` is
            // justified when the function it is built in is the one looking for
            // the workspace root, which is the rule; how many lines apart the
            // two sit is not.
            let from = lines[..=i]
                .iter()
                .rposition(|l| l.starts_with("fn ") || l.starts_with("pub fn "))
                .unwrap_or(0);
            let window = lines[from..=i].join("\n");
            if !window.contains("find_workspace_root") {
                offenders.push(format!(
                    "{}:{}: {}",
                    file.file_name().unwrap_or_default().to_string_lossy(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        found > 0,
        "no `CliError::no_workspace_root` sites found — this test stopped checking anything"
    );
    assert_eq!(
        found, 1,
        "every subcommand asks one helper where the workspace is, so one place \n         answers `no workspace manifest found`. A second site is either a \n         subcommand that went around the helper, or a different failure \n         borrowing this code."
    );
    assert!(
        offenders.is_empty(),
        "`C001` says no workspace manifest was found. These sites return it for \
         something else; give the real failure its own code, or return \
         `CliError::AlreadyReported` if its cause is already on stderr:\n  {}",
        offenders.join("\n  ")
    );
}
