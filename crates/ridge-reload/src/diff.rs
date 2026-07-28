//! Structural diff of two workspace snapshots.

use crate::snapshot::{FieldSnap, StateSnap, SymbolSnapshot, VariantSnap, WorkspaceSnapshot};

/// Every change between two snapshots, in deterministic order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    /// Per-module changes, sorted by module FQN.
    pub modules: Vec<ModuleChange>,
}

/// A module-level change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleChange {
    /// Module exists only in the new snapshot.
    Added { fqn: String },
    /// Module exists only in the old snapshot.
    Removed {
        fqn: String,
        had_public_symbols: bool,
    },
    /// Module exists in both with at least one symbol change.
    Changed {
        fqn: String,
        symbols: Vec<SymbolChange>,
    },
}

/// A symbol-level change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolChange {
    /// Symbol exists only in the new snapshot.
    Added { name: String },
    /// Symbol exists only in the old snapshot.
    Removed {
        name: String,
        old_kind: &'static str,
    },
    /// Same fn, different rendered signature.
    FnSignatureChanged {
        name: String,
        old: String,
        new: String,
    },
    /// Same fn and signature, different capability bits.
    FnCapsChanged {
        name: String,
        old_bits: u16,
        new_bits: u16,
    },
    /// Same const, different rendered type.
    ConstChanged {
        name: String,
        old: String,
        new: String,
    },
    /// Record whose declared fields differ.
    RecordShapeChanged {
        name: String,
        old_version: u32,
        old_fields: Vec<FieldSnap>,
        new_fields: Vec<FieldSnap>,
    },
    /// Union whose variant set or payloads differ.
    UnionVariantsChanged {
        name: String,
        old_version: u32,
        added: Vec<VariantSnap>,
        removed: Vec<VariantSnap>,
        /// Variant names whose payload changed.
        payload_changed: Vec<String>,
    },
    /// Alias whose rendered expansion changed.
    AliasChanged {
        name: String,
        old: String,
        new: String,
    },
    /// Actor whose state field list differs.
    ActorStateChanged {
        name: String,
        old_state: Vec<StateSnap>,
        new_state: Vec<StateSnap>,
    },
    /// Actor whose handler set or handler caps differ.
    ActorHandlersChanged {
        name: String,
        added: Vec<String>,
        removed: Vec<String>,
        /// (`handler`, `old_bits`, `new_bits`) triples.
        caps_changed: Vec<(String, u16, u16)>,
    },
    /// Same name, different symbol kind.
    KindChanged {
        name: String,
        old_kind: &'static str,
        new_kind: &'static str,
    },
}

/// Diffs old against new. Module and symbol order in the result is
/// deterministic (sorted by FQN, then name) so reports are stable.
#[must_use]
pub fn diff(old: &WorkspaceSnapshot, new: &WorkspaceSnapshot) -> ChangeSet {
    let mut fqns: Vec<&String> = old.modules.keys().chain(new.modules.keys()).collect();
    fqns.sort();
    fqns.dedup();

    let mut modules = Vec::new();
    for fqn in fqns {
        match (old.modules.get(fqn), new.modules.get(fqn)) {
            (Some(om), None) => modules.push(ModuleChange::Removed {
                fqn: fqn.clone(),
                had_public_symbols: !om.symbols.is_empty(),
            }),
            (None, Some(_)) => modules.push(ModuleChange::Added { fqn: fqn.clone() }),
            (Some(om), Some(nm)) => {
                let symbols = diff_symbols(&om.symbols, &nm.symbols);
                if !symbols.is_empty() {
                    modules.push(ModuleChange::Changed {
                        fqn: fqn.clone(),
                        symbols,
                    });
                }
            }
            (None, None) => {}
        }
    }
    ChangeSet { modules }
}

/// Per-symbol diff of two symbol tables, sorted by symbol name. An actor with
/// both state and handler deltas reports the state change first.
fn diff_symbols(
    old: &std::collections::BTreeMap<String, SymbolSnapshot>,
    new: &std::collections::BTreeMap<String, SymbolSnapshot>,
) -> Vec<SymbolChange> {
    let mut names: Vec<&String> = old.keys().chain(new.keys()).collect();
    names.sort();
    names.dedup();

    let mut out = Vec::new();
    for name in names {
        match (old.get(name), new.get(name)) {
            (Some(o), None) => {
                out.push(SymbolChange::Removed {
                    name: name.clone(),
                    old_kind: kind_name(o),
                });
            }
            (None, Some(_)) => out.push(SymbolChange::Added { name: name.clone() }),
            (Some(o), Some(n)) => diff_symbol_pair(name, o, n, &mut out),
            (None, None) => {}
        }
    }
    out
}

