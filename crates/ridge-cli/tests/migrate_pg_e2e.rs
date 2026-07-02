//! End-to-end check for `ridge migrate apply` and `ridge migrate status`
//! against a real Postgres database.
//!
//! Gated three ways, exactly like `crates/ridge-driver/tests/data_pg_e2e.rs`:
//! the `beam-runtime` feature, a `which` guard for `erl`/`erlc`, and the
//! `RIDGE_TEST_PG_URL` environment variable. Without a reachable database the
//! test skips rather than fails, so the default `cargo test` run is
//! unaffected.
//!
//! The flow mirrors the real workflow end to end: scaffold a workspace with
//! `src/migrations/Model.ridge`, run `ridge migrate add` to generate a
//! migration, then `ridge migrate apply` (with `RIDGE_DB_*` set from the
//! parsed URL) to run it against the live database, a second `apply` to
//! confirm it is idempotent, and `ridge migrate status` to confirm the
//! migration reads back as applied.
//!
//! This does not separately assert "the table landed" with a raw query: it
//! does not need to. `std.migrate`'s `run` applies each migration's schema
//! changes and its tracking-table record in one transaction (see
//! `applyOne` in `stdlib/migrate.ridge`) — a failed `CREATE TABLE` rolls the
//! whole thing back and neither commits, so a successful first `apply`
//! together with a no-op second `apply` already proves the table exists and
//! the record of it landed together.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::{write_file, TempWorkspace};

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// Connection settings parsed out of `RIDGE_TEST_PG_URL`.
///
/// Duplicated from `crates/ridge-driver/tests/data_pg_e2e.rs`'s `parse_pg_url`
/// rather than shared — the two test suites are intentionally kept loosely
/// coupled (see `tests/common/mod.rs`), and each integration-test binary
/// compiles independently regardless.
struct PgParts {
    host: String,
    port: String,
    user: String,
    password: String,
    database: String,
    sslmode: String,
}

/// Parse `postgres://user:password@host:port/database?sslmode=mode`. The
/// scheme, userinfo, host, and database are required; the port defaults to
/// `5432` and `sslmode` to `disable`.
fn parse_pg_url(url: &str) -> Option<PgParts> {
    let rest = url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))?;
    let (main, query) = match rest.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (rest, None),
    };
    let (userinfo, host_port_db) = main.split_once('@')?;
    let (user, password) = match userinfo.split_once(':') {
        Some((u, p)) => (u, p),
        None => (userinfo, ""),
    };
    let (host_port, database) = host_port_db.split_once('/')?;
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h, p),
        None => (host_port, "5432"),
    };
    let sslmode = query
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("sslmode=")))
        .unwrap_or("disable");
    Some(PgParts {
        host: host.to_owned(),
        port: port.to_owned(),
        user: user.to_owned(),
        password: password.to_owned(),
        database: database.to_owned(),
        sslmode: sslmode.to_owned(),
    })
}

/// A minimal one-entity model, in a table named for this test suite
/// specifically so it does not collide with the tables `data_pg_e2e.rs` and
/// `data_migrate_e2e.rs` use against the same CI database.
const MODEL: &str = r#"
import std.schema (EntitySchema, DbBigInt, DbText, Identity, mkColumn, withColumn, schema, generated, primaryKey)

fn widgetSchema () -> EntitySchema Unit =
    schema "Widget" "ridge_cli_migrate_e2e_widgets"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "label" "label" DbText false)

pub fn model () -> List (EntitySchema Unit) = [ widgetSchema () ]
"#;

/// Build a single-member `app` workspace with `src/migrations/Model.ridge`
/// and the `db`/`env` capabilities the generated apply/status driver needs.
fn make_workspace() -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"migrate-apply-cli-e2e\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/app/ridge.toml",
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"db\", \"env\"]\n",
    );
    write_file(
        &tw.path,
        "apps/app/src/Main.ridge",
        "pub fn main -> Int = 0\n",
    );
    write_file(&tw.path, "apps/app/src/migrations/Model.ridge", MODEL);
    tw
}

