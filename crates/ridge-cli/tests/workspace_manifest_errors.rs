//! A workspace manifest that is there and broken says what is wrong with it.
//!
//! Every command starts by walking upward for the `ridge.toml` that governs the
//! current directory. That walk used to treat a manifest it could not parse the
//! same way it treats a directory with no manifest at all: keep climbing. Two
//! things followed. A workspace whose own manifest was broken reported "no
//! workspace manifest found", pointing the reader at a file sitting right in
//! front of them; and when a valid workspace happened to exist further up, the
//! command silently ran against that one instead.
//!
//! The manifest errors themselves were never the problem — discovery raises
//! them with the right code and the right span. They had no way out: the caller
//! read the graph, found none, and reported the walk's error instead of the
//! parser's.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::{write_file, TempWorkspace};

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

const MEMBER_MANIFEST: &str =
    "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n";
const MEMBER_SRC: &str = "pub fn answer () -> Int = 42\n";

/// A workspace whose root manifest is `manifest`, with one well-formed member.
fn workspace_with_root_manifest(manifest: &str) -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(&tw.path, "ridge.toml", manifest);
    write_file(&tw.path, "apps/demo/ridge.toml", MEMBER_MANIFEST);
    write_file(&tw.path, "apps/demo/src/Main.ridge", MEMBER_SRC);
    tw
}

const VALID: &str = "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n";

/// Run `ridge check` in `dir` and return everything it printed.
fn check_output(dir: &std::path::Path) -> String {
    let out = ridge_cmd().arg("check").current_dir(dir).output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_well_formed_workspace_still_checks() {
    // The control. Every assertion below is about an error arriving; without
    // this, a build that failed for an unrelated reason would satisfy them all.
    let tw = workspace_with_root_manifest(VALID);
    ridge_cmd()
        .arg("check")
        .current_dir(&tw.path)
        .assert()
        .success();
}

#[test]
fn a_workspace_manifest_missing_its_name_says_so() {
    let tw =
        workspace_with_root_manifest("[workspace]\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n");
    let out = check_output(&tw.path);
    assert!(
        out.contains("M006"),
        "a missing required field should be reported as M006, got:\n{out}"
    );
    assert!(
        out.contains("name"),
        "the message should name the field that is missing, got:\n{out}"
    );
    assert!(
        !out.contains("no workspace manifest found"),
        "the manifest is right there and was read, got:\n{out}"
    );
}

#[test]
fn a_workspace_manifest_with_an_unknown_key_says_so() {
    let tw = workspace_with_root_manifest(
        "[workspace]\nname = \"w\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\nnonsense = true\n",
    );
    let out = check_output(&tw.path);
    assert!(
        out.contains("M019"),
        "an unknown top-level key should be reported as M019, got:\n{out}"
    );
    assert!(
        !out.contains("no workspace manifest found"),
        "the manifest is right there and was read, got:\n{out}"
    );
}

#[test]
fn a_workspace_manifest_that_is_not_toml_says_so() {
    let tw = workspace_with_root_manifest("[workspace\nname = \"w\"\n");
    let out = check_output(&tw.path);
    assert!(
        out.contains("M001"),
        "an unparseable manifest should be reported as a parse error, got:\n{out}"
    );
    assert!(
        !out.contains("no workspace manifest found"),
        "the manifest is right there; it could not be parsed, got:\n{out}"
    );
}

#[test]
fn a_broken_manifest_does_not_hand_the_command_a_different_workspace() {
    // The worst of the three, and the one that looks like success. With a valid
    // workspace further up, the walk climbed past the broken manifest and the
    // command checked the outer workspace instead — printing "Type-check
    // passed" for a project the reader never asked about.
    let outer = TempWorkspace::new();
    write_file(
        &outer.path,
        "ridge.toml",
        "[workspace]\nname = \"outer\"\nversion = \"0.1.0\"\nmembers = [\"none/*\"]\n",
    );
    write_file(
        &outer.path,
        "inner/ridge.toml",
        "[workspace\nname = \"inner\"\n",
    );

    let out = check_output(&outer.path.join("inner"));
    assert!(
        out.contains("M001"),
        "the broken manifest in the current directory is what governs here, got:\n{out}"
    );
    assert!(
        !out.contains("Type-check passed"),
        "a broken manifest must not be answered by checking some other workspace, got:\n{out}"
    );
}

#[test]
fn a_broken_member_manifest_is_still_found_from_inside_it() {
    // The regression this design has to avoid. A member's manifest cannot be
    // parsed either, but a valid workspace root does exist above it, so the
    // walk must still reach that root and let discovery report the member.
    // Stopping at the first unparseable file would turn this good message into
    // a claim that the member is a broken workspace.
    let tw = TempWorkspace::new();
    write_file(&tw.path, "ridge.toml", VALID);
    write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        "[project\nname = \"demo\"\n",
    );
    write_file(&tw.path, "apps/demo/src/Main.ridge", MEMBER_SRC);

    let from_member = check_output(&tw.path.join("apps/demo"));
    assert!(
        from_member.contains("M001"),
        "the member's own parse error is the answer, got:\n{from_member}"
    );
    assert!(
        from_member.contains("demo"),
        "and it should point at the member's manifest, got:\n{from_member}"
    );
}
