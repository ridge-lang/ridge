//! End-to-end hot-reload battery: a live node running OLD code applies an
//! upgrade manifest produced from NEW code, through `ridge_loader:apply/2`.
//!
//! Covered: state preserved on body changes, additive state-field migration,
//! rename migration, the base-version gate, actor `migrate` hooks, lazy
//! mailbox migration of stale-tagged records, multi-step migration chains
//! across two successive reloads, and the drop policy for non-migratable
//! messages. Gated on `beam-runtime` (real OTP) plus a `which` guard for
//! `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ridge_driver::{
    compile_workspace, manifest_path_for, plan_reload, snapshot_path_for, snapshot_vsn,
    CheckOptions, CompileOptions, EmitArtefacts, ReloadPlan, WorkspaceSnapshot,
};

struct ReloadNode {
    child: std::process::Child,
    old_snapshot: WorkspaceSnapshot,
    /// Read by the distributed-probe case; kept here so the harness stays
    /// uniform across cases.
    #[allow(dead_code)]
    base_vsn: String,
    manifest_path: std::path::PathBuf,
}

/// Absolute path to the counter fixture's single source file.
fn counter_source(ws: &common::TempWorkspace) -> std::path::PathBuf {
    ws.path.join("apps/demo/src/Counter.ridge")
}

/// Skip cleanly when OTP is not installed.
fn otp_available() -> bool {
    which::which("erlc").is_ok() && which::which("erl").is_ok()
}

/// The fixture actor's target module: locate the `_counter` beam on disk
/// (actor beams fan out from the parent module beam as `<parent>_<actor_lc>`).
fn actor_beam_of(beam_dir: &std::path::Path) -> String {
    std::fs::read_dir(beam_dir)
        .expect("read beam dir")
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
        })
        .find(|stem| stem.ends_with("_counter"))
        .expect("an actor beam ending in _counter")
}

/// Compile v1, boot the polling node, return the handle. `eval_extra` is
/// appended before `halt(0)` (for the mismatch case).
fn boot_v1(ws: &common::TempWorkspace, eval_extra: &str) -> ReloadNode {
    let dir = ws.path.clone();
    let artefacts =
        compile_workspace(CompileOptions::new(dir.clone()).with_emit(EmitArtefacts::Beam))
            .expect("v1 compile");
    assert!(
        !artefacts
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, ridge_diagnostics::Severity::Error)),
        "v1 must compile clean: {:?}",
        artefacts.diagnostics
    );
    let snap_path = snapshot_path_for(&dir, "debug");
    let old_snapshot: WorkspaceSnapshot =
        serde_json::from_str(&std::fs::read_to_string(&snap_path).expect("snapshot"))
            .expect("parse snapshot");
    let base_vsn = snapshot_vsn(&old_snapshot);
    let manifest_path = manifest_path_for(&dir, "debug");
    // Stale manifests from an earlier run must not short-circuit the poll.
    let _ = std::fs::remove_file(&manifest_path);
    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    let beam_mod = actor_beam_of(&beam_dir);
    // Erlang string literals treat backslashes as escapes — forward slashes.
    let manifest_fwd = manifest_path.to_string_lossy().replace('\\', "/");
    let eval = format!(
        "persistent_term:put(ridge_loader_vsn, <<\"{base}\">>),\n\
         H = {{ridge_handle, Pid, _}} = ridge_rt:spawn_actor('{beam_mod}', [], []),\n\
         ok = ridge_rt:send_op(H, {{tick}}),\n\
         ok = ridge_rt:send_op(H, {{tick}}),\n\
         2 = ridge_rt:ask(H, {{count}}, 5000),\n\
         W = fun W() -> case filelib:is_file(\"{manifest}\") of true -> ok; false -> timer:sleep(50), W() end end,\n\
         W(),\n\
         R = ridge_loader:apply(\"{manifest}\", <<\"{base}\">>),\n\
         io:format(\"APPLY=~p~n\", [R]),\n\
         io:format(\"STATE=~p~n\", [sys:get_state(Pid)]),\n\
         io:format(\"ASK=~p~n\", [ridge_rt:ask(H, {{count}}, 5000)]),\n\
         {eval_extra}\n\
         halt(0).",
        base = base_vsn,
        beam_mod = beam_mod,
        manifest = manifest_fwd,
        eval_extra = eval_extra,
    );
    let child = std::process::Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(eval)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn erl");
    ReloadNode {
        child,
        old_snapshot,
        base_vsn,
        manifest_path,
    }
}

