//! Reload mode errors must stop before workspace or runtime side effects.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::error::ErrorKind;
use ridge_cli::{Cli, RidgeCommand};

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_invocation_from(["ridge", "reload"].into_iter().chain(args.iter().copied()))
}

#[test]
fn reload_requires_exactly_one_mode() {
    assert_eq!(
        parse(&[]).unwrap_err().kind(),
        ErrorKind::MissingRequiredArgument
    );
    for args in [
        ["--check", "--node", "app@host"],
        ["--node", "app@host", "--check"],
    ] {
        let error = parse(&args).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        let message = error.to_string();
        assert!(
            message.contains("--check") && message.contains("--node"),
            "{message}"
        );
    }
}

#[test]
fn reload_rejects_options_from_the_other_mode() {
    for option in [
        vec!["--cookie", "secret"],
        vec!["--seed"],
        vec!["--timeout", "30000"],
        vec!["--purge-after", "60"],
        vec!["--json"],
        vec!["--json", "report.json"],
    ] {
        let mut args = vec!["--check"];
        args.extend(option.iter().copied());
        assert!(
            parse(&args).is_err(),
            "check accepted apply options: {args:?}"
        );
        assert!(
            parse(&option).is_err(),
            "options selected an implicit mode: {option:?}"
        );
    }
    assert!(parse(&["--node", "app@host", "--snapshot", "old.json"]).is_err());
    assert!(parse(&["--snapshot", "old.json"]).is_err());
}

#[test]
fn reload_accepts_each_mode_with_its_options_and_defaults() {
    let RidgeCommand::Reload(check) = parse(&["--check"]).unwrap().command else {
        panic!("expected reload");
    };
    assert!(check.check && check.node.is_none());
    assert_eq!(check.timeout, 30000);
    assert_eq!(check.purge_after, 60);
    parse(&["--check", "--snapshot", "old.json", "--release"]).unwrap();
    parse(&["--node", "app@host"]).unwrap();
    let RidgeCommand::Reload(apply) = parse(&[
        "--node",
        "app@host",
        "--cookie",
        "secret",
        "--seed",
        "--release",
        "--timeout",
        "123",
        "--purge-after",
        "0",
        "--json",
        "report.json",
    ])
    .unwrap()
    .command
    else {
        panic!("expected reload");
    };
    assert_eq!(apply.timeout, 123);
    assert_eq!(apply.purge_after, 0);
    assert!(apply.seed && apply.release);
    assert_eq!(apply.json.unwrap().to_str(), Some("report.json"));
    let RidgeCommand::Reload(stdout) = parse(&["--node", "app@host", "--json"]).unwrap().command
    else {
        panic!("expected reload");
    };
    assert!(stdout.json.unwrap().as_os_str().is_empty());
}

#[test]
fn reload_timer_boundaries_are_checked_during_argument_parsing() {
    for (flag, max) in [
        ("--timeout", 4_294_967_295_u64),
        ("--purge-after", 18_446_744_073_709_551_u64),
    ] {
        for value in [0, 1, max] {
            parse(&["--node", "app@host", flag, &value.to_string()]).unwrap();
        }
        for value in [max + 1, u64::MAX] {
            let error = parse(&["--node", "app@host", flag, &value.to_string()])
                .expect_err("out-of-range timer must fail before execution");
            assert_eq!(error.kind(), ErrorKind::ValueValidation);
            let message = error.to_string();
            assert!(
                message.contains(flag) && message.contains(&value.to_string()),
                "{message}"
            );
        }
    }
}

#[test]
fn reload_timer_errors_preserve_files_without_workspace_or_runtime_access() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let files = [
        ("ridge.toml", "deliberately invalid manifest"),
        ("target/ridge/debug/reload-snapshot.json", "debug baseline"),
        (
            "target/ridge/release/reload-snapshot.json",
            "release baseline",
        ),
        (
            "target/ridge/debug/reload-pending.json",
            "pending operation",
        ),
        (
            "target/ridge/debug/upgrade.manifest.json",
            "previous manifest",
        ),
        ("target/ridge/debug/beam/old.beam", "previous bytecode"),
    ];
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    for (flag, value) in [
        ("--timeout", "4294967296"),
        ("--timeout", "18446744073709551615"),
        ("--purge-after", "18446744073709552"),
        ("--purge-after", "18446744073709551615"),
    ] {
        let output = assert_cmd::Command::cargo_bin("ridge")
            .unwrap()
            .args(["reload", "--node", "app@host", "--release", flag, value])
            .current_dir(root)
            .env("PATH", "")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(flag) && error.contains(value), "{error}");
        for (path, text) in files {
            assert_eq!(
                std::fs::read(root.join(path)).unwrap(),
                text.as_bytes(),
                "{path}"
            );
        }
        assert!(!root.join("target/ridge/debug.reload.lock").exists());
        assert!(!root.join("target/ridge/release.reload.lock").exists());
    }
}

