//! §2.3 — [`CErlModule`] snapshot tests on Phase 5 micro-fixtures.
//!
//! For each fixture under `crates/ridge-lower/tests/fixtures/lower/*.rg`,
//! runs the full pipeline (resolve → typecheck → lower → codegen) and snapshots
//! the resulting `CErlModule` via `assert_debug_snapshot!`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{make_workspace, run_pipeline};
use ridge_codegen_erl::codegen_module_ast;
use std::fs;
use std::path::Path;

fn snapshot_fixture(fixture_name: &str) {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let fixture_path = Path::new(manifest_dir)
        .join("../ridge-lower/tests/fixtures/lower")
        .join(format!("{fixture_name}.rg"));

    let source = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("could not read fixture {}: {e}", fixture_path.display()));

    let tw = make_workspace(fixture_name, fixture_name, &source);
    let result = run_pipeline(&tw.path);

    assert!(
        !result.lowered.modules.is_empty(),
        "no modules in lowered workspace for fixture {fixture_name}"
    );
    let module_opt = &result.lowered.modules[0];
    assert!(
        module_opt.is_some(),
        "module[0] is None for fixture {fixture_name}"
    );

    let m = module_opt.as_ref().unwrap();
    let cerl_module = codegen_module_ast(m, &result.lowered)
        .unwrap_or_else(|e| panic!("codegen failed for fixture {fixture_name}: {e:?}"));

    insta::assert_debug_snapshot!(fixture_name, &cerl_module);
}

// ── Pipe ──────────────────────────────────────────────────────────────────────

#[test]
fn snap_pipe_simple() {
    snapshot_fixture("pipe_simple");
}

#[test]
fn snap_pipe_chained() {
    snapshot_fixture("pipe_chained");
}

// ── Propagate ─────────────────────────────────────────────────────────────────

#[test]
fn snap_propagate_result() {
    snapshot_fixture("propagate_result");
}

#[test]
fn snap_propagate_option() {
    snapshot_fixture("propagate_option");
}

// ── Try block ─────────────────────────────────────────────────────────────────

#[test]
fn snap_try_block_basic() {
    snapshot_fixture("try_block_basic");
}

#[test]
fn snap_try_block_nested_propagate() {
    snapshot_fixture("try_block_nested_propagate");
}

// ── Guard ─────────────────────────────────────────────────────────────────────

#[test]
fn snap_guard_single() {
    snapshot_fixture("guard_single");
}

#[test]
fn snap_guard_multi() {
    snapshot_fixture("guard_multi");
}

// ── Interpolation ─────────────────────────────────────────────────────────────

#[test]
fn snap_interp_int() {
    snapshot_fixture("interp_int");
}

#[test]
fn snap_interp_mixed_types() {
    snapshot_fixture("interp_mixed_types");
}

// ── With update ───────────────────────────────────────────────────────────────

#[test]
fn snap_with_simple() {
    snapshot_fixture("with_simple");
}

#[test]
fn snap_with_chained() {
    snapshot_fixture("with_chained");
}

// ── Actor ─────────────────────────────────────────────────────────────────────

#[test]
fn snap_actor_dispatch_no_init() {
    snapshot_fixture("actor_dispatch_no_init");
}

#[test]
fn snap_actor_dispatch_with_init() {
    snapshot_fixture("actor_dispatch_with_init");
}

// ── Inner fn ──────────────────────────────────────────────────────────────────

#[test]
fn snap_inner_fn_basic() {
    snapshot_fixture("inner_fn_basic");
}

#[test]
fn snap_inner_fn_recursive() {
    snapshot_fixture("inner_fn_recursive");
}

// ── Ask timeout ───────────────────────────────────────────────────────────────

#[test]
fn snap_ask_default_timeout() {
    snapshot_fixture("ask_default_timeout");
}

#[test]
fn snap_ask_explicit_timeout() {
    snapshot_fixture("ask_explicit_timeout");
}

#[test]
fn snap_ask_never_timeout() {
    snapshot_fixture("ask_never_timeout");
}