/// Edit source, recompile, plan (writes the manifest the node polls for).
/// Returns the plan so a case can inspect the manifest entries.
fn apply_edit(
    node: &ReloadNode,
    ws: &common::TempWorkspace,
    edit: impl FnOnce(&str) -> String,
) -> ReloadPlan {
    let src_path = counter_source(ws);
    let src = std::fs::read_to_string(&src_path).expect("src");
    std::fs::write(&src_path, edit(&src)).expect("write");
    let artefacts =
        compile_workspace(CompileOptions::new(ws.path.clone()).with_emit(EmitArtefacts::Beam))
            .expect("recompile");
    assert!(
        !artefacts
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, ridge_diagnostics::Severity::Error)),
        "edit must compile clean: {:?}",
        artefacts.diagnostics
    );
    let plan = plan_reload(
        &node.old_snapshot,
        CheckOptions::new(ws.path.clone()),
        &node.manifest_path,
    )
    .expect("plan_reload");
    assert!(
        plan.manifest.is_some(),
        "edit must be reloadable: {:?}",
        plan.report
    );
    plan
}

/// Join the node (60 s cap) and return its stdout.
fn join(mut node: ReloadNode) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut killed = false;
    loop {
        match node.child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            _ => {
                let _ = node.child.kill();
                killed = true;
                break;
            }
        }
    }
    let out = node.child.wait_with_output().expect("output");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !killed,
        "node did not finish within 60s\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

#[test]
fn reload_preserves_state_on_body_change() {
    if !otp_available() {
        eprintln!("erl/erlc not on PATH — skipping reload_preserves_state_on_body_change");
        return;
    }
    let ws = common::make_counter_workspace();
    let node = boot_v1(&ws, "");
    apply_edit(&node, &ws, |src| {
        src.replace("count <- count + 1", "count <- 1 + count")
    });
    let out = join(node);
    assert!(out.contains("APPLY={ok,#{"), "apply must succeed: {out}");
    assert!(
        out.contains("actors_migrated => 0"),
        "no state shape change, no migration: {out}"
    );
    assert!(
        out.contains("modules_loaded => 2"),
        "body change reloads the module and its actor: {out}"
    );
    assert!(
        out.contains("STATE=#{count => 2}"),
        "state preserved: {out}"
    );
    assert!(out.contains("ASK=2"), "new code serves requests: {out}");
}

#[test]
fn reload_migrates_added_state_field_with_default() {
    if !otp_available() {
        eprintln!("erl/erlc not on PATH — skipping reload_migrates_added_state_field_with_default");
        return;
    }
    let ws = common::make_counter_workspace();
    let node = boot_v1(&ws, "");
    apply_edit(&node, &ws, |src| {
        src.replace(
            "state count: Int = 0",
            "state count: Int = 0\n    state step: Int = 2",
        )
    });
    let out = join(node);
    assert!(out.contains("APPLY={ok,#{"), "apply must succeed: {out}");
    assert!(
        out.contains("actors_migrated => 1"),
        "one actor migrated: {out}"
    );
    assert!(
        out.contains("count => 2"),
        "existing field kept its value: {out}"
    );
    assert!(
        out.contains("step => 2"),
        "added field got its default: {out}"
    );
    assert!(out.contains("ASK=2"), "new code serves requests: {out}");
}

#[test]
fn reload_migrates_renamed_state_field() {
    if !otp_available() {
        eprintln!("erl/erlc not on PATH — skipping reload_migrates_renamed_state_field");
        return;
    }
    let ws = common::make_counter_workspace();
    let node = boot_v1(&ws, "");
    apply_edit(&node, &ws, |src| {
        src.replace("state count: Int = 0", "state total: Int = 0")
            .replace("count <- count + 1", "total <- total + 1")
            .replace("        count\n", "        total\n")
    });
    let out = join(node);
    assert!(out.contains("APPLY={ok,#{"), "apply must succeed: {out}");
    assert!(
        out.contains("actors_migrated => 1"),
        "one actor migrated: {out}"
    );
    assert!(
        out.contains("total => 2"),
        "renamed field kept its value: {out}"
    );
    assert!(
        !out.contains("count =>"),
        "old field name is gone from the state: {out}"
    );
    assert!(out.contains("ASK=2"), "new code serves requests: {out}");
}

#[test]
fn reload_rejects_base_vsn_mismatch() {
    if !otp_available() {
        eprintln!("erl/erlc not on PATH — skipping reload_rejects_base_vsn_mismatch");
        return;
    }
    let ws = common::make_counter_workspace();
    // After the good apply the node is at the NEW version, so re-applying the
    // same manifest (stale base) must be rejected and leave the state alone.
    let manifest_fwd = manifest_path_for(&ws.path, "debug")
        .to_string_lossy()
        .replace('\\', "/");
    let eval_extra = format!(
        "R2 = ridge_loader:apply(\"{manifest_fwd}\", <<\"deadbeefdeadbeef0000\">>),\n\
         io:format(\"APPLY2=~p~n\", [R2]),\n"
    );
    let node = boot_v1(&ws, &eval_extra);
    apply_edit(&node, &ws, |src| {
        src.replace(
            "state count: Int = 0",
            "state count: Int = 0\n    state step: Int = 2",
        )
    });
    let out = join(node);
    assert!(out.contains("APPLY={ok,#{"), "first apply succeeds: {out}");
    assert!(
        out.contains("APPLY2={error,{base_version_mismatch"),
        "stale base version rejected: {out}"
    );
    assert!(
        out.contains("step => 2"),
        "state reflects the good apply only: {out}"
    );
}

