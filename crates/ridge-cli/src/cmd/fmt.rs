//! `ridge fmt` — Format Ridge source files according to the standard style.
//!
//! ## Surface
//!
//! ```text
//! ridge fmt [--check] [--stdin] [<paths>...]
//! ```
//!
//! - Default: format every `.ridge` file in the current workspace recursively,
//!   in-place.
//! - `--check`: dry-run; exit 1 if any file would change.
//! - `--stdin`: read from stdin, write formatted output to stdout.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use ridge_manifest::find_workspace_root;

use crate::error::CliError;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Format Ridge source files according to the standard style.
#[derive(Debug, Parser)]
pub struct FmtArgs {
    /// Exit non-zero if any file would be reformatted; never write.
    #[arg(long)]
    pub check: bool,

    /// Read source from stdin, write formatted output to stdout. Ignores <paths>.
    #[arg(long)]
    pub stdin: bool,

    /// Files or directories to format. Defaults to the current workspace.
    #[arg(value_name = "PATHS")]
    pub paths: Vec<PathBuf>,
}

// ── Directory walker ──────────────────────────────────────────────────────────

/// Collect all `.ridge` files under `dir` recursively.
///
/// Skips hidden directories (names starting with `.`) and `target/` at any depth.
fn collect_rg_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), CliError> {
    let entries = std::fs::read_dir(dir).map_err(|e| CliError::FmtIoError {
        path: dir.to_path_buf(),
        source: e.to_string(),
    })?;

    for entry_result in entries {
        let entry = entry_result.map_err(|e| CliError::FmtIoError {
            path: dir.to_path_buf(),
            source: e.to_string(),
        })?;

        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Skip hidden directories and target/.
        if name.starts_with('.') || name == "target" {
            continue;
        }

        let ft = entry.file_type().map_err(|e| CliError::FmtIoError {
            path: path.clone(),
            source: e.to_string(),
        })?;

        let ext = path.extension();
        if ft.is_dir() {
            collect_rg_files(&path, out)?;
        } else if ft.is_file() && ext.is_some_and(|e| e == "ridge") {
            out.push(path);
        } else if ft.is_file() && ext.is_some_and(|e| e == "rg") {
            return Err(CliError::LegacyRgFile { path });
        }
    }
    Ok(())
}

/// Expand the user-supplied `paths` into a flat list of `.ridge` files.
///
/// Files are taken as-is; directories are walked recursively.  Non-existent
/// paths return `CliError::FmtPathNotFound`.
fn expand_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>, CliError> {
    let mut files = Vec::new();
    for p in paths {
        if !p.exists() {
            return Err(CliError::FmtPathNotFound { path: p.clone() });
        }
        if p.is_dir() {
            collect_rg_files(p, &mut files)?;
        } else {
            files.push(p.clone());
        }
    }
    Ok(files)
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge fmt`.
///
/// # Errors
///
/// Returns a [`CliError`] on unrecoverable I/O failures or when `--check`
/// finds files that would be reformatted.
pub fn execute(args: &FmtArgs, cwd: &Path) -> Result<(), CliError> {
    // ── stdin mode ────────────────────────────────────────────────────────────
    if args.stdin {
        return execute_stdin(args.check);
    }

    // ── filesystem mode ───────────────────────────────────────────────────────
    let files: Vec<PathBuf> = if args.paths.is_empty() {
        // No paths supplied — walk the workspace root.
        let root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;
        let mut v = Vec::new();
        collect_rg_files(&root, &mut v)?;
        v
    } else {
        expand_paths(&args.paths)?
    };

    if args.check {
        execute_check(&files)
    } else {
        execute_format(&files)
    }
}

// ── stdin helper ──────────────────────────────────────────────────────────────

/// Handle `ridge fmt [--check] --stdin`.
fn execute_stdin(check: bool) -> Result<(), CliError> {
    let mut src = String::new();
    io::stdin()
        .read_to_string(&mut src)
        .map_err(|e| CliError::FmtIoError {
            path: PathBuf::from("<stdin>"),
            source: e.to_string(),
        })?;

    match ridge_fmt::format_source(&src) {
        Ok(formatted) => {
            if check {
                // --check --stdin: exit 1 if input differs from formatted output.
                if src != formatted {
                    return Err(CliError::FmtCheckFailed { count: 1 });
                }
                return Ok(());
            }
            io::stdout()
                .write_all(formatted.as_bytes())
                .map_err(|e| CliError::FmtIoError {
                    path: PathBuf::from("<stdout>"),
                    source: e.to_string(),
                })?;
        }
        Err(e) => {
            // Unparseable: write original to stdout (do not corrupt), warn to
            // stderr, exit 1.
            eprintln!("warning: <stdin>: {e}");
            if !check {
                io::stdout()
                    .write_all(src.as_bytes())
                    .map_err(|io_err| CliError::FmtIoError {
                        path: PathBuf::from("<stdout>"),
                        source: io_err.to_string(),
                    })?;
            }
            return Err(CliError::FmtCheckFailed { count: 1 });
        }
    }
    Ok(())
}

// ── check helper ──────────────────────────────────────────────────────────────

/// Handle `ridge fmt --check <files>`.
fn execute_check(files: &[PathBuf]) -> Result<(), CliError> {
    let mut would_reformat = 0usize;

    for path in files {
        let src = read_file(path)?;

        match ridge_fmt::format_source(&src) {
            Ok(formatted) => {
                if src != formatted {
                    println!("would reformat {}", path.display());
                    would_reformat += 1;
                }
            }
            Err(e) => {
                eprintln!("warning: {}: {e}", path.display());
                would_reformat += 1;
            }
        }
    }

    if would_reformat > 0 {
        return Err(CliError::FmtCheckFailed {
            count: would_reformat,
        });
    }
    Ok(())
}

// ── in-place format helper ────────────────────────────────────────────────────

/// Handle `ridge fmt <files>` (in-place rewrite).
fn execute_format(files: &[PathBuf]) -> Result<(), CliError> {
    let mut io_error = false;

    for path in files {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {}: C103 FmtIoError: {e}", path.display());
                io_error = true;
                continue;
            }
        };

        match ridge_fmt::format_source(&src) {
            Ok(formatted) => {
                if src != formatted {
                    if let Err(e) = std::fs::write(path, formatted.as_bytes()) {
                        eprintln!("error: {}: C103 FmtIoError: {e}", path.display());
                        io_error = true;
                    }
                }
            }
            Err(e) => {
                // Warn but do not corrupt the file.
                eprintln!("warning: {}: {e}", path.display());
            }
        }
    }

    if io_error {
        return Err(CliError::FmtIoError {
            path: PathBuf::from("<multiple>"),
            source: "one or more files could not be read or written".to_string(),
        });
    }
    Ok(())
}

// ── I/O helper ────────────────────────────────────────────────────────────────

/// Read a file to string, returning a [`CliError::FmtIoError`] on failure.
fn read_file(path: &Path) -> Result<String, CliError> {
    std::fs::read_to_string(path).map_err(|e| CliError::FmtIoError {
        path: path.to_path_buf(),
        source: e.to_string(),
    })
}
