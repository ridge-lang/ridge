//! Regression: a manifest error must reach the user.
//!
//! Discovery is deliberately non-fatal — a project whose `ridge.toml` does not
//! parse is skipped so the rest of the workspace still builds. The errors it
//! recorded were then dropped at the boundary into resolution, so a workspace
//! whose only project was skipped had no modules, no errors, and reported
//! success: `check` printed "Type-check passed" over a source it never read.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{write_file, TempWorkspace};
use ridge_driver::{
    check_workspace, compile_workspace, CheckOptions, CompileOptions, EmitArtefacts, Profile,
};

/// A single-project workspace whose manifest names a capability that does not
/// exist. `allow` is the most common place to make this mistake — the
/// capability is `random`, and `rand` is the obvious guess.
fn workspace_with_unknown_capability() -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"demo\"\nversion = \"0.1.0\"\nmembers = [\".\"]\n\n\
         [project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n\
         [project.src]\nroot = \"src\"\n\n\
         [capabilities]\nallow = [\"io\", \"rand\"]\n",
    );
    // Deliberately broken, to prove the source is never even reached: the
    // manifest error has to be what gets reported, not a parse error.
    write_file(&tw.path, "src/Main.ridge", "fn main () = @@@ ###\n");
    tw
}

#[test]
fn check_reports_the_manifest_error() {
    let tw = workspace_with_unknown_capability();
    let artefacts = check_workspace(CheckOptions::new(tw.path)).expect("check runs");

    let codes: Vec<&str> = artefacts.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&"M011"),
        "an unknown capability must be reported; got: {codes:?}"
    );
}

#[test]
fn build_reports_the_manifest_error() {
    let tw = workspace_with_unknown_capability();
    let opts = CompileOptions::new(tw.path)
        .with_profile(Profile::Debug)
        .with_emit(EmitArtefacts::Core);
    let artefacts = compile_workspace(opts).expect("compile runs");

    let codes: Vec<&str> = artefacts.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&"M011"),
        "a build over a broken manifest must not report success; got: {codes:?}"
    );
}

/// The skipped project is the whole workspace here, so nothing downstream has
/// anything to complain about — which is exactly how the silence arose.
#[test]
fn the_manifest_error_is_the_only_thing_reported() {
    let tw = workspace_with_unknown_capability();
    let artefacts = check_workspace(CheckOptions::new(tw.path)).expect("check runs");

    assert!(
        !artefacts.diagnostics.is_empty(),
        "a workspace with no readable project must not check clean"
    );
}

/// A workspace whose members glob matches a directory with no `ridge.toml` is
/// the same class of error from a different code (`M004`), and travels the same
/// path.
#[test]
fn a_member_without_a_manifest_is_reported_too() {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"demo\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/orphan/src/Main.ridge",
        "fn main () -> Int = 0\n",
    );

    let artefacts = check_workspace(CheckOptions::new(tw.path)).expect("check runs");
    let codes: Vec<&str> = artefacts.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.contains(&"M004"),
        "a member directory with no manifest must be reported; got: {codes:?}"
    );
}

/// A workspace that is actually fine stays quiet — the new channel must not
/// invent diagnostics.
#[test]
fn a_healthy_workspace_reports_nothing() {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"demo\"\nversion = \"0.1.0\"\nmembers = [\".\"]\n\n\
         [project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n\
         [project.src]\nroot = \"src\"\n\n\
         [capabilities]\nallow = [\"io\"]\n",
    );
    write_file(&tw.path, "src/Main.ridge", "fn main () -> Int = 0\n");

    let artefacts = check_workspace(CheckOptions::new(tw.path)).expect("check runs");
    let codes: Vec<&str> = artefacts.diagnostics.iter().map(|d| d.code).collect();
    assert!(
        codes.is_empty(),
        "clean workspace should be quiet; got: {codes:?}"
    );
}
