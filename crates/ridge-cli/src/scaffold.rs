//! Shared scaffolder for `ridge new` and `ridge init`.
//!
//! Both subcommands produce the canonical single-project workspace layout
//! (§2.9):
//!
//! ```text
//! <name>/
//! ├── ridge.toml
//! ├── src/
//! │   └── Main.ridge
//! └── README.md
//! ```
//!
//! Templates are embedded at compile time via [`include_str!`] and
//! instantiated with [`std::fmt::format`] / `str::replace`.  No external
//! template engine is used.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::CliError;

// ── Embedded templates ────────────────────────────────────────────────────────

/// Raw `ridge.toml` template.  `{NAME}` is replaced with the project name.
const TOML_TEMPLATE: &str = include_str!("../templates/ridge.toml.tpl");

/// Raw `src/Main.ridge` template.  `{NAME}` is replaced with the project name.
const MAIN_RG_TEMPLATE: &str = include_str!("../templates/Main.ridge.tpl");

/// Raw `README.md` template.  `{NAME}` is replaced with the project name.
const README_TEMPLATE: &str = include_str!("../templates/README.md.tpl");

// ── Reserved names (§3.5, C203) ───────────────────────────────────────────────

/// Names that are reserved by the Ridge toolchain.  Comparison is case-insensitive.
const RESERVED_NAMES: &[&str] = &["std", "test", "core"];

// ── Validation ────────────────────────────────────────────────────────────────

/// Validate a project name.
///
/// Returns [`CliError::InvalidProjectName`] when the name is empty, contains
/// path-separator characters (`/`, `\`), starts with `.`, contains `..`, or
/// contains characters that are not portable across Linux, macOS, and Windows
/// (i.e. any of `< > : " | ? *` — the Windows-forbidden set).
///
/// Returns [`CliError::ReservedName`] when the (case-insensitively normalised)
/// name is `std`, `test`, or `core`.
///
/// Validation order: structural validity → reserved name (per §3.5 spec).
pub fn validate_name(name: &str) -> Result<(), CliError> {
    // ── Structural validity ───────────────────────────────────────────────────
    if name.is_empty() {
        return Err(CliError::InvalidProjectName {
            name: name.to_owned(),
        });
    }

    if name.starts_with('.') {
        return Err(CliError::InvalidProjectName {
            name: name.to_owned(),
        });
    }

    // Reject path-separator characters and Windows-forbidden characters.
    let forbidden_chars = ['/', '\\', '<', '>', ':', '"', '|', '?', '*'];
    if name.chars().any(|c| forbidden_chars.contains(&c)) {
        return Err(CliError::InvalidProjectName {
            name: name.to_owned(),
        });
    }

    // Reject the `..` component (even when embedded in a longer name).
    if name.contains("..") {
        return Err(CliError::InvalidProjectName {
            name: name.to_owned(),
        });
    }

    // ── Reserved names ────────────────────────────────────────────────────────
    let lower = name.to_ascii_lowercase();
    if RESERVED_NAMES.contains(&lower.as_str()) {
        return Err(CliError::ReservedName {
            name: name.to_owned(),
        });
    }

    Ok(())
}

// ── Scaffolder ────────────────────────────────────────────────────────────────

/// Write the canonical scaffold under `project_dir` using `name` as the
/// project name.
///
/// `project_dir` must already exist.  This function creates:
/// - `project_dir/ridge.toml`
/// - `project_dir/src/Main.ridge`
/// - `project_dir/README.md`
///
/// # Errors
///
/// Returns `CliError::InvalidProjectName` / `CliError::ReservedName` from
/// [`validate_name`], or an `std::io::Error` wrapped in a [`CliError`] if any
/// file creation fails.
fn write_scaffold(project_dir: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // ridge.toml
    let toml_content = TOML_TEMPLATE.replace("{NAME}", name);
    fs::write(project_dir.join("ridge.toml"), toml_content)?;

    // src/Main.ridge — use PathBuf::join for cross-platform path construction
    let src_dir = project_dir.join("src");
    fs::create_dir_all(&src_dir)?;
    let main_rg_content = MAIN_RG_TEMPLATE.replace("{NAME}", name);
    fs::write(src_dir.join("Main.ridge"), main_rg_content)?;

    // README.md
    let readme_content = README_TEMPLATE.replace("{NAME}", name);
    fs::write(project_dir.join("README.md"), readme_content)?;

    Ok(())
}

