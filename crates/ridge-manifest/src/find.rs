//! Filesystem search for the workspace root.

use std::path::{Path, PathBuf};

/// Walk up the directory tree from `start` to find the nearest directory that
/// contains a `ridge.toml` with a `[workspace]` table.
///
/// Returns `Some(path_to_workspace_dir)` on success, or `None` if no workspace
/// root could be found (i.e. the filesystem root was reached without finding a
/// qualifying `ridge.toml`).
///
/// # Algorithm
///
/// For each ancestor of `start` (inclusive):
/// 1. Check whether `<ancestor>/ridge.toml` exists.
/// 2. If so, read its content and test whether parsing it yields a
///    [`WorkspaceManifest`](crate::workspace::WorkspaceManifest).  A
///    `ridge.toml` that exists but does not contain `[workspace]` is skipped
///    (it is a project-only manifest).
/// 3. Return the directory of the first qualifying manifest found.
///
/// # Cross-platform note
///
/// Uses [`Path::join`] for all path construction — no string concatenation or
/// hard-coded separators.
#[must_use]
pub fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_file() {
        start.parent()?.to_owned()
    } else {
        start.to_owned()
    };

    loop {
        let candidate = current.join("ridge.toml");
        if candidate.is_file() {
            if let Ok(src) = std::fs::read_to_string(&candidate) {
                if is_workspace_toml(&src) {
                    return Some(current);
                }
            }
        }

        match current.parent().map(Path::to_owned) {
            Some(parent) => current = parent,
            None => return None,
        }
    }
}

/// Return `true` if `src` parses as TOML and contains a `[workspace]` table.
///
/// Deliberately uses a minimal parse — we only need to know whether the key
/// `workspace` exists at the top level.  This avoids pulling in the full
/// validation logic and handles forward-compat gracefully.
///
/// Note: uses `toml::from_str::<toml::Table>(...)` rather than
/// `src.parse::<toml::Value>()` — the latter regressed in `toml` 1.1 (the
/// `FromStr for Value` impl now stops at the first table header instead of
/// parsing the whole document).
fn is_workspace_toml(src: &str) -> bool {
    toml::from_str::<toml::Table>(src)
        .ok()
        .and_then(|t| t.get("workspace").cloned())
        .is_some()
}
