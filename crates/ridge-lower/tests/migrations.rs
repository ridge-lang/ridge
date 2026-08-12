//! Integration tests: `migrate` hooks lower into IR with their runtime
//! `from_hash` resolved against the injected version history.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items
)]

mod common;

use common::make_workspace;
use ridge_ir::IrItem;
use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_types::history::{VersionEntry, VersionHistory};

/// Run the full pipeline with an injected version history and return the
/// lowered workspace.
fn lower_with_history(id: &str, src: &str, history: &VersionHistory) -> ridge_ir::LoweredWorkspace {
    let tw = make_workspace(id, "main", src);
    let disc = discover_workspace(&tw.path);
    let ws_graph = disc.graph.expect("workspace graph must be present");
    let resolved = resolve_workspace(ws_graph);
    let checked = ridge_typecheck::typecheck_workspace_with_history(&resolved, history);
    assert!(
        checked.errors.is_empty(),
        "source must typecheck: {:?}",
        checked.errors
    );
    ridge_lower::lower_workspace(&checked.typed, &resolved).workspace
}

fn history_with_record(name: &str, ordinal: u32, hash: u64) -> VersionHistory {
    let mut h = VersionHistory::default();
    h.records.insert(
        ("demo.main".to_owned(), name.to_owned()),
        vec![VersionEntry {
            ordinal,
            hash,
            shape: vec![("name".to_owned(), "Text".to_owned())],
        }],
    );
    h
}

fn history_with_actor(name: &str, ordinal: u32, hash: u64) -> VersionHistory {
    let mut h = VersionHistory::default();
    h.actors.insert(
        ("demo.main".to_owned(), name.to_owned()),
        vec![VersionEntry {
            ordinal,
            hash,
            shape: vec![("count".to_owned(), "Int".to_owned())],
        }],
    );
    h
}

const ACTOR_SRC: &str = "pub actor Counter =\n    state count: Int = 0\n    state step: Int = 1\n    migrate (old: Counter@1) -> Counter =\n        { count = old.count, step = 1 }\n    on bump =\n        count <- count + 1\n";

#[test]
fn actor_migrate_member_lowers_with_resolved_hash() {
    let lowered = lower_with_history(
        "mig_actor",
        ACTOR_SRC,
        &history_with_actor("Counter", 1, 4242),
    );
    let module = lowered.modules[0].as_ref().expect("one module");
    let actor = module
        .items
        .iter()
        .find_map(|i| match i {
            IrItem::Actor(a) => Some(a),
            _ => None,
        })
        .expect("actor item");
    assert_eq!(actor.migrations.len(), 1);
    assert_eq!(actor.migrations[0].from_hash, Some(4242));
    assert_eq!(actor.migrations[0].param_name, "old");
    assert_eq!(actor.migrations[0].from_ordinal, 1);
}

#[test]
fn actor_migrate_without_history_lowers_with_none_hash() {
    // Typecheck reports T049 for the unknown version; lowering still carries
    // the hook with no chain target. Drive lowering directly (the pipeline
    // helper asserts error-free, which does not hold here).
    let tw = make_workspace("migrations_nohist", "main", ACTOR_SRC);
    let disc = discover_workspace(&tw.path);
    let ws_graph = disc.graph.expect("workspace graph must be present");
    let resolved = resolve_workspace(ws_graph);
    let checked =
        ridge_typecheck::typecheck_workspace_with_history(&resolved, &VersionHistory::default());
    let lowered = ridge_lower::lower_workspace(&checked.typed, &resolved).workspace;
    let module = lowered.modules[0].as_ref().expect("one module");
    let actor = module
        .items
        .iter()
        .find_map(|i| match i {
            IrItem::Actor(a) => Some(a),
            _ => None,
        })
        .expect("actor item");
    assert_eq!(actor.migrations.len(), 1);
    assert_eq!(actor.migrations[0].from_hash, None);
}

#[test]
fn type_migrate_section_lowers_to_migration_items() {
    let src = "pub type User = { name: Text, email: Text } do\n    migrate (old: User@1) -> User =\n        User { name = old.name, email = \"?\" }\nend\n";
    let lowered = lower_with_history("mig_type", src, &history_with_record("User", 1, 777));
    let module = lowered.modules[0].as_ref().expect("one module");
    let migrations: Vec<&ridge_ir::IrMigration> = module
        .items
        .iter()
        .filter_map(|i| match i {
            IrItem::Migration(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(migrations.len(), 1);
    assert_eq!(migrations[0].from_hash, Some(777));
    assert_eq!(migrations[0].param_name, "old");
}

#[test]
fn version_history_threads_into_lowered_workspace() {
    let lowered = lower_with_history(
        "mig_thread",
        ACTOR_SRC,
        &history_with_actor("Counter", 1, 4242),
    );
    assert_eq!(
        lowered
            .version_history
            .lookup_actor("demo.main", "Counter", 1)
            .map(|e| e.hash),
        Some(4242)
    );
}