// ── Distributed probe path (the exact mechanism `ridge run --reload` uses) ───

/// Boot a NAMED node that seeds state, registers the actor pid, prints READY,
/// and blocks forever (killed by the caller).
fn boot_named_v1(ws: &common::TempWorkspace, node_name: &str, cookie: &str) -> ReloadNode {
    let dir = ws.path.clone();
    let artefacts =
        compile_workspace(CompileOptions::new(dir.clone()).with_emit(EmitArtefacts::Beam))
            .expect("v1 compile");
    assert!(
        !artefacts
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, ridge_diagnostics::Severity::Error)),
        "v1 must compile clean: {:?}",
        artefacts.diagnostics
    );
    let snap_path = snapshot_path_for(&dir, "debug");
    let old_snapshot: WorkspaceSnapshot =
        serde_json::from_str(&std::fs::read_to_string(&snap_path).expect("snapshot"))
            .expect("parse snapshot");
    let base_vsn = snapshot_vsn(&old_snapshot);
    let manifest_path = manifest_path_for(&dir, "debug");
    let _ = std::fs::remove_file(&manifest_path);
    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    let beam_mod = actor_beam_of(&beam_dir);
    let eval = format!(
        "persistent_term:put(ridge_loader_vsn, <<\"{base}\">>),\n\
         H = {{ridge_handle, Pid, _}} = ridge_rt:spawn_actor('{beam_mod}', [], []),\n\
         ok = ridge_rt:send_op(H, {{tick}}),\n\
         ok = ridge_rt:send_op(H, {{tick}}),\n\
         2 = ridge_rt:ask(H, {{count}}, 5000),\n\
         register(counter_pid, Pid),\n\
         io:format(\"READY~n\"),\n\
         receive infinity -> ok end.",
        base = base_vsn,
        beam_mod = beam_mod,
    );
    let child = std::process::Command::new("erl")
        .arg("-name")
        .arg(node_name)
        .arg("-setcookie")
        .arg(cookie)
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(eval)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn named erl node");
    ReloadNode {
        child,
        old_snapshot,
        base_vsn,
        manifest_path,
    }
}

/// Read the node's stdout until the READY line (30 s cap).
fn await_ready(node: &mut ReloadNode) {
    use std::io::BufRead;
    let stdout = node.child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let deadline = std::time::Duration::from_secs(30);
    loop {
        match rx.recv_timeout(deadline) {
            Ok(line) if line.trim() == "READY" => return,
            Ok(_) => {}
            Err(e) => panic!("node never printed READY: {e}"),
        }
    }
}

