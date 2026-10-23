//! Run the checked-in wc example against independently specified counts.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::{write_file, TempWorkspace};

#[test]
fn dogfood_wc_counts_words_and_preserves_line_and_content_byte_counts() {
    let workspace = TempWorkspace::new();
    write_file(
        &workspace.path,
        "ridge.toml",
        include_str!("../../../dogfood/wc/ridge.toml"),
    );
    write_file(
        &workspace.path,
        "src/Main.ridge",
        include_str!("../../../dogfood/wc/src/Main.ridge"),
    );
    Command::cargo_bin("ridge")
        .unwrap()
        .current_dir(&workspace.path)
        .args(["build", "--bin", "wc"])
        .assert()
        .success();
    let executable = workspace.path.join("target/ridge/debug/wc.escript");
    assert!(executable.is_file());
    for (name, text, lines, words, bytes) in [
        ("empty.txt", "", 0, 0, 0),
        ("four words.txt", "the quick brown fox\n", 1, 4, 19),
        ("spaces.txt", "  alpha   beta  ", 1, 2, 16),
        ("tabs.txt", "\talpha\t\tbeta\t", 1, 2, 13),
        ("crlf.txt", "a b\r\nc\td\r\n", 2, 4, 6),
        ("blank.txt", " \t\r\n\n", 2, 0, 2),
        ("middle.txt", "a\n\nb\n", 3, 2, 2),
        ("unterminated.txt", "a\nb", 2, 2, 2),
        ("controls.txt", "a\u{b}b\u{c}c", 1, 3, 5),
        ("carriage.txt", "a\rb\r", 1, 2, 4),
        ("utf8.txt", "café 世界\n", 1, 2, 12),
        ("nonascii-space.txt", "a\u{a0}b", 1, 1, 4),
        ("punctuation.txt", ". , !", 1, 3, 5),
    ] {
        write_file(&workspace.path, name, text);
        let expected = format!(
            "  {lines} lines\n  {words} words\n  {bytes} bytes (content only, newlines stripped)\n  {name}\n"
        );
        let output = std::process::Command::new("escript")
            .arg(&executable)
            .arg(name)
            .current_dir(&workspace.path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{name}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            expected,
            "{name}"
        );
    }
    Command::cargo_bin("ridge")
        .unwrap()
        .current_dir(&workspace.path)
        .args(["run", "--", "four words.txt"])
        .assert()
        .success()
        .stdout("  1 lines\n  4 words\n  19 bytes (content only, newlines stripped)\n  four words.txt\n");
}
