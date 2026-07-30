//! Integration tests for `plan_reload` and the upgrade manifest.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use ridge_driver::{
    compile_workspace, manifest_path_for, plan_reload, snapshot_path_for, CheckOptions,
    CompileOptions, EmitArtefacts, ReloadPlan,
};

/// Absolute path to the counter fixture's single source file.
fn counter_source(ws: &common::TempWorkspace) -> std::path::PathBuf {
    ws.path.join("apps/demo/src/Counter.ridge")
}

/// Compile the fixture, read the snapshot it wrote, then apply `edit` to the
/// single source file and return the plan for the edited source.
fn plan_after_edit(ws: &common::TempWorkspace, edit: impl FnOnce(&str) -> String) -> ReloadPlan {
    let dir = ws.path.clone();
    compile_workspace(CompileOptions::new(dir.clone()).with_emit(EmitArtefacts::Core))
        .expect("initial compile");
    let snap_path = snapshot_path_for(&dir, "debug");
    let old: ridge_driver::WorkspaceSnapshot =
        serde_json::from_str(&std::fs::read_to_string(&snap_path).expect("snapshot"))
            .expect("parse snapshot");
    let src_path = counter_source(ws);
    let src = std::fs::read_to_string(&src_path).expect("src");
    std::fs::write(&src_path, edit(&src)).expect("write edit");
    let manifest_path = manifest_path_for(&dir, "debug");
    plan_reload(&old, CheckOptions::new(dir), &manifest_path).expect("plan_reload")
}

#[test]
fn additive_state_field_produces_manifest_with_actor_entry() {
    let ws = common::make_counter_workspace();
    let plan = plan_after_edit(&ws, |src| {
        src.replace(
            "state count: Int = 0",
            "state count: Int = 0\n    state step: Int = 2",
        )
    });
    let manifest = plan.manifest.expect("additive edit must be reloadable");
    assert_eq!(
        manifest.format,
        ridge_driver::reload::UPGRADE_MANIFEST_FORMAT
    );
    assert_ne!(manifest.base_vsn, manifest.new_vsn);
    assert!(manifest.modules.iter().any(|m| m.ends_with("_counter")));
    let actor = manifest.actors.first().expect("actor entry");
    assert!(actor.beam.ends_with("_counter"));
    assert!(actor.renames.is_empty());
    // The manifest file was written next to the snapshot.
    let written =
        std::fs::read_to_string(manifest_path_for(&ws.path, "debug")).expect("manifest file");
    assert!(written.contains("\"base_vsn\""));
}

#[test]
fn renamed_state_field_produces_rename_instruction() {
    let ws = common::make_counter_workspace();
    let plan = plan_after_edit(&ws, |src| {
        src.replace("state count: Int = 0", "state total: Int = 0")
            .replace("count <- count + 1", "total <- total + 1")
            .replace("        count\n", "        total\n")
    });
    let manifest = plan.manifest.expect("pure rename must be reloadable");
    let actor = manifest.actors.first().expect("actor entry");
    assert_eq!(
        actor.renames,
        vec![["count".to_string(), "total".to_string()]]
    );
}

#[test]
fn body_only_edit_reloads_module_without_actor_entries() {
    let ws = common::make_counter_workspace();
    let plan = plan_after_edit(&ws, |src| {
        src.replace("count <- count + 1", "count <- 1 + count")
    });
    let manifest = plan.manifest.expect("body-only edit must be reloadable");
    assert!(
        manifest.modules.iter().any(|m| m.ends_with("_counter")),
        "the actor module must reload on a body edit: {:?}",
        manifest.modules
    );
    assert!(manifest.actors.is_empty(), "no state shape change");
    assert!(
        plan.report.verdicts.is_empty(),
        "a body-only edit produces no verdicts"
    );
}

#[test]
fn incompatible_edit_yields_no_manifest() {
    let ws = common::make_counter_workspace();
    let plan = plan_after_edit(&ws, |src| {
        src.replace(
            "pub fn label () -> Text = \"counter\"",
            "pub fn label () -> Int = 0",
        )
    });
    assert!(plan.manifest.is_none());
    assert!(!plan.report.is_reloadable());
}

#[test]
fn retyped_state_field_yields_no_manifest() {
    let ws = common::make_counter_workspace();
    let plan = plan_after_edit(&ws, |src| {
        src.replace("state count: Int = 0", "state count: Text = \"zero\"")
            .replace("count <- count + 1", "()")
            .replace("on count () -> Int =", "on count () -> Text =")
    });
    assert!(plan.manifest.is_none());
    assert!(plan.report.has_holes());
}

#[test]
fn actor_with_migrate_hook_produces_hook_manifest_entry() {
    let ws = common::make_counter_workspace();
    let plan = plan_after_edit(&ws, |src| {
        src.replace(
            "state count: Int = 0",
            "state count: Int = 0\n    state step: Int = 1\n    migrate (old: Counter@1) -> Counter =\n        { count = old.count, step = 1 }",
        )
    });
    let manifest = plan.manifest.expect("hook edit must be reloadable");
    let actor = manifest.actors.first().expect("actor entry");
    assert!(actor.migrate_hook, "hook dispatch requested: {actor:?}");
    assert_ne!(actor.old_state_hash, 0);
    assert_ne!(actor.new_state_hash, 0);
    assert_ne!(actor.old_state_hash, actor.new_state_hash);
    assert!(
        actor.renames.is_empty(),
        "hooks carry no rename instructions"
    );
}

#[test]
fn automatic_migration_keeps_hook_flag_off() {
    let ws = common::make_counter_workspace();
    let plan = plan_after_edit(&ws, |src| {
        src.replace(
            "state count: Int = 0",
            "state count: Int = 0\n    state step: Int = 2",
        )
    });
    let actor = plan
        .manifest
        .expect("manifest")
        .actors
        .first()
        .expect("actor")
        .clone();
    assert!(!actor.migrate_hook);
}
