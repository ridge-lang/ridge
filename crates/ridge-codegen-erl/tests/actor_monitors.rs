//! Actor `onDown` member: end-to-end pipeline tests (no BEAM).
//!
//! Compiles a module whose actor declares an `onDown` member through the
//! full pipeline (resolve → typecheck → lower → codegen → print) and asserts
//! on the emitted Core Erlang: `handle_info/2` routes
//! `{'DOWN', Ref, 'process', _Pid, Reason}` through
//! `ridge_rt:exit_reason_to_ridge/1` into the user body with cast-style
//! `{'noreply', State}` leaves. Actors without the member keep the pure
//! stub. Runtime behaviour is exercised in `beam_e2e.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, run_pipeline};
use ridge_codegen_erl::{codegen_workspace, CodegenOptions};

const SOURCE: &str = "\
import std.io as Io
import std.actor (ExitReason, NotRunning, Shutdown, Crashed)

actor Watcher =
    state deaths: Int = 0

    on count () -> Int =
        deaths

    onDown io (m: Monitor) (reason: ExitReason) =
        deaths <- deaths + 1
        match reason
            NotRunning -> Io.println \"gone\"
            Shutdown -> Io.println \"stopped\"
            Crashed t -> Io.println t

fn io main () -> Unit =
    Io.println \"boot\"
";

const SOURCE_NO_ON_DOWN: &str = "\
actor Counter =
    state count: Int = 0

    on tick () -> Unit =
        count <- count + 1

fn main () -> Unit =
    ()
";

fn compile_to_core(name: &str, source: &str) -> String {
    let tw = make_workspace(name, name, source);
    let result = run_pipeline(&tw.path);
    assert!(
        !result.lowered.modules.is_empty(),
        "the module must lower (type errors would empty the workspace)"
    );

    // codegen_workspace emits one .core per module — actors are separate
    // gen_server modules — so concatenate them all for the assertions.
    let out = tempfile::tempdir().expect("tempdir");
    let mut opts = CodegenOptions::default();
    opts.out_root = out.path().to_path_buf();
    opts.invoke_erlc = false;
    opts.install_runtime = false;
    let codegen = codegen_workspace(&result.lowered, opts);
    assert!(
        codegen.errors.is_empty(),
        "codegen errors: {:?}",
        codegen.errors
    );

    let mut text = String::new();
    let core_dir = out.path().join("core");
    for entry in std::fs::read_dir(&core_dir).expect("core dir exists") {
        let entry = entry.expect("dir entry");
        text.push_str(&std::fs::read_to_string(entry.path()).expect("read core file"));
    }
    text
}

#[test]
fn pipeline_typechecks_and_lowers() {
    let tw = make_workspace("actor_monitors_tc", "actor_monitors", SOURCE);
    let result = run_pipeline(&tw.path);
    assert!(
        !result.lowered.modules.is_empty(),
        "the onDown module must lower (type errors would empty the workspace)"
    );
}

#[test]
fn on_down_member_routes_down_messages() {
    let core = compile_to_core("actor_monitors", SOURCE);
    for want in ["'DOWN'", "'process'", "'exit_reason_to_ridge'"] {
        assert!(
            core.contains(want),
            "expected {want} in handle_info, core:\n{core}"
        );
    }
    assert!(
        core.contains("'noreply'"),
        "the onDown body must yield noreply leaves, core:\n{core}"
    );
}

#[test]
fn actor_without_on_down_keeps_info_stub() {
    let core = compile_to_core("actor_no_on_down", SOURCE_NO_ON_DOWN);
    assert!(
        !core.contains("'DOWN'"),
        "actors without onDown keep the pure handle_info stub, core:\n{core}"
    );
    assert!(
        !core.contains("'exit_reason_to_ridge'"),
        "actors without onDown never map exit reasons, core:\n{core}"
    );
}
