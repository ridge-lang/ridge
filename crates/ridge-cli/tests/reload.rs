//! End-to-end tests for `ridge reload --node` driving the REAL CLI binary:
//! build a fixture workspace, boot a named BEAM node, and let the command
//! itself compile, plan, ship the bundle, and parse the report. Also covers
//! failure recovery: a known rejection restores the previous snapshot,
//! while an uncertain result invalidates it before another reload can plan.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{make_workspace, write_file};
use ridge_driver::Profile;

const COUNTER_SRC: &str =
    "pub fn label () -> Text = \"counter\"\n\nactor Counter =\n    state count: Int = 0\n\n    on tick =\n        count <- count + 1\n\n    on count () -> Int =\n        count\n";

const EDIT: &str = "state count: Int = 0\n    state step: Int = 2";

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

fn otp_available() -> bool {
    which::which("erl").is_ok()
}

/// `ridge build` the workspace; returns the dir holding the actor beam and
/// the actor module's beam name.
fn build_and_find_beams(ws: &common::TempWorkspace) -> (PathBuf, String) {
    build_profile(ws, Profile::Debug)
}

fn build_profile(ws: &common::TempWorkspace, profile: Profile) -> (PathBuf, String) {
    let mut command = ridge_cmd();
    command.arg("build").current_dir(&ws.path);
    if profile == Profile::Release {
        command.arg("--release");
    }
    let output = command.output().expect("ridge build spawn failed");
    assert!(
        output.status.success(),
        "ridge build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let target = ws
        .path
        .join("target")
        .join("ridge")
        .join(profile.dir_name());
    let mut found: Option<(PathBuf, String)> = None;
    for entry in walk(&target) {
        // `.core` intermediates share the stem — only a real `.beam` counts,
        // and its parent (<profile>/beam) is the dir the node needs on its path.
        if entry.extension().and_then(|e| e.to_str()) != Some("beam") {
            continue;
        }
        if let Some(stem) = entry.file_stem().and_then(|s| s.to_str()) {
            if stem.ends_with("_counter") {
                let dir = entry.parent().expect("beam parent").to_path_buf();
                found = Some((dir, stem.to_owned()));
            }
        }
    }
    found.expect("an actor beam ending in _counter")
}

/// Recursive file listing (tiny local walkdir; the workspace tests do not
/// pull the walkdir crate).
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// A named node handle that kills the child on drop — a panicking test must
/// not leak its erl node (it would inherit cargo's stdout pipe on Windows
/// and hang the whole test pipeline).
struct NodeGuard {
    child: std::process::Child,
}

impl Drop for NodeGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Boot a named node running two-ticked counter, READY-signalled, blocking.
fn boot_node(node_name: &str, cookie: &str, beam_dir: &Path, beam_mod: &str) -> NodeGuard {
    use std::io::BufRead;

    let eval = format!(
        "H = {{ridge_handle, Pid, _}} = ridge_rt:spawn_actor('{beam_mod}', [], []),\n\
         ok = ridge_rt:send_op(H, {{tick}}),\n\
         ok = ridge_rt:send_op(H, {{tick}}),\n\
         2 = ridge_rt:ask(H, {{count}}, 5000),\n\
         register(counter_pid, Pid),\n\
         io:format(\"READY~n\"),\n\
         receive infinity -> ok end."
    );
    let mut child = std::process::Command::new("erl")
        .arg("-name")
        .arg(node_name)
        .arg("-setcookie")
        .arg(cookie)
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(eval)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn named erl node");
    // Read until READY (30 s cap). stderr is collected so a boot failure is
    // diagnosable from the panic message.
    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let (err_tx, err_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        let _ = err_tx.send(buf);
    });
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(line) if line.trim() == "READY" => break,
            Ok(_) => {}
            Err(e) => {
                let node_err = err_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap_or_default();
                panic!("node never printed READY: {e}\nnode stderr:\n{node_err}");
            }
        }
    }
    NodeGuard { child }
}

/// Probe the node state over rpc from a short-lived erl node.
fn probe_state(cookie: &str, node_name: &str, seq: &str) -> String {
    let eval = format!(
        "io:format(\"STATE=~p~n\", [rpc:call('{node_name}', sys, get_state, [rpc:call('{node_name}', erlang, whereis, [counter_pid])])])."
    );
    probe_eval(cookie, seq, &eval)
}

