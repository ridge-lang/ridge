//! Execution regression for workspace members outside the conventional apps directory.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used)]

use assert_cmd::Command;

#[test]
fn run_declared_app_implicitly_and_by_manifest_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname = \"test\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname = \"executable\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/src/Main.ridge"),
        "pub fn main () -> Unit = ()\n",
    )
    .unwrap();
    for flags in [&[][..], &["--member", "executable"][..]] {
        Command::cargo_bin("ridge")
            .unwrap()
            .arg("run")
            .args(flags)
            .current_dir(dir.path())
            .assert()
            .success()
            .stdout("");
    }
}
