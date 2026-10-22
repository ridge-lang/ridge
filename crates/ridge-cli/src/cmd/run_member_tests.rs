#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;
use std::fmt::Write;

#[test]
fn shorthand_preserves_program_flags_and_subcommand_usage() {
    use crate::{Cli, RidgeCommand};
    let cli = Cli::try_parse_invocation_from(["ridge", "script.ridge", "--release", "two words"])
        .unwrap();
    let RidgeCommand::Run(args) = cli.command else {
        panic!("run shorthand")
    };
    assert_eq!(args.file.as_deref(), Some(Path::new("script.ridge")));
    assert!(!args.release);
    assert_eq!(args.extra_args, ["--release", "two words"]);
    assert!(Cli::try_parse_invocation_from(["ridge", "unknown"]).is_err());
    assert!(
        Cli::try_parse_invocation_from(["ridge", "run", "script.ridge", "--member", "app"])
            .is_err()
    );
}

fn workspace(members: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        format!("[workspace]\nname = \"test\"\nversion = \"0.1.0\"\nmembers = [{members}]\n"),
    )
    .unwrap();
    dir
}

fn project(root: &Path, path: &str, name: &str, kind: &str) {
    let dir = root.join(path);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ridge.toml"),
        format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\nkind = \"{kind}\"\nentry = \"src/Main.ridge\"\n"),
    )
    .unwrap();
}

fn select(root: &Path, flags: &[&str]) -> Result<String, CliError> {
    let args =
        RunArgs::try_parse_from(std::iter::once("run").chain(flags.iter().copied())).unwrap();
    resolve_executable_member(root, &args)
}

#[test]
fn declared_layouts_select_manifest_name() {
    for (path, kind) in [
        ("app", "app"),
        ("services/worker", "service"),
        ("libs/tool", "app"),
    ] {
        let dir = workspace(&format!("\"{path}\""));
        project(dir.path(), path, "executable", kind);
        assert_eq!(select(dir.path(), &[]).unwrap(), "executable");
        assert_eq!(
            select(dir.path(), &["--member", "executable"]).unwrap(),
            "executable"
        );
        assert!(matches!(
            select(dir.path(), &["--member", "wrong"]),
            Err(CliError::UnknownMember { .. })
        ));
    }
}

#[test]
fn root_member_and_overlapping_globs_are_not_ambiguous() {
    let dir = workspace("\".\"");
    let path = dir.path().join("ridge.toml");
    let mut src = std::fs::read_to_string(&path).unwrap();
    src.push_str("\n[project]\nname = \"root\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n");
    std::fs::write(path, src).unwrap();
    assert_eq!(select(dir.path(), &[]).unwrap(), "root");
    let dir = workspace("\"services/*\", \"services/worker\"");
    project(dir.path(), "services/worker", "worker", "service");
    assert_eq!(select(dir.path(), &["--watch"]).unwrap(), "worker");
}

#[test]
fn undeclared_apps_are_not_members() {
    let dir = workspace("\"libs/*\"");
    project(dir.path(), "apps/hidden", "hidden", "app");
    project(dir.path(), "libs/library", "library", "library");
    assert!(matches!(
        select(dir.path(), &[]),
        Err(CliError::NoExecutableMember)
    ));
    assert!(matches!(
        select(dir.path(), &["--member", "hidden"]),
        Err(CliError::UnknownMember { .. })
    ));
    assert!(matches!(
        select(dir.path(), &["--member", "library"]),
        Err(CliError::LibraryNotExecutable { .. })
    ));
}

#[test]
fn watch_requires_explicit_selection_for_multiple_executables() {
    let dir = workspace("\"services/*\"");
    project(dir.path(), "services/a", "a", "app");
    project(dir.path(), "services/b", "b", "service");
    assert!(matches!(
        select(dir.path(), &["--watch"]),
        Err(CliError::WatchAmbiguousMember)
    ));
    assert_eq!(
        select(dir.path(), &["--watch", "--member", "b"]).unwrap(),
        "b"
    );
    assert_eq!(select(dir.path(), &[]).unwrap(), "a");
}

#[test]
fn malformed_and_missing_member_manifests_are_not_skipped() {
    for malformed in [false, true] {
        let dir = workspace("\"app\", \"broken\"");
        project(dir.path(), "app", "app", "app");
        std::fs::create_dir(dir.path().join("broken")).unwrap();
        if malformed {
            std::fs::write(dir.path().join("broken/ridge.toml"), "[project\n").unwrap();
        }
        for flags in [&[][..], &["--member", "app"][..]] {
            assert!(matches!(
                select(dir.path(), flags),
                Err(CliError::MemberManifestInvalid { .. })
            ));
        }
        let discovery = ridge_resolve::discover_workspace(dir.path());
        assert!(!discovery.manifest_errors.is_empty());
        assert_eq!(discovery.graph.unwrap().projects.len(), 1);
    }
}

#[test]
fn duplicate_declared_names_are_reported_before_selection() {
    let dir = workspace("\"services/*\"");
    project(dir.path(), "services/a", "same", "app");
    project(dir.path(), "services/b", "same", "service");
    let Err(CliError::MemberManifestInvalid { rendered }) =
        select(dir.path(), &["--member", "same"])
    else {
        panic!("duplicate names must reject selection");
    };
    assert!(rendered.contains("duplicate"), "{rendered}");
}

#[test]
fn member_selection_does_not_walk_sources() {
    let dir = workspace("\"app\"");
    project(dir.path(), "app", "app", "app");
    std::fs::create_dir(dir.path().join("app/src")).unwrap();
    std::fs::write(dir.path().join("app/src/Legacy.rg"), "invalid source").unwrap();
    assert_eq!(select(dir.path(), &[]).unwrap(), "app");
    assert!(!ridge_resolve::discover_workspace(dir.path())
        .resolve_errors
        .is_empty());
}

#[test]
fn escaped_path_dependencies_remain_invalid() {
    let outer = tempfile::tempdir().unwrap();
    let dir = workspace("\"app\"");
    project(dir.path(), "app", "app", "app");
    let path = dir.path().join("app/ridge.toml");
    let mut src = std::fs::read_to_string(&path).unwrap();
    write!(
        src,
        "\n[dependencies]\nescaped = {{ path = '{}' }}\n",
        outer.path().display()
    )
    .unwrap();
    std::fs::write(path, src).unwrap();
    assert!(matches!(
        select(dir.path(), &[]),
        Err(CliError::MemberManifestInvalid { .. })
    ));
}
