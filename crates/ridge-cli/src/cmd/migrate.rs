//! `ridge migrate` — manage schema migrations generated from an entity model.
//!
//! ## Surface
//!
//! ```text
//! ridge migrate add <name>
//! ```
//!
//! `ridge migrate add <name>` is the Ridge analogue of EF Core's
//! `Add-Migration`: it diffs the entity model declared in
//! `<src_root>/migrations/Model.ridge` against the last persisted snapshot
//! (`<src_root>/migrations/Snapshot.ridge`), writes a new migration file that
//! captures the difference, and refreshes the snapshot so the next `migrate
//! add` diffs from the model's current shape.
//!
//! ## Algorithm
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
}

/// Diff the model against the last snapshot and write a new migration.
///
/// Writes `<src_root>/migrations/<STAMP>_<name>.ridge` and refreshes
/// `<src_root>/migrations/Snapshot.ridge`, where `<STAMP>` is a
/// `YYYYMMDDHHMMSS` UTC timestamp.  If the model has not changed since the
/// last snapshot, no files are written.
#[derive(Debug, Parser)]
pub struct AddArgs {
    /// Descriptive name for the migration (combined with a UTC timestamp).
    pub name: String,
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
    let migration_name = format!("{stamp}_{}", args.name);
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
    // Discover the driver module's BEAM atom before compiling.  This runs the
    // exact same discovery pass `compile_workspace` runs internally, over the
    // same on-disk files, so the FQN-sorted module list — and therefore the
    // assigned module id — is identical.
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

    // Compile in a throwaway package cache so this does not pollute the
    // user's global Ridge package cache.
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
/// snapshot module).
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
        "pub fn migrationOut () -> Text = Migrate.migrationModule (Migrate.migration \"{migration_name}\" (Migrate.diffSchemas {prev_expr} (model ())))"
    ));
    lines.push("pub fn snapshotOut () -> Text = Migrate.snapshotModule (model ())".to_owned());
    lines.push(String::new());

    lines.join("\n")
}

// ── BEAM execution ────────────────────────────────────────────────────────────

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
    use super::{format_utc_timestamp, validate_migration_name};

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
}
