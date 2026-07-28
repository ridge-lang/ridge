//! Glue between the compiler pipeline and `ridge-reload`: builds snapshots
//! from real workspaces and answers "would this edit hot-reload?"

use std::path::{Path, PathBuf};

use ridge_diagnostics::Severity;
use ridge_reload::check::CheckReport;
use ridge_reload::snapshot::WorkspaceSnapshot;

use crate::check::check_workspace_typed;
use crate::error::CheckError;
use crate::options::CheckOptions;

/// Where a profile's snapshot lives inside the workspace.
#[must_use]
pub fn snapshot_path_for(root: &Path, profile: &str) -> PathBuf {
    root.join("target").join("ridge").join(profile).join("reload-snapshot.json")
}

/// Errors a `reload --check` run can produce.
#[derive(Debug, thiserror::Error)]
pub enum ReloadCheckError {
    /// No snapshot file at the expected path — the workspace was never built.
    #[error("no build snapshot found at {0}; run `ridge build` first")]
    MissingSnapshot(PathBuf),
    /// The snapshot file exists but its format version is not supported.
    #[error("snapshot at {0} uses an unsupported format version")]
    UnsupportedFormat(PathBuf),
    /// The current source does not compile cleanly.
    #[error("current source does not compile cleanly; fix diagnostics before checking a reload")]
    SourceHasErrors,
    /// The check pipeline itself failed fatally.
    #[error(transparent)]
    Check(#[from] CheckError),
    /// Snapshot serialisation/deserialisation failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Re-checks the current source, diffs against the stored snapshot, and
/// classifies every change. Returns the verdict report.
///
/// ## Errors
///
/// Returns [`ReloadCheckError`] when the snapshot is missing or stale-format,
/// when the current source has error diagnostics, or when the check pipeline
/// fails fatally.
pub fn reload_check(
    options: CheckOptions,
    snapshot_path: &Path,
) -> Result<CheckReport, ReloadCheckError> {
    let raw = std::fs::read_to_string(snapshot_path)
        .map_err(|_| ReloadCheckError::MissingSnapshot(snapshot_path.to_path_buf()))?;
    let old: WorkspaceSnapshot = serde_json::from_str(&raw)?;
    if old.format != ridge_reload::snapshot::SNAPSHOT_FORMAT {
        return Err(ReloadCheckError::UnsupportedFormat(snapshot_path.to_path_buf()));
    }
    let artefacts = check_workspace_typed(options)?;
    if artefacts.diagnostics.iter().any(|d| matches!(d.severity, Severity::Error)) {
        return Err(ReloadCheckError::SourceHasErrors);
    }
    let new = ridge_reload::snapshot::extract_snapshot(&artefacts.resolved, &artefacts.typed);
    Ok(ridge_reload::check::check(&ridge_reload::diff::diff(&old, &new)))
}
