//! Workspace `forbid` rule enforcement (plan §4.9).
//!
//! # Overview
//!
//! [`check_forbid_rules`] walks every resolved import edge in the workspace
//! and emits [`ResolveError::ForbidViolation`] (`R013`) for any
//! `(importer_fqn, target_fqn)` pair that matches a workspace-level
//! `[workspace.rules].forbid` entry.
//!
//! The diagnostic is anchored at the offending `ImportDecl`'s span inside the
//! importing module.  `ResolveError::ForbidViolation::Display` renders the
//! spec §8.6 multi-line output (file/line of the import, rule text, and
//! suggestion) in-place.  No separate Phase-6 renderer pass is needed for
//! this code.
//!
//! # Algorithm (plan §4.9)
//!
//! For each `(importer, target)` edge in `imports`, for each [`ForbidRule`]:
//! - if `rule.from.matches(importer.fqn) && rule.to.matches(target.fqn)`,
//!   emit `R013` keyed at `ImportResolution::span`.
//!
//! Cost is `O(modules × imports_per_module × rules)`.  For a 100-module
//! workspace with an average of 5 imports per module and 20 rules, that is
//! `100 × 5 × 20 = 10 000` glob-match operations — well below any perceptible
//! threshold.
//!
//! # What is skipped
//!
//! - **Synthetic prelude `ImportResolution`s** — the R013/R015 prelude
//!   is implicit, and the user did not write it.  These IRs are detected by
//!   their empty span (`Span::point(0)`) and are silently skipped so they
//!   cannot trigger a forbid violation.
//! - **`ImportTarget::Unresolved`** — `R006` already fired and there is no
//!   meaningful target FQN to match against.  Suppressing R013 here matches
//!   the R011 cascading-error policy.
//!
//! # Self-forbid rules
//!
//! Per plan §4.9 step 2, a rule whose `from` and `to` patterns both match a
//! single edge is reported normally.  In practice, an edge where the
//! importer's FQN equals the target's FQN is already a `R004 SelfImport`,
//! and an `R013` may also fire if the user's manifest defined an explicit
//! self-forbid pattern; both diagnostics are emitted independently.
//!
//! # Integration
//!
//! This pass ships as a standalone function.  Like [`check_capabilities`],
//! wiring into the top-level `resolve_source` / `resolve_workspace`
//! entry point is deferred until snapshot-tests pin the public API.
//!
//! [`check_capabilities`]: crate::capabilities::check_capabilities
//! [`ForbidRule`]: crate::manifest::ForbidRule

