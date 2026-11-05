//! Recover bundle outcomes without executing an operation a second time.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{CliError, ReloadArgs};

const PROBE: &str = include_str!("reload_receipts_probe.erl.txt");

pub(super) fn pending_path(snapshot: &Path) -> PathBuf {
    snapshot.with_file_name("reload-pending.json")
}

/// Held through compilation, submission and reconciliation. OS ownership is
/// released on process death; the presence of the file is not a stale lock.
pub(super) fn lock(snapshot: &Path) -> Result<std::fs::File, CliError> {
    let path = snapshot
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .with_extension("reload.lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            eprintln!("error: no build baseline directory; run `ridge build` first (`ridge build --release` for the release profile)");
        } else {
            eprintln!("error: cannot acquire the workspace reload lock; another reload may be running ({error})");
        }
        CliError::AlreadyReported
    })?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        #[allow(
            unsafe_code,
            reason = "Rust 1.88 has no safe file locking API; flock borrows this owned file descriptor without retaining it"
        )]
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            eprintln!("error: another reload holds the workspace reload lock");
            return Err(CliError::AlreadyReported);
        }
    }
    Ok(file)
}

fn probe(erl: &Path, args: &ReloadArgs, input: &Value) -> Result<Value, String> {
    let mut data = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    serde_json::to_writer(data.as_file_mut(), input).map_err(|e| e.to_string())?;
    data.as_file_mut().flush().map_err(|e| e.to_string())?;
    let output = std::process::Command::new(erl)
        .arg("-name")
        .arg(format!(
            "ridge_reload_receipt_{}@127.0.0.1",
            std::process::id()
        ))
        .arg("-setcookie")
        .arg(args.cookie.clone().unwrap_or_else(super::default_cookie))
        .args(["-noshell", "-eval", PROBE, "-s", "init", "stop", "-extra"])
        .arg(args.node.as_deref().unwrap_or_default())
        .arg(data.path())
        .output()
        .map_err(|_| "could not start the operation probe".to_owned())?;
    if !output.status.success() {
        return Err("operation probe did not complete".to_owned());
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("RIDGE_OPERATION_JSON "))
        .ok_or_else(|| "operation probe returned no receipt".to_owned())
        .and_then(|line| {
            serde_json::from_str(line).map_err(|_| "invalid operation receipt".to_owned())
        })
}

pub(super) fn protocol(erl: &Path, args: &ReloadArgs) -> Result<String, CliError> {
    let value = probe(
        erl,
        args,
        &json!({"action":"protocol", "timeout":args.timeout}),
    );
    if let Ok(value) = value {
        if value["protocol"] == 1 {
            if let Some(incarnation) = value["incarnation"].as_str().filter(|s| !s.is_empty()) {
                return Ok(incarnation.to_owned());
            }
        }
    }
    eprintln!("error: could not confirm coordinated actor startup and queryable reload operations on the target; check connectivity and rebuild and restart older nodes with this compiler before reloading");
    Err(CliError::AlreadyReported)
}

pub(super) struct Payload {
    _directory: tempfile::TempDir,
    input: Value,
}

impl Payload {
    pub(super) fn freeze(
        manifest: &Path,
        beams: &[(String, PathBuf)],
        base: &str,
        incarnation: &str,
        args: &ReloadArgs,
    ) -> std::io::Result<Self> {
        let directory = tempfile::tempdir()?;
        let copy = directory.path().join("manifest.json");
        std::fs::copy(manifest, &copy)?;
        let mut files = Vec::new();
        for (index, (name, source)) in beams.iter().enumerate() {
            let target = directory.path().join(format!("{index}.beam"));
            std::fs::copy(source, &target)?;
            files.push(json!([name, target]));
        }
        let purge_ms = args.purge_after.checked_mul(1000).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "purge timeout exceeds milliseconds range",
            )
        })?;
        Ok(Self {
            _directory: directory,
            input: json!({"manifest":copy, "beams":files, "base":base,
                          "incarnation":incarnation, "seed":args.seed,
                          "purge_ms":purge_ms, "timeout":args.timeout}),
        })
    }

    pub(super) fn reserve(&self, erl: &Path, args: &ReloadArgs) -> Result<Value, String> {
        let mut input = self.input.clone();
        input["action"] = json!("prepare");
        let receipt = probe(erl, args, &input)?;
        if receipt["status"] != "reserved"
            || receipt["incarnation"] != input["incarnation"]
            || receipt["operation"].as_str().is_none()
            || receipt["digest"].as_str().is_none()
        {
            return Err("target could not reserve a reload operation".to_owned());
        }
        Ok(receipt)
    }

    pub(super) fn submit(
        &self,
        erl: &Path,
        args: &ReloadArgs,
        receipt: &Value,
    ) -> Result<Value, String> {
        let mut input = self.input.clone();
        input["action"] = json!("apply");
        input["operation"] = receipt["operation"].clone();
        probe(erl, args, &input)
    }
}

