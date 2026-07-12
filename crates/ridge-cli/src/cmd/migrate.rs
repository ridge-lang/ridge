//! `ridge migrate` — manage schema migrations generated from an entity model.
//!
//! ## Surface
//!
//! ```text
//! ridge migrate add <name>
//! ridge migrate apply
//! ridge migrate status
//! ```
//!
//! `ridge migrate add <name>` is the Ridge analogue of EF Core's
//! `Add-Migration`: it diffs the entity model declared in
//! `<src_root>/migrations/Model.ridge` against the last persisted snapshot
//! (`<src_root>/migrations/Snapshot.ridge`), writes a new migration file that
//! captures the difference, and refreshes the snapshot so the next `migrate
//! add` diffs from the model's current shape.
//!
//! `ridge migrate apply` is the analogue of `Update-Database`: it runs every
//! migration under `<src_root>/migrations/` that has not yet been recorded
//! against the target database, in chronological order. `ridge migrate
//! status` reports which of those migrations are already applied and which
//! are still pending. Both read the connection settings from the environment
//! — `RIDGE_DB_HOST`, `RIDGE_DB_PORT`, `RIDGE_DB_DATABASE`, `RIDGE_DB_USER`,
//! `RIDGE_DB_PASSWORD`, `RIDGE_DB_SSLMODE` — with `RIDGE_DB_DATABASE` and
//! `RIDGE_DB_USER` required; the rest default the same way `std.data`'s
//! `PostgresConfig` would. Neither command touches the workspace's manifest, so the
//! target project must already declare `[capabilities] allow = ["db",
//! "env"]` for the generated driver to compile.
//!
//! ## Algorithm (`add`)
//!
//! 1. Locate the workspace root and the target member (the workspace's `app`
//!    or `service` member — the one whose `entry` makes it executable).
//! 2. Require `<src_root>/migrations/Model.ridge` to exist.
//! 3. Generate a temporary driver module,
//!    `<src_root>/migrations/__migrate_driver.ridge`, in the same project so it
//!    can import `Model.ridge` (and, when one exists, `Snapshot.ridge`) without
//!    tripping the cross-project export check. Both files expose `model`, so the
//!    current model is imported unqualified and the previous one through a
//!    `Snap` alias. The driver exposes `changeCount`, `migrationOut`, and
//!    `snapshotOut`, all built on `std.migrate`'s diff/render engine.
//! 4. Compile the workspace to `.beam` in a temporary package cache.
//! 5. Locate the driver module's BEAM atom (`ridge_module_<id>`) by running
//!    the same discovery pass `compile_workspace` runs internally, and
//!    invoke its functions through a throwaway `erl` process.
//! 6. If the diff is empty, report "no changes" and stop.  Otherwise write
//!    the migration file and refresh the snapshot.
//! 7. Always delete the temporary driver module, on every exit path.
//!
//! ## Algorithm (`apply` / `status`)
//!
//! 1. Locate the workspace root and target member, the same as `add`.
//! 2. Discover the migration modules under `<src_root>/migrations/` — every
//!    `.ridge` file there except `Model.ridge`, `Snapshot.ridge`, and the
//!    temporary driver, sorted lexically (which is chronological, since each
//!    file stem is `m<YYYYMMDDHHMMSS>_<name>`). If there are none, report that
//!    and stop — neither command touches the database in that case.
//! 3. Validate that the required environment variables are set.
//! 4. Generate a temporary driver module the same way `add` does. `apply`'s
//!    driver imports every migration module, each under its own alias (`M0`,
//!    `M1`, ...) since they all expose `up`, and calls `std.migrate`'s `run`
//!    with the list built from those aliases in order. `status`'s driver only
//!    needs the connection and `std.migrate`'s `applied`. Both build the
//!    connection `PostgresConfig` from the environment at runtime inside the driver
//!    (via `std.env`), so no connection setting — least of all the password —
//!    is ever written to the generated `.ridge` file.
//! 5. Compile, locate the driver module, and run it, exactly like `add`.
//! 6. Render the driver's `Result` as `ok:<comma-separated names>` or
//!    `err:<message>` and parse that back on the CLI side.
//! 7. Always delete the temporary driver module, on every exit path.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};
use ridge_manifest::{find_workspace_root, parse_project, parse_workspace, Project, ProjectKind};
use ridge_resolve::discover_workspace;

use crate::error::CliError;
use crate::render::render_diagnostics;

// ── Argument structs ─────────────────────────────────────────────────────────

/// Manage schema migrations generated from an entity model.
#[derive(Debug, Parser)]
pub struct MigrateArgs {
    /// The `migrate` subcommand to run.
    #[command(subcommand)]
    pub command: MigrateCommand,
}

/// Available `ridge migrate` subcommands.
#[derive(Debug, Subcommand)]
pub enum MigrateCommand {
    /// Diff the current model against the last snapshot and write a migration.
    Add(AddArgs),
    /// Apply every pending migration to the target database, in order.
    Apply(ApplyArgs),
    /// Report which migrations are applied and which are pending.
    Status(StatusArgs),
    /// Roll back the most recently applied migrations, newest first.
    Rollback(RollbackArgs),
}

/// Diff the model against the last snapshot and write a new migration.
///
/// Writes `<src_root>/migrations/m<STAMP>_<name>.ridge` and refreshes
/// `<src_root>/migrations/Snapshot.ridge`, where `<STAMP>` is a
/// `YYYYMMDDHHMMSS` UTC timestamp.  If the model has not changed since the
/// last snapshot, no files are written.
#[derive(Debug, Parser)]
pub struct AddArgs {
    /// Descriptive name for the migration (combined with a UTC timestamp).
    pub name: String,
}

