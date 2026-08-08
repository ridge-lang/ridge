//! Integration tests for `ridge explain`.
//!
//! The unit tests next to the command cover normalisation and the shape of one
//! entry. What can only be checked from out here is the part a user actually
//! meets: that the binary answers at all, that it answers without a project
//! around it, and that a wrong code fails in a way that says where to look.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// The command says what the code means and which crate reports it.
#[test]
fn explain_names_the_meaning_and_the_crate() {
    let out = ridge_cmd()
        .args(["explain", "T031"])
        .output()
        .expect("ridge explain spawn failed");

    assert!(out.status.success(), "ridge explain T031 should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(stdout.starts_with("T031 — reported by "), "got:\n{stdout}");
    assert!(
        stdout.lines().count() >= 3,
        "the summary should be on its own line, got:\n{stdout}"
    );
}

/// What a terminal hands over is `[T031]`, and what a person types is `t031`.
///
/// Both are the same code, and neither should send someone to `--list` to find
/// out they had it right.
#[test]
fn a_pasted_or_lower_case_code_resolves() {
    let canonical = ridge_cmd()
        .args(["explain", "T031"])
        .output()
        .expect("spawn failed")
        .stdout;

    for spelling in ["t031", "[T031]", "[t031]"] {
        let out = ridge_cmd()
            .args(["explain", spelling])
            .output()
            .expect("spawn failed");
        assert!(out.status.success(), "{spelling} should resolve");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&canonical),
            "{spelling} should read the same entry as T031"
        );
    }
}

/// A code that does not exist fails with `C601` and says where the list is.
#[test]
fn an_unknown_code_reports_c601() {
    let out = ridge_cmd()
        .args(["explain", "Q001"])
        .output()
        .expect("spawn failed");

    assert!(!out.status.success(), "an unknown code must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("C601"), "got:\n{stderr}");
    assert!(stderr.contains("--list"), "got:\n{stderr}");
}

/// `--list` prints the whole table.
#[test]
fn list_prints_every_code() {
    let out = ridge_cmd()
        .args(["explain", "--list"])
        .output()
        .expect("spawn failed");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Checked against the registry rather than a number. A count has to be
    // rewritten every time the table changes — as a floor it went stale the
    // first time a code was retired — and it never said which code was missing
    // when it did fail.
    for entry in ridge_diagnostics::REGISTRY {
        assert!(
            stdout.contains(entry.code),
            "{} is in the registry but not in `--list`",
            entry.code
        );
    }
}

/// Asking what a code means works in a directory with no project in it.
///
/// A code means the same thing everywhere, and the moment someone most wants to
/// look one up is while their workspace is the thing that is broken.
#[test]
fn explain_needs_no_workspace() {
    let td = tempfile::TempDir::new().expect("create tempdir");

    ridge_cmd()
        .args(["explain", "C001"])
        .current_dir(td.path())
        .assert()
        .success();
}