#[test]
fn direct_reload_callers_reject_invalid_timers_before_reading_a_workspace() {
    const CHILD: &str = "RIDGE_TEST_RELOAD_INVALID_TIMER";
    if let Some(flag) = std::env::var_os(CHILD) {
        let flag = flag.to_string_lossy();
        let RidgeCommand::Reload(mut args) = parse(&["--node", "app@host"]).unwrap().command else {
            panic!("expected reload");
        };
        if flag == "--timeout" {
            args.timeout = 4_294_967_296;
        } else {
            args.purge_after = 18_446_744_073_709_552;
        }
        assert!(ridge_cli::cmd::reload::execute(&args, &std::env::current_dir().unwrap()).is_err());
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("ridge.toml"), "invalid manifest").unwrap();
    for flag in ["--timeout", "--purge-after"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "direct_reload_callers_reject_invalid_timers_before_reading_a_workspace",
                "--nocapture",
            ])
            .env(CHILD, flag)
            .current_dir(directory.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(flag) && error.contains("must be"),
            "numeric validation was bypassed: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("ridge.toml")).unwrap(),
            "invalid manifest"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[test]
fn direct_reload_callers_cannot_bypass_mode_validation() {
    let directory = tempfile::tempdir().unwrap();
    for option in ["node", "cookie", "seed", "json", "timeout", "purge"] {
        let RidgeCommand::Reload(mut args) = parse(&["--check"]).unwrap().command else {
            panic!("expected reload");
        };
        match option {
            "node" => args.node = Some("app@host".to_owned()),
            "cookie" => args.cookie = Some("secret".to_owned()),
            "seed" => args.seed = true,
            "json" => args.json = Some("report.json".into()),
            "timeout" => args.timeout = 1,
            "purge" => args.purge_after = 0,
            _ => unreachable!(),
        }
        assert!(matches!(
            ridge_cli::cmd::reload::execute(&args, directory.path()),
            Err(ridge_cli::CliError::AlreadyReported)
        ));
    }
    let RidgeCommand::Reload(mut args) = parse(&["--node", "app@host"]).unwrap().command else {
        panic!("expected reload");
    };
    args.snapshot = Some("old.json".into());
    assert!(matches!(
        ridge_cli::cmd::reload::execute(&args, directory.path()),
        Err(ridge_cli::CliError::AlreadyReported)
    ));
    assert!(std::fs::read_dir(directory.path())
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn invalid_reload_invocations_preserve_workspace_files_without_a_runtime() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path();
    let files = [
        (
            "ridge.toml",
            "[workspace]\nname = \"reload-args\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
        ),
        (
            "apps/demo/ridge.toml",
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
        ),
        (
            "apps/demo/src/counter.ridge",
            "pub fn label () -> Text = \"edited\"\n",
        ),
        (
            "target/ridge/debug/reload-snapshot.json",
            "running debug baseline",
        ),
        (
            "target/ridge/release/reload-snapshot.json",
            "running release baseline",
        ),
        (
            "target/ridge/debug/upgrade.manifest.json",
            "previous manifest",
        ),
        ("target/ridge/debug/beam/previous.beam", "previous bytecode"),
    ];
    for (path, content) in &files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    for args in [
        vec!["--check", "--node", "app@127.0.0.1"],
        vec!["--check", "--json", "report.json"],
        vec!["--check", "--seed"],
        vec!["--check", "--timeout", "30000"],
        vec!["--check", "--purge-after", "60"],
        vec!["--node", "app@127.0.0.1", "--snapshot", "old.json"],
        vec![],
    ] {
        assert_cmd::Command::cargo_bin("ridge")
            .unwrap()
            .arg("reload")
            .args(&args)
            .current_dir(root)
            .env("PATH", "")
            .assert()
            .code(2);
        for (path, content) in &files {
            assert_eq!(
                std::fs::read(root.join(path)).unwrap(),
                content.as_bytes(),
                "changed {path} for {args:?}"
            );
        }
        assert!(!root.join("report.json").exists());
    }
}