#[test]
fn migrate_apply_and_status_against_a_real_database() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping migrate_apply_and_status_against_a_real_database"
        );
        return;
    }
    let url = match std::env::var("RIDGE_TEST_PG_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!(
                "RIDGE_TEST_PG_URL not set — skipping migrate_apply_and_status_against_a_real_database"
            );
            return;
        }
    };
    let parts = parse_pg_url(&url)
        .unwrap_or_else(|| panic!("RIDGE_TEST_PG_URL is not a postgres:// URL: {url}"));

    let tw = make_workspace();

    // ── ridge migrate add: generate the migration from the model ─────────────
    let add_output = ridge_cmd()
        .arg("migrate")
        .arg("add")
        .arg("init")
        .current_dir(&tw.path)
        .output()
        .expect("ridge migrate add init spawn failed");
    assert!(
        add_output.status.success(),
        "ridge migrate add init failed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&add_output.stdout),
        String::from_utf8_lossy(&add_output.stderr)
    );

    // ── ridge migrate apply: run it against the live database ────────────────
    let apply_output = ridge_cmd()
        .arg("migrate")
        .arg("apply")
        .current_dir(&tw.path)
        .env("RIDGE_DB_HOST", &parts.host)
        .env("RIDGE_DB_PORT", &parts.port)
        .env("RIDGE_DB_DATABASE", &parts.database)
        .env("RIDGE_DB_USER", &parts.user)
        .env("RIDGE_DB_PASSWORD", &parts.password)
        .env("RIDGE_DB_SSLMODE", &parts.sslmode)
        .output()
        .expect("ridge migrate apply spawn failed");
    let apply_stdout = String::from_utf8_lossy(&apply_output.stdout).into_owned();
    assert!(
        apply_output.status.success(),
        "ridge migrate apply failed.\nstdout: {apply_stdout}\nstderr: {}",
        String::from_utf8_lossy(&apply_output.stderr)
    );
    assert!(
        apply_stdout.contains("Applied 1 migration(s):"),
        "expected exactly one migration applied, got stdout: {apply_stdout}"
    );
    assert!(
        apply_stdout.contains("_init"),
        "expected the applied migration name to be reported: {apply_stdout}"
    );

    // ── a second apply is a no-op: idempotent ─────────────────────────────────
    let apply_again = ridge_cmd()
        .arg("migrate")
        .arg("apply")
        .current_dir(&tw.path)
        .env("RIDGE_DB_HOST", &parts.host)
        .env("RIDGE_DB_PORT", &parts.port)
        .env("RIDGE_DB_DATABASE", &parts.database)
        .env("RIDGE_DB_USER", &parts.user)
        .env("RIDGE_DB_PASSWORD", &parts.password)
        .env("RIDGE_DB_SSLMODE", &parts.sslmode)
        .output()
        .expect("second ridge migrate apply spawn failed");
    let apply_again_stdout = String::from_utf8_lossy(&apply_again.stdout).into_owned();
    assert!(
        apply_again.status.success(),
        "second ridge migrate apply failed.\nstdout: {apply_again_stdout}\nstderr: {}",
        String::from_utf8_lossy(&apply_again.stderr)
    );
    assert!(
        apply_again_stdout.contains("Already up to date."),
        "expected the second apply to be a no-op, got stdout: {apply_again_stdout}"
    );

    // ── ridge migrate status: reports the migration as applied ───────────────
    let status_output = ridge_cmd()
        .arg("migrate")
        .arg("status")
        .current_dir(&tw.path)
        .env("RIDGE_DB_HOST", &parts.host)
        .env("RIDGE_DB_PORT", &parts.port)
        .env("RIDGE_DB_DATABASE", &parts.database)
        .env("RIDGE_DB_USER", &parts.user)
        .env("RIDGE_DB_PASSWORD", &parts.password)
        .env("RIDGE_DB_SSLMODE", &parts.sslmode)
        .output()
        .expect("ridge migrate status spawn failed");
    let status_stdout = String::from_utf8_lossy(&status_output.stdout).into_owned();
    assert!(
        status_output.status.success(),
        "ridge migrate status failed.\nstdout: {status_stdout}\nstderr: {}",
        String::from_utf8_lossy(&status_output.stderr)
    );
    assert!(
        status_stdout.contains("_init"),
        "expected the migration to be listed: {status_stdout}"
    );
    assert!(
        !status_stdout.contains("(pending)"),
        "expected the migration to be reported as applied, not pending: {status_stdout}"
    );

    // ── ridge migrate rollback: reverse the one applied migration ────────────
    let rollback_output = ridge_cmd()
        .arg("migrate")
        .arg("rollback")
        .arg("--steps")
        .arg("1")
        .current_dir(&tw.path)
        .env("RIDGE_DB_HOST", &parts.host)
        .env("RIDGE_DB_PORT", &parts.port)
        .env("RIDGE_DB_DATABASE", &parts.database)
        .env("RIDGE_DB_USER", &parts.user)
        .env("RIDGE_DB_PASSWORD", &parts.password)
        .env("RIDGE_DB_SSLMODE", &parts.sslmode)
        .output()
        .expect("ridge migrate rollback spawn failed");
    let rollback_stdout = String::from_utf8_lossy(&rollback_output.stdout).into_owned();
    assert!(
        rollback_output.status.success(),
        "ridge migrate rollback failed.\nstdout: {rollback_stdout}\nstderr: {}",
        String::from_utf8_lossy(&rollback_output.stderr)
    );
    assert!(
        rollback_stdout.contains("Rolled back 1 migration(s):"),
        "expected exactly one migration rolled back, got stdout: {rollback_stdout}"
    );
    assert!(
        rollback_stdout.contains("_init"),
        "expected the rolled-back migration name to be reported: {rollback_stdout}"
    );

    // ── status now reports the migration as pending again ────────────────────
    let status_after = ridge_cmd()
        .arg("migrate")
        .arg("status")
        .current_dir(&tw.path)
        .env("RIDGE_DB_HOST", &parts.host)
        .env("RIDGE_DB_PORT", &parts.port)
        .env("RIDGE_DB_DATABASE", &parts.database)
        .env("RIDGE_DB_USER", &parts.user)
        .env("RIDGE_DB_PASSWORD", &parts.password)
        .env("RIDGE_DB_SSLMODE", &parts.sslmode)
        .output()
        .expect("ridge migrate status spawn failed");
    let status_after_stdout = String::from_utf8_lossy(&status_after.stdout).into_owned();
    assert!(
        status_after.status.success(),
        "ridge migrate status failed after rollback.\nstdout: {status_after_stdout}\nstderr: {}",
        String::from_utf8_lossy(&status_after.stderr)
    );
    assert!(
        status_after_stdout.contains("(pending)"),
        "expected the migration to read as pending after rollback: {status_after_stdout}"
    );

    // ── re-apply lands it again: rollback left a clean, re-runnable state ─────
    let reapply_output = ridge_cmd()
        .arg("migrate")
        .arg("apply")
        .current_dir(&tw.path)
        .env("RIDGE_DB_HOST", &parts.host)
        .env("RIDGE_DB_PORT", &parts.port)
        .env("RIDGE_DB_DATABASE", &parts.database)
        .env("RIDGE_DB_USER", &parts.user)
        .env("RIDGE_DB_PASSWORD", &parts.password)
        .env("RIDGE_DB_SSLMODE", &parts.sslmode)
        .output()
        .expect("ridge migrate re-apply spawn failed");
    let reapply_stdout = String::from_utf8_lossy(&reapply_output.stdout).into_owned();
    assert!(
        reapply_output.status.success(),
        "ridge migrate apply after rollback failed.\nstdout: {reapply_stdout}\nstderr: {}",
        String::from_utf8_lossy(&reapply_output.stderr)
    );
    assert!(
        reapply_stdout.contains("Applied 1 migration(s):"),
        "expected the rolled-back migration to re-apply, got stdout: {reapply_stdout}"
    );
}
