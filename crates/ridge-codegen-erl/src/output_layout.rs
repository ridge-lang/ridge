//! Output-directory layout — `target/ridge/<profile>/`.
//!
//! The canonical layout (§3.3) is:
//!
//! ```text
//! target/ridge/<profile>/
//! ├── core/
//! ├── beam/
//! ├── runtime/
//! └── manifest.toml
//! ```
//!
//! `<profile>` is `debug` or `release` per [`BuildProfile`].

use crate::{BuildProfile, CodegenError};
use std::path::{Path, PathBuf};

/// Return the workspace-relative output root for the given build profile.
///
/// Pure path computation — no I/O.  Returns `"target/ridge/debug"` for
/// [`BuildProfile::Debug`] and `"target/ridge/release"` for
/// [`BuildProfile::Release`].
#[must_use]
pub fn resolve_out_root(profile: BuildProfile) -> PathBuf {
    let sub = match profile {
        BuildProfile::Debug => "debug",
        BuildProfile::Release => "release",
    };
    PathBuf::from("target/ridge").join(sub)
}

/// Return the path for a `.core` file within the output root.
///
/// Pure path computation — no I/O.
/// Returns `<out_root>/core/<beam_module_name>.core`.
#[must_use]
pub fn core_file_path(out_root: &Path, beam_module_name: &str) -> PathBuf {
    out_root
        .join("core")
        .join(format!("{beam_module_name}.core"))
}

/// Return the `beam/` subdirectory path within the output root.
///
/// Pure path computation — no I/O.
#[must_use]
pub fn beam_dir(out_root: &Path) -> PathBuf {
    out_root.join("beam")
}

/// Return the `runtime/` subdirectory path within the output root.
///
/// Pure path computation — no I/O.
#[must_use]
pub fn runtime_dir(out_root: &Path) -> PathBuf {
    out_root.join("runtime")
}

/// Create the `core/`, `beam/`, and `runtime/` subdirectories under `out_root`.
///
/// Idempotent — already-existing directories are left intact.  Any I/O
/// failure surfaces as [`CodegenError::OutputDirNotWritable`].
pub fn ensure_out_dirs(out_root: &Path) -> Result<(), CodegenError> {
    for subdir in &["core", "beam", "runtime"] {
        let dir = out_root.join(subdir);
        std::fs::create_dir_all(&dir).map_err(|e| CodegenError::OutputDirNotWritable {
            path: dir.clone(),
            io_err: e.to_string(),
        })?;
    }
    Ok(())
}
