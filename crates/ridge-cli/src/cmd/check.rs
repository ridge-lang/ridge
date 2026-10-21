//! `ridge check` — type-check a Ridge workspace without producing any artefacts.
//!
//! ## Surface
//!
//! ```text
//! ridge check [--member <name>]
//! ```
//!
//! Calls [`ridge_driver::check_workspace`] and renders any diagnostics.

use std::path::{Path, PathBuf};

use clap::Parser;
use ridge_driver::{check_workspace, CheckArtefacts, CheckOptions};

use crate::error::CliError;
use crate::render::{report_diagnostics, WarningPolicy};

// ── Argument struct ───────────────────────────────────────────────────────────

/// Type-check a workspace or source file without producing output files.
#[derive(Debug, Parser)]
pub struct CheckArgs {
    /// Check one .ridge file, retaining any workspace that governs it.
    #[arg(value_name = "FILE", conflicts_with = "member")]
    pub file: Option<PathBuf>,
    /// Only check the named workspace member.
    #[arg(long, value_name = "NAME")]
    pub member: Option<String>,

    /// Whether warnings are fatal.
    #[command(flatten)]
    pub warnings: WarningPolicy,
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge check`.
///
/// # Errors
///
/// Returns a [`CliError`] if the workspace root cannot be found or the driver
/// reports a fatal error.  Error-severity diagnostics are printed to stderr and
/// cause a non-zero exit; warnings are printed and counted in the summary line,
/// and only fail the check under `--deny-warnings`.
pub fn execute(args: &CheckArgs, cwd: &Path) -> Result<(), CliError> {
    // ── 1. Locate workspace root ──────────────────────────────────────────────
    let file = args
        .file
        .as_ref()
        .map(|path| crate::cmd::source_file_for(path, cwd))
        .transpose()?;
    let workspace_root = match &file {
        Some(file) => file.parent().unwrap_or(cwd).to_owned(),
        None => crate::cmd::workspace_root_for(cwd)?,
    };

    // ── 2. Check options ──────────────────────────────────────────────────────
    let mut opts = CheckOptions::new(workspace_root);
    if let Some(path) = file {
        opts.source = ridge_driver::SourceInput::File {
            path,
            require_main: false,
        };
    }
    opts.members = args.member.as_ref().map(|m| vec![m.clone()]);

    // ── 3. Type-check ─────────────────────────────────────────────────────────
    let CheckArtefacts {
        diagnostics,
        sources,
        ..
    } = check_workspace(opts).map_err(|e| {
        eprintln!("error: {e}");
        CliError::AlreadyReported
    })?;

    // ── 4. Render diagnostics ─────────────────────────────────────────────────
    let report = report_diagnostics(&diagnostics, &sources, args.warnings.deny_warnings);
    if report.fatal() {
        return Err(CliError::AlreadyReported);
    }

    println!("Type-check passed{}.", report.warning_suffix());
    Ok(())
}
