//! Compatibility classification of a change set.

use ridge_types::CapabilitySet;

use crate::diff::{ChangeSet, ModuleChange, SymbolChange};
use crate::render::capability_name;
use crate::scaffold::{self, FieldAction};

/// The reload verdict for one changed symbol or module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Reload applies with no state work.
    Compatible,
    /// Reload applies; the compiler derives the migration (additive actor
    /// state with defaults).
    AutoMigrate { note: String },
    /// Reload applies only after the user accepts/completes the scaffold.
    RequiresMigration { scaffold: String, has_holes: bool },
    /// Reload cannot apply; reason is user-facing.
    Incompatible { reason: String },
}

/// One row of the check report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolVerdict {
    /// Module FQN the change belongs to.
    pub module: String,
    /// Symbol name (module FQN again for module-level rows).
    pub symbol: String,
    /// The compatibility verdict.
    pub verdict: Verdict,
}

/// The full `reload --check` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// One verdict per change, in diff order.
    pub verdicts: Vec<SymbolVerdict>,
}

impl CheckReport {
    /// True when nothing is `Incompatible`.
    #[must_use]
    pub fn is_reloadable(&self) -> bool {
        self.verdicts.iter().all(|v| !matches!(v.verdict, Verdict::Incompatible { .. }))
    }

    /// True when any scaffold still contains a `???` hole.
    #[must_use]
    pub fn has_holes(&self) -> bool {
        self.verdicts.iter().any(|v| {
            matches!(v.verdict, Verdict::RequiresMigration { has_holes: true, .. })
        })
    }
}

/// Classifies every change. Deterministic output order (input order).
#[must_use]
pub fn check(cs: &ChangeSet) -> CheckReport {
    let mut verdicts = Vec::new();
    for m in &cs.modules {
        match m {
            ModuleChange::Added { fqn } => verdicts.push(SymbolVerdict {
                module: fqn.clone(),
                symbol: fqn.clone(),
                verdict: Verdict::Compatible,
            }),
            ModuleChange::Removed { fqn, had_public_symbols } => {
                let verdict = if *had_public_symbols {
                    Verdict::Incompatible {
                        reason: format!("module `{fqn}` with public symbols was removed"),
                    }
                } else {
                    Verdict::Compatible
                };
                verdicts.push(SymbolVerdict { module: fqn.clone(), symbol: fqn.clone(), verdict });
            }
            ModuleChange::Changed { fqn, symbols } => {
                for s in symbols {
                    verdicts.push(SymbolVerdict {
                        module: fqn.clone(),
                        symbol: change_name(s).to_string(),
                        verdict: classify(fqn, s),
                    });
                }
            }
        }
    }
    CheckReport { verdicts }
}

/// The symbol name carried by any `SymbolChange` variant.
fn change_name(c: &SymbolChange) -> &str {
    match c {
        SymbolChange::Added { name }
        | SymbolChange::Removed { name, .. }
        | SymbolChange::FnSignatureChanged { name, .. }
        | SymbolChange::FnCapsChanged { name, .. }
        | SymbolChange::ConstChanged { name, .. }
        | SymbolChange::RecordShapeChanged { name, .. }
        | SymbolChange::UnionVariantsChanged { name, .. }
        | SymbolChange::AliasChanged { name, .. }
        | SymbolChange::ActorStateChanged { name, .. }
        | SymbolChange::ActorHandlersChanged { name, .. }
        | SymbolChange::KindChanged { name, .. } => name,
    }
}