/// Run a short-lived probe node against the named dev node and return its
/// stdout (the probe mechanism is identical to the CLI's).
fn probe(cookie: &str, eval: &str, seq: u64) -> String {
    let output = std::process::Command::new("erl")
        .arg("-name")
        .arg(format!(
            "ridge_probe_e2e_{}_{seq}@127.0.0.1",
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

#[test]
fn reload_via_rpc_probe() {
    if !otp_available() {
        eprintln!("erl/erlc not on PATH — skipping reload_via_rpc_probe");
        return;
    }
    let ws = common::make_counter_workspace();
    let node_name = format!("ridge_e2e_{}@127.0.0.1", std::process::id());
    let cookie = "ridge_e2e_cookie";
    let mut node = boot_named_v1(&ws, &node_name, cookie);
    await_ready(&mut node);

    apply_edit(&node, &ws, |src| {
        src.replace(
            "state count: Int = 0",
            "state count: Int = 0\n    state step: Int = 2",
        )
    });

    let manifest_fwd = node.manifest_path.to_string_lossy().replace('\\', "/");
    let apply_eval = format!(
        "case rpc:call('{node_name}', ridge_loader, apply, [\"{manifest_fwd}\", <<\"{base}\">>]) of \
            {{ok, Rep}} -> io:format(\"RIDGE_RELOAD_OK ~w ~w ~w~n\", [maps:get(modules_loaded, Rep), maps:get(actors_migrated, Rep), maps:get(duration_ms, Rep)]); \
            Err -> io:format(\"RIDGE_RELOAD_ERR ~p~n\", [Err]) \
        end.",
        base = node.base_vsn,
    );
    let out = probe(cookie, &apply_eval, 1);
    assert!(
        out.contains("RIDGE_RELOAD_OK 2 1 "),
        "2 modules loaded, 1 actor migrated: {out}"
    );

    let state_eval = format!(
        "io:format(\"STATE=~p~n\", [rpc:call('{node_name}', sys, get_state, [rpc:call('{node_name}', erlang, whereis, [counter_pid])])])."
    );
    let out = probe(cookie, &state_eval, 2);
    assert!(out.contains("count => 2"), "count preserved: {out}");
    assert!(out.contains("step => 2"), "step migrated: {out}");

    let _ = node.child.kill();
    let _ = node.child.wait();
}

// ── Migrate hooks and lazy mailbox migration (store fixture) ─────────────────

/// The store fixture's single source file.
fn store_source(ws: &common::TempWorkspace) -> std::path::PathBuf {
    ws.path.join("apps/demo/src/Store.ridge")
}

/// `(parent_beam, actor_beam)` discovered from the beam dir. The parent keeps
/// the module's capital (`ridge_demo_Store`), so only the actor stem
/// (`ridge_demo_Store_store`) matches the lowercase `_store` suffix.
fn store_beams(beam_dir: &std::path::Path) -> (String, String) {
    let stems: Vec<String> = std::fs::read_dir(beam_dir)
        .expect("read beam dir")
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
        })
        .filter(|s| s.starts_with("ridge_demo"))
        .collect();
    let actor = stems
        .iter()
        .find(|s| s.ends_with("_store"))
        .expect("actor beam")
        .clone();
    let parent = stems
        .iter()
        .find(|s| !s.ends_with("_store"))
        .expect("parent beam")
        .clone();
    (parent, actor)
}

/// Compile v1 of the store fixture and snapshot it (mirrors `boot_v1`'s front
/// half, without booting a node).
fn compile_store_v1(ws: &common::TempWorkspace) -> (WorkspaceSnapshot, std::path::PathBuf) {
    let artefacts =
        compile_workspace(CompileOptions::new(ws.path.clone()).with_emit(EmitArtefacts::Beam))
            .expect("v1 compile");
    assert!(
        !artefacts
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, ridge_diagnostics::Severity::Error)),
        "v1 must compile clean: {:?}",
        artefacts.diagnostics
    );
    let snap: WorkspaceSnapshot = serde_json::from_str(
        &std::fs::read_to_string(snapshot_path_for(&ws.path, "debug")).expect("snapshot"),
    )
    .expect("parse snapshot");
    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("beam dir")
        .to_path_buf();
    (snap, beam_dir)
}

/// A node whose stdout is streamed line-by-line from a reader thread, so a
/// test can wait for mid-stream markers (multi-apply flows). `lines` keeps
/// every line seen so far: `wait_marker` consumes the channel, so the final
/// assertions read the full log from here instead.
struct StreamedNode {
    child: std::process::Child,
    rx: std::sync::mpsc::Receiver<String>,
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
    stdout_thread: Option<std::thread::JoinHandle<()>>,
    stderr_thread: Option<std::thread::JoinHandle<()>>,
}

/// Spawn an anonymous `-noshell` node running `eval`, streaming stdout and
/// collecting stderr in the background.
fn spawn_streamed(beam_dir: &std::path::Path, eval: &str) -> StreamedNode {
    let mut child = std::process::Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(eval)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn erl");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let lines: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let line_sink = std::sync::Arc::clone(&lines);
    let mut stdout = child.stdout.take().expect("stdout");
    let stdout_thread = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(&mut stdout).lines() {
            match line {
                Ok(l) => {
                    if let Ok(mut guard) = line_sink.lock() {
                        guard.push(l.clone());
                    }
                    if tx.send(l).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
    let stderr: std::sync::Arc<std::sync::Mutex<String>> = Default::default();
    let err_sink = std::sync::Arc::clone(&stderr);
    let mut stderr_pipe = child.stderr.take().expect("stderr");
    let stderr_thread = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let _ = stderr_pipe.read_to_string(&mut buf);
        if let Ok(mut guard) = err_sink.lock() {
            *guard = buf;
        }
    });
    StreamedNode {
        child,
        rx,
        lines,
        stderr,
        stdout_thread: Some(stdout_thread),
        stderr_thread: Some(stderr_thread),
    }
}

/// Wait (bounded) for a stdout line starting with `prefix`; returns the line.
fn wait_marker(node: &StreamedNode, prefix: &str, timeout: std::time::Duration) -> String {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match node.rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(line) if line.starts_with(prefix) => return line,
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "node produced no `{prefix}` marker within {timeout:?}"
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("node's stdout closed before the `{prefix}` marker")
            }
        }
    }
}

