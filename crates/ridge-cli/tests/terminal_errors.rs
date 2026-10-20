//! CLI errors quote control characters instead of sending terminal commands.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;

fn assert_safe_error(args: &[&str], cwd: &std::path::Path, code: &str, fragment: &str) {
    let output = Command::cargo_bin("ridge")
        .unwrap()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run ridge");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(stderr.contains(code), "{stderr:?}");
    assert!(
        stderr.contains(fragment),
        "quoted fragment missing: {stderr:?}"
    );
    let body = stderr.strip_suffix('\n').expect("one final line ending");
    assert!(
        body.chars().all(|c| !c.is_control()),
        "raw control in stderr: {stderr:?}"
    );
}

#[test]
fn terminal_errors_escape_project_name_sequences() {
    let dir = tempfile::tempdir().unwrap();
    assert_safe_error(
        &["new", "app/\u{1b}[31mred\u{1b}]0;title\u{7}"],
        dir.path(),
        "C201",
        "app/\\u{1b}[31mred\\u{1b}]0;title\\u{7}",
    );
    assert!(
        std::fs::read_dir(dir.path()).unwrap().next().is_none(),
        "invalid names create no files"
    );
}

#[test]
fn terminal_errors_escape_unknown_member_c1_and_line_breaks() {
    let workspace = common::make_app_workspace("Main", "pub fn main () -> Unit = ()\n");
    assert_safe_error(
        &["run", "--member", "bad\u{9b}2J\n\t\rname"],
        &workspace.path,
        "C005",
        "bad\\u{9b}2J\\n\\t\\rname",
    );
}

#[test]
fn terminal_errors_escape_explain_and_path_arguments() {
    let dir = tempfile::tempdir().unwrap();
    assert_safe_error(
        &["explain", "ZZ\u{1b}[2J"],
        dir.path(),
        "C601",
        "ZZ\\u{1b}[2J",
    );
    assert_safe_error(
        &["fmt", "café\u{1b}[2J.ridge"],
        dir.path(),
        "C102",
        "café\\u{1b}[2J.ridge",
    );
}