/// The compatibility relation over one symbol change.
fn classify(fqn: &str, c: &SymbolChange) -> Verdict {
    match c {
        SymbolChange::Added { .. } => Verdict::Compatible,
        SymbolChange::Removed { old_kind, .. } => Verdict::Incompatible {
            reason: format!("public {old_kind} was removed"),
        },
        SymbolChange::FnSignatureChanged { old, new, .. } => Verdict::Incompatible {
            reason: format!("signature changed ({old} -> {new})"),
        },
        SymbolChange::FnCapsChanged { old_bits, new_bits, .. } => {
            caps_verdict(*old_bits, *new_bits)
        }
        SymbolChange::ConstChanged { old, new, .. } => Verdict::Incompatible {
            reason: format!("const type changed ({old} -> {new})"),
        },
        SymbolChange::AliasChanged { old, new, .. } => Verdict::Incompatible {
            reason: format!("alias target changed ({old} -> {new})"),
        },
        SymbolChange::KindChanged { old_kind, new_kind, .. } => Verdict::Incompatible {
            reason: format!("symbol kind changed ({old_kind} -> {new_kind})"),
        },
        SymbolChange::RecordShapeChanged { name, old_version, old_fields, new_fields } => {
            let plan = scaffold::field_plan(old_fields, new_fields);
            let has_holes = plan.iter().any(|a| matches!(a, FieldAction::Hole { .. }));
            Verdict::RequiresMigration {
                scaffold: scaffold::record_migrate(
                    fqn,
                    name,
                    *old_version,
                    new_fields,
                    &plan,
                    &[],
                ),
                has_holes,
            }
        }
        SymbolChange::UnionVariantsChanged { removed, payload_changed, .. } => {
            if !removed.is_empty() {
                let names: Vec<&str> = removed.iter().map(|v| v.name.as_str()).collect();
                Verdict::Incompatible {
                    reason: format!("union variants removed: {}", names.join(", ")),
                }
            } else if !payload_changed.is_empty() {
                Verdict::Incompatible {
                    reason: format!("union variant payloads changed: {}", payload_changed.join(", ")),
                }
            } else {
                // Pure append: the reverse-dependency closure recompiles and
                // the workspace typechecks clean, so every match is safe.
                Verdict::Compatible
            }
        }
        SymbolChange::ActorStateChanged { name, old_state, new_state } => {
            actor_state_verdict(name, old_state, new_state)
        }
        SymbolChange::ActorHandlersChanged { removed, caps_changed, .. } => {
            if removed.is_empty() {
                let widened: Vec<&(String, u16, u16)> = caps_changed
                    .iter()
                    .filter(|(_, old, new)| new & !old != 0)
                    .collect();
                if widened.is_empty() {
                    Verdict::Compatible
                } else {
                    let details: Vec<String> = widened
                        .iter()
                        .map(|(h, old, new)| format!("`{h}` gained {}", gained_caps(*old, *new)))
                        .collect();
                    Verdict::Incompatible {
                        reason: format!("handler capabilities widened: {}", details.join(", ")),
                    }
                }
            } else {
                Verdict::Incompatible {
                    reason: format!("actor handlers removed: {}", removed.join(", ")),
                }
            }
        }
    }
}

/// Caps widening is incompatible; narrowing is compatible.
fn caps_verdict(old_bits: u16, new_bits: u16) -> Verdict {
    if new_bits & !old_bits == 0 {
        Verdict::Compatible
    } else {
        Verdict::Incompatible {
            reason: format!("capability set widened (gained {})", gained_caps(old_bits, new_bits)),
        }
    }
}