fn snapshot_version(text: &str) -> Result<String, String> {
    let snapshot =
        serde_json::from_str(text).map_err(|_| "invalid snapshot in reload journal".to_owned())?;
    Ok(ridge_driver::snapshot_vsn(&snapshot))
}

/// Write before dispatch, without replacing an earlier unresolved operation.
pub(super) fn save_pending(
    snapshot: &Path,
    node: &str,
    previous: &str,
    receipt: &Value,
) -> std::io::Result<Value> {
    let candidate = std::fs::read_to_string(snapshot)?;
    let before_version = snapshot_version(previous).map_err(std::io::Error::other)?;
    let new_version = snapshot_version(&candidate).map_err(std::io::Error::other)?;
    let journal = json!({"format":1, "node":node, "receipt":receipt,
                         "previous":previous, "candidate":candidate,
                         "base":before_version, "new_version":new_version});
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(pending_path(snapshot))?;
    serde_json::to_writer(&mut file, &journal)?;
    file.sync_all()?;
    Ok(journal)
}

enum Resolution {
    Applied(Value),
    Rejected,
    Pending,
    Unknown,
}

fn resolution(journal: &Value, receipt: &Value) -> Resolution {
    let identity = &journal["receipt"];
    if ["incarnation", "operation", "digest"]
        .iter()
        .any(|key| identity[*key].as_str().is_none() || receipt[*key] != identity[*key])
    {
        return Resolution::Unknown;
    }
    match receipt["status"].as_str() {
        Some("reserved" | "in_progress") => Resolution::Pending,
        Some("applied")
            if receipt["code_commit"] == "confirmed"
                && receipt["actor_migration"] == "complete"
                && receipt["cleanup"] == "acknowledged"
                && receipt["recovery_fenced"] == false
                && receipt["applied_version"] == journal["new_version"]
                && receipt["current_version"] == journal["new_version"]
                && receipt["report"].is_object() =>
        {
            let mut report = receipt["report"].clone();
            report["operation"] = json!({"id":receipt["operation"],
                "incarnation":receipt["incarnation"], "digest":receipt["digest"],
                "status":"applied", "code_commit":"confirmed",
                "actor_migration":"complete", "cleanup":"acknowledged"});
            Resolution::Applied(report)
        }
        Some("rejected")
            if receipt["code_commit"] == "unconfirmed"
                && receipt["recovery_fenced"] == false
                && (receipt["current_version"] == journal["base"]
                    || receipt["current_version"].is_null()) =>
        {
            Resolution::Rejected
        }
        _ => Resolution::Unknown,
    }
}