/// Join a streamed node (60 s cap) and return `(stdout_lines, stderr)`. The
/// reader threads are joined first so late lines and the full stderr are in
/// before the caller asserts on them.
fn join_streamed(mut node: StreamedNode) -> (Vec<String>, String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match node.child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            _ => {
                let _ = node.child.kill();
                let _ = node.child.wait();
                break;
            }
        }
    }
    if let Some(t) = node.stdout_thread.take() {
        let _ = t.join();
    }
    if let Some(t) = node.stderr_thread.take() {
        let _ = t.join();
    }
    let lines = node.lines.lock().map(|g| g.clone()).unwrap_or_default();
    let stderr = node.stderr.lock().map(|g| g.clone()).unwrap_or_default();
    (lines, stderr)
}

/// Edit + recompile + plan, writing the manifest the node polls for; the
/// plan's old snapshot is supplied explicitly (multi-apply flows rebase it).
fn apply_store_edit(
    old_snapshot: &WorkspaceSnapshot,
    ws: &common::TempWorkspace,
    edit: impl FnOnce(&str) -> String,
) -> ReloadPlan {
    let src = std::fs::read_to_string(store_source(ws)).expect("src");
    std::fs::write(store_source(ws), edit(&src)).expect("write edit");
    let artefacts =
        compile_workspace(CompileOptions::new(ws.path.clone()).with_emit(EmitArtefacts::Beam))
            .expect("recompile");
    assert!(
        !artefacts
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, ridge_diagnostics::Severity::Error)),
        "edit must compile clean: {:?}",
        artefacts.diagnostics
    );
    let plan = plan_reload(
        old_snapshot,
        CheckOptions::new(ws.path.clone()),
        &manifest_path_for(&ws.path, "debug"),
    )
    .expect("plan_reload");
    assert!(
        plan.manifest.is_some(),
        "edit must be reloadable: {:?}",
        plan.report
    );
    plan
}

#[test]
fn reload_applies_actor_migrate_hook() {
    if !otp_available() {
        eprintln!("erl/erlc not on PATH — skipping reload_applies_actor_migrate_hook");
        return;
    }
    let ws = common::make_counter_workspace();
    let node = boot_v1(&ws, "");
    // The hook's fill (`step = 1`) differs from the field default (`0`) on
    // purpose: seeing 1 in the post-upgrade state proves the hook ran, not
    // the mechanical default fill.
    let plan = apply_edit(&node, &ws, |src| {
        src.replace(
            "state count: Int = 0",
            "state count: Int = 0\n    state step: Int = 0\n    migrate (old: Counter@1) -> Counter =\n        { count = old.count, step = 1 }",
        )
    });
    let manifest = plan.manifest.as_ref().expect("manifest");
    let actor = manifest.actors.first().expect("hook actor entry");
    assert!(actor.migrate_hook, "hook dispatch: {actor:?}");
    assert_ne!(actor.old_state_hash, actor.new_state_hash);
    let out = join(node);
    assert!(out.contains("APPLY={ok,"), "{out}");
    assert!(
        out.contains("step => 1"),
        "hook filled the new field: {out}"
    );
    assert!(out.contains("count => 2"), "state preserved: {out}");
    assert!(out.contains("ASK=2"), "{out}");
}

