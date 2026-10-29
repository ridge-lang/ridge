//! Integration tests for `ridge fmt`.
//!
//! All tests run without OTP — `ridge fmt` does not invoke `erlc`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::fs;

use assert_cmd::Command;
use common::{make_workspace, write_file, TempWorkspace};
use predicates::str::contains;

// ── helpers ───────────────────────────────────────────────────────────────────

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// Source with multiple blank lines between top-level functions — the formatter
/// collapses them to exactly one.  This is a reliable "malformatted" source
/// because the expected output differs predictably.
const MALFORMATTED: &str = "fn foo x = x\n\n\n\nfn bar y = y\n";

/// The formatted version of `MALFORMATTED` — exactly one blank line between
/// the two top-level function declarations.
const FORMATTED: &str = "fn foo x = x\n\nfn bar y = y\n";

const CALLS_SOURCE: &str = "-- café 🧭\nimport std.actor as A\nactor Counter =\n    on ping = ()\n    on take (_: Unit) = ()\npub fn send (h: Handle Counter) -> Unit =\n    h ! ping -- preserve this\n    h ! take ()\n    let _ = A.send h ping\n";
const CALLS_MIGRATED: &str = "-- café 🧭\nimport std.actor as A\nactor Counter =\n    on ping = ()\n    on take (_: Unit) = ()\npub fn send (h: Handle Counter) -> Unit =\n    h ! ping () -- preserve this\n    h ! take ()\n    let _ = A.send h (ping ())\n";

#[test]
fn fmt_migrate_calls_is_explicit_typed_and_idempotent() {
    for ending in ["\n", "\r\n"] {
        let source = CALLS_SOURCE.replace('\n', ending);
        let expected = CALLS_MIGRATED.replace('\n', ending);
        let tw = make_workspace("Calls", &source);
        let path = tw.path.join("apps/demo/src/Calls.ridge");
        let formatted = ridge_cmd()
            .args(["fmt", "--stdin"])
            .write_stdin(source.clone())
            .assert()
            .success();
        assert!(!String::from_utf8_lossy(&formatted.get_output().stdout).contains("h ! ping ()"));
        ridge_cmd()
            .args(["fmt", "--migrate-calls", "--check"])
            .current_dir(&tw.path)
            .assert()
            .failure()
            .stdout(contains("would migrate"));
        assert_eq!(fs::read_to_string(&path).unwrap(), source);
        ridge_cmd()
            .args(["fmt", "--migrate-calls"])
            .current_dir(&tw.path)
            .assert()
            .success();
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        ridge_cmd()
            .arg("check")
            .current_dir(&tw.path)
            .assert()
            .success();
        ridge_cmd()
            .args(["fmt", "--migrate-calls", "--check"])
            .current_dir(&tw.path)
            .assert()
            .success();
        ridge_cmd()
            .args(["fmt", "--migrate-calls"])
            .current_dir(&tw.path)
            .assert()
            .success();
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }
}

#[test]
fn fmt_migrate_calls_supports_standalone_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Calls.ridge");
    fs::write(&path, CALLS_SOURCE).unwrap();
    ridge_cmd()
        .args(["fmt", "--migrate-calls"])
        .arg(&path)
        .current_dir(dir.path())
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&path).unwrap(), CALLS_MIGRATED);
    ridge_cmd()
        .arg("check")
        .arg(&path)
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn fmt_migrate_calls_refuses_unknown_or_invalid_messages_without_writes() {
    for source in [
        "pub fn unresolved h -> Unit = h ! ping\n",
        "actor Counter =\n    on take (_: Unit) = ()\npub fn send (h: Handle Counter) = h ! take\n",
    ] {
        let tw = make_workspace("Invalid", source);
        let path = tw.path.join("apps/demo/src/Invalid.ridge");
        ridge_cmd()
            .args(["fmt", "--migrate-calls"])
            .current_dir(&tw.path)
            .assert()
            .failure();
        assert_eq!(fs::read_to_string(path).unwrap(), source);
    }
    ridge_cmd()
        .args(["fmt", "--migrate-calls", "--stdin"])
        .assert()
        .failure();
    ridge_cmd()
        .args(["fmt", "--migrate-calls", "--migrate-tests"])
        .assert()
        .failure();
}

