//! T17 snapshot tests for the four canonical example programs.
//!
//! §9.3 — These four tests are the literal Phase 4 acceptance gate (spec
//! §11.3 Phase 4 `DoD` line 1299): "All examples type-check with correct
//! capabilities."
//!
//! Each test:
//! 1. Wraps the example in a synthetic single-module workspace (mirrors the
//!    Phase 3 approach from `crates/ridge-resolve/tests/snapshots.rs`).
//! 2. Runs the full resolve pipeline (`resolve_workspace`) then
//!    `typecheck_workspace`.
//! 3. Asserts `errors.is_empty()` — zero T### diagnostics.
//! 4. Captures an insta snapshot of a deterministic projection so that
//!    any drift in inference behaviour over the canonical programs is caught
//!    by `cargo insta test`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;

use ridge_ast::{Body, Expr as AstExpr, Item};
use ridge_resolve::{assign_node_ids, discover_workspace, resolve_workspace, NodeKind};
use ridge_typecheck::{typecheck_workspace, TypeError};
use tempfile::TempDir;

// ── Workspace helpers ─────────────────────────────────────────────────────────

fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// Wrap an example file in a minimal workspace for testing.
fn load_example_into_workspace(example_name: &str) -> TempDir {
    let example_src = format!(
        "{}/../../examples/{}.ridge",
        env!("CARGO_MANIFEST_DIR"),
        example_name
    );
    let src_content = fs::read_to_string(&example_src)
        .unwrap_or_else(|e| panic!("could not read example {example_src}: {e}"));

    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"examples-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        td.path(),
        &format!("apps/demo/src/{example_name}.ridge"),
        &src_content,
    );
    td
}

/// Wrap a source snippet in a minimal workspace for testing.
fn load_snippet_into_workspace(file_name: &str, src: &str) -> TempDir {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), &format!("apps/demo/src/{file_name}"), src);
    td
}

// ── Snapshot projection ───────────────────────────────────────────────────────

/// Deterministic, snapshot-friendly projection of a typecheck result for one
/// example.  No `NodeIds` or `TyVids` appear directly — we use string-rendered
/// forms where needed.
///
/// Phase 4.5 T6 additions: `node_types_populated`, `schemes_populated`, and
/// `inferred_caps_real_keys` are aggregate totals across all typed modules.
// OQ-PHASE45-008: aggregate-only snapshot totals (not per-module breakdowns).
#[allow(dead_code)]
#[derive(Debug)]
struct TypecheckSnapshot {
    /// Formatted T### errors sorted for cross-platform determinism.
    errors: Vec<String>,
    /// Number of typed modules produced.
    module_count: usize,
    /// Number of `TyCons` in the shared arena (builtins + user-defined).
    tycon_count: usize,
    /// Phase 4.5 T6: sum of `node_types` populated (non-None) slots across all
    /// modules.  Confirms that T3's `infer_expr` write-back is actually firing.
    node_types_populated: usize,
    /// Phase 4.5 T6: sum of `schemes` map entries across all modules.
    /// Confirms T4's SCC generalise write-back.
    schemes_populated: usize,
    /// Phase 4.5 T6: number of fn decls for which a *real* `NodeId` (not the
    /// legacy proxy `NodeId(f.span.start)`) was found via the `NodeIdMap` and
    /// inserted into `inferred_caps`.  Confirms T5's real-NodeId keying.
    inferred_caps_real_keys: usize,
}

fn format_terror(e: &TypeError) -> String {
    format!("{}: {}", e.code(), e)
}

