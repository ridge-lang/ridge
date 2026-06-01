//! Incremental-recompile benchmarks: how long a single-file edit takes to
//! re-check against an already-analysed workspace, versus a full rebuild.
//!
//! The `incremental_recompile/leaf_body` group is the headline number — the
//! latency a developer feels on each keystroke. `full_rebuild/seed` is the
//! baseline it should stay well under as the workspace grows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items
)]

use std::hint::black_box;
use std::path::Path;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use ridge_bench::{build_incremental_workspace, incremental_module_source};
use ridge_driver::{check_workspace_incremental, CheckOptions, IncrementalState, ModuleId};

/// Workspace sizes (module counts) each scenario is exercised at.
const SIZES: [usize; 3] = [50, 100, 200];

fn seed(root: &Path) -> IncrementalState {
    check_workspace_incremental(CheckOptions::new(root.to_path_buf()).with_retain_indices(true))
        .expect("seed the engine")
}

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

fn bench_incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_recompile");
    group.sample_size(20);
    for n in SIZES {
        let ws = build_incremental_workspace(n).expect("write workspace");
        let mut state = seed(ws.path());
        let leaf = leaf_module(&state, n);
        let leaf_src = incremental_module_source(n - 1);
        // Warm the caches so the first sample is not an outlier.
        let _ = state.recompile(leaf, &leaf_src);
        group.bench_with_input(BenchmarkId::new("leaf_body", n), &n, |b, _| {
            b.iter(|| {
                let set = state.recompile(leaf, &leaf_src);
                black_box(set);
            });
        });
    }
    group.finish();
}

fn bench_full_rebuild(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_rebuild");
    group.sample_size(10);
    for n in SIZES {
        let ws = build_incremental_workspace(n).expect("write workspace");
        let root = ws.path().to_path_buf();
        group.bench_with_input(BenchmarkId::new("seed", n), &root, |b, root| {
            b.iter(|| black_box(seed(black_box(root))));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_incremental, bench_full_rebuild);
criterion_main!(benches);
