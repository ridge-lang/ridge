//! `ridge reload` — source-level reload tooling.
//!
//! ## Surface
//!
//! ```text
//! ridge reload --check [--snapshot <path>]
//! ```
//!
//! Only `--check` exists for now: a dry-run verdict that never touches any
//! running system. It diffs the snapshot written by the last `ridge build`
//! against the current source and reports, per public symbol, whether a
//! reload would be compatible, need a (generated) migration, or be
//! incompatible.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use ridge_driver::reload::{reload_check, snapshot_path_for, CheckReport, Verdict};
use ridge_driver::{CheckOptions, Profile};
use ridge_manifest::find_workspace_root;

use crate::error::CliError;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Source-level reload tooling.
#[derive(Debug, ClapArgs)]
pub struct ReloadArgs {
    /// Dry-run: diff against the last build and report compatibility.
    #[arg(long)]
    pub check: bool,
    /// Override the snapshot path (default: target/ridge/<profile>/reload-snapshot.json).
    #[arg(long, value_name = "PATH")]
    pub snapshot: Option<PathBuf>,
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge reload`.
///
/// Exit status: success only when the report is reloadable and no scaffold
/// still has holes, so scripts can gate on it.
///
/// # Errors
///
/// Returns a [`CliError`] when `--check` is absent, the workspace root cannot
/// be found, the snapshot is missing or stale, or the current source does not
/// compile cleanly. Also returns an error (after printing the report) when
/// any change is incompatible or a scaffold has holes.
pub fn execute(args: &ReloadArgs, cwd: &Path) -> Result<(), CliError> {
    if !args.check {
        eprintln!("error: only `ridge reload --check` is available for now");
        return Err(CliError::AlreadyReported);
    }

    let root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;
    let snapshot = args
        .snapshot
        .clone()
        .unwrap_or_else(|| snapshot_path_for(&root, Profile::Debug.dir_name()));

    let report = match reload_check(CheckOptions::new(root), &snapshot) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(CliError::AlreadyReported);
        }
    };

    print_reload_rejection(&report);

    let ok = report.is_reloadable() && !report.has_holes();
    if ok {
        Ok(())
    } else {
        Err(CliError::AlreadyReported)
    }
}

/// Print the per-symbol verdicts and the summary line for a check report.
///
/// Shared by `ridge reload --check` and the `ridge run --reload` rejection
/// path.
pub(crate) fn print_reload_rejection(report: &CheckReport) {
    let (mut compatible, mut auto, mut migrate, mut incompatible) = (0u32, 0u32, 0u32, 0u32);
    for v in &report.verdicts {
        // Module-level rows repeat the FQN as the symbol; print it once.
        let target = if v.symbol == v.module {
            v.module.clone()
        } else {
            format!("{}.{}", v.module, v.symbol)
        };
        match &v.verdict {
            Verdict::Compatible => {
                compatible += 1;
                println!("compatible      {target}");
            }
            Verdict::AutoMigrate { note } => {
                auto += 1;
                println!("auto-migrate    {target}: {note}");
            }
            Verdict::CompatibleViaMigration { note } => {
                auto += 1;
                println!("migrate-hook    {target}: {note}");
            }
            Verdict::RequiresMigration { scaffold, .. } => {
                migrate += 1;
                println!(
                    "needs-migration {target} — apply this scaffold and re-check:\n{scaffold}"
                );
            }
            Verdict::Incompatible { reason } => {
                incompatible += 1;
                println!("incompatible    {target}: {reason}");
            }
        }
    }

    let ok = report.is_reloadable() && !report.has_holes();
    println!(
        "{}: {compatible} compatible, {auto} auto/hook-migrated, {migrate} need migration, {incompatible} incompatible",
        if ok { "reloadable" } else { "not reloadable" },
    );
}
