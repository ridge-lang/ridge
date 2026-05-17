//! `ridge init` — scaffold a Ridge project in the current directory.
//!
//! ## Surface
//!
//! ```text
//! ridge init
//! ```
//!
//! Like `ridge new` but operates on the current directory (§3.6).  The
//! directory must be empty (only `.git/` and `.gitignore` are tolerated).
//! The project name is derived from the directory name.

use std::path::Path;

use clap::Parser;

use crate::error::CliError;
use crate::scaffold;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Scaffold a Ridge project in the current directory.
///
/// The project name is taken from the current directory's name.  The
/// directory must be empty (only `.git/` and `.gitignore` are permitted).
#[derive(Debug, Parser)]
pub struct InitArgs {}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge init`.
///
/// # Errors
///
/// - [`CliError::CwdUnreadable`] — the current directory cannot be read.
/// - [`CliError::DirectoryNotEmpty`] — the directory contains unexpected files.
/// - [`CliError::InvalidProjectName`] — the directory name is not a valid name.
/// - [`CliError::ReservedName`] — the directory name is reserved.
pub fn execute(_args: &InitArgs, cwd: &Path) -> Result<(), CliError> {
    scaffold::init_project(cwd)
}