fn probe_eval(cookie: &str, seq: &str, eval: &str) -> String {
    let output = std::process::Command::new("erl")
        .arg("-name")
        .arg(format!(
            "ridge_cli_probe_{}_{seq}@127.0.0.1",
            std::process::id()
        ))
        .arg("-setcookie")
        .arg(cookie)
        .arg("-noshell")
        .arg("-eval")
        .arg(eval)
        .arg("-s")
        .arg("init")
        .arg("stop")
        .output()
        .expect("spawn probe");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `ridge reload --node` on the edited workspace; returns the process output.
fn reload_cmd(
    ws: &common::TempWorkspace,
    node_name: &str,
    cookie: &str,
    seed: bool,
    json: bool,
) -> std::process::Output {
    reload_profile(ws, node_name, cookie, seed, json, Profile::Debug)
}

fn reload_profile(
    ws: &common::TempWorkspace,
    node_name: &str,
    cookie: &str,
    seed: bool,
    json: bool,
    profile: Profile,
) -> std::process::Output {
    let mut cmd = ridge_cmd();
    cmd.arg("reload")
        .arg("--node")
        .arg(node_name)
        .arg("--cookie")
        .arg(cookie)
        .current_dir(&ws.path);
    if seed {
        cmd.arg("--seed");
    }
    if json {
        cmd.arg("--json").arg("report.json");
    }
    if profile == Profile::Release {
        cmd.arg("--release");
    }
    cmd.output().expect("ridge reload spawn failed")
}

fn profile_files(
    ws: &common::TempWorkspace,
    profile: Profile,
) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let root = ws.path.join("target/ridge").join(profile.dir_name());
    walk(&root)
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(&root).unwrap().to_owned();
            (relative, std::fs::read(path).unwrap())
        })
        .collect()
}

fn assert_profile_unchanged(
    ws: &common::TempWorkspace,
    profile: Profile,
    before: &std::collections::BTreeMap<PathBuf, Vec<u8>>,
) {
    let after = profile_files(ws, profile);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (path, bytes) in before {
        assert!(
            after.get(path) == Some(bytes),
            "changed {profile:?} artifact {}",
            path.display()
        );
    }
}

#[test]
fn reload_release_check_uses_selected_snapshot_and_explicit_override() {
    if !otp_available() {
        return;
    }
    let ws = make_workspace("Counter", COUNTER_SRC);
    build_profile(&ws, Profile::Release);
    let snapshot = ridge_driver::snapshot_path_for(&ws.path, "release");
    let custom = ws.path.join("selected-snapshot.json");
    std::fs::copy(&snapshot, &custom).unwrap();
    write_file(
        &ws.path,
        "apps/demo/src/Counter.ridge",
        &COUNTER_SRC.replace("\"counter\"", "\"edited\""),
    );
    let before = profile_files(&ws, Profile::Release);
    ridge_cmd()
        .args(["reload", "--check", "--release"])
        .current_dir(&ws.path)
        .assert()
        .success();
    assert_profile_unchanged(&ws, Profile::Release, &before);
    assert!(!ws.path.join("target/ridge/debug").exists());

    std::fs::remove_file(&snapshot).unwrap();
    let without_default = profile_files(&ws, Profile::Release);
    for release in [true, false] {
        let mut command = ridge_cmd();
        command
            .args(["reload", "--check", "--snapshot"])
            .arg(&custom)
            .current_dir(&ws.path);
        if release {
            command.arg("--release");
        }
        command.assert().success();
        assert_profile_unchanged(&ws, Profile::Release, &without_default);
        assert!(!ws.path.join("target/ridge/debug").exists());
    }
    ridge_cmd()
        .args(["reload", "--check", "--release"])
        .current_dir(&ws.path)
        .assert()
        .failure()
        .stderr(predicates::str::contains("ridge build --release"));
}

