//! `ridge reload` — source-level reload tooling.
//!
//! ## Surface
//!
//! ```text
//! ridge reload --check [--snapshot <path>]
//! ridge reload --node <name@host> [--cookie <c>] [--seed] [--release]
//!              [--timeout <ms>] [--purge-after <s>] [--json [<path>]]
//! ```
//!
//! `--check` is a dry-run verdict that never touches any running system.
//! `--node` applies the upgrade to a running node: it compiles the current
//! source, plans against the stored snapshot, ships the manifest and the
//! new `.beam` blobs through a short-lived probe node, and lets the target
//! node's loader suspend, migrate, and resume the affected actors.

use std::path::{Path, PathBuf};

use clap::Args as ClapArgs;
use ridge_driver::reload::{reload_check, snapshot_path_for, CheckReport, Verdict};
use ridge_driver::{
    collect_bundle_beams, compile_workspace, manifest_path_for, plan_reload, snapshot_vsn,
    CheckOptions, CompileOptions, EmitArtefacts, Profile, ToolchainError, WorkspaceSnapshot,
};

use crate::error::CliError;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Source-level reload tooling.
#[derive(Debug, ClapArgs)]
pub struct ReloadArgs {
    /// Dry-run: diff against the last build and report compatibility.
    #[arg(long)]
    pub check: bool,
    /// Override the snapshot path (default: target/ridge/<profile>/reload-snapshot.json).
    #[arg(long, value_name = "PATH")]
    pub snapshot: Option<PathBuf>,
    /// Apply the upgrade to a running node (e.g. app@host or app@127.0.0.1).
    #[arg(long, value_name = "NAME@HOST")]
    pub node: Option<String>,
    /// Erlang distribution cookie of the target node.
    #[arg(long, value_name = "COOKIE")]
    pub cookie: Option<String>,
    /// Seed the node's version marker when it has none (first reload of a
    /// node that was not booted with one). Off by default: without a
    /// marker the base-version gate answers "unknown" and the reload is
    /// refused loudly.
    #[arg(long)]
    pub seed: bool,
    /// Use the release profile's snapshot and artefacts.
    #[arg(long)]
    pub release: bool,
    /// rpc timeout for the apply call, in milliseconds.
    #[arg(long, value_name = "MS", default_value = "30000")]
    pub timeout: u64,
    /// Seconds of quiescence before the old code is purged (0 disables).
    #[arg(long, value_name = "SECS", default_value = "60")]
    pub purge_after: u64,
    /// Write the full JSON report to PATH (stdout when no path is given).
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
    pub json: Option<PathBuf>,
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge reload`.
///
/// Exit status: for `--check`, success only when the report is reloadable
/// and no scaffold still has holes; for `--node`, success only when the
/// node accepted and applied the upgrade.
///
/// # Errors
///
/// Returns a [`CliError`] when the arguments are inconsistent, the
/// workspace root cannot be found, the snapshot is missing or stale, the
/// current source does not compile cleanly, the plan rejects the edit, or
/// the node refuses or fails the upgrade.
pub fn execute(args: &ReloadArgs, cwd: &Path) -> Result<(), CliError> {
    if args.node.is_some() {
        return execute_apply(args, cwd);
    }
    if !args.check {
        eprintln!("error: expected `--check` or `--node <name@host>`");
        return Err(CliError::AlreadyReported);
    }

    let root = crate::cmd::workspace_root_for(cwd)?;
    let snapshot = args
        .snapshot
        .clone()
        .unwrap_or_else(|| snapshot_path_for(&root, Profile::Debug.dir_name()));

    let report = match reload_check(CheckOptions::new(root), &snapshot) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(CliError::AlreadyReported);
        }
    };

    print_reload_rejection(&report);

    let ok = report.is_reloadable() && !report.has_holes();
    if ok {
        Ok(())
    } else {
        Err(CliError::AlreadyReported)
    }
}

// ── ridge reload --node (production transport) ────────────────────────────────

/// Apply the current source as a hot upgrade to a running node.
///
/// The compile advances the on-disk snapshot to the NEW build, but the
/// node can change even if a later migration or the RPC fails. Restore the
/// pre-compile snapshot only when the node is known
/// to have rejected the code. An uncertain outcome invalidates the snapshot:
/// another reload must not plan against a potentially mixed running build.
fn execute_apply(args: &ReloadArgs, cwd: &Path) -> Result<(), CliError> {
    let root = crate::cmd::workspace_root_for(cwd)?;
    let profile = if args.release {
        Profile::Release
    } else {
        Profile::Debug
    };
    let profile_name = profile.dir_name();

    let erl_path = which::which("erl").map_err(|_| ToolchainError::erl_not_found())?;

    let snap_path = snapshot_path_for(&root, profile_name);
    let (old_snapshot, base_vsn, raw_snapshot) = read_running_snapshot(&snap_path)?;
    let json_line = match plan_and_apply(
        args,
        &erl_path,
        &root,
        profile_name,
        &old_snapshot,
        &base_vsn,
    ) {
        Ok(Some(line)) => line,
        Ok(None) => return Ok(()),
        Err(failure) => {
            if let Err(io) = recover_reload_snapshot(&snap_path, &raw_snapshot, &failure) {
                eprintln!(
                    "warning: could not recover the reload snapshot at {}: {io}",
                    snap_path.display()
                );
            }
            return Err(match failure {
                ReloadFailure::BeforeApply(e) => e,
                ReloadFailure::Uncertain => CliError::AlreadyReported,
            });
        }
    };

    // From here the node DID advance: the new snapshot on disk is correct,
    // and report-handling problems are warnings, not reload failures.
    let report: serde_json::Value = match serde_json::from_str(&json_line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: the node applied the upgrade but returned a malformed report: {e}");
            return Ok(());
        }
    };
    println!("{}", summary_line(&report));

    if let Some(json_path) = &args.json {
        let pretty = serde_json::to_string_pretty(&report).unwrap_or_default();
        if json_path.as_os_str().is_empty() {
            println!("{pretty}");
        } else if let Err(e) = std::fs::write(json_path, &pretty) {
            eprintln!(
                "warning: reload applied, but could not write the JSON report to {}: {e}",
                json_path.display()
            );
        }
    }
    Ok(())
}

/// Compile, plan, and apply through the probe. `Ok(None)` means the node
/// already runs this code (nothing to reload); `Ok(Some(_))` carries the
/// node's JSON report line. Errors distinguish a known rejection from an
/// uncertain outcome after RPC or a failure following a code swap.
fn plan_and_apply(
    args: &ReloadArgs,
    erl_path: &Path,
    root: &Path,
    profile_name: &str,
    old_snapshot: &WorkspaceSnapshot,
    base_vsn: &str,
) -> Result<Option<String>, ReloadFailure> {
    let node = args.node.clone().unwrap_or_default();
    let (manifest_path, manifest, beams) = compile_and_plan(root, profile_name, old_snapshot)?;
    if manifest.modules.is_empty() {
        println!("nothing to reload: the node already runs this code.");
        return Ok(None);
    }

    let cookie = args.cookie.clone().unwrap_or_else(default_cookie);
    let out = probe_apply_bundle(
        erl_path,
        &node,
        &cookie,
        &manifest_path,
        base_vsn,
        &beams,
        args.timeout,
        args.purge_after,
        args.seed,
    )
    .map_err(|msg| {
        eprintln!("error: reload probe failed: {msg}");
        report_uncertain_reload();
        ReloadFailure::Uncertain
    })?;
    parse_apply_result(&out)
}

/// Whether the pre-compile snapshot still describes the running node.
enum ReloadFailure {
    /// Compilation, planning, or a node rejection before any code was loaded.
    BeforeApply(CliError),
    /// The node may have changed; no local snapshot is safe for another plan.
    Uncertain,
}

impl From<CliError> for ReloadFailure {
    fn from(error: CliError) -> Self {
        Self::BeforeApply(error)
    }
}

/// Restore a known baseline or remove one whose correspondence is unknown.
fn recover_reload_snapshot(
    path: &Path,
    previous: &str,
    failure: &ReloadFailure,
) -> std::io::Result<()> {
    match failure {
        ReloadFailure::BeforeApply(_) => std::fs::write(path, previous),
        ReloadFailure::Uncertain => match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        },
    }
}

/// Explain recovery without claiming a failed RPC left the node unchanged.
fn report_uncertain_reload() {
    eprintln!("node code may have changed; the local reload snapshot is invalidated.");
    eprintln!("rebuild and restart the node before attempting another reload.");
}

/// Interpret probe markers without inferring a phase from exception text.
fn parse_apply_result(out: &str) -> Result<Option<String>, ReloadFailure> {
    let Some(json_line) = out
        .lines()
        .find_map(|l| l.strip_prefix("RIDGE_RELOAD_JSON "))
    else {
        if let Some(detail) = out
            .lines()
            .find_map(|l| l.strip_prefix("RIDGE_RELOAD_REJECTED "))
        {
            eprintln!("reload rejected before code was replaced: {detail}");
            return Err(ReloadFailure::BeforeApply(CliError::AlreadyReported));
        }
        let detail = out
            .lines()
            .find_map(|l| l.strip_prefix("RIDGE_RELOAD_ERR "))
            .unwrap_or_else(|| out.trim());
        eprintln!("reload failed at the node: {detail}");
        report_uncertain_reload();
        return Err(ReloadFailure::Uncertain);
    };
    Ok(Some(json_line.to_owned()))
}

/// Read the snapshot of the build the node is running, with its raw text
/// for the failure-path restore. Must happen BEFORE compiling, because
/// the compile replaces the file on disk.
fn read_running_snapshot(
    snap_path: &Path,
) -> Result<(WorkspaceSnapshot, String, String), CliError> {
    let text = std::fs::read_to_string(snap_path).map_err(|e| {
        eprintln!(
            "error: no build snapshot found at {}; run `ridge build` first ({e})",
            snap_path.display()
        );
        CliError::AlreadyReported
    })?;
    let snapshot: WorkspaceSnapshot = serde_json::from_str(&text).map_err(|e| {
        eprintln!(
            "error: cannot parse reload snapshot {}: {e}",
            snap_path.display()
        );
        CliError::AlreadyReported
    })?;
    let vsn = snapshot_vsn(&snapshot);
    Ok((snapshot, vsn, text))
}

/// What [`compile_and_plan`] returns: the manifest path, the manifest, and
/// the resolved bundle beams `(module name, .beam path)`.
type ApplyPlan = (
    PathBuf,
    ridge_driver::UpgradeManifest,
    Vec<(String, PathBuf)>,
);

/// Compile the current source, plan the upgrade against the running
/// snapshot, and collect the bundle's beam artefacts. A rejected plan
/// prints the verdicts and fails; an empty plan is a clean no-op error
/// the caller turns into a success message.
fn compile_and_plan(
    root: &Path,
    profile_name: &str,
    old_snapshot: &WorkspaceSnapshot,
) -> Result<ApplyPlan, CliError> {
    let artefacts =
        compile_workspace(CompileOptions::new(root.to_path_buf()).with_emit(EmitArtefacts::Beam))
            .map_err(|e| {
            eprintln!("error: {e}");
            CliError::AlreadyReported
        })?;
    if crate::render::report_diagnostics(&artefacts.diagnostics, &artefacts.sources, false).fatal()
    {
        return Err(CliError::AlreadyReported);
    }
    let Some(beam_dir) = artefacts.beam_files.iter().find_map(|p| p.parent()) else {
        eprintln!("error: the build produced no beam artefacts");
        return Err(CliError::AlreadyReported);
    };

    let manifest_path = manifest_path_for(root, profile_name);
    let plan = plan_reload(
        old_snapshot,
        CheckOptions::new(root.to_path_buf()),
        &manifest_path,
    )
    .map_err(|e| {
        eprintln!("error: reload planning failed: {e}");
        CliError::AlreadyReported
    })?;
    let Some(manifest) = plan.manifest else {
        print_reload_rejection(&plan.report);
        eprintln!("reload rejected — the node keeps running its current code.");
        return Err(CliError::AlreadyReported);
    };
    let beams = collect_bundle_beams(&manifest, beam_dir).map_err(|e| {
        eprintln!("error: {e}");
        CliError::AlreadyReported
    })?;
    Ok((manifest_path, manifest, beams))
}

/// The one-line human summary (the dev loop's report shape, extended with
/// restarts and the purge schedule).
fn summary_line(report: &serde_json::Value) -> String {
    let get = |key: &str| {
        report
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let (modules, migrated, restarted, messages, ms) = (
        get("modules_loaded"),
        get("actors_migrated"),
        get("actors_restarted"),
        get("messages_migrated"),
        get("duration_ms"),
    );
    let purge = match report.get("purge") {
        Some(p) if p.get("scheduled").and_then(serde_json::Value::as_bool) == Some(true) => {
            let after = p
                .get("after_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            format!("; old code purges in {}s", after / 1000)
        }
        _ => String::new(),
    };
    format!(
        "reloaded {modules} modules, migrated {migrated} actors (+{restarted} restarted), \
         {messages} in-flight messages in {ms}ms{purge}"
    )
}

/// A node-local cookie when none is given: matches what `ridge run --reload`
/// generates only by convention — production nodes should pass `--cookie`.
fn default_cookie() -> String {
    "ridge".to_owned()
}

/// Run the probe node: read the manifest and the beam blobs from disk,
/// apply the bundle on the target node over rpc, print markers the caller
/// parses (`RIDGE_RELOAD_JSON` on success, `RIDGE_RELOAD_REJECTED` when
/// code is unchanged, `RIDGE_RELOAD_ERR` when the outcome is uncertain).
#[allow(clippy::too_many_arguments)]
fn probe_apply_bundle(
    erl_path: &Path,
    node: &str,
    cookie: &str,
    manifest_path: &Path,
    base_vsn: &str,
    beams: &[(String, PathBuf)],
    timeout_ms: u64,
    purge_after_s: u64,
    seed: bool,
) -> Result<String, String> {
    let manifest_fwd = manifest_path.to_string_lossy().replace('\\', "/");
    let beam_entries = beams
        .iter()
        .map(|(name, path)| {
            format!(
                "{{<<\"{name}\">>, element(2, file:read_file(\"{}\"))}}",
                path.to_string_lossy().replace('\\', "/")
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let seed_eval = if seed {
        format!(
            "case rpc:call('{node}', ridge_loader, current_version, [], {timeout_ms}) of \
                undefined -> _ = rpc:call('{node}', persistent_term, put, [ridge_loader_vsn, <<\"{base_vsn}\">>], {timeout_ms}); \
                _ -> ok \
            end,\n"
        )
    } else {
        String::new()
    };
    let purge_ms = purge_after_s * 1000;
    let eval = format!(
        "rpc:call('{node}', ridge_rt, set_migrate_report, [structured], {timeout_ms}),\n\
         {seed_eval}\
         {{ok, MBin}} = file:read_file(\"{manifest_fwd}\"),\n\
         Bins = [{beam_entries}],\n\
         case rpc:call('{node}', ridge_loader, apply_bundle,\n\
         \x20     [MBin, Bins, #{{base_vsn => <<\"{base_vsn}\">>, purge_after_ms => {purge_ms}}}], {timeout_ms}) of\n\
         \x20   {{ok, Rep}} ->\n\
         \x20       Safe = Rep#{{restarts => [#{{module => M, reason => iolist_to_binary(io_lib:format(\"~p\", [Why]))}}\n\
         \x20                             || #{{module := M, reason := Why}} <- maps:get(restarts, Rep, [])]}},\n\
         \x20       io:format(\"RIDGE_RELOAD_JSON ~s~n\", [json:encode(Safe)]);\n\
         \x20   {{error, {{reload_rejected, _, _}}}} = Rejected ->\n\
         \x20       io:format(\"RIDGE_RELOAD_REJECTED ~0tp~n\", [Rejected]);\n\
         \x20   {{error, {{base_version_mismatch, _, _}}}} = Rejected ->\n\
         \x20       io:format(\"RIDGE_RELOAD_REJECTED ~0tp~n\", [Rejected]);\n\
         \x20   Err -> io:format(\"RIDGE_RELOAD_ERR ~p~n\", [Err])\n\
         end."
    );
    let output = std::process::Command::new(erl_path)
        .arg("-name")
        .arg(format!("ridge_reload_{}@127.0.0.1", std::process::id()))
        .arg("-setcookie")
        .arg(cookie)
        .arg("-noshell")
        .arg("-eval")
        .arg(eval)
        .arg("-s")
        .arg("init")
        .arg("stop")
        .output()
        .map_err(|e| format!("failed to spawn the probe node: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Print the per-symbol verdicts and the summary line for a check report.
///
/// Shared by `ridge reload --check` and the `ridge run --reload` rejection
/// path.
pub(crate) fn print_reload_rejection(report: &CheckReport) {
    let (mut compatible, mut auto, mut migrate, mut incompatible) = (0u32, 0u32, 0u32, 0u32);
    for v in &report.verdicts {
        // Module-level rows repeat the FQN as the symbol; print it once.
        let target = if v.symbol == v.module {
            v.module.clone()
        } else {
            format!("{}.{}", v.module, v.symbol)
        };
        match &v.verdict {
            Verdict::Compatible => {
                compatible += 1;
                println!("compatible      {target}");
            }
            Verdict::AutoMigrate { note } => {
                auto += 1;
                println!("auto-migrate    {target}: {note}");
            }
            Verdict::CompatibleViaMigration { note } => {
                auto += 1;
                println!("migrate-hook    {target}: {note}");
            }
            Verdict::RequiresMigration { scaffold, .. } => {
                migrate += 1;
                println!(
                    "needs-migration {target} — apply this scaffold and re-check:\n{scaffold}"
                );
            }
            Verdict::Incompatible { reason } => {
                incompatible += 1;
                println!("incompatible    {target}: {reason}");
            }
        }
    }

    let ok = report.is_reloadable() && !report.has_holes();
    println!(
        "{}: {compatible} compatible, {auto} auto/hook-migrated, {migrate} need migration, {incompatible} incompatible",
        if ok { "reloadable" } else { "not reloadable" },
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn rejected_reload_restores_the_running_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.json");
        std::fs::write(&path, "new-build").unwrap();
        let result = parse_apply_result(
            "RIDGE_RELOAD_REJECTED {error,{reload_rejected,preflight,{missing_blob,mod_b}}}\n",
        );
        let Err(failure @ ReloadFailure::BeforeApply(_)) = result else {
            panic!("preflight must preserve the previous baseline");
        };
        recover_reload_snapshot(&path, "running-build", &failure).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "running-build");
    }

    #[test]
    fn uncertain_reload_invalidates_the_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.json");
        for output in [
            "RIDGE_RELOAD_ERR {error,{upgrade_failed,error,bad_actor_metadata,[]}}\n",
            "RIDGE_RELOAD_ERR {badrpc,timeout}\n",
            "probe exited without reporting an outcome\n",
        ] {
            std::fs::write(&path, "new-build").unwrap();
            let Err(failure @ ReloadFailure::Uncertain) = parse_apply_result(output) else {
                panic!("unknown node state cannot retain a reload baseline");
            };
            recover_reload_snapshot(&path, "running-build", &failure).unwrap();
            assert!(!path.exists());
        }
    }

    #[test]
    fn successful_reload_keeps_the_new_report() {
        assert!(matches!(
            parse_apply_result("RIDGE_RELOAD_JSON {\"modules_loaded\":2}\n"),
            Ok(Some(report)) if report == "{\"modules_loaded\":2}"
        ));
    }
}
