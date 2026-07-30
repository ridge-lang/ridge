//! Glue between the compiler pipeline and `ridge-reload`: builds snapshots
//! from real workspaces and answers "would this edit hot-reload?"

use std::collections::{HashMap, VecDeque};
use std::hash::Hasher;
use std::path::{Path, PathBuf};

use ridge_codegen_erl::module::beam_name_for_fqn;
use ridge_diagnostics::Severity;
use ridge_reload::diff::{diff, ChangeSet, ModuleChange, SymbolChange};
use ridge_reload::scaffold::{self, FieldAction};
use ridge_reload::snapshot::extract_snapshot;
use ridge_resolve::{ModuleId, ResolvedWorkspace};
use rustc_hash::FxHasher;

use crate::check::check_workspace_typed_with_history;
use crate::error::CheckError;
use crate::options::CheckOptions;

pub use ridge_reload::check::{CheckReport, Verdict};
pub use ridge_reload::snapshot::WorkspaceSnapshot;
pub use ridge_reload::VersionHistory;

/// Where a profile's snapshot lives inside the workspace.
#[must_use]
pub fn snapshot_path_for(root: &Path, profile: &str) -> PathBuf {
    root.join("target")
        .join("ridge")
        .join(profile)
        .join("reload-snapshot.json")
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
    /// Writing the upgrade manifest failed.
    #[error("could not write upgrade manifest at {path}: {message}")]
    Io {
        /// Where the manifest was meant to land.
        path: PathBuf,
        /// The underlying I/O error message.
        message: String,
    },
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
    // Older formats read through `#[serde(default)]` fields as "no history" —
    // a safe v1 start. Only a NEWER format (written by a newer compiler) is
    // rejected.
    if old.format > ridge_reload::snapshot::SNAPSHOT_FORMAT {
        return Err(ReloadCheckError::UnsupportedFormat(
            snapshot_path.to_path_buf(),
        ));
    }
    let history = ridge_reload::snapshot::history_of(&old);
    let artefacts = check_workspace_typed_with_history(options, &history)?;
    if artefacts
        .diagnostics
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
    {
        return Err(ReloadCheckError::SourceHasErrors);
    }
    let new =
        ridge_reload::snapshot::extract_snapshot(&artefacts.resolved, &artefacts.typed, Some(&old));
    Ok(ridge_reload::check::check(&ridge_reload::diff::diff(
        &old, &new,
    )))
}

// ── Reload planning (upgrade manifest) ───────────────────────────────────────

/// Version of the upgrade manifest format written by [`plan_reload`].
///
/// Format 2 adds `migrate_hook`, `old_state_hash`, and `new_state_hash` to
/// actor entries. The runtime loader never reads this field — the bump is
/// documentary; older manifests keep loading through key defaults.
pub const UPGRADE_MANIFEST_FORMAT: u32 = 2;

/// Per-actor migration instructions carried by the upgrade manifest.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActorMigration {
    /// Target module name of the actor (`<parent>_<actor_lowercase>`).
    pub beam: String,
    /// `[from, to]` state-field renames, from the rename heuristic.
    pub renames: Vec<[String; 2]>,
    /// The actor's NEW source declares `migrate` members: the loader sends
    /// `{ridge_migrate_hook, old_state_hash}` instead of `{ridge_migrate, _}`.
    #[serde(default)]
    pub migrate_hook: bool,
    /// State shape hash of the OLD build (the FROM edge key the generated
    /// `code_change` clause matches). Narrowed to the signed two's-complement
    /// representation the runtime tags carry, so the JSON value round-trips
    /// into an exact term match.
    #[serde(default)]
    pub old_state_hash: i64,
    /// State shape hash of the NEW build (report/verification value), in the
    /// same signed representation as [`old_state_hash`](Self::old_state_hash).
    #[serde(default)]
    pub new_state_hash: i64,
}

/// The upgrade manifest consumed by the runtime loader.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UpgradeManifest {
    /// Manifest format version (currently [`UPGRADE_MANIFEST_FORMAT`]).
    pub format: u32,
    /// Version of the code the node must be running for this manifest to apply.
    pub base_vsn: String,
    /// Version the node advances to after a successful upgrade.
    pub new_vsn: String,
    /// Target module names to reload (changed modules + dependents + actors).
    pub modules: Vec<String>,
    /// Actors whose state shape changed, with migration instructions.
    pub actors: Vec<ActorMigration>,
}

/// The outcome of planning a reload against the current source.
pub struct ReloadPlan {
    /// Compatibility verdicts for every detected change.
    pub report: CheckReport,
    /// The snapshot of the current source (becomes the node's snapshot after
    /// a successful reload).
    pub new_snapshot: WorkspaceSnapshot,
    /// `Some` when the edit is auto-reloadable and the manifest was written.
    pub manifest: Option<UpgradeManifest>,
}

