//! Integration tests for `ridge migrate add`.
//!
//! Requires a real BEAM runtime (`erl`/`erlc` on PATH) since the command
//! compiles the workspace and runs the generated driver module.  Gated
//! behind `#[cfg(feature = "beam-runtime")]`, following the pattern in
//! `tests/run.rs`.
//!
//! Run with:
//! ```text
//! cargo test -p ridge-cli --features beam-runtime --test migrate
//! ```

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use assert_cmd::Command;
use common::{write_file, TempWorkspace};

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// A minimal two-entity model built with `std.schema`'s builders directly —
/// no external entity module or `deriving (Schema)` needed.
const MODEL_V1: &str = r#"
import std.schema (EntitySchema, DbBigInt, DbText, DbVarchar, Identity, mkColumn, withColumn, schema, generated, primaryKey, unique)

fn userSchema () -> EntitySchema Unit =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "email" "email" (DbVarchar 255) false |> unique)

fn postSchema () -> EntitySchema Unit =
    schema "Post" "posts"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "body" "body" DbText true)

pub fn model () -> List (EntitySchema Unit) = [ userSchema (), postSchema () ]
"#;

/// Build a single-member `app` workspace with `src/migrations/Model.ridge`
/// declaring `MODEL_V1`, plus a trivial `Main.ridge` entry point (required by
/// `kind = "app"`).
fn make_migrate_workspace() -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"migrate-cli-e2e\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/app/ridge.toml",
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    );
    write_file(
        &tw.path,
        "apps/app/src/Main.ridge",
        "pub fn main -> Int = 0\n",
    );
    write_file(&tw.path, "apps/app/src/migrations/Model.ridge", MODEL_V1);
    tw
}

/// Every `*_init.ridge` file directly under `dir` (the migration file, whose
/// name carries a timestamp prefix the test does not otherwise know).
fn find_migration_file(dir: &Path, suffix: &str) -> Option<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if name.ends_with(suffix) && name != "Snapshot.ridge" {
                Some(path)
            } else {
                None
            }
        })
}

#[test]
fn migrate_add_generates_migration_and_snapshot_then_detects_no_changes() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping migrate_add_generates_migration_and_snapshot_then_detects_no_changes"
        );
        return;
    }

    let tw = make_migrate_workspace();
    let migrations_dir = tw
        .path
        .join("apps")
        .join("app")
        .join("src")
        .join("migrations");

    // ── First run: the model is new, so a migration should be generated ──────
    let output = ridge_cmd()
        .arg("migrate")
        .arg("add")
        .arg("init")
        .current_dir(&tw.path)
        .output()
        .expect("ridge migrate add init spawn failed");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "ridge migrate add init failed.\nstdout: {stdout}\nstderr: {stderr}"
    );

    let snapshot_path = migrations_dir.join("Snapshot.ridge");
    assert!(
        snapshot_path.is_file(),
        "Snapshot.ridge was not written; stdout: {stdout}\nstderr: {stderr}"
    );

    let migration_path = find_migration_file(&migrations_dir, "_init.ridge").unwrap_or_else(|| {
        panic!("no <stamp>_init.ridge migration file found in {migrations_dir:?}")
    });
    let migration_src = std::fs::read_to_string(&migration_path).expect("read migration file");
    assert!(
        migration_src.contains("pub fn up () -> Migration ="),
        "migration file missing `up`: {migration_src}"
    );
    assert!(
        migration_src.contains(r#"createSchema (schema "User" "users""#),
        "migration file missing the User create step: {migration_src}"
    );
    assert!(
        migration_src.contains(r#"createSchema (schema "Post" "posts""#),
        "migration file missing the Post create step: {migration_src}"
    );

    let snapshot_src = std::fs::read_to_string(&snapshot_path).expect("read snapshot file");
    assert!(
        snapshot_src.contains("pub fn model () -> List (EntitySchema Unit) ="),
        "snapshot file missing `model`: {snapshot_src}"
    );

    // The temporary driver module must never linger.
    assert!(
        !migrations_dir.join("__migrate_driver.ridge").exists(),
        "the temporary driver module was left behind"
    );

    // The whole workspace (Model + Snapshot + the new migration) must still
    // re-compile clean.
    let build_output = ridge_cmd()
        .arg("build")
        .current_dir(&tw.path)
        .output()
        .expect("ridge build spawn failed");
    assert!(
        build_output.status.success(),
        "ridge build failed after migrate add.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_output.stdout),
        String::from_utf8_lossy(&build_output.stderr)
    );

    // ── Second run: no model change, so nothing new should be written ────────
    let output2 = ridge_cmd()
        .arg("migrate")
        .arg("add")
        .arg("noop")
        .current_dir(&tw.path)
        .output()
        .expect("ridge migrate add noop spawn failed");

    let stdout2 = String::from_utf8_lossy(&output2.stdout).into_owned();
    let stderr2 = String::from_utf8_lossy(&output2.stderr).into_owned();
    assert!(
        output2.status.success(),
        "ridge migrate add noop failed.\nstdout: {stdout2}\nstderr: {stderr2}"
    );
    assert!(
        stdout2.contains("No changes detected"),
        "expected a no-changes notice, got stdout: {stdout2}\nstderr: {stderr2}"
    );
    assert!(
        find_migration_file(&migrations_dir, "_noop.ridge").is_none(),
        "a migration file was written even though the model did not change"
    );
    assert!(
        !migrations_dir.join("__migrate_driver.ridge").exists(),
        "the temporary driver module was left behind after the no-changes run"
    );
}

#[test]
fn migrate_add_missing_model_reports_c401() {
    // The missing-Model.ridge check happens before the BEAM toolchain is
    // probed, so this test does not require `erl`/`erlc` on PATH.
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"migrate-cli-missing-model-e2e\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/app/ridge.toml",
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    );
    write_file(
        &tw.path,
        "apps/app/src/Main.ridge",
        "pub fn main -> Int = 0\n",
    );

    let output = ridge_cmd()
        .arg("migrate")
        .arg("add")
        .arg("init")
        .current_dir(&tw.path)
        .output()
        .expect("ridge migrate add init spawn failed");

    assert!(!output.status.success(), "expected a non-zero exit code");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("C401"),
        "expected a C401 MigrateModelMissing error, got stderr: {stderr}"
    );
}