fn replace_snapshot(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))?;
    file.write_all(text.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(super) fn finish(
    snapshot: &Path,
    journal: &Value,
    receipt: &Value,
) -> Result<String, super::ReloadFailure> {
    let (text, result) = match resolution(journal, receipt) {
        Resolution::Applied(report) => (journal["candidate"].as_str(), Ok(report.to_string())),
        Resolution::Rejected => {
            eprintln!(
                "reload rejected before code was replaced: {}; expected base <<\"{}\">>",
                receipt["rejection"].as_str().unwrap_or("target_rejection"),
                journal["base"].as_str().unwrap_or("unknown")
            );
            (
                journal["previous"].as_str(),
                Err(super::ReloadFailure::BeforeApply(CliError::AlreadyReported)),
            )
        }
        state => {
            let _ = std::fs::remove_file(snapshot);
            if matches!(state, Resolution::Pending) {
                eprintln!("reload operation is still pending; the local reload snapshot is invalidated. Run the same reload command to query it without recompiling or replaying migrations.");
            } else {
                eprintln!("reload outcome cannot be confirmed; the local reload snapshot is invalidated. The pending operation is retained at {}. Check the target; rebuild and restart if its receipt is unavailable or recovery is required.", pending_path(snapshot).display());
            }
            return Err(super::ReloadFailure::Uncertain);
        }
    };
    let restored = text
        .ok_or_else(|| std::io::Error::other("missing journal snapshot"))
        .and_then(|text| replace_snapshot(snapshot, text))
        .and_then(|()| std::fs::remove_file(pending_path(snapshot)));
    if let Err(error) = restored {
        eprintln!("error: could not reconcile the reload baseline: {error}; keep the pending operation for another query");
        return Err(super::ReloadFailure::Uncertain);
    }
    result
}

/// A pending operation is resolved before reading a baseline or compiling.
/// Even edited or now-invalid source cannot cause a second application.
pub(super) fn recover(
    erl: &Path,
    args: &ReloadArgs,
    snapshot: &Path,
) -> Result<Option<String>, CliError> {
    let text = match std::fs::read_to_string(pending_path(snapshot)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            eprintln!("error: cannot read pending reload operation: {error}");
            return Err(CliError::AlreadyReported);
        }
    };
    let journal: Value = serde_json::from_str(&text).map_err(|_| {
        eprintln!("error: invalid pending reload journal; refusing to compile or reload");
        CliError::AlreadyReported
    })?;
    if journal["format"] != 1
        || journal["node"].as_str() != args.node.as_deref()
        || journal["base"].as_str().is_none()
        || journal["new_version"].as_str().is_none()
        || journal["previous"]
            .as_str()
            .and_then(|s| snapshot_version(s).ok())
            .as_deref()
            != journal["base"].as_str()
        || journal["candidate"]
            .as_str()
            .and_then(|s| snapshot_version(s).ok())
            .as_deref()
            != journal["new_version"].as_str()
    {
        eprintln!("error: pending reload journal does not match this target or its snapshots; refusing to compile or reload");
        return Err(CliError::AlreadyReported);
    }
    let mut input = journal["receipt"].clone();
    if !input.is_object() {
        eprintln!("error: invalid pending operation identity");
        return Err(CliError::AlreadyReported);
    }
    input["action"] = json!("query");
    input["timeout"] = json!(args.timeout);
    let receipt = probe(erl, args, &input).unwrap_or(Value::Null);
    finish(snapshot, &journal, &receipt)
        .map(Some)
        .map_err(|_| CliError::AlreadyReported)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn identities() -> (Value, Value) {
        let journal = json!({"receipt":{"incarnation":"runtime-a","operation":"17","digest":"sha256-a"},
            "base":"old", "new_version":"new", "previous":"old snapshot", "candidate":"new snapshot"});
        let receipt = json!({"incarnation":"runtime-a", "operation":"17", "digest":"sha256-a",
            "status":"applied", "code_commit":"confirmed", "actor_migration":"complete",
            "cleanup":"acknowledged", "recovery_fenced":false, "applied_version":"new",
            "current_version":"new", "report":{"modules_loaded":2}});
        (journal, receipt)
    }

    #[test]
    fn receipt_reconciliation_requires_identity_completion_and_current_target() {
        let (journal, valid) = identities();
        assert!(matches!(
            resolution(&journal, &valid),
            Resolution::Applied(_)
        ));
        for (field, value) in [
            ("incarnation", json!("restarted")),
            ("operation", json!("18")),
            ("digest", json!("different-content")),
            ("code_commit", json!("unconfirmed")),
            ("actor_migration", json!("incomplete")),
            ("cleanup", json!("unscheduled")),
            ("current_version", json!("later-upgrade")),
            ("applied_version", json!("old")),
            ("recovery_fenced", json!(true)),
            ("status", json!("recovery_required")),
            ("report", Value::Null),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(
                matches!(resolution(&journal, &invalid), Resolution::Unknown),
                "{field}"
            );
        }
        assert!(matches!(
            resolution(&journal, &Value::Null),
            Resolution::Unknown
        ));
    }

    #[test]
    fn pending_receipt_fences_planning_then_authoritative_query_restores_candidate() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = directory.path().join("snapshot.json");
        let (journal, mut receipt) = identities();
        std::fs::write(&snapshot, "candidate on disk").unwrap();
        std::fs::write(pending_path(&snapshot), journal.to_string()).unwrap();
        receipt["status"] = json!("in_progress");
        assert!(matches!(
            finish(&snapshot, &journal, &receipt),
            Err(super::super::ReloadFailure::Uncertain)
        ));
        assert!(!snapshot.exists());
        assert!(pending_path(&snapshot).exists());
        receipt["status"] = json!("applied");
        assert!(finish(&snapshot, &journal, &receipt).is_ok());
        assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), "new snapshot");
        assert!(!pending_path(&snapshot).exists());
    }

    #[test]
    fn rejected_receipt_restores_old_baseline_but_unknown_keeps_journal() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = directory.path().join("snapshot.json");
        let (journal, mut receipt) = identities();
        for status in ["rejected", "recovery_required"] {
            std::fs::write(&snapshot, "new snapshot").unwrap();
            std::fs::write(pending_path(&snapshot), journal.to_string()).unwrap();
            receipt["status"] = json!(status);
            receipt["current_version"] = json!("old");
            receipt["code_commit"] = json!("unconfirmed");
            let result = finish(&snapshot, &journal, &receipt);
            if status == "rejected" {
                assert!(matches!(
                    result,
                    Err(super::super::ReloadFailure::BeforeApply(_))
                ));
                assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), "old snapshot");
                assert!(!pending_path(&snapshot).exists());
            } else {
                assert!(matches!(
                    result,
                    Err(super::super::ReloadFailure::Uncertain)
                ));
                assert!(!snapshot.exists());
                assert!(pending_path(&snapshot).exists());
            }
        }
    }

    #[test]
    fn workspace_lock_excludes_another_reload_and_releases_on_close() {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("debug");
        std::fs::create_dir(&profile).unwrap();
        let path = profile.join("snapshot.json");
        let held = lock(&path).unwrap();
        assert!(lock(&path).is_err());
        drop(held);
        assert!(lock(&path).is_ok());
    }
}