#[test]
fn reload_migrates_mailbox_record_lazily() {
    if !otp_available() {
        eprintln!("erl/erlc not on PATH — skipping reload_migrates_mailbox_record_lazily");
        return;
    }
    let ws = common::make_store_workspace();
    let (snap, beam_dir) = compile_store_v1(&ws);
    let base = snapshot_vsn(&snap);
    let manifest = manifest_path_for(&ws.path, "debug");
    let _ = std::fs::remove_file(&manifest);
    let (parent, actor) = store_beams(&beam_dir);
    let manifest_fwd = manifest.to_string_lossy().replace('\\', "/");
    // Suspend the actor, cast a v1-tagged note (it queues), then upgrade: the
    // loader's suspend+resume leaves the actor running, so the queued cast is
    // delivered to the NEW handle_cast, which migrates it at receive.
    let eval = format!(
        "persistent_term:put(ridge_loader_vsn, <<\"{base}\">>),\n\
         H = {{ridge_handle, Pid, _}} = ridge_rt:spawn_actor('{actor}', [], []),\n\
         H1 = maps:get('Note', '{parent}':'__ridge_record_versions'()),\n\
         ok = sys:suspend(Pid),\n\
         ok = ridge_rt:send_op(H, {{store, #{{'__ridge_v' => {{'{parent}', 'Note', H1}}, text => <<\"hello\">>}}}}),\n\
         W = fun W() -> case filelib:is_file(\"{manifest}\") of true -> ok; false -> timer:sleep(50), W() end end,\n\
         W(),\n\
         R = ridge_loader:apply(\"{manifest}\", <<\"{base}\">>),\n\
         io:format(\"APPLY=~p~n\", [R]),\n\
         io:format(\"GOT=~p~n\", [ridge_rt:ask(H, {{get}}, 5000)]),\n\
         io:format(\"MIGRATED=~p~n\", [ridge_rt:migration_count()]),\n\
         halt(0).",
        base = base,
        actor = actor,
        parent = parent,
        manifest = manifest_fwd,
    );
    let node = spawn_streamed(&beam_dir, &eval);
    apply_store_edit(&snap, &ws, |src| src.replace("text", "body"));
    let (lines, _stderr) = join_streamed(node);
    let out = lines.join("\n");
    assert!(out.contains("APPLY={ok,"), "{out}");
    assert!(
        out.contains("body => <<\"hello\">>"),
        "stale note migrated at receive: {out}"
    );
    assert!(!out.contains("text =>"), "old field name gone: {out}");
    let migrated = lines
        .iter()
        .find_map(|l| l.strip_prefix("MIGRATED="))
        .expect("MIGRATED marker");
    assert!(
        migrated.parse::<u64>().unwrap_or(0) >= 1,
        "lazy count bumped: {out}"
    );
}

#[test]
fn reload_migrates_record_chain_across_two_reloads() {
    if !otp_available() {
        eprintln!(
            "erl/erlc not on PATH — skipping reload_migrates_record_chain_across_two_reloads"
        );
        return;
    }
    let ws = common::make_store_workspace();
    let (snap_v1, beam_dir) = compile_store_v1(&ws);
    let base = snapshot_vsn(&snap_v1);
    let manifest = manifest_path_for(&ws.path, "debug");
    let _ = std::fs::remove_file(&manifest);
    let (parent, actor) = store_beams(&beam_dir);
    let manifest_fwd = manifest.to_string_lossy().replace('\\', "/");
    // Two upgrades in one node: after APPLY1 the test rebases the manifest
    // (delete + rewrite), which the node's D/W polls ride to APPLY2. The
    // second cast still carries the v1 tag, so the v3 beam must migrate it
    // straight to the v3 shape.
    let eval = format!(
        "persistent_term:put(ridge_loader_vsn, <<\"{base}\">>),\n\
         H = {{ridge_handle, Pid, _}} = ridge_rt:spawn_actor('{actor}', [], []),\n\
         H1 = maps:get('Note', '{parent}':'__ridge_record_versions'()),\n\
         ok = sys:suspend(Pid),\n\
         ok = ridge_rt:send_op(H, {{store, #{{'__ridge_v' => {{'{parent}', 'Note', H1}}, text => <<\"first\">>}}}}),\n\
         W = fun W() -> case filelib:is_file(\"{manifest}\") of true -> ok; false -> timer:sleep(50), W() end end,\n\
         D = fun D() -> case filelib:is_file(\"{manifest}\") of true -> timer:sleep(50), D(); false -> ok end end,\n\
         W(),\n\
         R1 = ridge_loader:apply(\"{manifest}\", <<\"{base}\">>),\n\
         io:format(\"APPLY1=~p~n\", [R1]),\n\
         io:format(\"GOT1=~p~n\", [ridge_rt:ask(H, {{get}}, 5000)]),\n\
         ok = sys:suspend(Pid),\n\
         ok = ridge_rt:send_op(H, {{store, #{{'__ridge_v' => {{'{parent}', 'Note', H1}}, text => <<\"second\">>}}}}),\n\
         D(), W(),\n\
         R2 = ridge_loader:apply(\"{manifest}\", ridge_loader:current_version()),\n\
         io:format(\"APPLY2=~p~n\", [R2]),\n\
         io:format(\"GOT2=~p~n\", [ridge_rt:ask(H, {{get}}, 5000)]),\n\
         halt(0).",
        base = base,
        actor = actor,
        parent = parent,
        manifest = manifest_fwd,
    );
    let node = spawn_streamed(&beam_dir, &eval);
    // First reload: text → body.
    apply_store_edit(&snap_v1, &ws, |src| src.replace("text", "body"));
    wait_marker(&node, "GOT1=", std::time::Duration::from_secs(60));
    // Rebase: the on-disk snapshot is v2's now. Clear the manifest so the
    // node's deletion-poll re-arms, then apply the second edit (body → title).
    let snap_v2: WorkspaceSnapshot = serde_json::from_str(
        &std::fs::read_to_string(snapshot_path_for(&ws.path, "debug")).expect("v2 snapshot"),
    )
    .expect("parse v2 snapshot");
    std::fs::remove_file(&manifest).expect("clear manifest");
    apply_store_edit(&snap_v2, &ws, |src| src.replace("body", "title"));
    let (lines, _stderr) = join_streamed(node);
    let out = lines.join("\n");
    assert!(
        out.contains("APPLY1={ok,") && out.contains("APPLY2={ok,"),
        "{out}"
    );
    assert!(out.contains("GOT1="), "{out}");
    // The second message rode BOTH upgrades with its v1 tag and landed as v3.
    assert!(
        out.contains("title => <<\"second\">>"),
        "v1-tagged message reached v3 shape: {out}"
    );
    assert!(!out.contains("text => <<\"second\">>"), "{out}");
}

