//! Layer A benchmarks: the pure compile pipeline, no `erlc` / BEAM.
//!
//! Two groups:
//! - `check` — `discover -> resolve -> typecheck` (the pass set where an
//!   accidental `O(n^2)` is most likely to hide). Measured at three sizes per
//!   shape so a regression shows as a bend in the curve.
//! - `emit_core` — the full pipeline through Core Erlang emission (`lower` +
//!   `codegen`, still no `erlc`), at one mid size per shape to bound CI time.
//!
//! These are deterministic and native (no BEAM boot), so they gate on each PR.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items
)]

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use ridge_bench::{
    deep_let_chain, many_functions, run_check, run_emit_core, wide_record, BenchWorkspace,
};

/// A corpus shape: display name, source generator, and the three sizes to run.
type Shape = (&'static str, fn(usize) -> String, [usize; 3]);

/// The corpus shapes and the sizes each is exercised at.
fn shapes() -> Vec<Shape> {
    vec![
        (
            "many_functions",
            many_functions as fn(usize) -> String,
            [10, 100, 500],
        ),
        ("deep_let_chain", deep_let_chain, [16, 64, 256]),
        ("wide_record", wide_record, [8, 64, 256]),
    ]
}

fn bench_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("check");
    group.sample_size(20);
    for (name, generate, sizes) in shapes() {
        for n in sizes {
            let ws = BenchWorkspace::new(&generate(n)).expect("write bench workspace");
            let root = ws.root();
            assert_eq!(run_check(&root), 0, "{name}/{n} must check cleanly");
            group.bench_with_input(BenchmarkId::new(name, n), &root, |b, root| {
                b.iter(|| black_box(run_check(black_box(root))));
            });
        }
    }
    group.finish();
}

fn bench_emit_core(c: &mut Criterion) {
    let cache = tempfile::Builder::new()
        .prefix("ridge-bench-cache-")
        .tempdir()
        .expect("create cache dir");
    let mut group = c.benchmark_group("emit_core");
    group.sample_size(10);
    for (name, generate, sizes) in shapes() {
        // Mid size only: codegen is mechanical, one sizeable point is enough to
        // catch a regression without paying for the largest input on every PR.
        let n = sizes[1];
        let ws = BenchWorkspace::new(&generate(n)).expect("write bench workspace");
        let root = ws.root();
        assert_ne!(
            run_emit_core(&root, cache.path()),
            usize::MAX,
            "{name}/{n} must emit Core without a fatal error"
        );
        group.bench_with_input(BenchmarkId::new(name, n), &root, |b, root| {
            b.iter(|| black_box(run_emit_core(black_box(root), cache.path())));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_check, bench_emit_core);
criterion_main!(benches);