/// Apply every migration under `<src_root>/migrations/` that has not yet
/// been recorded against the target database, in chronological order.
///
/// Connection settings come from the environment: `RIDGE_DB_HOST` (default
/// `localhost`), `RIDGE_DB_PORT` (default `5432`), `RIDGE_DB_DATABASE`
/// (required), `RIDGE_DB_USER` (required), `RIDGE_DB_PASSWORD` (default
/// empty), and `RIDGE_DB_SSLMODE` (default `disable`).
#[derive(Debug, Parser)]
pub struct ApplyArgs {}

/// Report which migrations under `<src_root>/migrations/` are already
/// applied to the target database and which are still pending.
///
/// Reads the same environment variables as `ridge migrate apply`.
#[derive(Debug, Parser)]
pub struct StatusArgs {}

/// Roll back the most recently applied migrations, newest first.
///
/// Reads the same environment variables as `ridge migrate apply`.
#[derive(Debug, Parser)]
pub struct RollbackArgs {
    /// How many migrations to roll back (most recent first).
    #[arg(long, default_value_t = 1)]
    pub steps: u32,
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge migrate`.
///
/// # Errors
///
/// Returns a [`CliError`] for workspace-structure problems, an invalid
/// migration name, a missing `Model.ridge`, a missing BEAM toolchain, a
/// failed compile, or an unexpected internal failure.
pub fn execute(args: &MigrateArgs, cwd: &Path) -> Result<(), CliError> {
    match &args.command {
        MigrateCommand::Add(add_args) => execute_add(add_args, cwd),
        MigrateCommand::Apply(_) => execute_apply(cwd),
        MigrateCommand::Status(_) => execute_status(cwd),
        MigrateCommand::Rollback(a) => execute_rollback(a, cwd),
    }
}

/// Execute `ridge migrate add <name>`.
fn execute_add(args: &AddArgs, cwd: &Path) -> Result<(), CliError> {
    validate_migration_name(&args.name)?;

    // ── 1. Locate workspace root and target member ───────────────────────────
    let workspace_root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;
    let project = resolve_target_project(&workspace_root)?;

    // ── 2. Require Model.ridge ────────────────────────────────────────────────
    let migrations_dir = project.src_root.join("migrations");
    let model_path = migrations_dir.join("Model.ridge");
    if !model_path.is_file() {
        return Err(CliError::MigrateModelMissing { path: model_path });
    }
    let has_snapshot = migrations_dir.join("Snapshot.ridge").is_file();

    // ── 3. Probe the BEAM toolchain up front (before writing anything) ───────
    let erl_path = which::which("erl").map_err(|_| CliError::MigrateErlangNotFound)?;
    which::which("erlc").map_err(|_| CliError::MigrateErlangNotFound)?;

    // ── 4. Write the temporary driver module ─────────────────────────────────
    let stamp = utc_timestamp_now();
    let migration_name = migration_file_stem(&stamp, &args.name);
    let driver_path = migrations_dir.join("__migrate_driver.ridge");
    let driver_source = build_driver_source(&project.name, has_snapshot, &migration_name);

    std::fs::write(&driver_path, &driver_source).map_err(|e| CliError::MigrateInternal {
        message: format!("could not write '{}': {e}", driver_path.display()),
    })?;

    // ── 5. Run the diff and write the outputs ────────────────────────────────
    // Whatever happens below, the temporary driver module must not linger —
    // clean it up on every path (success, no-changes, and error alike).
    let result = run_diff_and_write(
        &workspace_root,
        &erl_path,
        &project.name,
        &migrations_dir,
        &migration_name,
    );
    let _ = std::fs::remove_file(&driver_path);

    result
}

/// Compile the workspace, locate the driver module, run its diff, and write
/// the migration + snapshot files (or report "no changes").
fn run_diff_and_write(
    workspace_root: &Path,
    erl_path: &Path,
    project_name: &str,
    migrations_dir: &Path,
    migration_name: &str,
) -> Result<(), CliError> {
    let (_cache_dir, beam_dir, driver_beam_module) =
        compile_and_locate_driver(workspace_root, project_name)?;

    // ── Run the driver's changeCount () -> Int first ─────────────────────────
    let change_count = run_beam_int_fn(erl_path, &beam_dir, &driver_beam_module, "changeCount")?;

    if change_count == 0 {
        println!("No changes detected. The model matches the last snapshot.");
        return Ok(());
    }

    // ── Render the migration and the refreshed snapshot ──────────────────────
    let migration_out = run_beam_text_fn(erl_path, &beam_dir, &driver_beam_module, "migrationOut")?;
    let snapshot_out = run_beam_text_fn(erl_path, &beam_dir, &driver_beam_module, "snapshotOut")?;

    let migration_path = migrations_dir.join(format!("{migration_name}.ridge"));
    std::fs::write(&migration_path, &migration_out).map_err(|e| CliError::MigrateInternal {
        message: format!("could not write '{}': {e}", migration_path.display()),
    })?;

    let snapshot_path = migrations_dir.join("Snapshot.ridge");
    std::fs::write(&snapshot_path, &snapshot_out).map_err(|e| CliError::MigrateInternal {
        message: format!("could not write '{}': {e}", snapshot_path.display()),
    })?;

    println!("Wrote migration: {}", migration_path.display());
    println!("Updated snapshot: {}", snapshot_path.display());

    Ok(())
}

/// Execute `ridge migrate apply`.
fn execute_apply(cwd: &Path) -> Result<(), CliError> {
    // ── 1. Locate workspace root and target member ───────────────────────────
    let workspace_root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;
    let project = resolve_target_project(&workspace_root)?;

    // ── 2. Discover the migration modules ─────────────────────────────────────
    let migrations_dir = project.src_root.join("migrations");
    let stems = discover_migration_stems(&migrations_dir);
    if stems.is_empty() {
        println!("No migrations found. Run `ridge migrate add <name>` to create one.");
        return Ok(());
    }

    // ── 3. Validate the connection environment up front ──────────────────────
    validate_required_env_vars()?;

    // ── 4. Probe the BEAM toolchain ────────────────────────────────────────────
    let erl_path = which::which("erl").map_err(|_| CliError::MigrateErlangNotFound)?;
    which::which("erlc").map_err(|_| CliError::MigrateErlangNotFound)?;

    // ── 5. Write the temporary driver module ─────────────────────────────────
    let driver_path = migrations_dir.join("__migrate_driver.ridge");
    let driver_source = build_apply_driver_source(&project.name, &stems);
    std::fs::write(&driver_path, &driver_source).map_err(|e| CliError::MigrateInternal {
        message: format!("could not write '{}': {e}", driver_path.display()),
    })?;

    // ── 6. Compile, run, and report ────────────────────────────────────────────
    // The temporary driver module must not linger — clean it up on every path.
    let result = run_apply(&workspace_root, &erl_path, &project.name);
    let _ = std::fs::remove_file(&driver_path);

    result
}

/// Compile the workspace, locate the driver module, run its `applyOut`, and
/// report the outcome.
fn run_apply(workspace_root: &Path, erl_path: &Path, project_name: &str) -> Result<(), CliError> {
    let (_cache_dir, beam_dir, driver_beam_module) =
        compile_and_locate_driver(workspace_root, project_name)?;

    let raw = run_beam_text_fn(erl_path, &beam_dir, &driver_beam_module, "applyOut")?;
    let applied = parse_ok_err(&raw).map_err(|message| CliError::MigrateApplyFailed { message })?;

    if applied.is_empty() {
        println!("Already up to date.");
    } else {
        println!(
            "Applied {} migration(s): {}",
            applied.len(),
            applied.join(", ")
        );
    }

    Ok(())
}

/// Execute `ridge migrate status`.
fn execute_status(cwd: &Path) -> Result<(), CliError> {
    // ── 1. Locate workspace root and target member ───────────────────────────
    let workspace_root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;
    let project = resolve_target_project(&workspace_root)?;

    // ── 2. Discover the migration modules ─────────────────────────────────────
    let migrations_dir = project.src_root.join("migrations");
    let stems = discover_migration_stems(&migrations_dir);
    if stems.is_empty() {
        println!("No migrations found. Run `ridge migrate add <name>` to create one.");
        return Ok(());
    }

    // ── 3. Validate the connection environment up front ──────────────────────
    validate_required_env_vars()?;

    // ── 4. Probe the BEAM toolchain ────────────────────────────────────────────
    let erl_path = which::which("erl").map_err(|_| CliError::MigrateErlangNotFound)?;
    which::which("erlc").map_err(|_| CliError::MigrateErlangNotFound)?;

    // ── 5. Write the temporary driver module ─────────────────────────────────
    let driver_path = migrations_dir.join("__migrate_driver.ridge");
    let driver_source = build_status_driver_source();
    std::fs::write(&driver_path, &driver_source).map_err(|e| CliError::MigrateInternal {
        message: format!("could not write '{}': {e}", driver_path.display()),
    })?;

    // ── 6. Compile, run, and report ────────────────────────────────────────────
    // The temporary driver module must not linger — clean it up on every path.
    let result = run_status(&workspace_root, &erl_path, &project.name, &stems);
    let _ = std::fs::remove_file(&driver_path);

    result
}

/// Compile the workspace, locate the driver module, run its `statusOut`, and
/// print each discovered migration in order, marking the ones not in the
/// applied set as `(pending)`.
fn run_status(
    workspace_root: &Path,
    erl_path: &Path,
    project_name: &str,
    stems: &[String],
) -> Result<(), CliError> {
    let (_cache_dir, beam_dir, driver_beam_module) =
        compile_and_locate_driver(workspace_root, project_name)?;

    let raw = run_beam_text_fn(erl_path, &beam_dir, &driver_beam_module, "statusOut")?;
    let applied =
        parse_ok_err(&raw).map_err(|message| CliError::MigrateStatusFailed { message })?;

    for stem in stems {
        if applied.iter().any(|a| a == stem) {
            println!("{stem}");
        } else {
            println!("{stem} (pending)");
        }
    }

    Ok(())
}

/// Execute `ridge migrate rollback`.
fn execute_rollback(args: &RollbackArgs, cwd: &Path) -> Result<(), CliError> {
    // ── 1. Locate workspace root and target member ───────────────────────────
    let workspace_root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;
    let project = resolve_target_project(&workspace_root)?;

    // ── 2. Discover the migration modules ─────────────────────────────────────
    let migrations_dir = project.src_root.join("migrations");
    let stems = discover_migration_stems(&migrations_dir);
    if stems.is_empty() {
        println!("No migrations found. Run `ridge migrate add <name>` to create one.");
        return Ok(());
    }

    // ── 3. Validate the connection environment up front ──────────────────────
    validate_required_env_vars()?;

    // ── 4. Probe the BEAM toolchain ────────────────────────────────────────────
    let erl_path = which::which("erl").map_err(|_| CliError::MigrateErlangNotFound)?;
    which::which("erlc").map_err(|_| CliError::MigrateErlangNotFound)?;

    // ── 5. Write the temporary driver module ─────────────────────────────────
    let driver_path = migrations_dir.join("__migrate_driver.ridge");
    let driver_source = build_rollback_driver_source(&project.name, &stems, args.steps);
    std::fs::write(&driver_path, &driver_source).map_err(|e| CliError::MigrateInternal {
        message: format!("could not write '{}': {e}", driver_path.display()),
    })?;

    // ── 6. Compile, run, and report ────────────────────────────────────────────
    // The temporary driver module must not linger — clean it up on every path.
    let result = run_rollback(&workspace_root, &erl_path, &project.name);
    let _ = std::fs::remove_file(&driver_path);

    result
}

/// Compile the workspace, locate the driver module, run its `rollbackOut`, and
/// report the outcome.
fn run_rollback(
    workspace_root: &Path,
    erl_path: &Path,
    project_name: &str,
) -> Result<(), CliError> {
    let (_cache_dir, beam_dir, driver_beam_module) =
        compile_and_locate_driver(workspace_root, project_name)?;

    let raw = run_beam_text_fn(erl_path, &beam_dir, &driver_beam_module, "rollbackOut")?;
    let rolled =
        parse_ok_err(&raw).map_err(|message| CliError::MigrateRollbackFailed { message })?;

    if rolled.is_empty() {
        println!("Nothing to roll back.");
    } else {
        println!(
            "Rolled back {} migration(s): {}",
            rolled.len(),
            rolled.join(", ")
        );
    }

    Ok(())
}

// ── Member resolution ─────────────────────────────────────────────────────────

/// Resolve the workspace's target member project for `ridge migrate`.
///
/// Mirrors `ridge run`'s member resolution (see `cmd/run.rs`,
/// `resolve_executable_member`): the target is the workspace's `app` or
/// `service` member.  Multi-project workspaces list members under `apps/*`;
/// a single-project workspace has its root `ridge.toml` double as both the
/// `[workspace]` and `[project]` tables.  When more than one executable
/// member exists, the first one found is used — the same arbitrary-pick
/// behaviour `ridge run` falls back to for a plain (non-watch) run.
fn resolve_target_project(workspace_root: &Path) -> Result<Project, CliError> {
    let mut candidates = Vec::new();

    let apps_dir = workspace_root.join("apps");
    if let Ok(entries) = std::fs::read_dir(&apps_dir) {
        for entry in entries.flatten() {
            let manifest_path = entry.path().join("ridge.toml");
            let Ok(src) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(proj) = parse_project(&src, &manifest_path) else {
                continue;
            };
            if matches!(proj.kind, ProjectKind::App | ProjectKind::Service) {
                candidates.push(proj);
            }
        }
    }

    let root_manifest = workspace_root.join("ridge.toml");
    if let Ok(src) = std::fs::read_to_string(&root_manifest) {
        if let Ok(ws) = parse_workspace(&src, &root_manifest) {
            if ws.members_globs.iter().any(|p| p == ".") {
                if let Ok(proj) = parse_project(&src, &root_manifest) {
                    if matches!(proj.kind, ProjectKind::App | ProjectKind::Service) {
                        candidates.push(proj);
                    }
                }
            }
        }
    }

    if candidates.is_empty() {
        return Err(CliError::NoExecutableMember);
    }

    Ok(candidates.remove(0))
}

// ── Migration discovery (shared by apply/status) ──────────────────────────────

/// Discover the migration module stems under `migrations_dir`, sorted
/// chronologically.
///
/// A migration file is any `.ridge` file directly in the directory except
/// `Model.ridge`, `Snapshot.ridge`, and the temporary driver module. Sorting
/// lexically is sorting chronologically, since every stem is
/// `m<YYYYMMDDHHMMSS>_<name>` — a fixed-width numeric stamp right after the
/// same one-character prefix. A missing or unreadable directory reads as no
/// migrations at all, the same as an empty one.
fn discover_migration_stems(migrations_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(migrations_dir) else {
        return Vec::new();
    };

    let mut stems: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("ridge") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_owned();
            match stem.as_str() {
                "Model" | "Snapshot" | "__migrate_driver" => None,
                _ => Some(stem),
            }
        })
        .collect();

    stems.sort();
    stems
}

// ── Environment-variable validation (apply/status) ────────────────────────────

/// The environment variables `ridge migrate apply`/`ridge migrate status`
/// require to be set (and non-empty) before a connection is even attempted.
/// `RIDGE_DB_HOST`, `RIDGE_DB_PORT`, `RIDGE_DB_PASSWORD`, and
/// `RIDGE_DB_SSLMODE` all default sensibly and so are not required.
const REQUIRED_ENV_VARS: [&str; 2] = ["RIDGE_DB_DATABASE", "RIDGE_DB_USER"];

/// Which of [`REQUIRED_ENV_VARS`] are missing or empty, per `lookup`.
///
/// Takes a lookup function rather than reading `std::env` directly so the
/// check can be exercised against a fake environment in a test without
/// touching real process-global state.
fn missing_required_vars(lookup: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut missing = Vec::new();
    for name in REQUIRED_ENV_VARS {
        if lookup(name).is_none_or(|v| v.is_empty()) {
            missing.push(name.to_owned());
        }
    }
    missing
}

/// Validate that every required connection environment variable is set and
/// non-empty. Reads the environment only to check presence — the values
/// themselves are never captured or written anywhere; the generated driver
/// reads them itself, at runtime, via `std.env`.
fn validate_required_env_vars() -> Result<(), CliError> {
    let missing = missing_required_vars(|name| std::env::var(name).ok());
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CliError::MigrateEnvMissing { vars: missing })
    }
}

// ── Migration-name validation ──────────────────────────────────────────────────

/// Validate the name given to `ridge migrate add <name>`.
///
/// Restricted to ASCII letters, digits, `_`, and `-` so the name is safe to
/// embed both in the generated file name and in the driver module's Ridge
/// string literal (no quoting/escaping ever needed).
fn validate_migration_name(name: &str) -> Result<(), CliError> {
    let is_valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if is_valid {
        Ok(())
    } else {
        Err(CliError::MigrateInvalidName {
            name: name.to_owned(),
        })
    }
}

/// The file stem (and tracked migration name) for a migration created at
/// `stamp` with the given descriptive `name`: `m<stamp>_<name>`.
///
/// The leading `m` makes the stem import-safe — a Ridge module segment must
/// start with a lowercase letter (`ridge-lexer`'s `LowerIdent` token is
/// `[a-z][a-zA-Z0-9_]*`), and `stamp` starts with a digit. Without the
/// prefix, `<stamp>_<name>.ridge` would compile to a module whose own
/// migrations could never be imported — exactly the module `apply`/`status`
/// need to reach. The same string is both the file stem and the name passed
/// to `Migrate.migration`, so `apply`/`status` correlate a file with its
/// tracking-table row by one value.
fn migration_file_stem(stamp: &str, name: &str) -> String {
    format!("m{stamp}_{name}")
}

// ── Driver module generation ──────────────────────────────────────────────────

/// Build the source of the temporary driver module.
///
/// The driver imports the project's `Model` (and, when one already exists, the
/// previous `Snapshot` under the `Snap` alias — both export `model`, so the
/// current model is unqualified and the previous one is reached through `Snap`)
/// alongside `std.migrate`/`std.list`, and diffs the previous model
/// (`Snap.model ()`, or `[]` for the first migration) against the current one.
/// It exposes three zero-arity functions the caller runs on the BEAM:
/// `changeCount` (how many steps the diff produced), `migrationOut` (the
/// rendered migration module), and `snapshotOut` (the rendered, refreshed
/// snapshot module). The migration is rendered reversible: its `up` is the
/// forward diff (previous model to current) and its `down` the reverse diff
/// (current back to previous), so the generated migration rolls back for free.
fn build_driver_source(project_name: &str, has_snapshot: bool, migration_name: &str) -> String {
    let prev_expr = if has_snapshot {
        "(Snap.model ())"
    } else {
        "[]"
    };

    let mut lines = vec![format!("import {project_name}.migrations.Model (model)")];
    if has_snapshot {
        lines.push(format!("import {project_name}.migrations.Snapshot as Snap"));
    }
    lines.push("import std.migrate as Migrate".to_owned());
    lines.push("import std.list as List".to_owned());
    lines.push(String::new());
    lines.push(format!(
        "pub fn changeCount () -> Int = List.length (Migrate.diffSchemas {prev_expr} (model ()))"
    ));
    lines.push(format!(
        "pub fn migrationOut () -> Text = Migrate.migrationModule (Migrate.reversibleMigration \"{migration_name}\" (Migrate.diffSchemas {prev_expr} (model ())) (Migrate.diffSchemas (model ()) {prev_expr}))"
    ));
    lines.push("pub fn snapshotOut () -> Text = Migrate.snapshotModule (model ())".to_owned());
    lines.push(String::new());

    lines.join("\n")
}

/// The `PostgresConfig`-building helpers every `apply`/`status` driver shares: one
/// function per `PostgresConfig` field, each reading its own `RIDGE_DB_*` variable
/// through `std.env` and falling back to the documented default, plus `cfg`,
/// which assembles them into the record `connect` takes.
///
/// Read entirely at runtime, inside the compiled driver — no environment
/// value, connection setting, or secret ever appears in the generated
/// `.ridge` source itself.
fn config_helpers_source() -> String {
    // Every helper here needs `env` declared on its own signature, not just
    // on the driver's entry point: a capability is required on each function
    // that calls a capability-gated primitive (directly or transitively),
    // not only on the top-level caller — `Env.get` gates `cfgHost`, and
    // `cfg`, which calls `cfgHost`, needs `env` in turn.
    [
        "fn env cfgHost () -> Text =",
        "    match Env.get \"RIDGE_DB_HOST\"",
        "        Some v -> v",
        "        None   -> \"localhost\"",
        "",
        "fn env cfgPort () -> Int =",
        "    match Env.get \"RIDGE_DB_PORT\"",
        "        Some v ->",
        "            match Int.parse v",
        "                Some n -> n",
        "                None   -> 5432",
        "        None   -> 5432",
        "",
        "fn env cfgDatabase () -> Text =",
        "    match Env.get \"RIDGE_DB_DATABASE\"",
        "        Some v -> v",
        "        None   -> \"\"",
        "",
        "fn env cfgUser () -> Text =",
        "    match Env.get \"RIDGE_DB_USER\"",
        "        Some v -> v",
        "        None   -> \"\"",
        "",
        "fn env cfgPassword () -> Text =",
        "    match Env.get \"RIDGE_DB_PASSWORD\"",
        "        Some v -> v",
        "        None   -> \"\"",
        "",
        "fn env cfgSslMode () -> Text =",
        "    match Env.get \"RIDGE_DB_SSLMODE\"",
        "        Some v -> v",
        "        None   -> \"disable\"",
        "",
        "fn env cfg () -> PostgresConfig =",
        "    PostgresConfig { host = cfgHost (), port = cfgPort (), database = cfgDatabase (), user = cfgUser (), password = cfgPassword (), sslMode = cfgSslMode () }",
    ]
    .join("\n")
}

/// Build the source of the temporary driver module for `ridge migrate apply`.
///
/// Imports every migration module in `stems` under its own alias (`M0`,
/// `M1`, ...) — they all expose `up`, so a shared unqualified import would
/// collide — and calls `std.migrate`'s `run` with the aliased calls in
/// chronological order. Exposes one zero-arity function, `applyOut`, that
/// connects (building `PostgresConfig` from the environment), runs the migrations,
/// and renders the `Result` as `ok:<comma-separated applied names>` or
/// `err:<message>` — a connect failure renders through the same `err:`
/// branch, so the caller does not need to distinguish the two.
fn build_apply_driver_source(project_name: &str, stems: &[String]) -> String {
    let mut lines = Vec::new();
    for (i, stem) in stems.iter().enumerate() {
        lines.push(format!("import {project_name}.migrations.{stem} as M{i}"));
    }
    lines.push("import std.migrate as Migrate".to_owned());
    lines.push("import std.data (PostgresConfig, connect)".to_owned());
    lines.push("import std.env as Env".to_owned());
    lines.push("import std.int as Int".to_owned());
    lines.push("import std.text as Text".to_owned());
    lines.push(String::new());
    lines.push(config_helpers_source());
    lines.push(String::new());

    let ups: Vec<String> = (0..stems.len()).map(|i| format!("M{i}.up ()")).collect();
    lines.push(format!(
        "pub fn db env applyOut () -> Text =\n    match connect (cfg ())\n        Err e -> Text.join \"\" [\"err:\", e.message]\n        Ok conn ->\n            match Migrate.run conn [ {} ]\n                Ok applied -> Text.join \"\" [\"ok:\", Text.join \",\" applied]\n                Err e      -> Text.join \"\" [\"err:\", e.message]",
        ups.join(", ")
    ));
    lines.push(String::new());

    lines.join("\n")
}

/// Build the source of the temporary driver module for `ridge migrate rollback`.
///
/// The same shape as the `apply` driver — every migration module imported under
/// its own alias (`M0`, `M1`, ...) so `std.migrate`'s `rollback` can look up each
/// migration's reverse steps by name — but its entry point calls `rollback` with
/// the requested step count in place of `run`. Exposes one zero-arity function,
/// `rollbackOut`, that connects, rolls back the most recent `steps` migrations, and
/// renders the `Result` as `ok:<comma-separated rolled-back names>` or
/// `err:<message>`.
fn build_rollback_driver_source(project_name: &str, stems: &[String], count: u32) -> String {
    let mut lines = Vec::new();
    for (i, stem) in stems.iter().enumerate() {
        lines.push(format!("import {project_name}.migrations.{stem} as M{i}"));
    }
    lines.push("import std.migrate as Migrate".to_owned());
    lines.push("import std.data (PostgresConfig, connect)".to_owned());
    lines.push("import std.env as Env".to_owned());
    lines.push("import std.int as Int".to_owned());
    lines.push("import std.text as Text".to_owned());
    lines.push(String::new());
    lines.push(config_helpers_source());
    lines.push(String::new());

    let ups: Vec<String> = (0..stems.len()).map(|i| format!("M{i}.up ()")).collect();
    lines.push(format!(
        "pub fn db env rollbackOut () -> Text =\n    match connect (cfg ())\n        Err e -> Text.join \"\" [\"err:\", e.message]\n        Ok conn ->\n            match Migrate.rollback conn [ {} ] {}\n                Ok rolled -> Text.join \"\" [\"ok:\", Text.join \",\" rolled]\n                Err e      -> Text.join \"\" [\"err:\", e.message]",
        ups.join(", "),
        count
    ));
    lines.push(String::new());

    lines.join("\n")
}

/// Build the source of the temporary driver module for `ridge migrate
/// status`.
///
/// Needs no migration modules — only the connection and `std.migrate`'s
/// `applied`. `applied` is a plain re-export of the `Adapter` class method
/// `migrationsApplied`; calling that class method directly, unqualified,
/// from outside `std.data` compiles and type-checks but fails at codegen
/// (`E002: no stdlib bridge`) — class methods are not bridged for direct
/// external use the way a module's own top-level functions are. Exposes one
/// zero-arity function, `statusOut`, rendered the same `ok:`/`err:` way
/// `applyOut` is.
fn build_status_driver_source() -> String {
    let lines = vec![
        "import std.data (PostgresConfig, connect)".to_owned(),
        "import std.migrate as Migrate".to_owned(),
        "import std.env as Env".to_owned(),
        "import std.int as Int".to_owned(),
        "import std.text as Text".to_owned(),
        String::new(),
        config_helpers_source(),
        String::new(),
        "pub fn db env statusOut () -> Text =\n    match connect (cfg ())\n        Err e -> Text.join \"\" [\"err:\", e.message]\n        Ok conn ->\n            match Migrate.applied conn\n                Err e      -> Text.join \"\" [\"err:\", e.message]\n                Ok applied -> Text.join \"\" [\"ok:\", Text.join \",\" applied]".to_owned(),
        String::new(),
    ];

    lines.join("\n")
}

// ── Driver output parsing (apply/status) ──────────────────────────────────────

/// Parse the `ok:<comma-separated names>` / `err:<message>` protocol
/// `applyOut`/`statusOut` render their `Result` as.
///
/// An `ok:` with nothing after it is zero names, not one empty name — split
/// only when there is something to split.
fn parse_ok_err(raw: &str) -> Result<Vec<String>, String> {
    if let Some(rest) = raw.strip_prefix("ok:") {
        if rest.is_empty() {
            return Ok(Vec::new());
        }
        return Ok(rest.split(',').map(str::to_owned).collect());
    }
    if let Some(rest) = raw.strip_prefix("err:") {
        return Err(rest.to_owned());
    }
    Err(format!("unrecognized driver output: {raw:?}"))
}

// ── BEAM execution ────────────────────────────────────────────────────────────

/// Compile the workspace and locate the generated driver module's BEAM atom
/// and containing directory.
///
/// Shared by `add`, `apply`, and `status`: each writes its own
/// `__migrate_driver.ridge` first, then calls this to compile and find it.
/// Discovers the driver module's BEAM atom before compiling — this runs the
/// exact same discovery pass `compile_workspace` runs internally, over the
/// same on-disk files, so the FQN-sorted module list — and therefore the
/// assigned module id — is identical. Compiles in a throwaway package cache
/// so this does not pollute the user's global Ridge package cache; the
/// returned [`tempfile::TempDir`] must be kept alive by the caller for as
/// long as the `.beam` files it holds are still needed (dropping it removes
/// the directory).
fn compile_and_locate_driver(
    workspace_root: &Path,
    project_name: &str,
) -> Result<(tempfile::TempDir, PathBuf, String), CliError> {
    let disc = discover_workspace(workspace_root);
    let graph = disc.graph.ok_or_else(|| CliError::MigrateInternal {
        message: "workspace discovery failed while resolving the migration driver module"
            .to_owned(),
    })?;
    let driver_fqn = format!("{project_name}.migrations.__migrate_driver");
    let driver_module = graph
        .modules
        .iter()
        .find(|m| m.fully_qualified_name == driver_fqn)
        .ok_or_else(|| CliError::MigrateInternal {
            message: format!("could not find the generated driver module '{driver_fqn}'"),
        })?;
    let driver_beam_module = format!("ridge_module_{}", driver_module.id.0);

    let cache_dir = tempfile::TempDir::new().map_err(|e| CliError::MigrateInternal {
        message: format!("could not create a temporary package cache: {e}"),
    })?;

    let compile_opts = CompileOptions::new(workspace_root.to_owned())
        .with_emit(EmitArtefacts::Beam)
        .with_cache_root(cache_dir.path().to_owned());
    let artefacts = compile_workspace(compile_opts).map_err(|e| CliError::MigrateInternal {
        message: format!("compile failed: {e}"),
    })?;

    if !artefacts.diagnostics.is_empty() {
        render_diagnostics(&artefacts.diagnostics, &artefacts.sources);
        return Err(CliError::MigrateCompileFailed);
    }
    if artefacts.beam_files.is_empty() {
        return Err(CliError::MigrateInternal {
            message: "compile produced no .beam files".to_owned(),
        });
    }

    let beam_dir = artefacts
        .beam_files
        .first()
        .and_then(|p| p.parent())
        .map_or_else(|| PathBuf::from("."), Path::to_owned);

    Ok((cache_dir, beam_dir, driver_beam_module))
}

/// Run a zero-arity `Text`-returning driver function and return its exact
/// output (no trailing newline is added or stripped — the whole stdout is
/// the rendered value).
fn run_beam_text_fn(
    erl_path: &Path,
    beam_dir: &Path,
    module: &str,
    fun: &str,
) -> Result<String, CliError> {
    let expr = format!("io:format(\"~s\", [{module}:{fun}()]), halt().");
    run_beam_eval(erl_path, beam_dir, &expr)
}

/// Run a zero-arity `Int`-returning driver function and return its value.
fn run_beam_int_fn(
    erl_path: &Path,
    beam_dir: &Path,
    module: &str,
    fun: &str,
) -> Result<i64, CliError> {
    let expr = format!("io:format(\"~w\", [{module}:{fun}()]), halt().");
    let out = run_beam_eval(erl_path, beam_dir, &expr)?;
    out.trim()
        .parse::<i64>()
        .map_err(|_| CliError::MigrateInternal {
            message: format!("could not parse driver output as an integer: {out:?}"),
        })
}

/// Spawn `erl -noshell -pa <beam_dir> -eval <expr>` and return its stdout.
///
/// Returns [`CliError::MigrateInternal`] if the process cannot be spawned or
/// produces no output (the driver crashed before printing).
fn run_beam_eval(erl_path: &Path, beam_dir: &Path, expr: &str) -> Result<String, CliError> {
    let output = std::process::Command::new(erl_path)
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(expr)
        .output()
        .map_err(|e| CliError::MigrateInternal {
            message: format!("failed to spawn erl: {e}"),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::MigrateInternal {
            message: format!("erl produced no output; stderr: {stderr}"),
        });
    }
    Ok(stdout)
}

// ── Timestamp (dependency-free) ────────────────────────────────────────────────

/// Format the current UTC time as `YYYYMMDDHHMMSS`.
fn utc_timestamp_now() -> String {
    let epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format_utc_timestamp(epoch_secs)
}

/// Format a Unix-epoch second count as a `YYYYMMDDHHMMSS` UTC timestamp.
///
/// Implemented without a date/time crate dependency: the day count is
/// converted to a proleptic-Gregorian civil date via Howard Hinnant's
/// `civil_from_days` algorithm, then the time-of-day is formatted directly
/// from the remaining seconds.
fn format_utc_timestamp(epoch_secs: u64) -> String {
    let days = i64::try_from(epoch_secs / 86_400).unwrap_or(i64::MAX);
    let time_of_day = epoch_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;
    format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

/// Convert a day count since the Unix epoch (1970-01-01) into a proleptic
/// Gregorian `(year, month, day)` civil date.
///
/// Howard Hinnant's `civil_from_days` algorithm — public domain, see
/// <http://howardhinnant.github.io/date_algorithms.html>.
#[allow(
    clippy::similar_names,
    reason = "era/doe/yoe/y/doy/mp/day/month/year are the algorithm's published names; \
              renaming them would make this harder to cross-check against the reference"
)]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{
        build_apply_driver_source, build_rollback_driver_source, build_status_driver_source,
        discover_migration_stems, format_utc_timestamp, migration_file_stem, missing_required_vars,
        parse_ok_err, validate_migration_name,
    };

    #[test]
    fn format_utc_timestamp_epoch_zero() {
        assert_eq!(format_utc_timestamp(0), "19700101000000");
    }

    #[test]
    fn format_utc_timestamp_known_instant() {
        assert_eq!(format_utc_timestamp(1_700_000_000), "20231114221320");
    }

    #[test]
    fn format_utc_timestamp_leap_day() {
        assert_eq!(format_utc_timestamp(1_709_210_096), "20240229123456");
    }

    #[test]
    fn validate_migration_name_accepts_alnum_underscore_hyphen() {
        assert!(validate_migration_name("init").is_ok());
        assert!(validate_migration_name("add_users_table").is_ok());
        assert!(validate_migration_name("add-users-table").is_ok());
        assert!(validate_migration_name("Init2").is_ok());
    }

    #[test]
    fn validate_migration_name_rejects_empty_and_unsafe_chars() {
        assert!(validate_migration_name("").is_err());
        assert!(validate_migration_name("has space").is_err());
        assert!(validate_migration_name("has\"quote").is_err());
        assert!(validate_migration_name("has/slash").is_err());
    }

    // ── migration_file_stem (the import-safe naming fix) ──────────────────────

    #[test]
    fn migration_file_stem_prefixes_the_stamp_with_a_lowercase_letter() {
        let stem = migration_file_stem("20260701120000", "init");
        assert_eq!(stem, "m20260701120000_init");
        assert!(stem.chars().next().unwrap().is_ascii_lowercase());
    }

    // ── discover_migration_stems ───────────────────────────────────────────────

    #[test]
    fn discover_migration_stems_excludes_model_snapshot_and_driver() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("Model.ridge"), "").unwrap();
        std::fs::write(tmp.path().join("Snapshot.ridge"), "").unwrap();
        std::fs::write(tmp.path().join("__migrate_driver.ridge"), "").unwrap();
        std::fs::write(tmp.path().join("m20260101000000_init.ridge"), "").unwrap();
        std::fs::write(tmp.path().join("m20260102000000_second.ridge"), "").unwrap();

        let stems = discover_migration_stems(tmp.path());

        assert_eq!(
            stems,
            vec!["m20260101000000_init", "m20260102000000_second"]
        );
    }

    #[test]
    fn discover_migration_stems_sorts_chronologically_regardless_of_write_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("m20260102000000_second.ridge"), "").unwrap();
        std::fs::write(tmp.path().join("m20260101000000_first.ridge"), "").unwrap();

        let stems = discover_migration_stems(tmp.path());

        assert_eq!(
            stems,
            vec!["m20260101000000_first", "m20260102000000_second"]
        );
    }

    #[test]
    fn discover_migration_stems_empty_dir_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(discover_migration_stems(tmp.path()).is_empty());
    }

    #[test]
    fn discover_migration_stems_missing_dir_returns_empty() {
        let missing = std::path::Path::new("this/does/not/exist/ridge-migrate-test");
        assert!(discover_migration_stems(missing).is_empty());
    }

    // ── missing_required_vars (env -> PostgresConfig validation) ──────────────────────

    #[test]
    fn missing_required_vars_reports_both_when_unset() {
        let missing = missing_required_vars(|_name| None);
        assert_eq!(missing, vec!["RIDGE_DB_DATABASE", "RIDGE_DB_USER"]);
    }

    #[test]
    fn missing_required_vars_empty_when_both_present() {
        let missing = missing_required_vars(|name| match name {
            "RIDGE_DB_DATABASE" => Some("app".to_owned()),
            "RIDGE_DB_USER" => Some("app_user".to_owned()),
            _ => None,
        });
        assert!(missing.is_empty());
    }

    #[test]
    fn missing_required_vars_rejects_an_empty_value() {
        let missing = missing_required_vars(|name| match name {
            "RIDGE_DB_DATABASE" => Some(String::new()),
            "RIDGE_DB_USER" => Some("app_user".to_owned()),
            _ => None,
        });
        assert_eq!(missing, vec!["RIDGE_DB_DATABASE"]);
    }

    // ── driver-source builders ─────────────────────────────────────────────────

    #[test]
    fn build_apply_driver_source_aliases_each_migration_and_runs_them_in_order() {
        let src =
            build_apply_driver_source("demo", &["m1_init".to_owned(), "m2_second".to_owned()]);

        assert!(src.contains("import demo.migrations.m1_init as M0"));
        assert!(src.contains("import demo.migrations.m2_second as M1"));
        assert!(src.contains("Migrate.run conn [ M0.up (), M1.up () ]"));
        assert!(src.contains("pub fn db env applyOut () -> Text"));
        // Every connection setting is read at runtime through `std.env`, not
        // spliced in as a literal — `cfgPassword` in particular never touches
        // the actual password value.
        assert!(src.contains("Env.get \"RIDGE_DB_PASSWORD\""));
        assert!(src.contains("Env.get \"RIDGE_DB_HOST\""));
    }

    #[test]
    fn build_rollback_driver_source_aliases_each_migration_and_rolls_back_n_steps() {
        let src = build_rollback_driver_source(
            "demo",
            &["m1_init".to_owned(), "m2_second".to_owned()],
            2,
        );

        assert!(src.contains("import demo.migrations.m1_init as M0"));
        assert!(src.contains("import demo.migrations.m2_second as M1"));
        // Every migration is passed so `rollback` can find each one's reverse steps
        // by name, and the requested step count is spliced in as the integer argument.
        assert!(src.contains("Migrate.rollback conn [ M0.up (), M1.up () ] 2"));
        assert!(src.contains("pub fn db env rollbackOut () -> Text"));
        // Connection settings are still read at runtime through `std.env`, never
        // spliced into the generated source.
        assert!(src.contains("Env.get \"RIDGE_DB_PASSWORD\""));
        assert!(src.contains("Env.get \"RIDGE_DB_HOST\""));
    }

    #[test]
    fn build_rollback_driver_source_defaults_to_a_single_step() {
        let src = build_rollback_driver_source("demo", &["m1_init".to_owned()], 1);
        assert!(src.contains("Migrate.rollback conn [ M0.up () ] 1"));
    }

    #[test]
    fn build_status_driver_source_needs_no_migration_imports() {
        let src = build_status_driver_source();

        assert!(src.contains("import std.data (PostgresConfig, connect)"));
        assert!(src.contains("import std.migrate as Migrate"));
        assert!(src.contains("Migrate.applied conn"));
        assert!(src.contains("pub fn db env statusOut () -> Text"));
        assert!(!src.contains(".migrations."));
    }

    // ── parse_ok_err (the driver output protocol) ──────────────────────────────

    #[test]
    fn parse_ok_err_reads_applied_names() {
        assert_eq!(parse_ok_err("ok:m1,m2").unwrap(), vec!["m1", "m2"]);
    }

    #[test]
    fn parse_ok_err_reads_an_empty_applied_set_as_zero_names() {
        assert_eq!(parse_ok_err("ok:").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn parse_ok_err_reads_the_error_message() {
        assert_eq!(
            parse_ok_err("err:connection refused").unwrap_err(),
            "connection refused"
        );
    }
}