#[test]
fn reload_release_preserves_other_profile_and_uses_release_history() {
    if !otp_available() {
        return;
    }
    let ws = make_workspace(
        "Counter",
        &COUNTER_SRC.replace("\"counter\"", "\"debug-profile\""),
    );
    build_and_find_beams(&ws);
    let debug_before = profile_files(&ws, Profile::Debug);
    let release_v1 = COUNTER_SRC.replace(
        "state count: Int = 0",
        "state count: Int = 0\n    state saved: Int = 7",
    );
    write_file(&ws.path, "apps/demo/src/Counter.ridge", &release_v1);
    let (beam_dir, actor) = build_profile(&ws, Profile::Release);
    let snapshot = ridge_driver::snapshot_path_for(&ws.path, "release");
    let running_snapshot = std::fs::read_to_string(&snapshot).unwrap();
    let node = format!("ridge_cli_release_{}@127.0.0.1", std::process::id());
    let cookie = "ridge_cli_release_cookie";
    let _guard = boot_node(&node, cookie, &beam_dir, &actor);
    let next = COUNTER_SRC.replace("\"counter\"", "\"release-upgrade\"").replace(
        "state count: Int = 0",
        "state count: Int = 0\n    state step: Int = 0\n    migrate (old: Counter@1) -> Counter =\n        { count = old.count, step = old.saved }",
    );
    write_file(&ws.path, "apps/demo/src/Counter.ridge", &next);
    let rejected = reload_profile(&ws, &node, cookie, false, false, Profile::Release);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("reload rejected before code was replaced"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&snapshot).unwrap(),
        running_snapshot
    );
    assert_profile_unchanged(&ws, Profile::Debug, &debug_before);
    let old_state = probe_state(cookie, &node, "release_rejected");
    assert!(
        old_state.contains("count => 2") && old_state.contains("saved => 7"),
        "{old_state}"
    );

    let applied = reload_profile(&ws, &node, cookie, true, true, Profile::Release);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let state = probe_state(cookie, &node, "release_applied");
    assert!(
        state.contains("count => 2") && state.contains("step => 7"),
        "{state}"
    );
    assert!(!state.contains("saved =>"), "{state}");
    assert_profile_unchanged(&ws, Profile::Debug, &debug_before);
    let new_snapshot: ridge_driver::WorkspaceSnapshot =
        serde_json::from_str(&std::fs::read_to_string(&snapshot).unwrap()).unwrap();
    let version = ridge_driver::snapshot_vsn(&new_snapshot);
    let parent = actor.strip_suffix("_counter").unwrap();
    let metadata = probe_eval(cookie, "release_metadata", &format!(
        "io:format(\"VERSION=~p LABEL=~p~n\", [rpc:call('{node}', ridge_loader, current_version, []), rpc:call('{node}', '{parent}', label, [])])."
    ));
    assert!(
        metadata.contains(&format!("VERSION=<<\"{version}\">>")),
        "{metadata}"
    );
    assert!(metadata.contains("release-upgrade"), "{metadata}");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(ridge_driver::manifest_path_for(&ws.path, "release")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["new_vsn"], version);
}

#[test]
fn reload_release_uncertainty_invalidates_only_selected_snapshot() {
    if !otp_available() {
        return;
    }
    let ws = make_workspace("Counter", COUNTER_SRC);
    build_profile(&ws, Profile::Release);
    write_file(
        &ws.path,
        "target/ridge/debug/reload-snapshot.json",
        "other profile baseline",
    );
    let debug_before = profile_files(&ws, Profile::Debug);
    write_file(
        &ws.path,
        "apps/demo/src/Counter.ridge",
        &COUNTER_SRC.replace("state count: Int = 0", EDIT),
    );
    let dead = format!("ridge_cli_release_dead_{}@127.0.0.1", std::process::id());
    let output = reload_profile(
        &ws,
        &dead,
        "local_probe_cookie",
        false,
        false,
        Profile::Release,
    );
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("local reload snapshot is invalidated"),
        "{error}"
    );
    assert!(!ridge_driver::snapshot_path_for(&ws.path, "release").exists());
    assert_profile_unchanged(&ws, Profile::Debug, &debug_before);
    assert!(ridge_driver::manifest_path_for(&ws.path, "release").is_file());
    let missing = reload_profile(
        &ws,
        &dead,
        "local_probe_cookie",
        false,
        false,
        Profile::Release,
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("ridge build --release"));
    assert_profile_unchanged(&ws, Profile::Debug, &debug_before);
}