#[test]
fn fmt_migrate_calls_keeps_readonly_sources_intact() {
    let tw = make_workspace("Calls", CALLS_SOURCE);
    let path = tw.path.join("apps/demo/src/Calls.ridge");
    let original = fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    let result = ridge_cmd()
        .args(["fmt", "--migrate-calls"])
        .current_dir(&tw.path)
        .assert()
        .failure();
    fs::set_permissions(&path, original).unwrap();
    result.stderr(contains("C103"));
    assert_eq!(fs::read_to_string(path).unwrap(), CALLS_SOURCE);
}

// ── Test 1: --check passes on an already-formatted file ──────────────────────

/// `ridge fmt --check` exits 0 when every file is already formatted.
#[test]
fn fmt_test_check_passes_on_already_formatted() {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(&tw.path, "apps/demo/src/Demo.ridge", FORMATTED);

    // First pass: no-op (file is already formatted).
    ridge_cmd()
        .arg("fmt")
        .current_dir(&tw.path)
        .assert()
        .success();

    // Second pass with --check: must exit 0 because nothing changed.
    ridge_cmd()
        .arg("fmt")
        .arg("--check")
        .current_dir(&tw.path)
        .assert()
        .success();
}

// ── Test 2: --check fails on a malformatted file ──────────────────────────────

/// `ridge fmt --check` exits 1 when a file would be reformatted, and prints
/// `would reformat <path>` to stdout.
#[test]
fn fmt_test_check_fails_on_malformatted() {
    let tw = make_workspace("Demo", MALFORMATTED);
    let rg_path = tw
        .path
        .join("apps")
        .join("demo")
        .join("src")
        .join("Demo.ridge");

    ridge_cmd()
        .arg("fmt")
        .arg("--check")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stdout(contains("would reformat"));

    // Verify the file was NOT modified by --check.
    let contents = fs::read_to_string(&rg_path).expect("read .ridge file");
    assert_eq!(contents, MALFORMATTED, "--check must not modify the file");
}

// ── Test 3: in-place rewrite ──────────────────────────────────────────────────

/// `ridge fmt` rewrites a malformatted file in-place with normalised output.
#[test]
fn fmt_test_in_place_rewrites_file() {
    let tw = make_workspace("Demo", MALFORMATTED);
    let rg_path = tw
        .path
        .join("apps")
        .join("demo")
        .join("src")
        .join("Demo.ridge");

    // Verify the source starts malformatted.
    assert_eq!(
        fs::read_to_string(&rg_path).expect("read before"),
        MALFORMATTED
    );

    ridge_cmd()
        .arg("fmt")
        .current_dir(&tw.path)
        .assert()
        .success();

    let after = fs::read_to_string(&rg_path).expect("read after");
    assert_eq!(
        after, FORMATTED,
        "file content after `ridge fmt` must equal the formatted source"
    );
}

// ── Test 4: --stdin writes formatted output to stdout ────────────────────────

/// `ridge fmt --stdin` reads from stdin and writes the formatted source to
/// stdout; exit code 0.
#[test]
fn fmt_test_stdin_writes_to_stdout() {
    ridge_cmd()
        .arg("fmt")
        .arg("--stdin")
        .write_stdin(MALFORMATTED)
        .assert()
        .success()
        .stdout(FORMATTED);
}

// ── Test 5: --check --stdin exits 1 when changes are needed ──────────────────

/// `ridge fmt --check --stdin` exits 1 when the input is not already formatted.
/// Nothing is written to stdout in check mode.
#[test]
fn fmt_test_stdin_check_exits_nonzero_when_changes_needed() {
    ridge_cmd()
        .arg("fmt")
        .arg("--check")
        .arg("--stdin")
        .write_stdin(MALFORMATTED)
        .assert()
        .failure()
        // In --check --stdin mode, nothing is written to stdout.
        .stdout("");
}
