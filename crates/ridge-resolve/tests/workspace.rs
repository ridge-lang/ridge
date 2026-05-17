//! T14 workspace-level snapshot tests (plan §10 T14, files-touched list
//! includes `tests/workspace.rs`).
//!
//! Two synthetic multi-project workspaces (committed on-disk fixtures):
//! - `acme_happy/`  — no architectural-rule violation; `errors` must be empty.
//! - `acme_forbid/` — exactly one `R013 ForbidViolation` per plan `DoD`.
//!
//! Fixture trees live under `tests/fixtures/workspace/` and are loaded via
//! `acme_workspace_path(name)`.  Snapshots live in
//! `tests/snapshots/snapshots__t14_*.snap`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_resolve::{
    discover_workspace, resolve_workspace, ImportTarget, ResolveError, ResolvedVisibility,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ── Fixture helpers ───────────────────────────────────────────────────────────

/// Return the path to a committed workspace fixture tree.
///
/// `name` must be a subdirectory of `tests/fixtures/workspace/` relative to
/// `CARGO_MANIFEST_DIR` (e.g. `"acme_happy"`, `"acme_forbid"`).
fn acme_workspace_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/workspace")
        .join(name)
}

// ── Snapshot value type ──────────────────────────────────────────────────────

/// Deterministic, snapshot-friendly view of a multi-project workspace after
/// the full T7..T13 pipeline + T12 forbid pass.
///
/// - `module_fqns`: all discovered modules sorted by FQN.
/// - `errors`: every `R-error` (discovery + import + cycle + forbid) formatted
///   `"<R-code>: <body>"` and sorted for cross-platform determinism.
/// - `forbid_violations`: full structured detail for any `R013` so the `DoD`
///   "exactly 1 `R013` on `acme_forbid` with the spec §8.6 fields" is reviewable.
/// - `import_summary`: per-module list of `"importer ⇒ alias -> target"`.
///
/// Fields are consumed by `insta` via the derived `Debug`; suppress the
/// dead-code lint that cannot see through the formatter.
#[allow(dead_code)]
#[derive(Debug)]
struct WorkspaceSnapshot {
    module_fqns: Vec<String>,
    errors: Vec<String>,
    forbid_violations: Vec<ForbidViolationView>,
    import_summary: BTreeMap<String, Vec<String>>,
}

#[allow(dead_code)]
#[derive(Debug)]
struct ForbidViolationView {
    importer: String,
    target: String,
    rule: String,
    span: String,
}

fn format_error(e: &ResolveError) -> String {
    use ridge_lexer::Span;
    fn span_str(s: Span) -> String {
        format!("{}..{}", s.start, s.end)
    }
    let code = e.code();
    let body = match e {
        ResolveError::ForbidViolation {
            rule_text,
            importer_fqn,
            target_fqn,
            import_span,
            ..
        } => format!(
            "ForbidViolation importer={importer_fqn:?} target={target_fqn:?} rule={rule_text:?} span={}",
            span_str(*import_span)
        ),
        other => format!("{other:?}"),
    };
    format!("{code}: {body}")
}

