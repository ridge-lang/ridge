//! `erlc` toolchain probe and Core Erlang compilation helpers.
//!
//! Discovers the Erlang compiler, verifies its OTP version meets the minimum
//! requirement (OTP 26+, per spec §11.2), and compiles `.core` files to `.beam`.

use crate::{BuildProfile, CodegenError};
use std::path::{Path, PathBuf};

/// Minimum supported OTP major version (per plan §3.7 / spec §11.2 line 1189).
pub const MIN_OTP_VERSION: u32 = 26;

/// Information about a discovered `erlc` toolchain.
#[derive(Debug, Clone)]
pub struct ErlcInfo {
    /// Absolute path to the `erlc` executable.
    pub path: PathBuf,
    /// Major OTP version (e.g. `26` for OTP 26).
    pub version: u32,
}

/// Probe the `erlc` toolchain and verify its OTP version is ≥ [`MIN_OTP_VERSION`].
///
/// # Resolution order
///
/// 1. If `opt_path` is `Some`, that path is used directly.
/// 2. Otherwise the executable is located via `PATH` using the [`which`] crate.
///
/// # Failure surface
///
/// - Executable not found → [`CodegenError::ErlcNotFound`].
///   `searched_paths` is always `vec![]` because [`which`] does not expose the
///   list of paths it searched in a stable API.
/// - Executable found but version is below [`MIN_OTP_VERSION`] →
///   [`CodegenError::ErlcVersionTooOld`].
/// - Executable found but `erlc -v` output cannot be parsed → treated as
///   [`CodegenError::ErlcVersionTooOld`] with the raw output in `found`, since
///   no separate "unparseable" variant exists in [`CodegenError`].
pub fn probe(opt_path: Option<&Path>) -> Result<ErlcInfo, CodegenError> {
    let path = resolve_erlc_path(opt_path)?;

    let output = std::process::Command::new(&path)
        .arg("-v")
        .output()
        .map_err(|_| CodegenError::ErlcNotFound {
            searched_paths: vec![],
        })?;

    // `erlc -v` may print to stdout or stderr depending on the OTP version.
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // On some platforms (e.g. Windows + OTP 28) `erlc -v` produces no output.
    // Fall back to `erl -eval 'erlang:system_info(otp_release)' -noshell`
    // which always prints just the release number (e.g. "28").
    let version = if let Some(v) = parse_version(&combined) {
        v
    } else {
        probe_version_via_erl(&path)?
    };

    validate(version)?;

    Ok(ErlcInfo { path, version })
}

/// Validate that `found` is ≥ [`MIN_OTP_VERSION`].
///
/// Exposed so integration tests can exercise the version-rejection rule
/// without spawning a real `erlc` process.
pub fn validate(found: u32) -> Result<(), CodegenError> {
    if found >= MIN_OTP_VERSION {
        Ok(())
    } else {
        Err(CodegenError::ErlcVersionTooOld {
            found: format!("OTP {found}"),
            minimum: format!("OTP {MIN_OTP_VERSION}"),
        })
    }
}

// ── compile_core ─────────────────────────────────────────────────────────────

/// The result of compiling a single `.core` file with `erlc +from_core`.
#[derive(Debug, Clone)]
pub struct CompiledArtifact {
    /// Path to the produced `.beam` file.
    pub beam_path: PathBuf,
    /// Captured `erlc` stderr (warnings / info lines, even on success).
    pub stderr: String,
}

