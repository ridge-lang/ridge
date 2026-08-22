//! Filesystem search for the workspace root.

use std::path::{Path, PathBuf};

/// What the upward walk found.
///
/// The walk used to answer with an `Option`, which gave a manifest that exists
/// and cannot be parsed the same answer as no manifest at all. Two things
/// followed from that. A workspace whose own `ridge.toml` was malformed was
/// reported as missing, sending the reader after a file sitting in front of
/// them; and where a valid workspace existed further up, the command ran
/// against that one instead, printing a success for a project nobody asked
/// about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRoot {
    /// The directory whose `ridge.toml` declares a `[workspace]` table.
    Found(PathBuf),
    /// No `ridge.toml` with a `[workspace]` table anywhere above `start`, and
    /// every manifest passed on the way up parsed cleanly.
    NotFound,
    /// No workspace root above `start`, and this manifest on the way up could
    /// not be parsed. It is reported instead of "not found" because a file that
    /// is there and broken is not an absent one. The path is handed back rather
    /// than an error: the manifest parser owns what to say about it, and says
    /// it with a code and a source frame.
    Malformed(PathBuf),
}

/// Walk up the directory tree from `start` to find the nearest directory that
/// contains a `ridge.toml` with a `[workspace]` table.
///
/// # Algorithm
///
/// For each ancestor of `start` (inclusive):
/// 1. Check whether `<ancestor>/ridge.toml` exists.
/// 2. If so, read it and test whether it declares a `[workspace]` table. A
///    `ridge.toml` that parses and does not is a project-only manifest, and the
///    walk continues past it.
/// 3. A `ridge.toml` in `start` itself that cannot be read or parsed ends the
///    walk immediately. The manifest in the directory you are standing in
///    always governs that directory; an ancestor only governs it if it can be
///    read well enough to say so. Without this the walk climbs past a broken
///    manifest and the command runs against whatever workspace happens to sit
///    further up, reporting success for a project nobody asked about.
/// 4. A `ridge.toml` further up that cannot be read or parsed is remembered and
///    the walk continues, because a member with a broken manifest still has to
///    find the workspace above it so discovery can report the member with its
///    own code. The remembered path is the answer only if the walk ends without
///    finding a workspace root at all.
///
/// # Cross-platform note
///
/// Uses [`Path::join`] for all path construction — no string concatenation or
/// hard-coded separators.
#[must_use]
pub fn find_workspace_root(start: &Path) -> WorkspaceRoot {
    let mut current = if start.is_file() {
        match start.parent() {
            Some(p) => p.to_owned(),
            None => return WorkspaceRoot::NotFound,
        }
    } else {
        start.to_owned()
    };

    // An unreadable manifest above `start`, kept in case nothing further up
    // qualifies. One in `start` itself does not wait: it is returned on the
    // spot.
    let mut malformed: Option<PathBuf> = None;
    let mut at_start = true;

    loop {
        let candidate = current.join("ridge.toml");
        if candidate.is_file() {
            let readable = std::fs::read_to_string(&candidate)
                .ok()
                .and_then(|src| workspace_table_present(&src));
            match readable {
                Some(true) => return WorkspaceRoot::Found(current),
                // Parsed, no `[workspace]`: a project-only manifest, so keep
                // climbing for the workspace that lists it as a member.
                Some(false) => {}
                None if at_start => return WorkspaceRoot::Malformed(candidate),
                None => {
                    if malformed.is_none() {
                        malformed = Some(candidate);
                    }
                }
            }
        }
        at_start = false;

        match current.parent().map(Path::to_owned) {
            Some(parent) => current = parent,
            None => {
                return malformed.map_or(WorkspaceRoot::NotFound, WorkspaceRoot::Malformed);
            }
        }
    }
}

/// Whether `src` declares a top-level `workspace` table, or `None` when it is
/// not TOML at all.
///
/// Deliberately a minimal parse — all this needs to know is whether the key
/// exists at the top level, which avoids pulling in the full validation logic
/// and handles forward-compat gracefully. The three-way answer is what lets the
/// caller tell a project manifest apart from a broken one.
///
/// Note: uses `toml::from_str::<toml::Table>(...)` rather than
/// `src.parse::<toml::Value>()` — the latter regressed in `toml` 1.1 (the
/// `FromStr for Value` impl now stops at the first table header instead of
/// parsing the whole document).
fn workspace_table_present(src: &str) -> Option<bool> {
    toml::from_str::<toml::Table>(src)
        .ok()
        .map(|t| t.get("workspace").is_some())
}
