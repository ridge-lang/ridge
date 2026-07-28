//! End-to-end check that the OTP hot-upgrade seam works on a compiled Ridge
//! actor: `sys:change_code/4` against a live `gen_server` preserves its
//! state, and the actor module exposes `__ridge_state_version/0` for the
//! future code loader to read.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
actor Counter =
    state count: Int = 0

    on tick =
        count <- count + 1

    on count () -> Int =
        count

fn spawn main () -> Unit =
    ()
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"code-change-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn sys_change_code_preserves_actor_state() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping sys_change_code_preserves_actor_state");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-code-change-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-code-change-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    assert!(
        artefacts.diagnostics.is_empty(),
        "no compile errors expected; got {:?}",
        artefacts.diagnostics
    );

    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();

    // The actor compiles to `<parent_beam>_<actor_lc>`; the parent beam name
    // is FQN-derived (`app.Main` → `ridge_app_Main`). Actor beams are not
    // listed in `artefacts.beam_files` (one entry per source module), so
    // locate it on disk in the beam dir.
    let actor_module = std::fs::read_dir(&beam_dir)
        .expect("read beam dir")
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_owned)
        })
        .find(|stem| stem.ends_with("_counter"))
        .expect("an actor beam ending in _counter");

    let expr = format!(
        "H = {{ridge_handle, Pid, _}} = ridge_rt:spawn_actor({actor_module}, [], []), \
         ok = ridge_rt:send_op(H, {{tick}}), \
         1 = ridge_rt:ask(H, {{count}}, 5000), \
         ok = sys:suspend(Pid), \
         ok = sys:change_code(Pid, {actor_module}, old, []), \
         ok = sys:resume(Pid), \
         1 = ridge_rt:ask(H, {{count}}, 5000), \
         io:format(\"version=~p~n\", [{actor_module}:'__ridge_state_version'()]), \
         io:format(\"change_code_ok~n\", []), \
         halt()."
    );

    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("spawn erl");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "erl exited non-zero\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("change_code_ok"),
        "sys:change_code round-trip must complete; stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("version={'Counter',"),
        "state version accessor must return {{'Counter', Hash}}; stdout:\n{stdout}"
    );
}
