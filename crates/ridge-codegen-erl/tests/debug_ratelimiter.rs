#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

mod common;

use ridge_codegen_erl::{codegen_workspace, CodegenOptions};
use std::fs;
use std::path::Path;

#[test]
fn dump_rate_limiter_core() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let example_path = Path::new(manifest_dir).join("../../examples/rate_limiter.ridge");
    let source = fs::read_to_string(&example_path).expect("read rate_limiter.ridge");

    let tw = common::make_workspace("debug_rate", "rate_limiter", &source);
    let pipeline = common::run_pipeline(&tw.path);

    let out_dir = tempfile::TempDir::new().expect("create temp dir");
    let out_path = out_dir.path().to_path_buf();

    let mut opts = CodegenOptions::default();
    opts.out_root = out_path.clone();
    opts.invoke_erlc = false;
    opts.install_runtime = true;

    let result = codegen_workspace(&pipeline.lowered, opts);

    let core_dir = out_path.join("core");
    if core_dir.exists() {
        for entry in fs::read_dir(&core_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Only show actor modules
            if name_str.contains("limiter") || name_str.contains("worker") {
                let content = fs::read_to_string(entry.path()).unwrap();
                eprintln!("=== {name_str} ===\n{content}\n");
            }
        }
    }

    for e in &result.errors {
        eprintln!("ERROR: {e:?}");
    }
}
