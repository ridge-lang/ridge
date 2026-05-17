//! Git dependency resolver for `ridge-pkg`.
//!
//! Clones `git = "https://…", tag/branch = "…"` dependencies into a
//! platform-aware cache directory using the system `git` binary with pinned,
//! reproducible flags (D152).
//!
//! # Decisions
//!
//! - **D153** (OQ-C012): HTTPS-only; SSH URLs rejected with `P003`.
//! - **D152** (OQ-C011): System `git` via `std::process::Command`; pure-Rust
//!   `gitoxide` deferred to 0.2.0.
//! - **D144** (OQ-C003): XDG-compliant cache root via `directories` crate.
//! - **D160**: Floating-branch tracking emits `P004 FloatingBranchAdvisory`
//!   warning, not an error.
//! - **R17**: Lenient `git --version` parse — first `\d+\.\d+` after the word
//!   `version` wins; unparseable output → `P009`, not `P008`.
//! - **R4**: `git.exe` on Windows discovered via `which::which("git")`.

use std::path::{Path, PathBuf};
use std::process::Command;

use ridge_manifest::{parse_project, GitRev, ProjectManifest};
use which::which;

use crate::cache::{git_cache_path, parse_git_url};
use crate::error::{PkgError, PkgWarning};

// ── Minimum required git version ──────────────────────────────────────────────

const MIN_GIT_MAJOR: u32 = 2;
const MIN_GIT_MINOR: u32 = 20;

// ── Public entry point ────────────────────────────────────────────────────────