/// Classifies one symbol present in both snapshots. Produces zero, one, or
/// (for actors with state + handler deltas) two changes.
#[expect(
    clippy::too_many_lines,
    reason = "flat exhaustive match over all symbol-kind pairs; splitting would scatter the classification table"
)]
fn diff_symbol_pair(
    name: &str,
    old: &SymbolSnapshot,
    new: &SymbolSnapshot,
    out: &mut Vec<SymbolChange>,
) {
    match (old, new) {
        (
            SymbolSnapshot::Fn {
                signature: os,
                caps_bits: ob,
            },
            SymbolSnapshot::Fn {
                signature: ns,
                caps_bits: nb,
            },
        ) => {
            if os != ns {
                out.push(SymbolChange::FnSignatureChanged {
                    name: name.to_string(),
                    old: os.clone(),
                    new: ns.clone(),
                });
            } else if ob != nb {
                out.push(SymbolChange::FnCapsChanged {
                    name: name.to_string(),
                    old_bits: *ob,
                    new_bits: *nb,
                });
            }
        }
        (SymbolSnapshot::Const { signature: os }, SymbolSnapshot::Const { signature: ns }) => {
            if os != ns {
                out.push(SymbolChange::ConstChanged {
                    name: name.to_string(),
                    old: os.clone(),
                    new: ns.clone(),
                });
            }
        }
        (
            SymbolSnapshot::Record {
                version: ov,
                fields: of,
            },
            SymbolSnapshot::Record { fields: nf, .. },
        ) => {
            if of != nf {
                out.push(SymbolChange::RecordShapeChanged {
                    name: name.to_string(),
                    old_version: *ov,
                    old_fields: of.clone(),
                    new_fields: nf.clone(),
                });
            }
        }
        (
            SymbolSnapshot::Union {
                version: ov,
                variants: ovars,
            },
            SymbolSnapshot::Union {
                variants: nvars, ..
            },
        ) => {
            let added: Vec<VariantSnap> = nvars
                .iter()
                .filter(|v| ovars.iter().all(|o| o.name != v.name))
                .cloned()
                .collect();
            let removed: Vec<VariantSnap> = ovars
                .iter()
                .filter(|v| nvars.iter().all(|n| n.name != v.name))
                .cloned()
                .collect();
            let payload_changed: Vec<String> = ovars
                .iter()
                .filter_map(|o| {
                    nvars
                        .iter()
                        .find(|n| n.name == o.name)
                        .filter(|n| n.payload != o.payload)
                        .map(|_| o.name.clone())
                })
                .collect();
            if !added.is_empty() || !removed.is_empty() || !payload_changed.is_empty() {
                out.push(SymbolChange::UnionVariantsChanged {
                    name: name.to_string(),
                    old_version: *ov,
                    added,
                    removed,
                    payload_changed,
                });
            }
        }
        (SymbolSnapshot::Alias { target: ot }, SymbolSnapshot::Alias { target: nt }) => {
            if ot != nt {
                out.push(SymbolChange::AliasChanged {
                    name: name.to_string(),
                    old: ot.clone(),
                    new: nt.clone(),
                });
            }
        }
        (
            SymbolSnapshot::Actor {
                state: os,
                handlers: oh,
            },
            SymbolSnapshot::Actor {
                state: ns,
                handlers: nh,
            },
        ) => {
            if os != ns {
                out.push(SymbolChange::ActorStateChanged {
                    name: name.to_string(),
                    old_state: os.clone(),
                    new_state: ns.clone(),
                });
            }
            let added: Vec<String> = nh
                .keys()
                .filter(|h| !oh.contains_key(*h))
                .cloned()
                .collect();
            let removed: Vec<String> = oh
                .keys()
                .filter(|h| !nh.contains_key(*h))
                .cloned()
                .collect();
            let caps_changed: Vec<(String, u16, u16)> = oh
                .iter()
                .filter_map(|(h, ob)| {
                    nh.get(h)
                        .filter(|nb| *nb != ob)
                        .map(|nb| (h.clone(), *ob, *nb))
                })
                .collect();
            if !added.is_empty() || !removed.is_empty() || !caps_changed.is_empty() {
                out.push(SymbolChange::ActorHandlersChanged {
                    name: name.to_string(),
                    added,
                    removed,
                    caps_changed,
                });
            }
        }
        _ => {
            if kind_name(old) != kind_name(new) {
                out.push(SymbolChange::KindChanged {
                    name: name.to_string(),
                    old_kind: kind_name(old),
                    new_kind: kind_name(new),
                });
            }
        }
    }
}