#[test]
fn reload_drops_non_migratable_and_unknown_hash_messages() {
    if !otp_available() {
        eprintln!(
            "erl/erlc not on PATH — skipping reload_drops_non_migratable_and_unknown_hash_messages"
        );
        return;
    }
    let ws = common::make_store_workspace();
    let (snap, beam_dir) = compile_store_v1(&ws);
    let base = snapshot_vsn(&snap);
    let manifest = manifest_path_for(&ws.path, "debug");
    let _ = std::fs::remove_file(&manifest);
    let (parent, actor) = store_beams(&beam_dir);
    let manifest_fwd = manifest.to_string_lossy().replace('\\', "/");
    let eval = format!(
        "persistent_term:put(ridge_loader_vsn, <<\"{base}\">>),\n\
         H = {{ridge_handle, Pid, _}} = ridge_rt:spawn_actor('{actor}', [], []),\n\
         W = fun W() -> case filelib:is_file(\"{manifest}\") of true -> ok; false -> timer:sleep(50), W() end end,\n\
         W(),\n\
         R = ridge_loader:apply(\"{manifest}\", <<\"{base}\">>),\n\
         io:format(\"APPLY=~p~n\", [R]),\n\
         Before = ridge_rt:migration_count(),\n\
         Bad = #{{'__ridge_v' => {{'{parent}', 'Note', 999999999}}, text => <<\"x\">>}},\n\
         ok = ridge_rt:send_op(H, {{store, Bad}}),\n\
         Got = ridge_rt:ask(H, {{get}}, 5000),\n\
         io:format(\"GOT=~p~n\", [Got]),\n\
         io:format(\"BEFORE=~p~n\", [Before]),\n\
         io:format(\"MIGRATED=~p~n\", [ridge_rt:migration_count()]),\n\
         halt(0).",
        base = base,
        actor = actor,
        parent = parent,
        manifest = manifest_fwd,
    );
    let node = spawn_streamed(&beam_dir, &eval);
    apply_store_edit(&snap, &ws, |src| src.replace("text", "body"));
    let (lines, stderr) = join_streamed(node);
    let out = lines.join("\n");
    assert!(out.contains("APPLY={ok,"), "{out}");
    assert!(
        stderr.contains("dropped non-migratable"),
        "loud dev report: {stderr}"
    );
    assert!(out.contains("GOT="), "actor survived the drop: {out}");
    let before: u64 = lines
        .iter()
        .find_map(|l| l.strip_prefix("BEFORE="))
        .and_then(|s| s.parse().ok())
        .expect("BEFORE marker");
    let after: u64 = lines
        .iter()
        .find_map(|l| l.strip_prefix("MIGRATED="))
        .and_then(|s| s.parse().ok())
        .expect("MIGRATED marker");
    assert_eq!(before, after, "a dropped message applies no edge: {out}");
}

// ── Blue/green isolation: one failing migration never aborts the upgrade ─────

