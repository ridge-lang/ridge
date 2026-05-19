//! `ridge new <name>` — scaffold a new Ridge project.
//!
//! ## Surface
//!
//! ```text
//! ridge new <name>
//! ```
//!
//! Creates `<name>/` in the current directory with the canonical layout
//! (§2.9): `ridge.toml`, `src/Main.ridge`, `README.md`.

use std::path::Path;

use clap::Parser;

use crate::error::CliError;
use crate::scaffold;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Scaffold a new Ridge project in a new directory.
///
/// Creates `<name>/` in the current directory containing `ridge.toml`,
/// `src/Main.ridge`, and `README.md`.
#[derive(Debug, Parser)]
pub struct NewArgs {
    /// Name of the new project (also used as the directory name).
    pub name: String,
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge new <name>`.
///
/// # Errors
///
/// - [`CliError::InvalidProjectName`] — `<name>` is not a valid portable name.
/// - [`CliError::ReservedName`] — `<name>` is reserved by the toolchain.
/// - [`CliError::DirectoryExists`] — `<name>/` already exists.
pub fn execute(args: &NewArgs, cwd: &Path) -> Result<(), CliError> {
    scaffold::new_project(&args.name, cwd)
}