/// Compute the three Phase 4.5 T6 aggregate totals for a typecheck result.
///
/// - `node_types_populated`: count of non-None slots in `TypedModule.node_types`
///   summed across all modules.
/// - `schemes_populated`: count of `TypedModule.schemes` entries summed across
///   all modules.
/// - `inferred_caps_real_keys`: for each top-level fn in each module, check
///   whether `assign_node_ids` gives a real `NodeId` (from the body span lookup)
///   that is present in `inferred_caps`; count the number of fns where this
///   succeeds.  This mirrors the T5 keying logic in `typecheck_module_inner`.
fn compute_phase45_totals(result: &ridge_typecheck::TypecheckResult) -> (usize, usize, usize) {
    let mut node_types_populated = 0usize;
    let mut schemes_populated = 0usize;
    let mut inferred_caps_real_keys = 0usize;

    for module in &result.typed.modules {
        // T3 total: count non-None slots in node_types.
        node_types_populated += module.node_types.iter().filter(|t| t.is_some()).count();

        // T4 total: count schemes entries.
        schemes_populated += module.schemes.len();

        // T5 total: count fns where the real NodeId was found and is in inferred_caps.
        // OQ-PHASE45-005: mirrors keying in typecheck_module_inner Step D.
        let (node_id_map, _) = assign_node_ids(&module.ast);
        for item in &module.ast.items {
            if let Item::Fn(f) = item {
                // Body::Ffi has no expression span — skip.
                let expr = match &f.body {
                    Body::Expr(e) => e,
                    Body::Ffi { .. } => continue,
                };
                let (body_span, body_kind) = match expr {
                    AstExpr::Block(b) => (b.span, NodeKind::Block),
                    AstExpr::Try { span, .. } => (*span, NodeKind::Try),
                    other => (other.span(), NodeKind::Expr),
                };
                if let Some(nid) = node_id_map.get(body_span, body_kind) {
                    if module.inferred_caps.contains_key(&nid) {
                        inferred_caps_real_keys += 1;
                    }
                }
            }
        }
    }

    (
        node_types_populated,
        schemes_populated,
        inferred_caps_real_keys,
    )
}

fn snapshot_example(example_name: &str) -> TypecheckSnapshot {
    let td = load_example_into_workspace(example_name);

    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "{example_name}: R-errors during discovery: {:?}",
        disc.resolve_errors
    );
    assert!(
        disc.manifest_errors.is_empty(),
        "{example_name}: M-errors during discovery: {:?}",
        disc.manifest_errors
    );
    let ws_graph = disc.graph.expect("workspace graph present");
    let resolved = resolve_workspace(ws_graph);

    // Ensure Phase 3 resolve also completed cleanly.
    assert!(
        resolved.errors.is_empty(),
        "{example_name}: R-errors in resolve: {:#?}",
        resolved.errors
    );

    let result = typecheck_workspace(&resolved);

    let mut formatted: Vec<String> = result
        .errors
        .iter()
        .map(|(_, e)| format_terror(e))
        .collect();
    formatted.sort();

    let module_count = result.typed.modules.len();
    let tycon_count = result.typed.tycons.len();

    // Phase 4.5 T6: compute aggregate totals.
    // OQ-PHASE45-008: aggregate-only; no per-module breakdown in the snapshot.
    let (node_types_populated, schemes_populated, inferred_caps_real_keys) =
        compute_phase45_totals(&result);

    drop(td);
    TypecheckSnapshot {
        errors: formatted,
        module_count,
        tycon_count,
        node_types_populated,
        schemes_populated,
        inferred_caps_real_keys,
    }
}

// ── Acceptance gate tests ─────────────────────────────────────────────────────

/// §9.3 / §11.3 Phase 4 `DoD` acceptance: `log_analyzer.ridge` types cleanly.
#[test]
fn typecheck_log_analyzer() {
    let snap = snapshot_example("log_analyzer");
    assert!(
        snap.errors.is_empty(),
        "log_analyzer must typecheck with zero T-errors; errors: {:#?}",
        snap.errors
    );
    insta::assert_debug_snapshot!("t17_log_analyzer", snap);
}

/// §9.3 / §11.3 Phase 4 `DoD` acceptance: `url_shortener.ridge` types cleanly.
#[test]
fn typecheck_url_shortener() {
    let snap = snapshot_example("url_shortener");
    assert!(
        snap.errors.is_empty(),
        "url_shortener must typecheck with zero T-errors; errors: {:#?}",
        snap.errors
    );
    insta::assert_debug_snapshot!("t17_url_shortener", snap);
}

/// §9.3 / §11.3 Phase 4 `DoD` acceptance: `game_of_life.ridge` types cleanly.
#[test]
fn typecheck_game_of_life() {
    let snap = snapshot_example("game_of_life");
    assert!(
        snap.errors.is_empty(),
        "game_of_life must typecheck with zero T-errors; errors: {:#?}",
        snap.errors
    );
    insta::assert_debug_snapshot!("t17_game_of_life", snap);
}