// ── `ridge new` ───────────────────────────────────────────────────────────────

/// Scaffold a new project in `<cwd>/<name>/`.
///
/// Validates the name, refuses if the directory already exists, then writes
/// the canonical layout.
///
/// # Errors
///
/// - [`CliError::InvalidProjectName`] — name is structurally invalid.
/// - [`CliError::ReservedName`] — name is reserved.
/// - [`CliError::DirectoryExists`] — `<name>/` already exists in `cwd`.
pub fn new_project(name: &str, cwd: &Path) -> Result<(), CliError> {
    // ── 1. Validate name ──────────────────────────────────────────────────────
    validate_name(name)?;

    // ── 2. Refuse if directory already exists ─────────────────────────────────
    let project_dir: PathBuf = cwd.join(name);
    if project_dir.exists() {
        return Err(CliError::DirectoryExists {
            name: name.to_owned(),
        });
    }

    // ── 3. Create directory tree and write files ──────────────────────────────
    fs::create_dir_all(&project_dir).map_err(|e| {
        eprintln!(
            "error: could not create directory '{}': {e}",
            project_dir.display()
        );
        CliError::DirectoryExists {
            name: name.to_owned(),
        }
    })?;

    write_scaffold(&project_dir, name).map_err(|e| {
        eprintln!("error: scaffold write failed: {e}");
        CliError::DirectoryExists {
            name: name.to_owned(),
        }
    })?;

    println!("Created project '{name}' in {}/", project_dir.display());
    println!("Run: cd {name} && ridge build");

    Ok(())
}

// ── `ridge init` ──────────────────────────────────────────────────────────────

/// Scaffold a new project in the current working directory `cwd`.
///
/// The project name is derived from `cwd.file_name()`.  The directory must be
/// empty (ignoring `.git/` and `.gitignore`).
///
/// # Errors
///
/// - [`CliError::CwdUnreadable`] — `cwd` cannot be read.
/// - [`CliError::DirectoryNotEmpty`] — `cwd` contains files other than `.git/`
///   and `.gitignore`.
/// - [`CliError::InvalidProjectName`] / [`CliError::ReservedName`] — the
///   directory name is not a valid project name.
pub fn init_project(cwd: &Path) -> Result<(), CliError> {
    // ── 1. Derive name from cwd.file_name() ───────────────────────────────────
    let name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(CliError::CwdUnreadable)?
        .to_owned();

    // ── 2. Validate name ──────────────────────────────────────────────────────
    validate_name(&name)?;

    // ── 3. Assert directory is empty (allow .git/ and .gitignore) ─────────────
    let entries = fs::read_dir(cwd).map_err(|_| CliError::CwdUnreadable)?;

    for entry in entries {
        let entry = entry.map_err(|_| CliError::CwdUnreadable)?;
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();

        // Allow .git directory and .gitignore file.
        if file_name_str == ".git" || file_name_str == ".gitignore" {
            continue;
        }

        // Any other entry makes the directory non-empty.
        return Err(CliError::DirectoryNotEmpty);
    }

    // ── 4. Write scaffold ─────────────────────────────────────────────────────
    write_scaffold(cwd, &name).map_err(|e| {
        eprintln!("error: scaffold write failed: {e}");
        CliError::CwdUnreadable
    })?;

    println!("Initialised project '{name}' in current directory.");
    println!("Run: ridge build");

    Ok(())
}