/// Comma-separated names of the capabilities in `new` but not in `old`.
fn gained_caps(old_bits: u16, new_bits: u16) -> String {
    CapabilitySet::from_bits(new_bits & !old_bits)
        .iter()
        .map(capability_name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Actor state deltas: pure additions with defaults auto-migrate; a pure
/// rename yields a complete scaffold; anything else yields a scaffold with
/// holes.
fn actor_state_verdict(
    name: &str,
    old_state: &[crate::snapshot::StateSnap],
    new_state: &[crate::snapshot::StateSnap],
) -> Verdict {
    let has_removed = old_state
        .iter()
        .any(|o| !new_state.iter().any(|n| n.name == o.name));
    let added: Vec<&crate::snapshot::StateSnap> = new_state
        .iter()
        .filter(|s| !old_state.iter().any(|o| o.name == s.name))
        .collect();
    let retyped = old_state.iter().any(|o| {
        new_state.iter().any(|n| n.name == o.name && n.ty != o.ty)
    });

    if !has_removed && !retyped && !added.is_empty() && added.iter().all(|s| s.has_default)
    {
        let names: Vec<String> = added.iter().map(|s| format!("`{}`", s.name)).collect();
        let note = if names.len() == 1 {
            format!("state field {} gets its default", names[0])
        } else {
            format!("state fields {} get their defaults", names.join(", "))
        };
        return Verdict::AutoMigrate { note };
    }

    let plan = scaffold::state_plan(old_state, new_state);
    let has_holes = plan.iter().any(|a| matches!(a, FieldAction::Hole { .. }));
    // Actors have no parsed `@version` yet; scaffolds reference version 1.
    Verdict::RequiresMigration {
        scaffold: scaffold::actor_migrate(name, 1, &plan, &[]),
        has_holes,
    }
}

#[cfg(test)]
mod tests {
    use crate::diff::{ChangeSet, ModuleChange, SymbolChange};
    use crate::snapshot::{FieldSnap, StateSnap, VariantSnap};

    use super::*;

    fn cs(symbols: Vec<SymbolChange>) -> ChangeSet {
        ChangeSet {
            modules: vec![ModuleChange::Changed { fqn: "app.m".to_string(), symbols }],
        }
    }

    fn only(cs: &ChangeSet) -> Verdict {
        let report = check(cs);
        assert_eq!(report.verdicts.len(), 1, "expected one verdict");
        report.verdicts[0].verdict.clone()
    }

    fn fields(pairs: &[(&str, &str)]) -> Vec<FieldSnap> {
        pairs
            .iter()
            .map(|(n, t)| FieldSnap { name: (*n).to_string(), ty: (*t).to_string() })
            .collect()
    }

    fn states(pairs: &[(&str, &str, bool)]) -> Vec<StateSnap> {
        pairs
            .iter()
            .map(|(n, t, d)| StateSnap {
                name: (*n).to_string(),
                ty: (*t).to_string(),
                has_default: *d,
            })
            .collect()
    }

    fn variants(pairs: &[(&str, &str)]) -> Vec<VariantSnap> {
        pairs
            .iter()
            .map(|(n, p)| VariantSnap { name: (*n).to_string(), payload: (*p).to_string() })
            .collect()
    }

    // ── fns ─────────────────────────────────────────────────────────────

    #[test]
    fn added_fn_compatible() {
        let v = only(&cs(vec![SymbolChange::Added { name: "f".to_string() }]));
        assert_eq!(v, Verdict::Compatible);
    }

    #[test]
    fn removed_pub_fn_incompatible() {
        let v = only(&cs(vec![SymbolChange::Removed { name: "f".to_string(), old_kind: "fn" }]));
        assert!(matches!(v, Verdict::Incompatible { .. }), "{v:?}");
    }

    #[test]
    fn fn_signature_change_incompatible() {
        let v = only(&cs(vec![SymbolChange::FnSignatureChanged {
            name: "f".to_string(),
            old: "fn(Int) -> Int".to_string(),
            new: "fn(Text) -> Int".to_string(),
        }]));
        match v {
            Verdict::Incompatible { reason } => {
                assert!(reason.contains("fn(Int) -> Int -> fn(Text) -> Int"), "{reason}");
            }
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn fn_caps_widening_incompatible() {
        // bit 0 = io
        let v = only(&cs(vec![SymbolChange::FnCapsChanged {
            name: "f".to_string(),
            old_bits: 0,
            new_bits: 1,
        }]));
        match v {
            Verdict::Incompatible { reason } => assert!(reason.contains("io"), "{reason}"),
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn fn_caps_narrowing_compatible() {
        let v = only(&cs(vec![SymbolChange::FnCapsChanged {
            name: "f".to_string(),
            old_bits: 1,
            new_bits: 0,
        }]));
        assert_eq!(v, Verdict::Compatible);
    }

    // ── records ─────────────────────────────────────────────────────────

    #[test]
    fn record_additive_requires_migration_with_hole() {
        let v = only(&cs(vec![SymbolChange::RecordShapeChanged {
            name: "User".to_string(),
            old_version: 1,
            old_fields: fields(&[("name", "Text")]),
            new_fields: fields(&[("name", "Text"), ("email", "Text")]),
        }]));
        match v {
            Verdict::RequiresMigration { scaffold, has_holes } => {
                assert!(has_holes, "added field has no fill");
                assert!(scaffold.contains("email: ???"), "{scaffold}");
                assert!(scaffold.contains("@version(2)"), "{scaffold}");
            }
            other => panic!("expected RequiresMigration, got {other:?}"),
        }
    }

    #[test]
    fn record_rename_heuristic_full_scaffold() {
        let v = only(&cs(vec![SymbolChange::RecordShapeChanged {
            name: "User".to_string(),
            old_version: 1,
            old_fields: fields(&[("name", "Text"), ("age", "Int")]),
            new_fields: fields(&[("full_name", "Text"), ("age", "Int")]),
        }]));
        match v {
            Verdict::RequiresMigration { scaffold, has_holes } => {
                assert!(!has_holes, "rename scaffold is complete");
                assert!(scaffold.contains("full_name: old.name"), "{scaffold}");
            }
            other => panic!("expected RequiresMigration, got {other:?}"),
        }
    }

    #[test]
    fn record_field_removed_requires_migration() {
        let v = only(&cs(vec![SymbolChange::RecordShapeChanged {
            name: "User".to_string(),
            old_version: 1,
            old_fields: fields(&[("name", "Text"), ("age", "Int")]),
            new_fields: fields(&[("name", "Text")]),
        }]));
        match v {
            Verdict::RequiresMigration { scaffold, has_holes } => {
                assert!(!has_holes, "dropping a field needs no fill");
                assert!(!scaffold.contains("age"), "{scaffold}");
            }
            other => panic!("expected RequiresMigration, got {other:?}"),
        }
    }

    #[test]
    fn record_field_retyped_requires_migration() {
        let v = only(&cs(vec![SymbolChange::RecordShapeChanged {
            name: "User".to_string(),
            old_version: 1,
            old_fields: fields(&[("age", "Int")]),
            new_fields: fields(&[("age", "Text")]),
        }]));
        match v {
            Verdict::RequiresMigration { has_holes, .. } => {
                assert!(has_holes, "retyped field cannot be auto-filled");
            }
            other => panic!("expected RequiresMigration, got {other:?}"),
        }
    }

    // ── unions ──────────────────────────────────────────────────────────

    #[test]
    fn union_append_compatible() {
        let v = only(&cs(vec![SymbolChange::UnionVariantsChanged {
            name: "E".to_string(),
            old_version: 1,
            added: variants(&[("D", "")]),
            removed: vec![],
            payload_changed: vec![],
        }]));
        assert_eq!(v, Verdict::Compatible);
    }

    #[test]
    fn union_variant_removed_incompatible() {
        let v = only(&cs(vec![SymbolChange::UnionVariantsChanged {
            name: "E".to_string(),
            old_version: 1,
            added: vec![],
            removed: variants(&[("C", "Text")]),
            payload_changed: vec![],
        }]));
        match v {
            Verdict::Incompatible { reason } => assert!(reason.contains('C'), "{reason}"),
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn union_payload_change_incompatible() {
        let v = only(&cs(vec![SymbolChange::UnionVariantsChanged {
            name: "E".to_string(),
            old_version: 1,
            added: vec![],
            removed: vec![],
            payload_changed: vec!["B".to_string()],
        }]));
        match v {
            Verdict::Incompatible { reason } => assert!(reason.contains('B'), "{reason}"),
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    // ── aliases, consts ─────────────────────────────────────────────────

    #[test]
    fn alias_change_incompatible() {
        let v = only(&cs(vec![SymbolChange::AliasChanged {
            name: "Bag".to_string(),
            old: "List<Int>".to_string(),
            new: "List<Text>".to_string(),
        }]));
        assert!(matches!(v, Verdict::Incompatible { .. }), "{v:?}");
    }

    #[test]
    fn const_change_incompatible() {
        let v = only(&cs(vec![SymbolChange::ConstChanged {
            name: "limit".to_string(),
            old: "Int".to_string(),
            new: "Float".to_string(),
        }]));
        assert!(matches!(v, Verdict::Incompatible { .. }), "{v:?}");
    }

    // ── actors ──────────────────────────────────────────────────────────

    #[test]
    fn actor_state_additive_with_default_auto_migrates() {
        let v = only(&cs(vec![SymbolChange::ActorStateChanged {
            name: "Counter".to_string(),
            old_state: states(&[("count", "Int", true)]),
            new_state: states(&[("count", "Int", true), ("step", "Int", true)]),
        }]));
        match v {
            Verdict::AutoMigrate { note } => assert!(note.contains("step"), "{note}"),
            other => panic!("expected AutoMigrate, got {other:?}"),
        }
    }

    #[test]
    fn actor_state_additive_without_default_needs_scaffold() {
        let v = only(&cs(vec![SymbolChange::ActorStateChanged {
            name: "Counter".to_string(),
            old_state: states(&[("count", "Int", true)]),
            new_state: states(&[("count", "Int", true), ("step", "Int", false)]),
        }]));
        match v {
            Verdict::RequiresMigration { scaffold, has_holes } => {
                assert!(has_holes);
                assert!(scaffold.contains("step: ???"), "{scaffold}");
                assert!(scaffold.contains("migrate (old: Counter@1) -> Counter"), "{scaffold}");
            }
            other => panic!("expected RequiresMigration, got {other:?}"),
        }
    }

    #[test]
    fn actor_state_rename_heuristic_full_scaffold() {
        let v = only(&cs(vec![SymbolChange::ActorStateChanged {
            name: "Counter".to_string(),
            old_state: states(&[("count", "Int", true)]),
            new_state: states(&[("total", "Int", true)]),
        }]));
        match v {
            Verdict::RequiresMigration { scaffold, has_holes } => {
                assert!(!has_holes, "rename scaffold is complete");
                assert!(scaffold.contains("total: old.count"), "{scaffold}");
            }
            other => panic!("expected RequiresMigration, got {other:?}"),
        }
    }

    #[test]
    fn actor_handler_removed_incompatible() {
        let v = only(&cs(vec![SymbolChange::ActorHandlersChanged {
            name: "Counter".to_string(),
            added: vec![],
            removed: vec!["get".to_string()],
            caps_changed: vec![],
        }]));
        match v {
            Verdict::Incompatible { reason } => assert!(reason.contains("get"), "{reason}"),
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    #[test]
    fn actor_handler_added_compatible() {
        let v = only(&cs(vec![SymbolChange::ActorHandlersChanged {
            name: "Counter".to_string(),
            added: vec!["reset".to_string()],
            removed: vec![],
            caps_changed: vec![],
        }]));
        assert_eq!(v, Verdict::Compatible);
    }

    #[test]
    fn actor_handler_caps_widening_incompatible() {
        let v = only(&cs(vec![SymbolChange::ActorHandlersChanged {
            name: "Counter".to_string(),
            added: vec![],
            removed: vec![],
            caps_changed: vec![("bump".to_string(), 0, 1)],
        }]));
        match v {
            Verdict::Incompatible { reason } => {
                assert!(reason.contains("bump") && reason.contains("io"), "{reason}");
            }
            other => panic!("expected Incompatible, got {other:?}"),
        }
    }

    // ── kinds, modules ──────────────────────────────────────────────────

    #[test]
    fn kind_change_incompatible() {
        let v = only(&cs(vec![SymbolChange::KindChanged {
            name: "Thing".to_string(),
            old_kind: "fn",
            new_kind: "record",
        }]));
        assert!(matches!(v, Verdict::Incompatible { .. }), "{v:?}");
    }

    #[test]
    fn module_added_compatible() {
        let report = check(&ChangeSet {
            modules: vec![ModuleChange::Added { fqn: "app.new".to_string() }],
        });
        assert_eq!(report.verdicts[0].verdict, Verdict::Compatible);
    }

    #[test]
    fn module_with_public_symbols_removed_incompatible() {
        let report = check(&ChangeSet {
            modules: vec![ModuleChange::Removed {
                fqn: "app.old".to_string(),
                had_public_symbols: true,
            }],
        });
        match &report.verdicts[0].verdict {
            Verdict::Incompatible { reason } => assert!(reason.contains("app.old"), "{reason}"),
            other => panic!("expected Incompatible, got {other:?}"),
        }
        // An emptied-out module removal is safe.
        let report2 = check(&ChangeSet {
            modules: vec![ModuleChange::Removed {
                fqn: "app.empty".to_string(),
                had_public_symbols: false,
            }],
        });
        assert_eq!(report2.verdicts[0].verdict, Verdict::Compatible);
    }

    // ── aggregation ─────────────────────────────────────────────────────

    #[test]
    fn report_reloadable_only_without_incompatible() {
        let good = check(&cs(vec![
            SymbolChange::Added { name: "f".to_string() },
            SymbolChange::FnCapsChanged { name: "g".to_string(), old_bits: 1, new_bits: 0 },
        ]));
        assert!(good.is_reloadable());
        let bad = check(&cs(vec![
            SymbolChange::Added { name: "f".to_string() },
            SymbolChange::Removed { name: "g".to_string(), old_kind: "fn" },
        ]));
        assert!(!bad.is_reloadable());
    }

    #[test]
    fn report_has_holes_tracks_scaffold_holes() {
        let with_holes = check(&cs(vec![SymbolChange::RecordShapeChanged {
            name: "User".to_string(),
            old_version: 1,
            old_fields: fields(&[("name", "Text")]),
            new_fields: fields(&[("name", "Text"), ("email", "Text")]),
        }]));
        assert!(with_holes.has_holes());
        assert!(with_holes.is_reloadable(), "holes are not incompatibilities");

        let complete = check(&cs(vec![SymbolChange::RecordShapeChanged {
            name: "User".to_string(),
            old_version: 1,
            old_fields: fields(&[("name", "Text")]),
            new_fields: fields(&[("full_name", "Text")]),
        }]));
        assert!(!complete.has_holes());
    }
}
