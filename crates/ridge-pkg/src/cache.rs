//! Cache layout helpers for `ridge-pkg`.
//!
//! The cache is **append-only** in 0.1.0 (GC deferred to 0.2.0+).
//!
//! Layout: `<cache_root>/git/<host>/<owner>/<repo>/<rev>/`
//!
//! Platform-specific roots:
//! - **Linux**: `$XDG_CACHE_HOME/ridge/git/…` (default `~/.cache/ridge/git/…`)
//! - **macOS**: `~/Library/Caches/ridge/git/…`
//! - **Windows**: `%LOCALAPPDATA%\Ridge\cache\git\…`
//!
//! Implemented via the `directories` crate — NOT `dirs` — to honour XDG
//! semantics on Linux.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error::PkgError;

// ── Public helpers ────────────────────────────────────────────────────────────

/// Return the platform-aware `ridge-pkg` cache root.
///
/// Uses `directories::ProjectDirs` which honours `$XDG_CACHE_HOME` on Linux
/// and uses the correct platform directories on macOS and Windows.
///
/// # Errors
///
/// Returns `P103 PkgCacheRootUnavailable` if the home directory cannot be
/// determined.
pub fn cache_root() -> Result<PathBuf, PkgError> {
    // Per-user, shared cache so the same dep across workspaces is fetched only once.
    let dirs = ProjectDirs::from("org", "Ridge", "ridge")
        .ok_or(PkgError::PkgCacheRootUnavailable)?;
    Ok(dirs.cache_dir().to_owned())
}

/// Build the full cache path for a git dependency.
///
/// Shape: `<cache_root>/git/<host>/<owner>/<repo>/<rev>/`
///
/// The `<rev>` component is the tag or branch name.  On Windows, path
/// separators are native (`\`); all joins use `Path::join` (§1.3 #5).
///
/// # Errors
///
/// Propagates `P103` from [`cache_root`] if called without a user-supplied
/// root.
#[must_use]
pub fn git_cache_path(
    base_cache_root: &Path,
    host: &str,
    owner: &str,
    repo: &str,
    rev: &str,
) -> PathBuf {
    base_cache_root
        .join("git")
        .join(host)
        .join(owner)
        .join(repo)
        .join(rev)
}

/// Parse a git URL into `(host, owner, repo)` components for cache-path
/// construction.
///
/// Supported schemes:
/// - `https://host/owner/repo[.git]`
/// - `http://host/owner/repo[.git]`
/// - `file:///path/to/repo[.git]` — used in tests; the last two path
///   components become `("_local", last-2-segment, last-segment)`.
///
/// Returns `None` if the URL cannot be parsed into usable components.
#[must_use]
pub fn parse_git_url(url: &str) -> Option<(String, String, String)> {
    // ── file:// — local path (test fixture or CI local server) ──────────────
    if url.starts_with("file://") {
        return parse_file_url(url);
    }

    // ── https:// / http:// ────────────────────────────────────────────────────
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;

    let mut parts = without_scheme.splitn(4, '/');
    let host = parts.next()?;
    let owner = parts.next()?;
    let repo_raw = parts.next()?;
    // Strip .git suffix if present.
    let repo = repo_raw.strip_suffix(".git").unwrap_or(repo_raw);

    if host.is_empty() || owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some((host.to_owned(), owner.to_owned(), repo.to_owned()))
}

/// Parse a `file://` URL into `("_local", parent-dir, repo-name)`.
fn parse_file_url(url: &str) -> Option<(String, String, String)> {
    // Strip file:// or file:/// prefix.
    let path_part = url
        .strip_prefix("file:///")
        .or_else(|| url.strip_prefix("file://"))?;

    // Split on '/' to get path segments; ignore empty segments.
    let segments: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();

    // Need at least two segments for owner/repo.
    if segments.len() < 2 {
        return None;
    }

    let repo_raw = *segments.last()?;
    let repo = repo_raw.strip_suffix(".git").unwrap_or(repo_raw);
    let owner = segments.get(segments.len() - 2).copied()?;

    Some(("_local".to_owned(), owner.to_owned(), repo.to_owned()))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_git_url_github() {
        let result = parse_git_url("https://github.com/acme/mylib");
        assert_eq!(
            result,
            Some(("github.com".into(), "acme".into(), "mylib".into()))
        );
    }

    #[test]
    fn parse_git_url_with_dot_git() {
        let result = parse_git_url("https://github.com/acme/mylib.git");
        assert_eq!(
            result,
            Some(("github.com".into(), "acme".into(), "mylib".into()))
        );
    }

    #[test]
    fn parse_git_url_returns_none_for_ssh() {
        assert!(parse_git_url("git@github.com:acme/mylib").is_none());
    }

    #[test]
    fn git_cache_path_shape() {
        let root = PathBuf::from("/tmp/ridge-cache");
        let path = git_cache_path(&root, "github.com", "acme", "mylib", "v1.0");
        assert_eq!(
            path,
            PathBuf::from("/tmp/ridge-cache/git/github.com/acme/mylib/v1.0")
        );
    }
}
