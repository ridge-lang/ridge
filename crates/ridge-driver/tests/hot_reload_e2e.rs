//! End-to-end hot-reload battery: a live node running OLD code applies an
//! upgrade manifest produced from NEW code, through `ridge_loader:apply/2`.
//!
//! Covered: state preserved on body changes, additive state-field migration,
//! rename migration, and the base-version gate. Gated on `beam-runtime`
//! (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ridge_driver::{
    compile_workspace, manifest_path_for, plan_reload, snapshot_path_for, snapshot_vsn,
    CheckOptions, CompileOptions, EmitArtefacts, WorkspaceSnapshot,
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
fn apply_edit(node: &ReloadNode, ws: &common::TempWorkspace, edit: impl FnOnce(&str) -> String) {
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
