//! CLI subcommand implementations.
//!
//! Each submodule contains the argument struct and the `execute` function for
//! one `ridge` subcommand.

pub mod build;
pub mod check;
pub mod explain;
pub mod fmt;
pub mod init;
pub mod migrate;
pub mod new;
pub mod reload;
pub mod repl;
pub mod run;
pub mod test;

use std::path::{Path, PathBuf};

use ridge_manifest::WorkspaceRoot;

use crate::CliError;

/// The workspace directory that governs `cwd`.
///
/// Every subcommand starts here, and they used to start with eleven copies of
/// the same line, each collapsing "there is no manifest" and "the manifest will
/// not parse" into one answer.
///
/// A manifest that is there and broken is reported here, with the code and
/// message its own parser produces, rather than left to the compile pipeline.
/// Not every subcommand runs one — `ridge fmt` walks files and never parses the
/// manifest — so deferring turned a wrong message into no message at all, which
/// is worse than the bug being fixed.
///
/// # Errors
///
/// `C001` when no `ridge.toml` declaring a `[workspace]` table exists at or
/// above `cwd`, and the manifest parser's own `M###` when one exists and cannot
/// be read.
pub fn workspace_root_for(cwd: &Path) -> Result<PathBuf, CliError> {
    match ridge_manifest::find_workspace_root(cwd) {
        WorkspaceRoot::Found(dir) => Ok(dir),
        WorkspaceRoot::Malformed(manifest) => Err(CliError::WorkspaceManifestInvalid {
            rendered: render_manifest_failure(&manifest),
        }),
        WorkspaceRoot::NotFound => Err(CliError::no_workspace_root(cwd)),
    }
}

/// What the manifest parser says about `path`, with its code.
///
/// The walk hands back the path rather than an error precisely so the message
/// comes from the parser that owns it, instead of a second description written
/// next to the walk.
fn render_manifest_failure(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(src) => match ridge_manifest::parse_workspace(&src, path) {
            Err(e) => format!("[{}] {e}", e.code()),
            // The walk only reports a path it could not parse, so reaching here
            // means the file changed between the two reads.
            Ok(_) => format!(
                "`{}` could not be read when the workspace root was located",
                path.display()
            ),
        },
        Err(e) => format!("`{}` could not be read: {e}", path.display()),
    }
}