/// Content version of a snapshot: stable hash of its canonical JSON.
#[must_use]
pub fn snapshot_vsn(snapshot: &WorkspaceSnapshot) -> String {
    let json = serde_json::to_string(snapshot).unwrap_or_default();
    let mut h = FxHasher::default();
    h.write(json.as_bytes());
    format!("{:016x}", h.finish())
}

/// Where the upgrade manifest for a profile lives (`target/ridge/<profile>/`).
#[must_use]
pub fn manifest_path_for(root: &Path, profile: &str) -> PathBuf {
    root.join("target")
        .join("ridge")
        .join(profile)
        .join("upgrade.manifest.json")
}

/// Diff the running snapshot against the current source.
///
/// When the edit is auto-reloadable, writes the upgrade manifest the runtime
/// loader consumes. The caller (CLI) owns booting, probing, and fallbacks.
///
/// ## Errors
///
/// Returns [`ReloadCheckError`] when the current source has error
/// diagnostics, when the check pipeline fails fatally, or when the manifest
/// file cannot be written.
pub fn plan_reload(
    old_snapshot: &WorkspaceSnapshot,
    options: CheckOptions,
    manifest_path: &Path,
) -> Result<ReloadPlan, ReloadCheckError> {
    let history = ridge_reload::snapshot::history_of(old_snapshot);
    let artefacts = check_workspace_typed_with_history(options, &history)?;
    if artefacts
        .diagnostics
        .iter()
        .any(|d| matches!(d.severity, Severity::Error))
    {
        return Err(ReloadCheckError::SourceHasErrors);
    }
    let new_snapshot = extract_snapshot(&artefacts.resolved, &artefacts.typed, Some(old_snapshot));
    let cs = diff(old_snapshot, &new_snapshot);
    let report = ridge_reload::check::check(&cs);
    if !report.is_reloadable() || report.has_holes() {
        return Ok(ReloadPlan {
            report,
            new_snapshot,
            manifest: None,
        });
    }
    let manifest = build_manifest(old_snapshot, &new_snapshot, &cs, &artefacts.resolved);
    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(manifest_path, &json).map_err(|e| ReloadCheckError::Io {
        path: manifest_path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(ReloadPlan {
        report,
        new_snapshot,
        manifest: Some(manifest),
    })
}

/// Changed modules + their transitive dependents, mapped to target module
/// names (source modules and their actor modules), sorted for determinism.
fn build_manifest(
    old_snapshot: &WorkspaceSnapshot,
    new_snapshot: &WorkspaceSnapshot,
    cs: &ChangeSet,
    resolved: &ResolvedWorkspace,
) -> UpgradeManifest {
    // FQN → ModuleId over the NEW workspace (added modules are in it).
    let id_of: HashMap<&str, ModuleId> = resolved
        .graph
        .modules
        .iter()
        .map(|m| (m.fully_qualified_name.as_str(), m.id))
        .collect();

    let changed_fqns: Vec<&str> = cs
        .modules
        .iter()
        .filter_map(|m| match m {
            ModuleChange::Added { fqn } | ModuleChange::Changed { fqn, .. } => Some(fqn.as_str()),
            ModuleChange::Removed { .. } => None,
        })
        .collect();

    // Reverse-dependency closure over the changed set (dependents recompile:
    // their consumers' generated code bakes in the changed module's surface).
    let mut closure: Vec<ModuleId> = Vec::new();
    let mut visited = vec![false; resolved.graph.deps.len()];
    let mut queue: VecDeque<ModuleId> = VecDeque::new();
    for fqn in &changed_fqns {
        if let Some(id) = id_of.get(fqn) {
            #[allow(clippy::cast_possible_truncation)]
            let idx = id.0 as usize;
            if !visited[idx] {
                visited[idx] = true;
                queue.push_back(*id);
            }
        }
    }
    while let Some(m) = queue.pop_front() {
        closure.push(m);
        for (a, row) in resolved.graph.deps.iter().enumerate() {
            if row.contains(&m) && !visited[a] {
                visited[a] = true;
                queue.push_back(ModuleId(u32::try_from(a).unwrap_or(u32::MAX)));
            }
        }
    }

    let mut modules: Vec<String> = Vec::new();
    for id in &closure {
        #[allow(clippy::cast_possible_truncation)]
        let fqn = &resolved.graph.modules[id.0 as usize].fully_qualified_name;
        let Ok(beam) = beam_name_for_fqn(fqn, *id) else {
            continue;
        };
        modules.push(beam.clone());
        // Actor modules fan out from source modules as `<beam>_<actor_lc>`.
        if let Some(msnap) = new_snapshot.modules.get(fqn) {
            for (name, sym) in &msnap.symbols {
                if matches!(sym, ridge_reload::snapshot::SymbolSnapshot::Actor { .. }) {
                    modules.push(format!("{beam}_{}", name.to_lowercase()));
                }
            }
        }
    }
    modules.sort();
    modules.dedup();

    let actors = actor_migrations(cs, &id_of, old_snapshot, new_snapshot);

    UpgradeManifest {
        format: UPGRADE_MANIFEST_FORMAT,
        base_vsn: snapshot_vsn(old_snapshot),
        new_vsn: snapshot_vsn(new_snapshot),
        modules,
        actors,
    }
}

/// One manifest entry per actor whose state shape changed, carrying the
/// rename instructions the loader cannot derive at runtime plus the hook
/// dispatch data (old/new state hashes) the loader cannot know at all.
fn actor_migrations(
    cs: &ChangeSet,
    id_of: &HashMap<&str, ModuleId>,
    old_snapshot: &WorkspaceSnapshot,
    new_snapshot: &WorkspaceSnapshot,
) -> Vec<ActorMigration> {
    let mut out = Vec::new();
    for m in &cs.modules {
        let ModuleChange::Changed { fqn, symbols } = m else {
            continue;
        };
        for s in symbols {
            let SymbolChange::ActorStateChanged {
                name,
                old_state,
                new_state,
                ..
            } = s
            else {
                continue;
            };
            let plan = scaffold::state_plan(old_state, new_state);
            let renames = plan
                .iter()
                .filter_map(|a| match a {
                    FieldAction::Rename { from, to } => Some([from.clone(), to.clone()]),
                    _ => None,
                })
                .collect();
            let (old_hash, new_hash, hook) = actor_hash_info(old_snapshot, new_snapshot, fqn, name);
            if let Some(id) = id_of.get(fqn.as_str()) {
                if let Ok(beam) = beam_name_for_fqn(fqn, *id) {
                    out.push(ActorMigration {
                        beam: format!("{beam}_{}", name.to_lowercase()),
                        renames,
                        migrate_hook: hook,
                        old_state_hash: old_hash,
                        new_state_hash: new_hash,
                    });
                }
            }
        }
    }
    out
}

/// `(old hash, new hash, hook?)` for one actor from the two snapshots.
///
/// A pre-history snapshot carries hash 0 — the loader still sends the hook
/// form when `hook` is set; a zero old-hash simply matches no clause and the
/// actor resumes on old state with a loud error (the existing failure
/// semantics for an unmigratable actor).
fn actor_hash_info(
    old: &WorkspaceSnapshot,
    new: &WorkspaceSnapshot,
    fqn: &str,
    name: &str,
) -> (i64, i64, bool) {
    let lookup = |snap: &WorkspaceSnapshot| -> Option<(u64, bool)> {
        match snap.modules.get(fqn)?.symbols.get(name)? {
            ridge_reload::snapshot::SymbolSnapshot::Actor {
                hash,
                migrate_edges,
                ..
            } => Some((*hash, !migrate_edges.is_empty())),
            _ => None,
        }
    };
    let (old_hash, _) = lookup(old).unwrap_or((0, false));
    let (new_hash, hook) = lookup(new).unwrap_or((0, false));
    // The runtime matches these against signed tag values: narrow to the same
    // two's-complement representation the code generator emits.
    #[allow(clippy::cast_possible_wrap)]
    let old_narrowed = old_hash as i64;
    #[allow(clippy::cast_possible_wrap)]
    let new_narrowed = new_hash as i64;
    (old_narrowed, new_narrowed, hook)
}

/// Read the profile's stored snapshot, if it exists and parses.
///
/// Shared by [`load_version_history`] and the compile pipeline's snapshot
/// write, which both consume the previous build's snapshot before it is
/// replaced.
#[must_use]
pub fn read_prev_snapshot(root: &Path, profile: &str) -> Option<WorkspaceSnapshot> {
    let text = std::fs::read_to_string(snapshot_path_for(root, profile)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Best-effort load of the previous build's version history from the
/// profile's snapshot.
///
/// Any failure — missing file, corrupt JSON, a NEWER format — yields an empty
/// history: a safe v1 start, never a crash, and never a compile failure (a
/// fresh build behaves identically).
#[must_use]
pub fn load_version_history(root: &Path, profile: &str) -> VersionHistory {
    let Some(snap) = read_prev_snapshot(root, profile) else {
        return VersionHistory::default();
    };
    if snap.format > ridge_reload::snapshot::SNAPSHOT_FORMAT {
        return VersionHistory::default();
    }
    ridge_reload::snapshot::history_of(&snap)
}