#[test]
fn reload_node_end_to_end_via_real_cli() {
    if !otp_available() {
        eprintln!("erl not on PATH — skipping reload_node_end_to_end_via_real_cli");
        return;
    }
    let ws = make_workspace("Counter", COUNTER_SRC);
    let (beam_dir, beam_mod) = build_and_find_beams(&ws);
    let node_name = format!("ridge_cli_e2e_{}@127.0.0.1", std::process::id());
    let cookie = "ridge_cli_e2e_cookie";
    let _node = boot_node(&node_name, cookie, &beam_dir, &beam_mod);

    // Edit: additive state field.
    let src = std::fs::read_to_string(ws.path.join("apps/demo/src/Counter.ridge")).expect("src");
    write_file(
        &ws.path,
        "apps/demo/src/Counter.ridge",
        &src.replace("state count: Int = 0", EDIT),
    );

    let output = reload_cmd(&ws, &node_name, cookie, true, true);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "reload must succeed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("reloaded 2 modules, migrated 1 actors"),
        "one-line summary: {stdout}"
    );
    assert!(stdout.contains("purges in 60s"), "purge schedule: {stdout}");

    let report_text = std::fs::read_to_string(ws.path.join("report.json")).expect("report.json");
    let report: serde_json::Value = serde_json::from_str(&report_text).expect("valid JSON report");
    assert_eq!(report["actors_migrated"], 1, "{report_text}");
    assert_eq!(report["purge"]["scheduled"], true, "{report_text}");

    let state = probe_state(cookie, &node_name, "a");
    assert!(state.contains("count => 2"), "state preserved: {state}");
    assert!(state.contains("step => 2"), "state migrated: {state}");
}

#[test]
fn reload_node_failure_restores_snapshot() {
    if !otp_available() {
        eprintln!("erl not on PATH — skipping reload_node_failure_restores_snapshot");
        return;
    }
    let ws = make_workspace("Counter", COUNTER_SRC);
    let (beam_dir, beam_mod) = build_and_find_beams(&ws);
    let cookie = "ridge_cli_e2e_cookie";
    let node_name = format!("ridge_cli_retry_{}@127.0.0.1", std::process::id());
    let _node = boot_node(&node_name, cookie, &beam_dir, &beam_mod);
    let snap_path = ws
        .path
        .join("target")
        .join("ridge")
        .join("debug")
        .join("reload-snapshot.json");
    let snap_before = std::fs::read_to_string(&snap_path).expect("snapshot after build");

    // The live v1 node has no version seed, so it rejects before loading code.
    let src = std::fs::read_to_string(ws.path.join("apps/demo/src/Counter.ridge")).expect("src");
    write_file(
        &ws.path,
        "apps/demo/src/Counter.ridge",
        &src.replace("state count: Int = 0", EDIT),
    );
    let output = reload_cmd(&ws, &node_name, cookie, false, false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "an unseeded node must reject the reload: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("reload rejected before code was replaced"),
        "clean rejection: {stderr}"
    );
    let running: ridge_driver::WorkspaceSnapshot = serde_json::from_str(&snap_before).unwrap();
    let expected_version = ridge_driver::snapshot_vsn(&running);
    assert!(
        stderr.contains(&format!("<<\"{expected_version}\">>")),
        "version text must stay readable on one diagnostic line: {stderr}"
    );

    // The bug this guards: the compile advanced the on-disk snapshot to a
    // build the node never ran; without the restore, every retry mismatches.
    let snap_after = std::fs::read_to_string(&snap_path).expect("snapshot after failure");
    assert_eq!(
        snap_before, snap_after,
        "a rejected apply must restore the pre-compile snapshot"
    );

    // Seeding the same v1 node makes the retry safe and successful.
    let output = reload_cmd(&ws, &node_name, cookie, true, false);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "retry after restore must succeed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("migrated 1 actors"), "{stdout}");
}

#[test]
fn reload_node_uncertain_failure_invalidates_snapshot() {
    if !otp_available() {
        eprintln!("erl not on PATH — skipping reload_node_uncertain_failure_invalidates_snapshot");
        return;
    }
    let ws = make_workspace("Counter", COUNTER_SRC);
    build_and_find_beams(&ws);
    let src = std::fs::read_to_string(ws.path.join("apps/demo/src/Counter.ridge")).unwrap();
    write_file(
        &ws.path,
        "apps/demo/src/Counter.ridge",
        &src.replace("state count: Int = 0", EDIT),
    );
    let dead_name = format!("ridge_cli_dead_{}@127.0.0.1", std::process::id());
    let output = reload_cmd(&ws, &dead_name, "ridge_cli_e2e_cookie", false, false);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("local reload snapshot is invalidated"),
        "{stderr}"
    );
    assert!(stderr.contains("rebuild and restart"), "{stderr}");
    assert!(!ws
        .path
        .join("target/ridge/debug/reload-snapshot.json")
        .exists());
}
