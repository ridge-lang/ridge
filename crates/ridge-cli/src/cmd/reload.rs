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

use clap::{builder::TypedValueParser, Args as ClapArgs};
use ridge_codegen_erl::runtime::{MAX_RELOAD_PURGE_DELAY_MS, MAX_RELOAD_RPC_TIMEOUT_MS};
use ridge_driver::reload::{
    reload_check, snapshot_path_for, CheckReport, ReloadCheckError, Verdict,
};
use ridge_driver::{
    collect_bundle_beams, compile_workspace, manifest_path_for, plan_reload, snapshot_vsn,
    CheckOptions, CompileOptions, EmitArtefacts, Profile, ToolchainError, WorkspaceSnapshot,
};

use crate::error::CliError;

#[path = "reload_receipts.rs"]
mod receipts;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Source-level reload tooling.
#[derive(Debug, ClapArgs)]
#[command(group(clap::ArgGroup::new("reload_mode").required(true).args(["check", "node"])),
    after_help = "Apply success confirms the completed upgrade operation, not replacement actor availability or rollback of application effects. Failed actors need application-configured supervision. A timeout does not cancel admitted work; repeat the same command to query a pending operation before compiling further edits. JSON actors_restarted is a legacy fallback count; actor_fallback describes termination observations.")]
pub struct ReloadArgs {
    /// Dry-run: diff against the last build and report compatibility.
    #[arg(long, conflicts_with_all = ["cookie", "seed", "timeout", "purge_after", "json"])]
    pub check: bool,
    /// Override the snapshot path (default: target/ridge/<profile>/reload-snapshot.json).
    #[arg(long, value_name = "PATH", requires = "check", conflicts_with = "node")]
    pub snapshot: Option<PathBuf>,
    /// Apply the upgrade to a running node (e.g. app@host or app@127.0.0.1).
    #[arg(long, value_name = "NAME@HOST")]
    pub node: Option<String>,
    /// Distribution cookie for trusted peers; it does not encrypt the transport.
    #[arg(long, value_name = "COOKIE", requires = "node")]
    pub cookie: Option<String>,
    /// Seed the node's version marker when it has none (first reload of a
    /// node that was not booted with one). Off by default: without a
    /// marker the base-version gate answers "unknown" and the reload is
    /// refused loudly. Requires an updated target loader; the marker is
    /// published after confirmed loading and safe completion or forward
    /// recovery; a lost client result does not prove the marker unchanged.
    #[arg(long, requires = "node")]
    pub seed: bool,
    /// Use the release profile's snapshot and artefacts.
    #[arg(long)]
    pub release: bool,
    /// RPC timeout in milliseconds (0 is nonblocking; maximum 4294967295).
    #[arg(long, value_name = "MS", default_value = "30000", requires = "node",
        value_parser = clap::value_parser!(u64).range(..=MAX_RELOAD_RPC_TIMEOUT_MS))]
    pub timeout: u64,
    /// Seconds before safe cleanup of old code (0 disables; maximum 18446744073709551).
    #[arg(long, value_name = "SECS", default_value = "60", requires = "node",
        value_parser = clap::value_parser!(u64).range(..=MAX_RELOAD_PURGE_DELAY_MS / 1000))]
    pub purge_after: u64,
    /// Write the full JSON report to PATH (stdout when no path is given).
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "", requires = "node",
        value_parser = clap::builder::OsStringValueParser::new().map(PathBuf::from))]
    pub json: Option<PathBuf>,
}

impl ReloadArgs {
    const fn profile(&self) -> Profile {
        if self.release {
            Profile::Release
        } else {
            Profile::Debug
        }
    }