/// Compile a `.core` file to a `.beam` file using `erlc +from_core`.
///
/// Invokes `erlc +from_core [+debug_info|+bin_opt_info] +{i,"<runtime_dir>"} -o <beam_out_dir> <core_path>`.
/// The profile controls which optimisation flags are passed:
/// - [`BuildProfile::Debug`] — `+debug_info` (no stripping).
/// - [`BuildProfile::Release`] — `+bin_opt_info` (no debug chunks).
///
/// # Errors
///
/// - Spawning `erlc` fails (e.g. executable not found) → [`CodegenError::ErlcNotFound`].
/// - `erlc` exits with a non-zero code → [`CodegenError::ErlcRejectedInput`].
/// - `erlc` exits zero but the expected `.beam` file is absent →
///   [`CodegenError::ErlcUnexpectedOutput`].
pub fn compile_core(
    erlc_path: &Path,
    core_path: &Path,
    beam_out_dir: &Path,
    runtime_dir: &Path,
    profile: BuildProfile,
) -> Result<CompiledArtifact, CodegenError> {
    // Normalise backslashes to forward slashes for the Erlang string literal
    // embedded in the +{i, "..."} term (Windows \ is an escape char in Erlang).
    let runtime_dir_fwd = runtime_dir.to_string_lossy().replace('\\', "/");
    let include_term = format!("+{{i, \"{runtime_dir_fwd}\"}}");

    let profile_flag = match profile {
        BuildProfile::Debug => "+debug_info",
        BuildProfile::Release => "+bin_opt_info",
    };

    let output = std::process::Command::new(erlc_path)
        .arg("+from_core")
        .arg(profile_flag)
        .arg(&include_term)
        .arg("-o")
        .arg(beam_out_dir)
        .arg(core_path)
        .output()
        .map_err(|_| CodegenError::ErlcNotFound {
            searched_paths: vec![],
        })?;

    if !output.status.success() {
        return Err(CodegenError::ErlcRejectedInput {
            core_path: core_path.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        });
    }

    // Derive expected beam path from the core file stem.
    let stem = core_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let beam_path = beam_out_dir.join(format!("{stem}.beam"));

    if !beam_path.exists() {
        return Err(CodegenError::ErlcUnexpectedOutput {
            core_path: core_path.to_path_buf(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(CompiledArtifact {
        beam_path,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Parse the major OTP version number from `erlc -v` output.
///
/// Looks for the pattern `Erlang/OTP <N>` (case-sensitive, as produced by OTP)
/// and extracts `<N>`.  Returns `None` if no match is found.
pub(crate) fn parse_version(stdout: &str) -> Option<u32> {
    // Typical output: "Erlang/OTP 26 [erts-14.0.2] ..."
    for line in stdout.lines() {
        if let Some(rest) = line.find("Erlang/OTP ").map(|i| &line[i + 11..]) {
            let version_str = rest.split_whitespace().next()?;
            if let Ok(n) = version_str
                .trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse::<u32>()
            {
                return Some(n);
            }
            // Try parsing the raw token as-is (handles clean "26" or "27").
            if let Ok(n) = version_str.parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// Derive the OTP release number by running `erl -noshell -eval …`.
///
/// Used as a fallback when `erlc -v` produces no parseable output (observed
/// on Windows with OTP 28).  Locates `erl` by replacing the `erlc` filename
/// in the resolved path, or falls back to PATH lookup.
fn probe_version_via_erl(erlc_path: &Path) -> Result<u32, CodegenError> {
    // Try sibling `erl` next to `erlc` first; fall back to PATH.
    let erl_path: PathBuf = erlc_path
        .parent()
        .map(|p| p.join("erl"))
        .filter(|p| p.exists())
        .or_else(|| which::which("erl").ok())
        .ok_or_else(|| CodegenError::ErlcNotFound {
            searched_paths: vec![],
        })?;

    let output = std::process::Command::new(&erl_path)
        .args([
            "-noshell",
            "-eval",
            "io:format(\"~s\", [erlang:system_info(otp_release)]), halt().",
        ])
        .output()
        .map_err(|_| CodegenError::ErlcNotFound {
            searched_paths: vec![],
        })?;

    let raw = String::from_utf8_lossy(&output.stdout);
    raw.trim()
        .parse::<u32>()
        .map_err(|_| CodegenError::ErlcVersionTooOld {
            found: raw.trim().to_owned(),
            minimum: format!("OTP {MIN_OTP_VERSION}"),
        })
}

/// Resolve the `erlc` executable path.
///
/// If `opt_path` is `Some`, returns it directly (converted to an owned
/// `PathBuf`).  Otherwise delegates to [`which::which`].
fn resolve_erlc_path(opt_path: Option<&Path>) -> Result<PathBuf, CodegenError> {
    opt_path.map_or_else(
        || {
            which::which("erlc").map_err(|_| CodegenError::ErlcNotFound {
                searched_paths: vec![],
            })
        },
        |p| Ok(p.to_path_buf()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_extracts_major() {
        let otp26 = "Erlang/OTP 26 [erts-14.0.2] [source] [64-bit]";
        assert_eq!(parse_version(otp26), Some(26));

        let otp27 = "Erlang/OTP 27 [erts-15.0] [source]";
        assert_eq!(parse_version(otp27), Some(27));
    }

    #[test]
    fn parse_version_returns_none_for_garbage() {
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version(""), None);
    }
}