/// Resolve a `git = "…"` dependency, cloning into the cache if necessary.
///
/// `dep_name` is the local alias used in `ridge.toml` (for warning messages).
///
/// Returns `(source_root, manifest, warnings)`.
///
/// # Errors
///
/// See `P001`–`P009` in [`crate::error::PkgError`].
pub fn resolve_git_dep(
    dep_name: &str,
    url: &str,
    rev: &GitRev,
    cache_root: &Path,
) -> Result<(PathBuf, ProjectManifest, Vec<PkgWarning>), PkgError> {
    // ── 1. HTTPS-only guard (D153, OQ-C012) ──────────────────────────────────
    reject_ssh_url(url)?; // OQ-C012

    // ── 2. Locate git binary (R4 — Windows git.exe detection) ────────────────
    let git_path = locate_git()?;

    // ── 3. Parse and validate git version (R17 — lenient parse) ──────────────
    check_git_version(&git_path)?;

    // ── 4. Determine ref name and emit floating-branch warning (D160) ─────────
    let (git_ref, warnings) = ref_and_warnings(dep_name, rev)?;

    // ── 5. Parse URL into cache path components ────────────────────────────────
    let (host, owner, repo) = parse_git_url(url).ok_or_else(|| PkgError::PkgGitFetchFailed {
        url: url.to_owned(),
        message: "URL does not have host/owner/repo segments".to_owned(),
        exit_code: -1,
    })?;

    let dest = git_cache_path(cache_root, &host, &owner, &repo, &git_ref);

    // ── 6. Clone if not already cached ────────────────────────────────────────
    if dest.exists() {
        // Validate existing cache entry with git fsck; re-clone on corruption.
        if !fsck_ok(&git_path, &dest) {
            // Remove corrupted entry and retry once.
            if let Err(e) = std::fs::remove_dir_all(&dest) {
                return Err(PkgError::PkgCacheWriteFailed {
                    path: dest,
                    message: format!("could not clear corrupted cache entry: {e}"),
                });
            }
            run_clone(url, &git_ref, &dest, &git_path)?;
        }
    } else {
        run_clone(url, &git_ref, &dest, &git_path)?;
    }

    // ── 7. Parse the dep's ridge.toml ─────────────────────────────────────────
    let manifest_path = dest.join("ridge.toml");
    if !manifest_path.exists() {
        return Err(PkgError::PkgPathManifestMissing {
            path: manifest_path,
        });
    }

    let toml_src =
        std::fs::read_to_string(&manifest_path).map_err(|e| PkgError::PkgManifestParseFailed {
            path: manifest_path.clone(),
            source: ridge_manifest::ManifestError::TomlParseFailed {
                path: manifest_path.clone(),
                message: e.to_string(),
            },
        })?;

    let manifest = parse_project(&toml_src, &manifest_path).map_err(|source| {
        PkgError::PkgManifestParseFailed {
            path: manifest_path.clone(),
            source,
        }
    })?;

    Ok((dest, manifest, warnings))
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Reject SSH-scheme URLs (D153, OQ-C012).
fn reject_ssh_url(url: &str) -> Result<(), PkgError> {
    // OQ-C012: HTTPS-only in 0.1.0; SSH deferred to 0.2.0.
    if url.starts_with("git@") || url.starts_with("ssh://") {
        return Err(PkgError::PkgGitSchemeUnsupported {
            url: url.to_owned(),
        });
    }
    Ok(())
}

/// Locate the `git` binary via PATH lookup (R4).
fn locate_git() -> Result<PathBuf, PkgError> {
    which("git").map_err(|_| PkgError::PkgGitNotInstalled)
}

/// Parse `git --version` and enforce the 2.20 minimum.
///
/// Lenient strategy (R17): first `\d+\.\d+` token after the literal
/// `version` keyword wins.  Unparseable → `P009`; too old → `P008`.
fn check_git_version(git_path: &Path) -> Result<(), PkgError> {
    let output = Command::new(git_path)
        .arg("--version")
        .output()
        .map_err(|_| PkgError::PkgGitNotInstalled)?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_and_check_version(stdout.trim())
}

/// Visible for testing — accepts a pre-formed version string.
pub fn parse_and_check_version(version_output: &str) -> Result<(), PkgError> {
    // R17: find first digit-sequence after the word "version".
    let after_version = version_output
        .find("version")
        .map(|i| &version_output[i + "version".len()..]);

    let version_str = after_version.and_then(|s| {
        // First token that starts with a digit.
        s.split_whitespace()
            .find(|tok| tok.starts_with(|c: char| c.is_ascii_digit()))
    });

    let version_str = version_str.ok_or_else(|| PkgError::PkgGitVersionUnparseable {
        output: version_output.to_owned(),
    })?;

    // Take the first two numeric components — ignore anything after the second dot.
    let mut parts = version_str.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
        PkgError::PkgGitVersionUnparseable {
            output: version_output.to_owned(),
        }
    })?;

    let minor: u32 = parts
        .next()
        .and_then(|s| {
            // Strip non-digit suffix (e.g. "39" from "39.2").
            let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .ok_or_else(|| PkgError::PkgGitVersionUnparseable {
            output: version_output.to_owned(),
        })?;

    if major < MIN_GIT_MAJOR || (major == MIN_GIT_MAJOR && minor < MIN_GIT_MINOR) {
        return Err(PkgError::PkgGitTooOld {
            found_version: format!("{major}.{minor}"),
            upgrade_hint: platform_upgrade_hint(),
        });
    }
    Ok(())
}

/// Return the ref name and any floating-branch warnings (D160).
fn ref_and_warnings(dep_name: &str, rev: &GitRev) -> Result<(String, Vec<PkgWarning>), PkgError> {
    match rev {
        GitRev::Tag(tag) => Ok((tag.clone(), vec![])),
        GitRev::Branch(branch) => {
            // D160: emit advisory warning; not an error.
            let warning = PkgWarning::FloatingBranchAdvisory {
                dep_name: dep_name.to_owned(),
                branch: branch.clone(),
            };
            Ok((branch.clone(), vec![warning]))
        }
        GitRev::Commit(commit) if commit.is_empty() => {
            // Sentinel from parse_git_rev when no rev was specified.
            Err(PkgError::PkgGitCommitUnsupported {
                name: dep_name.to_owned(),
            })
        }
        GitRev::Commit(_) => Err(PkgError::PkgGitCommitUnsupported {
            name: dep_name.to_owned(),
        }),
    }
}

/// Execute the pinned, reproducible git clone command (D152, OQ-C011).
///
/// Flags rationale (§3.9):
/// - `-c protocol.version=2` — modern, deterministic transfer
/// - `-c http.followRedirects=false` — prevents hijacked-DNS redirect
/// - `-c advice.detachedHead=false` — suppresses detached-HEAD noise in CI
/// - `--depth 1 --no-progress --no-tags --single-branch --branch <ref>`
///   — minimal fetch with no extra refs
///
/// On failure, classifies stderr into the appropriate P-code.
fn run_clone(url: &str, git_ref: &str, dest: &Path, git_path: &Path) -> Result<(), PkgError> {
    // OQ-C011: std::process::Command shell-out with pinned flags (D152).
    let output = Command::new(git_path)
        .args([
            "-c",
            "protocol.version=2",
            "-c",
            "http.followRedirects=false",
            "-c",
            "advice.detachedHead=false",
            "clone",
            "--depth",
            "1",
            "--no-progress",
            "--no-tags",
            "--single-branch",
            "--branch",
            git_ref,
            url,
        ])
        .arg(dest)
        .output()
        .map_err(|e| PkgError::PkgGitFetchFailed {
            url: url.to_owned(),
            message: e.to_string(),
            exit_code: -1,
        })?;

    if output.status.success() {
        return Ok(());
    }

    let exit_code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();

    // Classify the failure.
    if stderr.contains("not found")
        || stderr.contains("remote: repository not found")
        || stderr.contains("pathspec")
        || stderr.contains("reference")
        || stderr.contains("no such ref")
        || stderr.contains("couldn't find remote ref")
    {
        return Err(PkgError::PkgGitTagUnknown {
            git_ref: git_ref.to_owned(),
            url: url.to_owned(),
        });
    }

    if stderr.contains("no space left")
        || stderr.contains("disk full")
        || stderr.contains("permission denied")
        || stderr.contains("read-only file system")
    {
        return Err(PkgError::PkgCacheWriteFailed {
            path: dest.to_owned(),
            message: format!("git clone stderr: {stderr}"),
        });
    }

    Err(PkgError::PkgGitFetchFailed {
        url: url.to_owned(),
        message: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code,
    })
}

/// Run `git fsck` on an existing cached clone; returns `true` if healthy.
fn fsck_ok(git_path: &Path, dest: &Path) -> bool {
    Command::new(git_path)
        .args(["fsck", "--quiet"])
        .current_dir(dest)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Platform-specific upgrade hint for `P008 PkgGitTooOld`.
fn platform_upgrade_hint() -> String {
    #[cfg(target_os = "windows")]
    {
        "upgrade with: winget install Git.Git".to_owned()
    }
    #[cfg(target_os = "macos")]
    {
        "upgrade with: brew upgrade git".to_owned()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "upgrade with: apt install git  (or your distro's package manager)".to_owned()
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_url_rejected() {
        assert!(reject_ssh_url("git@github.com:acme/foo").is_err());
        assert!(reject_ssh_url("ssh://github.com/acme/foo").is_err());
    }

    #[test]
    fn https_url_accepted() {
        assert!(reject_ssh_url("https://github.com/acme/foo").is_ok());
    }

    #[test]
    fn version_parse_accepts_macos_git() {
        // R17: Apple Git suffix must not cause P009.
        assert!(
            parse_and_check_version("git version 2.39.2 (Apple Git-143)").is_ok(),
            "Apple Git suffix should not cause P009"
        );
    }

    #[test]
    fn version_parse_too_old() {
        let result = parse_and_check_version("git version 2.10.0");
        assert!(result.is_err(), "expected Err but got Ok");
        if let Err(err) = result {
            assert_eq!(err.code(), "P008");
        }
    }

    #[test]
    fn version_parse_unparseable() {
        let result = parse_and_check_version("git custom build ???");
        assert!(result.is_err(), "expected Err but got Ok");
        if let Err(err) = result {
            assert_eq!(err.code(), "P009");
        }
    }

    #[test]
    fn version_parse_exact_minimum() {
        // 2.20.0 is the exact minimum — must be accepted.
        assert!(parse_and_check_version("git version 2.20.0").is_ok());
    }
}