/// §9.3 / §11.3 Phase 4 `DoD` acceptance: `rate_limiter.ridge` types cleanly.
#[test]
fn typecheck_rate_limiter() {
    let snap = snapshot_example("rate_limiter");
    assert!(
        snap.errors.is_empty(),
        "rate_limiter must typecheck with zero T-errors; errors: {:#?}",
        snap.errors
    );
    insta::assert_debug_snapshot!("t17_rate_limiter", snap);
}

// ── Phase 4.5 T6 fixture tests ────────────────────────────────────────────────

/// Phase 4.5 T6 acceptance: per-expression type recording fires for a fn with
/// literal, ident, and call expressions — verifies T3 write-back coverage.
///
/// Assert: `node_types_populated >= 5` (at minimum the literal, ident-ref,
/// call, and the two fn body blocks contribute expression entries).
// OQ-PHASE45-001: single Expr variant covers all non-wrapper expression shapes.
#[test]
fn phase45_per_expr_typing_basic() {
    let fixture_path = format!(
        "{}/tests/fixtures/phase45/per_expr_typing_basic.ridge",
        env!("CARGO_MANIFEST_DIR")
    );
    let src = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("could not read fixture {fixture_path}: {e}"));
    let td = load_snippet_into_workspace("per_expr_typing_basic.ridge", &src);

    let disc = discover_workspace(td.path());
    let ws_graph = disc.graph.expect("workspace graph");
    let resolved = resolve_workspace(ws_graph);
    let result = typecheck_workspace(&resolved);

    let (node_types_populated, _, _) = compute_phase45_totals(&result);
    assert!(
        node_types_populated >= 5,
        "expected node_types_populated >= 5, got {node_types_populated}"
    );
    drop(td);
}

/// Phase 4.5 T6 acceptance: SCC generalisation writes schemes for top-level
/// polymorphic fns — verifies T4 scheme write-back coverage.
///
/// Assert: `schemes_populated >= 2` (at least the two top-level fns `identity`
/// and `constant` have their generalised Schemes in `TypedModule.schemes`).
// OQ-PHASE45-003: top-level decl schemes only; let-bound locals excluded.
#[test]
fn phase45_polymorphic_let() {
    let fixture_path = format!(
        "{}/tests/fixtures/phase45/polymorphic_let.ridge",
        env!("CARGO_MANIFEST_DIR")
    );
    let src = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("could not read fixture {fixture_path}: {e}"));
    let td = load_snippet_into_workspace("polymorphic_let.ridge", &src);

    let disc = discover_workspace(td.path());
    let ws_graph = disc.graph.expect("workspace graph");
    let resolved = resolve_workspace(ws_graph);
    let result = typecheck_workspace(&resolved);

    let (_, schemes_populated, _) = compute_phase45_totals(&result);
    assert!(
        schemes_populated >= 2,
        "expected schemes_populated >= 2, got {schemes_populated}"
    );
    drop(td);
}

/// Phase 4.5 T6 acceptance: lambda body types are recorded by T3's `infer_expr`
/// shim — verifies that nested expression positions (inside a lambda body)
/// are covered by the write-back pass.
///
/// Assert: `node_types_populated >= 5` (at minimum the lambda body, its
/// subexpressions, and the outer fn body each contribute at least one entry).
// OQ-PHASE45-001: lambda body stamped with NodeKind::Expr (not Block/Try).
#[test]
fn phase45_lambda_body_typing() {
    let fixture_path = format!(
        "{}/tests/fixtures/phase45/lambda_body_typing.ridge",
        env!("CARGO_MANIFEST_DIR")
    );
    let src = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("could not read fixture {fixture_path}: {e}"));
    let td = load_snippet_into_workspace("lambda_body_typing.ridge", &src);

    let disc = discover_workspace(td.path());
    let ws_graph = disc.graph.expect("workspace graph");
    let resolved = resolve_workspace(ws_graph);
    let result = typecheck_workspace(&resolved);

    let (node_types_populated, _, _) = compute_phase45_totals(&result);
    assert!(
        node_types_populated >= 5,
        "expected node_types_populated >= 5, got {node_types_populated}"
    );
    drop(td);
}
