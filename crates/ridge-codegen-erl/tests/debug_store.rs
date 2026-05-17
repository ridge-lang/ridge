//! Temporary test to examine generated Core Erlang for store actor.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

mod common;

use ridge_codegen_erl::{codegen_workspace, CodegenOptions};
use std::fs;
use std::path::Path;

#[test]
fn dump_url_shortener_store_core() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let example_path = Path::new(manifest_dir).join("../../examples/url_shortener.rg");
    let source = fs::read_to_string(&example_path).expect("read url_shortener.rg");

    let tw = common::make_workspace("debug_store", "url_shortener", &source);
    let pipeline = common::run_pipeline(&tw.path);

    let out_dir = tempfile::TempDir::new().expect("create temp dir");
    let out_path = out_dir.path().to_path_buf();

    let mut opts = CodegenOptions::default();
    opts.out_root = out_path.clone();
    opts.invoke_erlc = false; // don't invoke erlc
    opts.install_runtime = true;

    let result = codegen_workspace(&pipeline.lowered, opts);

    // Print all generated .core files
    let core_dir = out_path.join("core");
    if core_dir.exists() {
        for entry in fs::read_dir(&core_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().is_some_and(|e| e == "core") {
                let content = fs::read_to_string(entry.path()).unwrap();
                eprintln!(
                    "=== {} ===\n{}\n",
                    entry.file_name().to_string_lossy(),
                    content
                );
            }
        }
    }

    // Print any errors
    for e in &result.errors {
        eprintln!("ERROR: {e:?}");
    }
}
