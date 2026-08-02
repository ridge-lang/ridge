//! Depth hardening, end to end.
//!
//! The parser bounds its own recursion at `MAX_PARSE_DEPTH` (256) and reports
//! `P028` past it (covered by `ridge-parser/tests/fuzz.rs`). What that suite
//! does not cover is everything *after* the parser: typecheck, lowering, and
//! Core emission also walk the tree recursively and have no depth guard of
//! their own. These tests pin the pipeline-level guarantee:
//!
//! 1. the deepest nesting the parser admits compiles through every later
//!    phase without a stack overflow, and
//! 2. nesting past the parser's limit surfaces as a clean `P028` diagnostic
//!    through the driver's normal diagnostics channel — never a crash.
//!
//! Everything runs on a 64 MiB stack thread (same pattern as the parser fuzz
//! harness) so the phases' own recursion is the limiting factor, not the
//! test runner's small default stack.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// Run `f` on a thread with a 64 MiB stack and propagate any panic.
fn on_big_stack(f: impl FnOnce() + Send + 'static) {
    let handle = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .name("ridge-driver-depth".to_string())
        .spawn(f)
        .expect("failed to spawn depth-test thread");
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

/// `open` × `depth`, then `core`, then `close` × `depth`.
fn wrap(open: &str, core: &str, close: &str, depth: usize) -> String {
    format!("{}{core}{}", open.repeat(depth), close.repeat(depth))
}

/// A program whose single function body is a list literal nested `depth`
/// levels deep — real AST nodes at every level, unlike parentheses, which
/// the parser flattens away.
fn deep_list_program(depth: usize) -> String {
    format!("pub fn deep () =\n    {}\n", wrap("[", "0", "]", depth))
}

/// Largest list-nesting depth the parser admits without a `P028`. Discovered
/// at runtime so the test tracks the parser's guard instead of hard-coding a
/// number that drifts when the descent's frame cost changes.
fn max_admitted_list_depth() -> usize {
    let mut max = 0;
    for depth in 1..512 {
        let r = ridge_parser::parse_source(&deep_list_program(depth));
        if r.errors.iter().any(|e| e.code() == "P028") {
            break;
        }
        max = depth;
    }
    assert!(max > 0, "parser admitted no nesting at all");
    max
}

/// Compile `source` in Core-only mode (no external toolchain) and return the
/// diagnostics.
fn compile(source: &str) -> Vec<ridge_diagnostics::Diagnostic> {
    let tw = common::make_workspace("main", source);
    let options = CompileOptions::new(tw.path).with_emit(EmitArtefacts::Core);
    compile_workspace(options)
        .expect("compile_workspace")
        .diagnostics
}

#[test]
fn deepest_admitted_nesting_compiles_end_to_end() {
    on_big_stack(|| {
        let max = max_admitted_list_depth();
        // Sit a few levels under the guard so the test does not flake if the
        // parser's per-level frame cost shifts by one or two.
        let depth = max.saturating_sub(4);
        let diags = compile(&deep_list_program(depth));
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.severity, ridge_diagnostics::Severity::Error))
            .collect();
        assert!(
            errors.is_empty(),
            "depth {depth} (parser max {max}) must compile clean through \
             typecheck/lower/codegen; got: {errors:?}"
        );
    });
}

#[test]
fn past_parser_limit_is_a_clean_p028_diagnostic() {
    on_big_stack(|| {
        let max = max_admitted_list_depth();
        let diags = compile(&deep_list_program(max + 50));
        let codes: Vec<&str> = diags.iter().map(|d| d.code).collect();
        assert!(
            codes.contains(&"P028"),
            "depth past the parser limit must surface P028 via diagnostics, \
             got codes: {codes:?}"
        );
    });
}
