//! T17 — workspace-fixture harness for `ridge-typecheck` (plan §10 T17,
//! §9.5, §11.3 `DoD` lines 1547–1551).
//!
//! Runs the full `discover → resolve → typecheck` pipeline on each
//! committed multi-project workspace under `tests/fixtures/workspace/`:
//!
//! - `acme_typed_happy/` — two cooperating projects (`acme.shared` +
//!   `acme.app`); cross-project imports flow records and pure fns.  Must
//!   typecheck with zero T-errors and produce 2 typed modules.
//! - `acme_typed_caps/`  — two projects exercising cross-project capability
//!   propagation (`{io}`-requiring fn re-used in another project's `pub`
//!   surface).  Must typecheck cleanly.
//!
//! These fixtures verify D076 (`exported_externally` flag) and D077
//! (absent `[project.exports].public` defaults) end-to-end.
//!
//! Each test also captures an `insta` snapshot of a deterministic
//! projection (errors / `module_count` / `tycon_count`) so that any drift
//! in cross-project inference is caught by `cargo insta test`. This
//! satisfies plan §14.3: "4 example snapshots + 2 workspace snapshots
//! (6 total)".

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("workspace")
        .join(name)
}

/// Deterministic, snapshot-friendly projection of a workspace typecheck
/// result. Mirrors the shape used by `snapshots.rs::TypecheckSnapshot`
/// so the 6 Phase 4 snapshots have a uniform format.
#[allow(dead_code)]
#[derive(Debug)]
struct WorkspaceSnapshot {
    /// Formatted T### errors sorted for cross-platform determinism.
    errors: Vec<String>,
    /// Number of typed modules produced.
    module_count: usize,
    /// Number of `TyCons` in the shared arena (builtins + user-defined).
    tycon_count: usize,
}

fn format_terror(e: &TypeError) -> String {
    format!("{}: {}", e.code(), e)
}

fn snapshot_workspace(name: &str) -> WorkspaceSnapshot {
    let root = fixture_root(name);
    assert!(
        root.is_dir(),
        "workspace fixture missing: {}",
        root.display()
    );

    let disc = discover_workspace(&root);
    assert!(
        disc.resolve_errors.is_empty(),
        "{name}: discovery R-errors: {:#?}",
        disc.resolve_errors
    );
    assert!(
        disc.manifest_errors.is_empty(),
        "{name}: discovery M-errors: {:#?}",
        disc.manifest_errors
    );
    let ws_graph = disc.graph.expect("workspace graph present");
    let resolved = resolve_workspace(ws_graph);
    assert!(
        resolved.errors.is_empty(),
        "{name}: resolve errors: {:#?}",
        resolved.errors
    );

    let result = typecheck_workspace(&resolved);

    let mut formatted: Vec<String> = result
        .errors
        .iter()
        .map(|(_, e)| format_terror(e))
        .collect();
    formatted.sort();

    WorkspaceSnapshot {
        errors: formatted,
        module_count: result.typed.modules.len(),
        tycon_count: result.typed.tycons.len(),
    }
}

/// `acme_typed_happy` — the two-project happy path; zero T-errors expected
/// across both modules.
#[test]
fn workspace_acme_typed_happy_typechecks_clean() {
    let snap = snapshot_workspace("acme_typed_happy");
    assert!(
        snap.errors.is_empty(),
        "acme_typed_happy: expected zero T-errors, got {:#?}",
        snap.errors
    );
    assert_eq!(
        snap.module_count, 2,
        "acme_typed_happy: expected 2 typed modules (shared.types + app.main)"
    );
    insta::assert_debug_snapshot!("t17_workspace_acme_typed_happy", snap);
}

/// `acme_typed_caps` — cross-project capability propagation; zero T-errors
/// expected since the consumer correctly re-declares `{io}`.
#[test]
fn workspace_acme_typed_caps_typechecks_clean() {
    let snap = snapshot_workspace("acme_typed_caps");
    assert!(
        snap.errors.is_empty(),
        "acme_typed_caps: expected zero T-errors, got {:#?}",
        snap.errors
    );
    assert_eq!(
        snap.module_count, 2,
        "acme_typed_caps: expected 2 typed modules (logger.log + app.main)"
    );
    insta::assert_debug_snapshot!("t17_workspace_acme_typed_caps", snap);
}
