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
    CheckOptions, CompileOptions, EmitArtefacts, Profile, WorkspaceSnapshot,
};
use ridge_manifest::find_workspace_root;

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

    let root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;
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
fn execute_apply(args: &ReloadArgs, cwd: &Path) -> Result<(), CliError> {
    let node = args.node.clone().unwrap_or_default();
    let root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;
    let profile = if args.release {
        Profile::Release
    } else {
        Profile::Debug
    };
    let profile_name = profile.dir_name();

    let erl_path = which::which("erl").map_err(|_| {
        eprintln!("error: C004 ErlangNotFound: erl not found on PATH");
        CliError::NoWorkspaceRoot
    })?;

    let (old_snapshot, base_vsn) = read_running_snapshot(&root, profile_name)?;
    let (manifest_path, manifest, beams) = compile_and_plan(&root, profile_name, &old_snapshot)?;
    if manifest.modules.is_empty() {
        println!("nothing to reload: the node already runs this code.");
        return Ok(());
    }

    let cookie = args.cookie.clone().unwrap_or_else(default_cookie);
    let out = probe_apply_bundle(
        &erl_path,
        &node,
        &cookie,
        &manifest_path,
        &base_vsn,
        &beams,
        args.timeout,
        args.purge_after,
        args.seed,
    )
    .map_err(|msg| {
        eprintln!("error: could not reach the node: {msg}");
        CliError::AlreadyReported
    })?;
    let Some(json_line) = out
        .lines()
        .find_map(|l| l.strip_prefix("RIDGE_RELOAD_JSON "))
    else {
        let detail = out
            .lines()
            .find_map(|l| l.strip_prefix("RIDGE_RELOAD_ERR "))
            .unwrap_or_else(|| out.trim());
        eprintln!("reload failed at the node: {detail}");
        eprintln!("node is unchanged unless the failure was reported after the load step.");
        return Err(CliError::AlreadyReported);
    };

    let report: serde_json::Value = serde_json::from_str(json_line).map_err(|e| {
        eprintln!("error: the node returned a malformed report: {e}");
        CliError::AlreadyReported
    })?;
    println!("{}", summary_line(&report));

    if let Some(json_path) = &args.json {
        let pretty = serde_json::to_string_pretty(&report).unwrap_or_default();
        if json_path.as_os_str().is_empty() {
            println!("{pretty}");
        } else if let Err(e) = std::fs::write(json_path, &pretty) {
            eprintln!(
                "error: could not write the JSON report to {}: {e}",
                json_path.display()
            );
            return Err(CliError::AlreadyReported);
        }
    }
    Ok(())
}

/// Read the snapshot of the build the node is running. Must happen BEFORE
/// compiling, because the compile replaces the file on disk.
fn read_running_snapshot(
    root: &Path,
    profile_name: &str,
) -> Result<(WorkspaceSnapshot, String), CliError> {
    let snap_path = snapshot_path_for(root, profile_name);
    let text = std::fs::read_to_string(&snap_path).map_err(|e| {
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
    Ok((snapshot, vsn))
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
    if !artefacts.diagnostics.is_empty() {
        crate::render::render_diagnostics(&artefacts.diagnostics, &artefacts.sources);
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
/// parses (`RIDGE_RELOAD_JSON` on success, `RIDGE_RELOAD_ERR` on any
/// node-side refusal or failure).
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
