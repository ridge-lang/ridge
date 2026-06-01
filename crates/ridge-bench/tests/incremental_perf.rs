//! A CI-gated guard that incremental recompilation stays cheap on a sizeable
//! workspace.
//!
//! The assertion is relative (incremental ≪ full rebuild) rather than an
//! absolute millisecond budget, so it catches a gross regression without going
//! flaky across CI hardware. The detailed millisecond numbers live in the
//! criterion benchmark (`cargo bench -p ridge-bench --bench incremental`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Instant;

use ridge_bench::{build_incremental_workspace, incremental_module_source};
use ridge_driver::{
    check_workspace_incremental, collect_diagnostics, CheckOptions, IncrementalState, ModuleId,
};

fn leaf_module(state: &IncrementalState, n: usize) -> ModuleId {
    let suffix = format!(".Mod{}", n - 1);
    state
        .resolved
        .graph
        .modules
        .iter()
        .find(|m| m.fully_qualified_name.ends_with(&suffix))
        .map(|m| m.id)
        .expect("leaf module present")
}

#[test]
fn leaf_body_recompile_is_far_cheaper_than_a_full_rebuild() {
    let n = 120;
    let ws = build_incremental_workspace(n).expect("write workspace");
    let root = ws.path().to_path_buf();

    let opts = || CheckOptions::new(root.clone()).with_retain_indices(true);

    let t0 = Instant::now();
    let mut state = check_workspace_incremental(opts()).expect("seed the engine");
    let full = t0.elapsed();

    let diags = collect_diagnostics(
        &state.disc_resolve_errors,
        &state.resolved,
        &state.type_errors,
        &state.source_cache(),
    );
    assert!(diags.is_empty(), "the corpus must check cleanly: {diags:?}");

    let leaf = leaf_module(&state, n);
    let leaf_src = incremental_module_source(n - 1);

    // Warm one recompile, then time a second at steady state.
    let _ = state.recompile(leaf, &leaf_src);
    let t1 = Instant::now();
    let set = state.recompile(leaf, &leaf_src);
    let inc = t1.elapsed();

    assert_eq!(
        set.len(),
        1,
        "a leaf body edit must recompile only the leaf module"
    );
    assert!(
        inc.saturating_mul(3) < full,
        "incremental recompile ({inc:?}) must be far cheaper than a full rebuild \
         ({full:?}) of {n} modules"
    );
}