use crate::error::ResolveError;
use crate::imports::{ImportResolution, ImportTarget};
use crate::stdlib_builtin::BUILTINS;
use crate::{ModuleId, WorkspaceGraph};

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the workspace `forbid` rule pass over every resolved import.
///
/// `imports` is the per-module [`ImportResolution`] vector produced by
/// [`resolve_imports`](crate::imports::resolve_imports).  `imports[i]` lists
/// the imports of the module whose `ModuleId.0 == i`.
///
/// New errors are appended to `errors`.  The caller owns the vector; this
/// function never clears it.
pub fn check_forbid_rules(
    ws: &WorkspaceGraph,
    imports: &[Vec<ImportResolution>],
    errors: &mut Vec<(ModuleId, ResolveError)>,
) {
    let rules = &ws.manifest.forbid_rules;
    if rules.is_empty() {
        return;
    }

    for (idx, module_imports) in imports.iter().enumerate() {
        let Some(importer_meta) = ws.modules.get(idx) else {
            continue;
        };
        let importer_fqn = importer_meta.fully_qualified_name.as_str();
        let importer_mid = ModuleId(u32::try_from(idx).unwrap_or(u32::MAX));

        for ir in module_imports {
            // Synthetic prelude IRs use `Span::point(0)` (empty span).  The
            // user did not write them, so they cannot violate user-authored
            // architectural rules.
            if ir.span.is_empty() {
                continue;
            }

            let Some(target_fqn) = target_fqn(&ir.target, ws) else {
                continue;
            };

            for rule in rules {
                if rule.from.matches(importer_fqn) && rule.to.matches(target_fqn) {
                    errors.push((
                        importer_mid,
                        ResolveError::ForbidViolation {
                            rule_text: format!("from = {:?} to = {:?}", rule.from.raw, rule.to.raw),
                            importer_fqn: importer_fqn.to_owned(),
                            target_fqn: target_fqn.to_owned(),
                            import_span: ir.span,
                            manifest_span: None, // TODO(DR-02): plumb toml_edit span
                            suggestion: None,    // use Display default
                        },
                    ));
                }
            }
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Resolve an [`ImportTarget`] to a dotted FQN string for rule matching.
///
/// Returns `None` for [`ImportTarget::Unresolved`] (no target) or for an
/// out-of-bounds `ModuleId` / `StdlibModuleId` (defensive — never happens on
/// well-formed inputs).
fn target_fqn<'a>(target: &'a ImportTarget, ws: &'a WorkspaceGraph) -> Option<&'a str> {
    match target {
        ImportTarget::WorkspaceModule(mid) => ws
            .modules
            .get(mid.0 as usize)
            .map(|m| m.fully_qualified_name.as_str()),
        ImportTarget::BuiltinStdlib(sid) => BUILTINS.get(sid.0 as usize).map(|m| m.name),
        ImportTarget::External { module, .. } => Some(module.as_str()),
        ImportTarget::Unresolved => None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ridge_ast::Span;

    use super::*;
    use crate::globs::GlobPattern;
    use crate::imports::{ImportResolution, ImportTarget};
    use crate::manifest::{ForbidRule, WorkspaceManifest};
    use crate::stdlib_builtin::StdlibModuleId;
    use crate::{ModuleId, ModuleMetadata, NodeId, ProjectId, WorkspaceGraph};

    // ── Fixture builders ──────────────────────────────────────────────────────

    fn module(id: u32, fqn: &str) -> ModuleMetadata {
        ModuleMetadata {
            id: ModuleId(id),
            project: ProjectId(0),
            fully_qualified_name: fqn.to_owned(),
            file_path: PathBuf::from(format!("src/{fqn}.ridge")),
            span_within_file: Span::point(0),
        }
    }

    fn rule(from: &str, to: &str) -> ForbidRule {
        ForbidRule {
            from: GlobPattern::new(from).unwrap(),
            to: GlobPattern::new(to).unwrap(),
            source_span: Span::point(0),
        }
    }

    fn manifest(rules: Vec<ForbidRule>) -> WorkspaceManifest {
        WorkspaceManifest {
            name: "acme".to_owned(),
            version: "0.1.0".to_owned(),
            members_globs: vec![],
            dependencies: vec![],
            forbid_rules: rules,
            capabilities_deny: vec![],
            source_path: PathBuf::from("ridge.toml"),
        }
    }

    /// Build a workspace from a list of `(fqn, [target_module_id])` rows.
    /// Each row defines a module and its outgoing workspace import edges.
    fn workspace(modules: Vec<&str>, rules: Vec<ForbidRule>) -> WorkspaceGraph {
        let modules: Vec<ModuleMetadata> = modules
            .into_iter()
            .enumerate()
            .map(|(i, fqn)| module(u32::try_from(i).unwrap(), fqn))
            .collect();
        let len = modules.len();
        WorkspaceGraph {
            root: PathBuf::from("/ws"),
            manifest: manifest(rules),
            projects: vec![],
            modules,
            deps: vec![vec![]; len],
        }
    }

    /// Build a non-empty span — distinct from the synthetic prelude span
    /// `Span::point(0)`.
    fn user_span() -> Span {
        // Real ImportDecl spans are non-empty; encode that by using a 7-byte
        // span starting at byte 1 (mimicking `import X` at column 1).
        Span::new(1, 8)
    }

    /// Build an `ImportResolution` for a workspace target.
    fn ws_import(target_id: u32, span: Span) -> ImportResolution {
        ImportResolution {
            decl_node: NodeId(0),
            target: ImportTarget::WorkspaceModule(ModuleId(target_id)),
            alias: None,
            explicit_items: None,
            effective_bindings: vec![],
            span,
        }
    }

    /// Build an `ImportResolution` for a stdlib target.
    fn std_import(stdlib_id: u32, span: Span) -> ImportResolution {
        ImportResolution {
            decl_node: NodeId(0),
            target: ImportTarget::BuiltinStdlib(StdlibModuleId(stdlib_id)),
            alias: None,
            explicit_items: None,
            effective_bindings: vec![],
            span,
        }
    }

    /// Build an `ImportResolution` for an unresolved target.
    fn unresolved_import(span: Span) -> ImportResolution {
        ImportResolution {
            decl_node: NodeId(0),
            target: ImportTarget::Unresolved,
            alias: None,
            explicit_items: None,
            effective_bindings: vec![],
            span,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Test 1 — the canonical positive case: one rule, one matching edge,
    /// exactly one R013 with the right importer/target/rule fields.
    #[test]
    fn t1_single_rule_matches_emits_r013() {
        let ws = workspace(
            vec!["acme.domain.UseCases", "acme.infra.Postgres"],
            vec![rule("acme.domain.**", "acme.infra.**")],
        );
        // module 0 imports module 1.
        let imports = vec![vec![ws_import(1, user_span())], vec![]];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);

        assert_eq!(errors.len(), 1, "expected exactly 1 R013, got: {errors:?}");
        let (_, first_err) = &errors[0];
        match first_err {
            ResolveError::ForbidViolation {
                rule_text,
                importer_fqn,
                target_fqn,
                import_span,
                ..
            } => {
                assert_eq!(importer_fqn, "acme.domain.UseCases");
                assert_eq!(target_fqn, "acme.infra.Postgres");
                assert!(
                    rule_text.contains("acme.domain.**"),
                    "rule_text should reference from pattern; got: {rule_text:?}"
                );
                assert_eq!(*import_span, user_span());
            }
            other => panic!("expected ForbidViolation, got: {other:?}"),
        }
        assert_eq!(first_err.code(), "R013");
    }

    /// Test 2 — rule does not match the edge → no error.
    #[test]
    fn t2_no_match_no_error() {
        let ws = workspace(
            vec!["acme.api.Main", "acme.domain.UseCases"],
            vec![rule("acme.domain.**", "acme.infra.**")],
        );
        let imports = vec![vec![ws_import(1, user_span())], vec![]];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    /// Test 3 — empty rule list short-circuits to zero errors regardless of
    /// the import shape.
    #[test]
    fn t3_no_rules_short_circuits() {
        let ws = workspace(vec!["acme.a.X", "acme.b.Y"], vec![]);
        let imports = vec![vec![ws_import(1, user_span())], vec![]];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);
        assert!(errors.is_empty());
    }

    /// Test 4 — multiple rules; only one matches the edge → exactly one R013
    /// with the matching rule's text.
    #[test]
    fn t4_multiple_rules_only_matching_emits() {
        let ws = workspace(
            vec!["acme.domain.UseCases", "acme.infra.Postgres"],
            vec![
                rule("acme.api.**", "acme.infra.**"),     // does not match
                rule("acme.domain.**", "acme.infra.**"),  // matches
                rule("acme.shared.**", "acme.domain.**"), // does not match
            ],
        );
        let imports = vec![vec![ws_import(1, user_span())], vec![]];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);
        assert_eq!(errors.len(), 1, "expected 1 error, got: {errors:?}");
        match &errors[0].1 {
            ResolveError::ForbidViolation { rule_text, .. } => {
                assert!(
                    rule_text.contains("acme.domain.**"),
                    "expected rule_text to contain from pattern; got: {rule_text:?}"
                );
            }
            other => panic!("expected ForbidViolation, got: {other:?}"),
        }
    }

    /// Test 5 — `Unresolved` target is silently skipped (R006 already fired).
    #[test]
    fn t5_unresolved_target_is_skipped() {
        let ws = workspace(
            vec!["acme.domain.UseCases"],
            vec![rule("acme.domain.**", "acme.**")],
        );
        let imports = vec![vec![unresolved_import(user_span())]];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);
        assert!(
            errors.is_empty(),
            "Unresolved imports must not produce R013, got: {errors:?}"
        );
    }

    /// Test 6 — synthetic prelude IRs (empty span) are skipped even if their
    /// stdlib target would match a forbid rule.  Without this, every module
    /// in a workspace whose forbid rules cover stdlib paths would emit
    /// spurious R013s for the prelude.
    #[test]
    fn t6_synthetic_prelude_is_skipped() {
        let ws = workspace(
            vec!["acme.domain.UseCases"],
            // A rule that would match the std.option prelude IR if not skipped.
            vec![rule("acme.domain.**", "std.**")],
        );
        // StdlibModuleId(7) = std.option per stdlib_builtin.rs ordering.
        let imports = vec![vec![std_import(7, Span::point(0))]];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);
        assert!(
            errors.is_empty(),
            "synthetic prelude IRs must not trigger R013, got: {errors:?}"
        );
    }

    /// Test 7 — a rule may target a stdlib module via its dotted name,
    /// e.g. `forbid acme.domain.* -> std.fs` to ban filesystem access from
    /// the domain layer.  An explicit `import std.fs` (non-empty span)
    /// triggers R013 normally.
    #[test]
    fn t7_stdlib_target_can_be_forbidden() {
        // BUILTINS includes std.fs at some StdlibModuleId; we use std.list
        // (id 4) as a stand-in known to exist in the canonical builtin table.
        // The rule pattern `std.list` exact-matches one segment `list` after
        // `std`.
        let ws = workspace(
            vec!["acme.domain.X"],
            vec![rule("acme.domain.**", "std.list")],
        );
        // StdlibModuleId(4) = std.list per stdlib_builtin.rs.
        let imports = vec![vec![std_import(4, user_span())]];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);
        assert_eq!(errors.len(), 1, "expected 1 R013, got: {errors:?}");
        match &errors[0].1 {
            ResolveError::ForbidViolation { target_fqn, .. } => {
                assert_eq!(target_fqn, "std.list");
            }
            other => panic!("expected ForbidViolation, got: {other:?}"),
        }
    }

    /// Test 8 — cross-product: two importers, one rule.  Only the importer
    /// matching `rule.from` triggers R013, the other (matching the same
    /// target but a different `from`) does not.
    #[test]
    fn t8_cross_product_only_matching_importer_fires() {
        let ws = workspace(
            vec![
                "acme.domain.A", // matches `acme.domain.**` (importer)
                "acme.api.B",    // does not match `acme.domain.**`
                "acme.infra.Pg", // both target this
            ],
            vec![rule("acme.domain.**", "acme.infra.**")],
        );
        let imports = vec![
            vec![ws_import(2, user_span())], // domain.A → infra.Pg : R013
            vec![ws_import(2, user_span())], // api.B    → infra.Pg : OK
            vec![],                          // infra.Pg has no imports
        ];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);
        assert_eq!(errors.len(), 1, "expected 1 R013, got: {errors:?}");
        match &errors[0].1 {
            ResolveError::ForbidViolation { importer_fqn, .. } => {
                assert_eq!(importer_fqn, "acme.domain.A");
            }
            other => panic!("expected ForbidViolation, got: {other:?}"),
        }
    }

    /// Test 9 — multiple rules can each match the same edge independently;
    /// each emits its own R013 (one diagnostic per matching rule).
    #[test]
    fn t9_multiple_matching_rules_emit_one_r013_each() {
        let ws = workspace(
            vec!["acme.domain.X", "acme.infra.Y"],
            vec![
                rule("acme.domain.**", "acme.infra.**"), // matches
                rule("acme.**", "acme.infra.**"),        // also matches
            ],
        );
        let imports = vec![vec![ws_import(1, user_span())], vec![]];

        let mut errors = Vec::new();
        check_forbid_rules(&ws, &imports, &mut errors);
        assert_eq!(errors.len(), 2, "expected 2 R013, got: {errors:?}");
        // Both errors share importer/target; only the rule string differs.
        for (_, e) in &errors {
            match e {
                ResolveError::ForbidViolation {
                    importer_fqn,
                    target_fqn,
                    ..
                } => {
                    assert_eq!(importer_fqn, "acme.domain.X");
                    assert_eq!(target_fqn, "acme.infra.Y");
                }
                other => panic!("expected ForbidViolation, got: {other:?}"),
            }
        }
    }
}
