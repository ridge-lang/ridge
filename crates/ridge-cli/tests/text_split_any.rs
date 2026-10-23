//! Check the public splitAny contract across the import and runtime boundary.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
#[cfg(feature = "beam-runtime")]
use common::make_app_workspace;
use common::make_workspace;
use predicates::str::contains;

#[test]
fn text_split_any_checks_list_separators_and_rejects_text_separators() {
    for (import, name) in [
        ("import std.text as Text", "Text.splitAny"),
        ("import std.text (splitAny)", "splitAny"),
    ] {
        let source = format!(
            "{import}\npub fn direct () -> List Text = {name} [\",\", \";\"] \"a,b;c\"\npub fn partial () -> List Text =\n    let split = {name} [\",\", \";\"]\n    split \"a,b;c\"\n"
        );
        let workspace = make_workspace("Main", &source);
        Command::cargo_bin("ridge")
            .unwrap()
            .current_dir(&workspace.path)
            .arg("check")
            .assert()
            .success();

        for expression in [
            format!("{name} \",\" \"a,b\""),
            format!("{name} [1] \"a,b\""),
            format!("{name} [\",\"] 1"),
        ] {
            let bad = make_workspace(
                "Main",
                &format!("{import}\npub fn bad () -> List Text = {expression}\n"),
            );
            Command::cargo_bin("ridge")
                .unwrap()
                .current_dir(&bad.path)
                .arg("check")
                .assert()
                .failure()
                .stderr(contains("type mismatch in call"));
        }
    }
}

#[cfg(feature = "beam-runtime")]
#[test]
fn text_split_any_runs_imported_direct_partial_and_higher_order_calls() {
    let workspace = make_app_workspace(
        "Main",
        r#"import std.text as Text
import std.list as List
import std.io as Io

pub fn io main () -> Unit =
    Io.println (Text.join "|" (Text.splitAny [",", ";"] "a,b;c"))
    let split = Text.splitAny [",", ";"]
    Io.println (Text.join "|" (split "x;y,z"))
    let rows = List.map (Text.splitAny [",", ";"]) ["a,b", "c;d"]
    Io.println (Text.join "/" (List.map (Text.join "|") rows))
    Io.println (Text.join "|" (Text.splitAny [] "a,b;c"))
    Io.println (Text.join "|" (Text.splitAny [",", ";"] ",a;;b,"))
    Io.println (Text.join "|" (Text.splitAny [",", ";"] ""))
    Io.println (Text.join "|" (Text.splitAny ["::", ";"] "café::world;end"))
    Io.println (Text.join "|" (Text.split "," "a,b"))
"#,
    );
    Command::cargo_bin("ridge")
        .unwrap()
        .current_dir(&workspace.path)
        .arg("run")
        .assert()
        .success()
        .stdout("a|b|c\nx|y|z\na|b/c|d\na,b;c\n|a||b|\n\ncaf\u{e9}|world|end\na|b\n");
}
