//! T13 — `.core` text snapshots on the four Ridge examples.
//!
//! For each example under `examples/<name>.rg` (repo root), runs the full
//! pipeline (resolve → typecheck → lower → codegen → print) and snapshots
//! the resulting Core Erlang text via `insta::assert_snapshot!`.
//!
//! `%% File:` and `%% Caps:` annotations must be visible in the snapshot text
//! (OQ-E011, §3.11).  If an example fails to lower or codegen, the test panics
//! with a clear message describing what failed; no `#[ignore]` is used.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, run_pipeline};
use ridge_codegen_erl::codegen_module_ast;
use ridge_codegen_erl::printer::print_module;
use std::fs;
use std::path::Path;

fn snapshot_example(name: &str) {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let example_path = Path::new(manifest_dir)
        .join("../../examples")
        .join(format!("{name}.rg"));

    let source = fs::read_to_string(&example_path)
        .unwrap_or_else(|e| panic!("could not read example {}: {e}", example_path.display()));

    let tw = make_workspace(name, name, &source);
    let result = run_pipeline(&tw.path);

    assert!(
        !result.lowered.modules.is_empty(),
        "no modules in lowered workspace for example {name}"
    );
    let module_opt = &result.lowered.modules[0];
    assert!(
        module_opt.is_some(),
        "module[0] is None for example {name} — Phase 5 lowering returned no module"
    );

    let m = module_opt.as_ref().unwrap();
    let cerl_module = codegen_module_ast(m, &result.lowered)
        .unwrap_or_else(|e| panic!("codegen (Phase 6) failed for example {name}: {e:?}"));

    let core_text = print_module(&cerl_module);
    insta::assert_snapshot!(name, &core_text);
}

// ── Examples ──────────────────────────────────────────────────────────────────

#[test]
fn snap_log_analyzer() {
    snapshot_example("log_analyzer");
}

#[test]
fn snap_url_shortener() {
    snapshot_example("url_shortener");
}

#[test]
fn snap_game_of_life() {
    snapshot_example("game_of_life");
}

#[test]
fn snap_rate_limiter() {
    snapshot_example("rate_limiter");
}
