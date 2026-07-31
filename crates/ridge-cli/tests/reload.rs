//! End-to-end tests for `ridge reload --node` driving the REAL CLI binary:
//! build a fixture workspace, boot a named BEAM node, and let the command
//! itself compile, plan, ship the bundle, and parse the report. Also covers
//! the failure path: a failed apply must restore the pre-compile snapshot,
//! or the next run diffs against a build the node never ran.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{make_workspace, write_file};

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
    let output = ridge_cmd()
        .arg("build")
        .current_dir(&ws.path)
        .output()
        .expect("ridge build spawn failed");
    assert!(
        output.status.success(),
        "ridge build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let target = ws.path.join("target").join("ridge").join("debug");
    let mut found: Option<(PathBuf, String)> = None;
    for entry in walk(&target) {
        // `.core` intermediates share the stem — only a real `.beam` counts,
        // and its parent (debug/beam) is the dir the node needs on its path.
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
    use std::io::BufRead;
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
    cmd.output().expect("ridge reload spawn failed")
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
    let snap_path = ws
        .path
        .join("target")
        .join("ridge")
        .join("debug")
        .join("reload-snapshot.json");
    let snap_before = std::fs::read_to_string(&snap_path).expect("snapshot after build");

    // Edit, then aim the reload at a node that does not exist.
    let src = std::fs::read_to_string(ws.path.join("apps/demo/src/Counter.ridge")).expect("src");
    write_file(
        &ws.path,
        "apps/demo/src/Counter.ridge",
        &src.replace("state count: Int = 0", EDIT),
    );
    let cookie = "ridge_cli_e2e_cookie";
    let dead_name = format!("ridge_cli_dead_{}@127.0.0.1", std::process::id());
    let output = reload_cmd(&ws, &dead_name, cookie, false, false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a dead node must fail the reload: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("reload failed at the node"),
        "clean node-down error: {stderr}"
    );

    // The bug this guards: the compile advanced the on-disk snapshot to a
    // build the node never ran; without the restore, every retry mismatches.
    let snap_after = std::fs::read_to_string(&snap_path).expect("snapshot after failure");
    assert_eq!(
        snap_before, snap_after,
        "a failed apply must restore the pre-compile snapshot"
    );

    // And the retry against a live node then works on the first try.
    let node_name = format!("ridge_cli_retry_{}@127.0.0.1", std::process::id());
    let _node = boot_node(&node_name, cookie, &beam_dir, &beam_mod);
    let output = reload_cmd(&ws, &node_name, cookie, true, false);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "retry after restore must succeed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("migrated 1 actors"), "{stdout}");
}