    // Direct library callers bypass clap, but must not bypass the mode boundary.
    const fn validate_mode(&self) -> Result<(), &'static str> {
        if self.check == self.node.is_some() {
            return Err("expected exactly one of --check or --node <name@host>");
        }
        if self.snapshot.is_some() && !self.check {
            return Err("--snapshot requires --check");
        }
        if self.node.is_none()
            && (self.cookie.is_some()
                || self.seed
                || self.json.is_some()
                || self.timeout != 30000
                || self.purge_after != 60)
        {
            return Err("--cookie, --seed, --json, --timeout and --purge-after require --node");
        }
        Ok(())
    }

    // Library callers can construct or mutate ReloadArgs without Clap.
    fn validate_timers(&self) -> Result<(), String> {
        if self.timeout > MAX_RELOAD_RPC_TIMEOUT_MS {
            return Err(format!(
                "--timeout must be between 0 and {MAX_RELOAD_RPC_TIMEOUT_MS} milliseconds; got {}",
                self.timeout
            ));
        }
        if self.purge_after > MAX_RELOAD_PURGE_DELAY_MS / 1000 {
            return Err(format!(
                "--purge-after must be between 0 and {} seconds (0 disables cleanup); got {}",
                MAX_RELOAD_PURGE_DELAY_MS / 1000,
                self.purge_after
            ));
        }
        Ok(())
    }
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
    if let Err(message) = args.validate_mode() {
        eprintln!("error: {message}");
        return Err(CliError::AlreadyReported);
    }
    if let Err(message) = args.validate_timers() {
        eprintln!("error: {message}");
        return Err(CliError::AlreadyReported);
    }
    let profile = args.profile();
    if args.node.is_some() {
        return execute_apply(args, cwd, profile);
    }

    let root = crate::cmd::workspace_root_for(cwd)?;
    let snapshot = args
        .snapshot
        .clone()
        .unwrap_or_else(|| snapshot_path_for(&root, profile.dir_name()));

    let report = match reload_check(CheckOptions::new(root), &snapshot) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            if profile == Profile::Release
                && args.snapshot.is_none()
                && matches!(e, ReloadCheckError::MissingSnapshot(_))
            {
                eprintln!("help: run `ridge build --release` to create the release baseline.");
            }
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
fn execute_apply(args: &ReloadArgs, cwd: &Path, profile: Profile) -> Result<(), CliError> {
    let root = crate::cmd::workspace_root_for(cwd)?;
    let profile_name = profile.dir_name();

    let erl_path = which::which("erl").map_err(|_| ToolchainError::erl_not_found())?;

    let snap_path = snapshot_path_for(&root, profile_name);
    let _lease = receipts::lock(&snap_path)?;
    if let Some(line) = receipts::recover(&erl_path, args, &snap_path)? {
        println!("recovered the previous reload; run reload again to apply further source edits.");
        print_apply_report(args, &line);
        return Ok(());
    }
    let (old_snapshot, base_vsn, raw_snapshot) = read_running_snapshot(&snap_path, profile)?;
    // Check before compilation and snapshot recovery: a local target can share
    // output files and lazily load them even when a later RPC would reject.
    let incarnation = receipts::protocol(&erl_path, args)?;
    let json_line = match plan_and_apply(
        args,
        &erl_path,
        &root,
        profile,
        &old_snapshot,
        &base_vsn,
        &raw_snapshot,
        &incarnation,
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

    print_apply_report(args, &json_line);
    Ok(())
}

fn print_apply_report(args: &ReloadArgs, json_line: &str) {
    // From here the node DID advance: the new snapshot on disk is correct,
    // and report-handling problems are warnings, not reload failures.
    let report: serde_json::Value = match serde_json::from_str(json_line) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: the node applied the upgrade but returned a malformed report: {e}");
            return;
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
}

/// Compile, plan, and apply through the probe. `Ok(None)` means the node
/// already runs this code (nothing to reload); `Ok(Some(_))` carries the
/// node's JSON report line. Errors distinguish a known rejection from an
/// uncertain outcome after RPC or a failure following a code swap.
#[allow(clippy::too_many_arguments)]
fn plan_and_apply(
    args: &ReloadArgs,
    erl_path: &Path,
    root: &Path,
    profile: Profile,
    old_snapshot: &WorkspaceSnapshot,
    base_vsn: &str,
    raw_snapshot: &str,
    incarnation: &str,
) -> Result<Option<String>, ReloadFailure> {
    let (manifest_path, manifest, beams) = compile_and_plan(root, profile, old_snapshot)?;
    if manifest.modules.is_empty() {
        println!("nothing to reload: the node already runs this code.");
        return Ok(None);
    }

    let before_apply = |message: String| {
        eprintln!("error: {message}");
        ReloadFailure::BeforeApply(CliError::AlreadyReported)
    };
    let payload = receipts::Payload::freeze(&manifest_path, &beams, base_vsn, incarnation, args)
        .map_err(|error| before_apply(error.to_string()))?;
    let receipt = payload.reserve(erl_path, args).map_err(before_apply)?;
    let snapshot = snapshot_path_for(root, profile.dir_name());
    let journal = receipts::save_pending(
        &snapshot,
        args.node.as_deref().unwrap_or_default(),
        raw_snapshot,
        &receipt,
    )
    .map_err(|error| before_apply(error.to_string()))?;
    let result = payload
        .submit(erl_path, args, &receipt)
        .unwrap_or(serde_json::Value::Null);
    receipts::finish(&snapshot, &journal, &result).map(Some)
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

/// Read the snapshot of the build the node is running, with its raw text
/// for the failure-path restore. Must happen BEFORE compiling, because
/// the compile replaces the file on disk.
fn read_running_snapshot(
    snap_path: &Path,
    profile: Profile,
) -> Result<(WorkspaceSnapshot, String, String), CliError> {
    let release_flag = if profile == Profile::Release {
        " --release"
    } else {
        ""
    };
    let text = std::fs::read_to_string(snap_path).map_err(|e| {
        eprintln!(
            "error: no build snapshot found at {}; run `ridge build{release_flag}` first ({e})",
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
    profile: Profile,
    old_snapshot: &WorkspaceSnapshot,
) -> Result<ApplyPlan, CliError> {
    let artefacts = compile_workspace(
        CompileOptions::new(root.to_path_buf())
            .with_profile(profile)
            .with_emit(EmitArtefacts::Beam),
    )
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

    let manifest_path = manifest_path_for(root, profile.dir_name());
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
/// fallback observations and the purge schedule).
pub(crate) fn summary_line(report: &serde_json::Value) -> String {
    let get = |key: &str| {
        report
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let (modules, migrated, attempts, ms) = (
        get("modules_loaded"),
        get("actors_migrated"),
        get("messages_migrated"),
        get("duration_ms"),
    );
    let fallback = fallback_summary(report);
    let purge = match report.get("purge") {
        Some(p) if p.get("scheduled").and_then(serde_json::Value::as_bool) == Some(true) => {
            let after = p
                .get("after_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if p.get("policy").and_then(serde_json::Value::as_str) == Some("soft") {
                format!("; safe old code cleanup scheduled in {}s", after / 1000)
            } else {
                format!(
                    "; old code cleanup scheduled in {}s (target policy unconfirmed)",
                    after / 1000
                )
            }
        }
        _ => String::new(),
    };
    let tracking = if report
        .get("purge")
        .and_then(|p| p.get("tracking_lost"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        "; earlier cleanup tracking was lost; inspect target diagnostics"
    } else {
        ""
    };
    format!(
        "reloaded {modules} modules, migrated {migrated} actors{fallback}, \
         {attempts} record-migration attempts (node counter) in {ms}ms{purge}{tracking}"
    )
}

fn fallback_summary(report: &serde_json::Value) -> String {
    if let Some(fallback) = report.get("actor_fallback") {
        let get = |key: &str| fallback.get(key).and_then(serde_json::Value::as_u64);
        if let (Some(affected), Some(requested), Some(terminated), Some(status)) = (
            get("affected"),
            get("termination_requested"),
            get("terminated"),
            fallback
                .get("replacement_status")
                .and_then(serde_json::Value::as_str),
        ) {
            if affected == 0 && requested == 0 && terminated == 0 && status == "not_required" {
                return " (0 actor fallbacks)".to_owned();
            }
            if affected > 0
                && requested <= affected
                && terminated <= affected
                && status == "not_observed"
            {
                let requests = if requested == 1 {
                    "request"
                } else {
                    "requests"
                };
                let fallbacks = if affected == 1 {
                    "fallback"
                } else {
                    "fallbacks"
                };
                return format!(
                    " ({affected} actor {fallbacks}: {terminated} terminated, \
                                {requested} termination {requests}; replacements not observed)"
                );
            }
        }
    } else if let Some(count) = report
        .get("actors_restarted")
        .and_then(serde_json::Value::as_u64)
    {
        return format!(" ({count} legacy fallback entries; replacement status unconfirmed)");
    }
    " (actor fallback status unavailable)".to_owned()
}

/// A node-local cookie when none is given: matches what `ridge run --reload`
/// generates only by convention — production nodes should pass `--cookie`.
fn default_cookie() -> String {
    "ridge".to_owned()
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
#[allow(clippy::panic)]
mod timer_report_tests {
    #[test]
    fn fallback_summary_does_not_claim_a_confirmed_replacement() {
        let report = serde_json::json!({"actors_restarted":9,"messages_migrated":17,
            "actor_fallback":{"affected":2,"termination_requested":1,"terminated":2,
                "replacement_status":"not_observed"}});
        let text = super::summary_line(&report);
        assert!(text.contains("2 actor fallbacks"), "{text}");
        assert!(text.contains("2 terminated"), "{text}");
        assert!(text.contains("1 termination request"), "{text}");
        assert!(text.contains("replacements not observed"), "{text}");
        assert!(!text.contains("restarted"), "{text}");
        assert!(
            text.contains("17 record-migration attempts (node counter)"),
            "{text}"
        );
        assert!(!text.contains("in-flight messages"), "{text}");
    }

    #[test]
    fn legacy_report_keeps_fallback_and_termination_uncertainty_explicit() {
        let text = super::summary_line(&serde_json::json!({"actors_restarted":3}));
        assert!(text.contains("3 legacy fallback entries"), "{text}");
        assert!(text.contains("replacement status unconfirmed"), "{text}");
        assert!(!text.contains("3 terminated"), "{text}");
        assert!(!text.contains("restarted"), "{text}");
    }

    #[test]
    fn incomplete_fallback_data_cannot_be_presented_as_zero_terminations() {
        let text = super::summary_line(&serde_json::json!({"actor_fallback":{
            "affected":1,"termination_requested":1,"replacement_status":"private_data"}}));
        assert!(text.contains("actor fallback status unavailable"), "{text}");
        assert!(!text.contains("0 terminated"), "{text}");
        assert!(!text.contains("private_data"), "{text}");
    }

    #[test]
    fn cleanup_tracking_loss_is_visible_even_when_new_cleanup_is_disabled() {
        for enabled in [false, true] {
            let mut report = serde_json::json!({"purge":{"scheduled":enabled,
                "tracking_lost":true,"after_ms":1000,"policy":"soft"}});
            assert!(super::summary_line(&report).contains("cleanup tracking was lost"));
            report["purge"]["tracking_lost"] = serde_json::Value::Bool(false);
            assert!(!super::summary_line(&report).contains("tracking was lost"));
        }
    }
}
