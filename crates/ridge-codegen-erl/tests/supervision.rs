//! Typed supervision: end-to-end pipeline tests (no BEAM).
//!
//! Compiles a module that uses `child`, `std.actor.supervise`,
//! `std.actor.startChild`, and the compiler-known `std.actor.tryAsk` through
//! the full pipeline (resolve → typecheck → lower → codegen → print) and
//! asserts on the emitted Core Erlang: the OTP child-spec map shape and the
//! `ridge_rt` supervision calls. Runtime behaviour (`ridge_rt:try_ask/3`
//! etc.) is BEAM-side runtime work and is not exercised here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, run_pipeline};
use ridge_codegen_erl::codegen_module_ast;
use ridge_codegen_erl::printer::print_module;

const SOURCE: &str = "\
import std.io as Io
import std.int as Int
import std.actor as Actor
import std.actor (OneForOne, Noproc, Timeout)

actor Counter =
    state count: Int = 0

    init (start: Int) =
        count <- start

    on getCount () -> Int =
        count

fn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 [child Counter (0)]?
    let c = Actor.startChild sup (child Counter (1))?
    let r = Actor.tryAsk c getCount 1000
    match r
        Ok n        -> Io.println (Int.toText n)
        Err Noproc  -> Io.println \"noproc\"
        Err Timeout -> Io.println \"timeout\"
    Ok ()
";

fn compile_to_core() -> String {
    let tw = make_workspace("supervision", "supervision", SOURCE);
    let result = run_pipeline(&tw.path);

    let module_opt = &result.lowered.modules[0];
    let m = module_opt
        .as_ref()
        .expect("module[0] is None — Phase 5 lowering returned no module");
    let cerl_module = codegen_module_ast(m, &result.lowered).expect("codegen (Phase 6) failed");
    print_module(&cerl_module)
}

#[test]
fn pipeline_typechecks_and_lowers() {
    let tw = make_workspace("supervision_tc", "supervision", SOURCE);
    let result = run_pipeline(&tw.path);
    assert!(
        !result.lowered.modules.is_empty(),
        "the supervision module must lower (type errors would empty the workspace)"
    );
}

#[test]
fn child_spec_map_shape_in_core() {
    let core = compile_to_core();
    // The OTP child-spec map: plain map with id/start/restart/shutdown keys.
    // The id is a BINARY (Ridge Text) — printed in Core Erlang bit-syntax
    // form (`c` = 99 is the first byte of "counter") — so `stopChild` /
    // `whichChildren` / `childId` comparisons against Text values match.
    assert!(
        core.contains("'id'=>#{#<99>(8,1,'integer'"),
        "spec id must be the actor's lowercase name as a binary, core:\n{core}"
    );
    assert!(
        !core.contains("'id'=>'counter'"),
        "spec id must not be an atom (binary ids keep comparisons uniform), core:\n{core}"
    );
    assert!(
        core.contains("'start_link'"),
        "spec start must name start_link, core:\n{core}"
    );
    assert!(
        core.contains("_counter'"),
        "spec start must name the actor BEAM module, core:\n{core}"
    );
    assert!(
        core.contains("'restart'=>'permanent'"),
        "spec restart default must be 'permanent', core:\n{core}"
    );
    assert!(
        core.contains("'shutdown'=>5000"),
        "spec shutdown default must be 5000, core:\n{core}"
    );
    // The two `child Counter (…)` calls carry their init arg list inline.
    assert!(
        core.contains("[0]") && core.contains("[1]"),
        "spec start args must be the init args, core:\n{core}"
    );
}

#[test]
fn supervision_calls_route_through_ridge_rt() {
    let core = compile_to_core();
    for want in [
        "'start_supervisor'",
        "'start_supervised_child'",
        "'try_ask'",
    ] {
        assert!(
            core.contains(want),
            "expected {want} call in emitted core, core:\n{core}"
        );
    }
    // `tryAsk` lowers with the handler-tag message tuple and the timeout.
    assert!(
        core.contains("{'getCount'}"),
        "tryAsk message must be the bare handler-tag tuple, core:\n{core}"
    );
    // The `OneForOne` strategy variant crosses the FFI boundary as the
    // verbatim atom (the runtime maps it to OTP's `one_for_one`).
    assert!(
        core.contains("'OneForOne'"),
        "strategy variant must lower to the verbatim atom, core:\n{core}"
    );
}
