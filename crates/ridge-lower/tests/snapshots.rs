//! Phase 5 snapshot tests — four canonical example programs.
//!
//! Each test loads an example from `examples/`, runs the full pipeline, and
//! asserts the snapshot of the `LoweredModule`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::{load_example_workspace, render_lowered_module, run_pipeline};

fn snapshot_example(name: &str) {
    let tw = load_example_workspace(name);
    let result = run_pipeline(&tw.path);

    assert!(
        !result.lowered.modules.is_empty(),
        "no modules in lowered workspace for example {name}"
    );
    let module_opt = &result.lowered.modules[0];
    assert!(module_opt.is_some(), "module[0] is None for example {name}");

    let rendered = render_lowered_module(module_opt.as_ref().unwrap());
    insta::assert_snapshot!(name, rendered);
}

#[test]
fn lower_log_analyzer() {
    snapshot_example("log_analyzer");
}

#[test]
fn lower_url_shortener() {
    snapshot_example("url_shortener");
}

#[test]
fn lower_game_of_life() {
    snapshot_example("game_of_life");
}

#[test]
fn lower_rate_limiter() {
    snapshot_example("rate_limiter");
}