/// Run the full discovery → graph → symbols → imports → cycles → forbid pipeline
/// over the workspace at `path` and produce a deterministic [`WorkspaceSnapshot`].
fn snapshot_workspace(path: &Path) -> WorkspaceSnapshot {
    let disc = discover_workspace(path);
    assert!(
        disc.resolve_errors.is_empty(),
        "discovery R-errors: {:?}",
        disc.resolve_errors
    );
    assert!(
        disc.manifest_errors.is_empty(),
        "discovery M-errors: {:?}",
        disc.manifest_errors
    );
    let ws = disc.graph.expect("graph present on happy path");
    let resolved = resolve_workspace(ws);

    let mut module_fqns: Vec<String> = resolved
        .graph
        .modules
        .iter()
        .map(|m| m.fully_qualified_name.clone())
        .collect();
    module_fqns.sort();

    let mut import_summary: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for rm in &resolved.modules {
        let importer_fqn = resolved.graph.modules[rm.id.0 as usize]
            .fully_qualified_name
            .clone();
        let mut entries: Vec<String> = rm
            .imports
            .iter()
            .map(|ir| {
                let alias = ir.alias.clone().unwrap_or_else(|| "<bare>".to_string());
                let target = match &ir.target {
                    ImportTarget::WorkspaceModule(m) => format!(
                        "WorkspaceModule({})",
                        resolved.graph.modules[m.0 as usize].fully_qualified_name
                    ),
                    ImportTarget::BuiltinStdlib(m) => format!("BuiltinStdlib({})", m.0),
                    ImportTarget::External { .. } => "External".to_string(),
                    ImportTarget::Unresolved => "Unresolved".to_string(),
                    _ => "Unknown".to_string(),
                };
                format!("{alias} -> {target}")
            })
            .collect();
        entries.sort();
        import_summary.insert(importer_fqn, entries);
    }

    let forbid_violations: Vec<ForbidViolationView> = resolved
        .errors
        .iter()
        .filter_map(|(_, e)| match e {
            ResolveError::ForbidViolation {
                rule_text,
                importer_fqn,
                target_fqn,
                import_span,
                ..
            } => Some(ForbidViolationView {
                importer: importer_fqn.clone(),
                target: target_fqn.clone(),
                rule: rule_text.clone(),
                span: format!("{}..{}", import_span.start, import_span.end),
            }),
            _ => None,
        })
        .collect();

    let mut formatted: Vec<String> = resolved
        .errors
        .iter()
        .map(|(_, e)| format_error(e))
        .collect();
    formatted.sort();

    WorkspaceSnapshot {
        module_fqns,
        errors: formatted,
        forbid_violations,
        import_summary,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `acme_happy` — no `forbid` rule, both modules resolve cleanly.  `DoD` §14.4
/// requires zero R-errors.
#[test]
fn t14_snapshot_acme_happy() {
    let path = acme_workspace_path("acme_happy");
    let snap = snapshot_workspace(&path);
    assert!(
        snap.errors.is_empty(),
        "acme_happy must produce zero R-errors; errors: {:#?}",
        snap.errors
    );
    assert!(
        snap.forbid_violations.is_empty(),
        "acme_happy must produce zero forbid violations"
    );
    insta::assert_debug_snapshot!("t14_acme_happy", snap);
}

/// `acme_forbid` — one `forbid` rule that the only edge violates.  `DoD` §10 T14
/// + spec §8.6 require exactly one structured `R013`.
#[test]
fn t14_snapshot_acme_forbid() {
    let path = acme_workspace_path("acme_forbid");
    let snap = snapshot_workspace(&path);
    assert_eq!(
        snap.errors.len(),
        1,
        "acme_forbid must emit exactly 1 R013; errors: {:#?}",
        snap.errors
    );
    assert_eq!(
        snap.forbid_violations.len(),
        1,
        "acme_forbid must record exactly 1 structured R013"
    );
    let v = &snap.forbid_violations[0];
    assert_eq!(v.importer, "acme.domain.RegisterUser");
    assert_eq!(v.target, "acme.infra.Postgres");
    // rule_text uses the new DR-02 format: from = "..." to = "..."
    assert!(
        v.rule.contains("acme.domain.**"),
        "rule should contain from pattern; got: {:?}",
        v.rule
    );
    insta::assert_debug_snapshot!("t14_acme_forbid", snap);
}

/// DR-08 round-trip test: `exported_externally` is set for every `pub` symbol
/// in a project with `[project.exports].public = ["**"]`.
///
/// `acme_happy` has two projects (both `public = ["**"]`):
/// - `acme.infra.Postgres` — exports `pub fn connect()`
/// - `acme.domain.RegisterUser` — exports `pub fn doIt()`
///
/// After `resolve_workspace`, every non-synthesised `pub` symbol in both modules
/// must have `exported_externally = true`.
#[test]
fn dr08_exported_externally_roundtrip() {
    let path = acme_workspace_path("acme_happy");
    let disc = discover_workspace(&path);
    assert!(
        disc.resolve_errors.is_empty(),
        "discovery R-errors: {:?}",
        disc.resolve_errors
    );
    let ws = disc.graph.expect("graph present");
    let resolved = resolve_workspace(ws);

    // No manifest errors from M020 (all symbols are pub).
    assert!(
        resolved.manifest_errors.is_empty(),
        "acme_happy: unexpected M-errors from apply_external_exports: {:?}",
        resolved.manifest_errors
    );

    // Every pub symbol in every module must have exported_externally = true.
    // Non-pub symbols must have exported_externally = false.
    let mut pub_exported = 0usize;
    let mut nonpub_not_exported = 0usize;

    for rm in &resolved.modules {
        for entry in &rm.symbols.entries {
            if entry.visibility == ResolvedVisibility::Pub {
                assert!(
                    entry.exported_externally,
                    "pub symbol {:?} in {:?} must have exported_externally = true",
                    entry.name, rm.id
                );
                pub_exported += 1;
            } else {
                assert!(
                    !entry.exported_externally,
                    "non-pub symbol {:?} in {:?} must have exported_externally = false",
                    entry.name, rm.id
                );
                nonpub_not_exported += 1;
            }
        }
    }

    // Sanity: at least one pub symbol must exist (doIt, connect).
    assert!(
        pub_exported > 0,
        "expected at least one pub exported symbol"
    );
    // Sanity: the non-pub count tracks correctly (may be zero if all are pub).
    let _ = nonpub_not_exported; // used in assertion loop above
}
