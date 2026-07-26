//! Actor `terminate` callback: end-to-end pipeline tests (no BEAM).
//!
//! Compiles a module whose actor declares a `terminate` member through the
//! full pipeline (resolve → typecheck → lower → codegen → print) and asserts
//! on the emitted Core Erlang: a real `terminate/2` (not the no-op stub),
//! the OTP-reason mapping through `ridge_rt:exit_reason_to_ridge/1`, and the
//! internal `trap_exit` that makes the callback reachable on supervisor
//! shutdown. Runtime behaviour is exercised in `beam_e2e.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, run_pipeline};
use ridge_codegen_erl::{codegen_workspace, CodegenOptions};

const SOURCE: &str = "\
import std.io as Io
import std.actor (ExitReason, Shutdown, Crashed)

actor Worker =
    state n: Int = 0

    on tick () -> Unit =
        n <- n + 1

    terminate io (reason: ExitReason) =
        match reason
            Shutdown -> Io.println \"clean\"
            Crashed m -> Io.println m

fn io main () -> Unit =
    Io.println \"boot\"
";

const SOURCE_NO_TERMINATE: &str = "\
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
    let tw = make_workspace("actor_terminate_tc", "actor_terminate", SOURCE);
    let result = run_pipeline(&tw.path);
    assert!(
        !result.lowered.modules.is_empty(),
        "the terminate module must lower (type errors would empty the workspace)"
    );
}

#[test]
fn terminate_member_emits_real_callback() {
    let core = compile_to_core("actor_terminate", SOURCE);
    assert!(
        core.contains("'exit_reason_to_ridge'"),
        "terminate must map the OTP reason through the runtime, core:\n{core}"
    );
    // The no-op stub body is the bare atom 'ok' with a boilerplate
    // annotation; a real callback lowers the user body instead.
    assert!(
        !core.contains("boilerplate no-op stub (§4.28)\"\n    end\n\n'code_change'"),
        "terminate must no longer be the no-op stub, core:\n{core}"
    );
}

#[test]
fn terminate_member_sets_trap_exit_in_init() {
    let core = compile_to_core("actor_terminate_trap", SOURCE);
    assert!(
        core.contains("'process_flag'") && core.contains("'trap_exit'"),
        "init must set trap_exit so the supervisor shutdown reaches terminate, core:\n{core}"
    );
}

#[test]
fn actor_without_terminate_keeps_stub_and_no_trap_exit() {
    let core = compile_to_core("actor_no_terminate", SOURCE_NO_TERMINATE);
    assert!(
        !core.contains("'trap_exit'"),
        "actors without terminate must not trap exits, core:\n{core}"
    );
    assert!(
        !core.contains("'exit_reason_to_ridge'"),
        "actors without terminate keep the no-op stub, core:\n{core}"
    );
}