const fn kind_name(s: &SymbolSnapshot) -> &'static str {
    match s {
        SymbolSnapshot::Fn { .. } => "fn",
        SymbolSnapshot::Const { .. } => "const",
        SymbolSnapshot::Record { .. } => "record",
        SymbolSnapshot::Union { .. } => "union",
        SymbolSnapshot::Alias { .. } => "alias",
        SymbolSnapshot::Actor { .. } => "actor",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::snapshot::{ModuleSnapshot, SNAPSHOT_FORMAT};

    use super::*;

    fn fun(sig: &str, bits: u16) -> SymbolSnapshot {
        SymbolSnapshot::Fn {
            signature: sig.to_string(),
            caps_bits: bits,
        }
    }

    fn record(fields: &[(&str, &str)]) -> SymbolSnapshot {
        SymbolSnapshot::Record {
            version: 1,
            fields: fields
                .iter()
                .map(|(n, t)| FieldSnap {
                    name: (*n).to_string(),
                    ty: (*t).to_string(),
                })
                .collect(),
        }
    }

    fn union(variants: &[(&str, &str)]) -> SymbolSnapshot {
        SymbolSnapshot::Union {
            version: 1,
            variants: variants
                .iter()
                .map(|(n, p)| VariantSnap {
                    name: (*n).to_string(),
                    payload: (*p).to_string(),
                })
                .collect(),
        }
    }

    fn actor(state: &[(&str, &str, bool)], handlers: &[(&str, u16)]) -> SymbolSnapshot {
        SymbolSnapshot::Actor {
            state: state
                .iter()
                .map(|(n, t, d)| StateSnap {
                    name: (*n).to_string(),
                    ty: (*t).to_string(),
                    has_default: *d,
                })
                .collect(),
            handlers: handlers
                .iter()
                .map(|(n, b)| ((*n).to_string(), *b))
                .collect(),
        }
    }

    fn ws(modules: &[(&str, &[(&str, SymbolSnapshot)])]) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            format: SNAPSHOT_FORMAT,
            modules: modules
                .iter()
                .map(|(fqn, syms)| {
                    (
                        (*fqn).to_string(),
                        ModuleSnapshot {
                            symbols: syms
                                .iter()
                                .map(|(n, s)| ((*n).to_string(), s.clone()))
                                .collect::<BTreeMap<_, _>>(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn unchanged_snapshot_diffs_empty() {
        let a = ws(&[("app.m", &[("f", fun("fn() -> Int", 0))])]);
        assert_eq!(diff(&a, &a).modules, vec![]);
    }

    #[test]
    fn added_and_removed_modules() {
        let old = ws(&[("app.old", &[("f", fun("fn() -> Int", 0))])]);
        let new = ws(&[("app.new", &[("g", fun("fn() -> Int", 0))])]);
        assert_eq!(
            diff(&old, &new).modules,
            vec![
                ModuleChange::Added {
                    fqn: "app.new".to_string()
                },
                ModuleChange::Removed {
                    fqn: "app.old".to_string(),
                    had_public_symbols: true
                },
            ]
        );
        // A removed module with no public symbols reports had_public_symbols = false.
        let old2 = ws(&[("app.empty", &[])]);
        assert_eq!(
            diff(&old2, &new).modules,
            vec![
                ModuleChange::Removed {
                    fqn: "app.empty".to_string(),
                    had_public_symbols: false
                },
                ModuleChange::Added {
                    fqn: "app.new".to_string()
                },
            ]
        );
    }

    #[test]
    fn added_removed_changed_symbols() {
        let old = ws(&[(
            "app.m",
            &[("a", fun("fn() -> Int", 0)), ("b", fun("fn() -> Int", 0))],
        )]);
        let new = ws(&[(
            "app.m",
            &[
                ("a", fun("fn(Int) -> Int", 0)),
                ("c", fun("fn() -> Int", 0)),
            ],
        )]);
        assert_eq!(
            diff(&old, &new).modules,
            vec![ModuleChange::Changed {
                fqn: "app.m".to_string(),
                symbols: vec![
                    SymbolChange::FnSignatureChanged {
                        name: "a".to_string(),
                        old: "fn() -> Int".to_string(),
                        new: "fn(Int) -> Int".to_string(),
                    },
                    SymbolChange::Removed {
                        name: "b".to_string(),
                        old_kind: "fn"
                    },
                    SymbolChange::Added {
                        name: "c".to_string()
                    },
                ],
            }]
        );
    }

    #[test]
    fn caps_only_change_is_distinct() {
        let old = ws(&[("app.m", &[("f", fun("fn() -> Unit", 0))])]);
        let new = ws(&[("app.m", &[("f", fun("fn() -> Unit", 1))])]);
        assert_eq!(
            diff(&old, &new).modules,
            vec![ModuleChange::Changed {
                fqn: "app.m".to_string(),
                symbols: vec![SymbolChange::FnCapsChanged {
                    name: "f".to_string(),
                    old_bits: 0,
                    new_bits: 1,
                }],
            }]
        );
    }

    #[test]
    fn record_shape_change_classified() {
        let old = ws(&[("app.m", &[("User", record(&[("name", "Text")]))])]);
        let new = ws(&[(
            "app.m",
            &[("User", record(&[("name", "Text"), ("age", "Int")]))],
        )]);
        match &diff(&old, &new).modules[0] {
            ModuleChange::Changed { symbols, .. } => match &symbols[0] {
                SymbolChange::RecordShapeChanged {
                    name,
                    old_version,
                    old_fields,
                    new_fields,
                } => {
                    assert_eq!(name, "User");
                    assert_eq!(*old_version, 1);
                    assert_eq!(old_fields.len(), 1);
                    assert_eq!(new_fields.len(), 2);
                }
                other => panic!("expected RecordShapeChanged, got {other:?}"),
            },
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn union_variant_delta_classified() {
        let old = ws(&[(
            "app.m",
            &[("E", union(&[("A", ""), ("B", "Int"), ("C", "Text")]))],
        )]);
        let new = ws(&[(
            "app.m",
            &[("E", union(&[("A", ""), ("B", "Float"), ("D", "")]))],
        )]);
        match &diff(&old, &new).modules[0] {
            ModuleChange::Changed { symbols, .. } => match &symbols[0] {
                SymbolChange::UnionVariantsChanged {
                    added,
                    removed,
                    payload_changed,
                    ..
                } => {
                    assert_eq!(
                        added,
                        &[VariantSnap {
                            name: "D".to_string(),
                            payload: String::new()
                        }]
                    );
                    assert_eq!(
                        removed,
                        &[VariantSnap {
                            name: "C".to_string(),
                            payload: "Text".to_string()
                        }]
                    );
                    assert_eq!(payload_changed, &["B".to_string()]);
                }
                other => panic!("expected UnionVariantsChanged, got {other:?}"),
            },
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn actor_state_and_handler_deltas() {
        let old = ws(&[(
            "app.m",
            &[(
                "C",
                actor(&[("count", "Int", true)], &[("bump", 0), ("get", 0)]),
            )],
        )]);
        let new = ws(&[(
            "app.m",
            &[(
                "C",
                actor(
                    &[("count", "Int", true), ("step", "Int", false)],
                    &[("bump", 1), ("set", 0)],
                ),
            )],
        )]);
        match &diff(&old, &new).modules[0] {
            ModuleChange::Changed { symbols, .. } => {
                assert_eq!(symbols.len(), 2, "state and handler changes are distinct");
                match &symbols[0] {
                    SymbolChange::ActorStateChanged { new_state, .. } => {
                        assert_eq!(new_state.len(), 2);
                    }
                    other => panic!("expected ActorStateChanged, got {other:?}"),
                }
                match &symbols[1] {
                    SymbolChange::ActorHandlersChanged {
                        added,
                        removed,
                        caps_changed,
                        ..
                    } => {
                        assert_eq!(added, &["set".to_string()]);
                        assert_eq!(removed, &["get".to_string()]);
                        assert_eq!(caps_changed, &[("bump".to_string(), 0, 1)]);
                    }
                    other => panic!("expected ActorHandlersChanged, got {other:?}"),
                }
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn kind_change_classified() {
        let old = ws(&[("app.m", &[("Thing", fun("fn() -> Int", 0))])]);
        let new = ws(&[("app.m", &[("Thing", record(&[("x", "Int")]))])]);
        assert_eq!(
            diff(&old, &new).modules,
            vec![ModuleChange::Changed {
                fqn: "app.m".to_string(),
                symbols: vec![SymbolChange::KindChanged {
                    name: "Thing".to_string(),
                    old_kind: "fn",
                    new_kind: "record",
                }],
            }]
        );
    }
}
