//! `ridge check` — type-check a Ridge workspace without producing any artefacts.
//!
//! ## Surface
//!
//! ```text
//! ridge check [--member <name>]
//! ```
//!
//! Calls [`ridge_driver::check_workspace`] and renders any diagnostics.

use std::path::Path;

use clap::Parser;
use ridge_driver::{check_workspace, CheckArtefacts, CheckOptions};
use ridge_manifest::find_workspace_root;

use crate::error::CliError;
use crate::render::render_diagnostics;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Type-check the workspace without producing any output files.
#[derive(Debug, Parser)]
pub struct CheckArgs {
    /// Only check the named workspace member.
    #[arg(long, value_name = "NAME")]
    pub member: Option<String>,
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge check`.
///
/// # Errors
///
/// Returns a [`CliError`] if the workspace root cannot be found or the driver
/// reports a fatal error.  Non-fatal diagnostics are printed to stderr and
/// also cause a non-zero exit.
pub fn execute(args: &CheckArgs, cwd: &Path) -> Result<(), CliError> {
    // ── 1. Locate workspace root ──────────────────────────────────────────────
    let workspace_root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;

    // ── 2. Check options ──────────────────────────────────────────────────────
    let mut opts = CheckOptions::new(workspace_root);
    opts.members = args.member.as_ref().map(|m| vec![m.clone()]);

    // ── 3. Type-check ─────────────────────────────────────────────────────────
    let CheckArtefacts {
        diagnostics,
        sources,
        ..
    } = check_workspace(opts).map_err(|e| {
        eprintln!("error: {e}");
        CliError::NoWorkspaceRoot
    })?;

    // ── 4. Render diagnostics ─────────────────────────────────────────────────
    if !diagnostics.is_empty() {
        render_diagnostics(&diagnostics, &sources);
        return Err(CliError::NoWorkspaceRoot);
    }

    println!("Type-check passed.");
    Ok(())
}