#[test]
fn reload_restarts_actor_with_throwing_migrate_hook() {
    if !otp_available() {
        eprintln!(
            "erl/erlc not on PATH — skipping reload_restarts_actor_with_throwing_migrate_hook"
        );
        return;
    }
    let ws = common::make_counter_workspace();
    let artefacts =
        compile_workspace(CompileOptions::new(ws.path.clone()).with_emit(EmitArtefacts::Beam))
            .expect("v1 compile");
    assert!(
        !artefacts
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, ridge_diagnostics::Severity::Error)),
        "v1 must compile clean: {:?}",
        artefacts.diagnostics
    );
    let snap: WorkspaceSnapshot = serde_json::from_str(
        &std::fs::read_to_string(snapshot_path_for(&ws.path, "debug")).expect("snapshot"),
    )
    .expect("parse snapshot");
    let base = snapshot_vsn(&snap);
    let manifest = manifest_path_for(&ws.path, "debug");
    let _ = std::fs::remove_file(&manifest);
    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("beam dir")
        .to_path_buf();
    let beam_mod = actor_beam_of(&beam_dir);
    let manifest_fwd = manifest.to_string_lossy().replace('\\', "/");
    // Two instances of the same actor: P1 seeded to count=2 (the hook divides
    // by `2 - old.count` and crashes for it), P2 left at count=0 (the hook
    // lands step=5). A monitor process stands in for the supervisor: when P1
    // dies it respawns the actor, which must come up on the NEW code with the
    // init state. The upgrade itself must succeed and migrate P2.
    let eval = format!(
        "persistent_term:put(ridge_loader_vsn, <<\"{base}\">>),\n\
         H1 = {{ridge_handle, P1, _}} = ridge_rt:spawn_actor('{beam_mod}', [], []),\n\
         ok = ridge_rt:send_op(H1, {{tick}}),\n\
         ok = ridge_rt:send_op(H1, {{tick}}),\n\
         2 = ridge_rt:ask(H1, {{count}}, 5000),\n\
         H2 = {{ridge_handle, P2, _}} = ridge_rt:spawn_actor('{beam_mod}', [], []),\n\
         0 = ridge_rt:ask(H2, {{count}}, 5000),\n\
         spawn(fun() ->\n\
         \x20   Mref = monitor(process, P1),\n\
         \x20   receive\n\
         \x20       {{'DOWN', Mref, process, P1, _}} ->\n\
         \x20           {{ridge_handle, Pn, _}} = ridge_rt:spawn_actor('{beam_mod}', [], []),\n\
         \x20           register(reborn, Pn)\n\
         \x20   end\n\
         end),\n\
         W = fun W() -> case filelib:is_file(\"{manifest}\") of true -> ok; false -> timer:sleep(50), W() end end,\n\
         W(),\n\
         R = ridge_loader:apply(\"{manifest}\", <<\"{base}\">>),\n\
         io:format(\"APPLY=~p~n\", [R]),\n\
         timer:sleep(500),\n\
         io:format(\"ALIVE1=~p~n\", [is_process_alive(P1)]),\n\
         io:format(\"ASK2=~p~n\", [ridge_rt:ask(H2, {{count}}, 5000)]),\n\
         io:format(\"STATE2=~p~n\", [sys:get_state(P2)]),\n\
         case whereis(reborn) of\n\
         \x20   undefined -> io:format(\"REBORN=none~n\");\n\
         \x20   Pn2 -> io:format(\"REBORN=~p~n\", [sys:get_state(Pn2)])\n\
         end,\n\
         halt(0).",
        base = base,
        beam_mod = beam_mod,
        manifest = manifest_fwd,
    );
    let node = spawn_streamed(&beam_dir, &eval);
    // Additive field + hook that crashes only when count = 2. Edit +
    // recompile + plan inline (apply_edit needs a ReloadNode; this case
    // boots its own streamed node).
    let src_path = counter_source(&ws);
    let src = std::fs::read_to_string(&src_path).expect("src");
    std::fs::write(
        &src_path,
        src.replace(
            "state count: Int = 0",
            "state count: Int = 0\n    state step: Int = 0\n    migrate (old: Counter@1) -> Counter =\n        { count = old.count, step = 10 / (2 - old.count) }",
        ),
    )
    .expect("write edit");
    let artefacts =
        compile_workspace(CompileOptions::new(ws.path.clone()).with_emit(EmitArtefacts::Beam))
            .expect("recompile");
    assert!(
        !artefacts
            .diagnostics
            .iter()
            .any(|d| matches!(d.severity, ridge_diagnostics::Severity::Error)),
        "edit must compile clean: {:?}",
        artefacts.diagnostics
    );
    let plan = plan_reload(&snap, CheckOptions::new(ws.path.clone()), &manifest)
        .expect("plan_reload");
    assert!(
        plan.manifest.is_some(),
        "edit must be reloadable: {:?}",
        plan.report
    );
    let (lines, _stderr) = join_streamed(node);
    let out = lines.join("\n");
    assert!(out.contains("APPLY={ok,"), "upgrade completes: {out}");
    assert!(
        out.contains("actors_migrated => 1"),
        "the healthy sibling migrated: {out}"
    );
    assert!(
        out.contains("actors_restarted => 1"),
        "the failing actor is reported as restarted: {out}"
    );
    assert!(
        out.contains("ALIVE1=false"),
        "the failing actor was never resumed on corrupt state: {out}"
    );
    assert!(out.contains("ASK2=0"), "sibling serves requests: {out}");
    assert!(
        out.contains("STATE2=#{count => 0,step => 5}"),
        "sibling state went through the hook: {out}"
    );
    assert!(
        out.contains("REBORN=#{count => 0,step => 0}"),
        "respawned actor boots the NEW code with init state: {out}"
    );
}
