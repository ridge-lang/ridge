//! Layer B integration: compile the Ridge micro-benchmarks to BEAM and run
//! them through `ridge_bench_runner`, asserting each reports a timing.
//!
//! Gated behind `beam-runtime` (a real OTP is required to compile to BEAM and
//! run the harness) plus a `which` guard so a machine without `erl`/`erlc` sees
//! a passing skip rather than a failure. Run with:
//!
//!   `cargo test -p ridge-bench --features beam-runtime --test layer_b -- --nocapture`
//!
//! `--nocapture` prints the median/p99 lines, which are the reproducible local
//! numbers the measurement layer exists to produce.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_bench::BenchWorkspace;
use ridge_codegen_erl::{erlc, runtime};
use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// The Ridge bench bodies, compiled into a throwaway workspace at test time.
const BENCH_SOURCE: &str = include_str!("../benches/beam/Bench.ridge");

/// The three benchmarks the source defines (each a `pub fn bench_*/0`).
const EXPECTED_BENCHES: &[&str] = &[
    "bench_list_sum_10k",
    "bench_string_build",
    "bench_record_churn",
];

#[test]
#[cfg_attr(
    not(feature = "beam-runtime"),
    ignore = "requires OTP installation; run with --features beam-runtime"
)]
fn layer_b_benches_compile_and_report_timings() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping layer_b_benches_compile_and_report_timings");
        return;
    }

    // Compile the bench module to BEAM in a throwaway workspace.
    let ws = BenchWorkspace::new(BENCH_SOURCE).expect("write bench workspace");
    let cache = tempfile::Builder::new()
        .prefix("ridge-bench-cache-")
        .tempdir()
        .expect("cache dir");
    let artefacts = compile_workspace(
        CompileOptions::new(ws.root())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    // Locate the beam output dir and the user module(s) from the artefacts.
    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    let out_root = beam_dir
        .parent()
        .expect("beam dir has a parent")
        .to_path_buf();

    let modules: Vec<String> = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .filter(|stem| stem.starts_with("ridge_module_"))
        .map(ToOwned::to_owned)
        .collect();
    assert!(
        !modules.is_empty(),
        "expected at least one user module in {:?}",
        artefacts.beam_files
    );

    // Install + compile the bench runner into the same beam dir.
    let erlc_info = erlc::probe(None).expect("probe erlc");
    runtime::install_bench_runner(&out_root).expect("install bench runner");
    runtime::compile_bench_runner(&erlc_info.path, &out_root).expect("compile bench runner");

    // Run every bench in a single BEAM boot.
    let mut cmd = Command::new("erl");
    cmd.arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-s")
        .arg("ridge_bench_runner")
        .arg("run");
    for m in &modules {
        cmd.arg(m);
    }
    cmd.arg("-s").arg("init").arg("stop");
    let output = cmd.output().expect("run bench harness");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("--- Layer B results ---\n{stdout}");
    if !stderr.trim().is_empty() {
        eprintln!("--- stderr ---\n{stderr}");
    }

    for bench in EXPECTED_BENCHES {
        let line = stdout
            .lines()
            .find(|l| l.contains(&format!("\"bench\":\"{bench}\"")))
            .unwrap_or_else(|| panic!("missing result line for {bench}:\n{stdout}"));
        assert!(
            line.contains("\"median_ns\":") && !line.contains("\"error\":true"),
            "{bench} must report a timing, got: {line}"
        );
    }
}
