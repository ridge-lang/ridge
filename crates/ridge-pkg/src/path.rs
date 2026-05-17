//! Path dependency resolver for `ridge-pkg`.
//!
//! A `path = "../foo"` dependency is resolved by joining the given path
//! relative to the project's manifest directory, verifying that
//! `<resolved>/ridge.toml` exists, and parsing it via
//! `ridge_manifest::parse_project`.
//!
//! Path traversal via `..` is **permitted** in 0.1.0 — the resolver is
//! permissive per §3.9 ("allowed; keep permissive for 0.1.0 but document").
//! Containment enforcement is deferred to 0.2.0.

use std::path::{Path, PathBuf};

use ridge_manifest::{parse_project, ProjectManifest};

use crate::error::PkgError;

// ── Public entry point ────────────────────────────────────────────────────────

/// Resolve a `path = "…"` dependency relative to `project_manifest_dir`.
///
/// Steps:
/// 1. Join `dep_path` onto `project_manifest_dir` (§1.3 #5: `Path::join` only).
/// 2. Verify `<resolved>/ridge.toml` exists → `P101` if missing.
/// 3. Parse the manifest → `P102` on parse error.
///
/// Returns `(source_root, manifest)` where `source_root` is the directory
/// containing `ridge.toml`.
///
/// # Errors
///
/// - `P101 PkgPathManifestMissing` — resolved path has no `ridge.toml`.
/// - `P102 PkgManifestParseFailed` — `ridge.toml` exists but is invalid.
pub fn resolve_path_dep(
    project_manifest_dir: &Path,
    dep_path: &Path,
) -> Result<(PathBuf, ProjectManifest), PkgError> {
    // 1. Resolve path.  We use `join` only — no string concat (§1.3 #5).
    let resolved = project_manifest_dir.join(dep_path);

    // 2. Canonicalise for a stable path in ResolvedDep.  On Windows, this
    //    produces a UNC path; that is fine for our purposes.
    let canonical = resolved
        .canonicalize()
        .map_err(|_| PkgError::PkgPathManifestMissing {
            path: resolved.clone(),
        })?;

    // 3. Check that ridge.toml exists inside the canonical dir.
    let manifest_path = canonical.join("ridge.toml");
    if !manifest_path.exists() {
        return Err(PkgError::PkgPathManifestMissing {
            path: manifest_path,
        });
    }

    // 4. Read and parse the manifest.
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

    Ok((canonical, manifest))
}
