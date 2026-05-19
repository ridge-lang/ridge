#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

mod common;

use std::fs;
use std::path::Path;

#[test]
fn dump_worker_ir() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let example_path = Path::new(manifest_dir).join("../../examples/rate_limiter.ridge");
    let source = fs::read_to_string(&example_path).expect("read rate_limiter.ridge");

    let tw = common::make_workspace("debug_worker_ir", "rate_limiter", &source);
    let pipeline = common::run_pipeline(&tw.path);

    // Find the worker actor module
    for m in pipeline.lowered.modules.iter().flatten() {
        for item in &m.items {
            if let ridge_ir::IrItem::Actor(actor) = item {
                if actor.name == "Worker" {
                    eprintln!("Worker actor: {}", actor.name);
                    for handler in &actor.dispatch {
                        if handler.message_name == "run" {
                            eprintln!("run handler body: {:#?}", handler.body);
                        }
                    }
                }
            }
        }
    }
}
