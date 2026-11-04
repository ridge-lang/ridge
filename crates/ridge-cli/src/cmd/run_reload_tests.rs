#![allow(clippy::unwrap_used, clippy::panic)]

use super::*;

#[cfg(feature = "beam-runtime")]
#[allow(dead_code)]
#[path = "../../tests/common/server.rs"]
mod node_server;

fn snapshot(format: u32) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        format,
        modules: std::collections::BTreeMap::default(),
    }
}

#[test]
fn dev_rejection_preserves_running_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    std::fs::write(&path, "new build snapshot").unwrap();
    let mut running = Some(snapshot(3));
    let report = finish_dev_reload(
        DevReloadOutcome::Rejected("missing_beam".into()),
        &mut running,
        snapshot(4),
        &path,
    );
    assert_eq!(running, Some(snapshot(3)));
    assert_eq!(read_snapshot(&path).unwrap(), snapshot(3));
    assert!(report.contains("before code was replaced"));
}

#[test]
fn dev_uncertainty_removes_both_baselines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    std::fs::write(&path, "new build snapshot").unwrap();
    let mut running = Some(snapshot(3));
    let report = finish_dev_reload(
        DevReloadOutcome::Uncertain("target_rpc_failed".into()),
        &mut running,
        snapshot(4),
        &path,
    );
    assert!(running.is_none());
    assert!(!path.exists());
    assert!(report.contains("may have changed"));
    assert!(!report.contains("unchanged"));
}

#[test]
fn dev_success_advances_running_baseline() {
    let mut running = Some(snapshot(3));
    let report = finish_dev_reload(
        DevReloadOutcome::Applied("reloaded 2 modules".into()),
        &mut running,
        snapshot(4),
        Path::new("unused"),
    );
    assert_eq!(running, Some(snapshot(4)));
    assert_eq!(report, "reloaded 2 modules");
}

/// Real disk loader -> distributed probe -> the watcher's baseline handler.
/// In particular, the metadata failure happens after both new modules load.
#[cfg(feature = "beam-runtime")]
#[test]
fn dev_probe_preserves_rejections_and_fences_a_committed_failure() {
    let directory = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime = root.join("../ridge-codegen-erl/runtime");
    let compiled = process::Command::new("erlc")
        .arg("-Werror")
        .arg("-o")
        .arg(directory.path())
        .arg(runtime.join("ridge_loader.erl"))
        .arg(runtime.join("ridge_rt.erl"))
        .arg(root.join("../ridge-driver/tests/fixtures/reload_dev_probe.erl"))
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    for (seq, case) in ["missing", "corrupt", "not_purged", "valid", "postcommit"]
        .iter()
        .enumerate()
    {
        let work = directory.path().join(format!("{case} ' path"));
        std::fs::create_dir(&work).unwrap();
        let node = format!("ridge_dev_test_{}_{seq}@127.0.0.1", std::process::id());
        let mut cmd = process::Command::new("erl");
        cmd.current_dir(&work)
            .args([
                "-name",
                &node,
                "-setcookie",
                "reload_test",
                "-noshell",
                "-pa",
            ])
            .arg(directory.path())
            .args([
                "-eval",
                &format!("reload_dev_probe:setup({case}), receive stop -> ok end."),
            ]);
        let mut server = node_server::Server::spawn(cmd).unwrap();
        let ready =
            server.wait_for_output("dev-probe-ready", "", std::time::Duration::from_secs(15));
        assert!(ready.is_ok(), "{ready:?}: {}", server.diagnostics());
        let path = work.join("snapshot.json");
        std::fs::write(&path, "compiled snapshot").unwrap();
        let mut running = Some(snapshot(3));
        let result = probe_apply(
            Path::new("erl"),
            &node,
            "reload_test",
            &work.join("dev.json"),
            "base",
            seq as u64,
        );
        let report = finish_dev_reload(result, &mut running, snapshot(4), &path);
        let query = process::Command::new("erl")
            .args(["-name", &format!("ridge_dev_query_{}@127.0.0.1", std::process::id()), "-setcookie", "reload_test", "-noshell", "-eval",
                "[Node] = init:get_plain_arguments(), io:format(\"FACTS ~s~n\", [json:encode(rpc:call(list_to_atom(Node),reload_dev_probe,state,[]))]).", "-s", "init", "stop", "-extra", &node])
            .output().unwrap();
        assert!(query.status.success(), "{query:?}");
        let stdout = String::from_utf8_lossy(&query.stdout);
        let facts: serde_json::Value = serde_json::from_str(
            stdout
                .lines()
                .find_map(|l| l.strip_prefix("FACTS "))
                .unwrap(),
        )
        .unwrap();
        assert_dev_outcome(case, &facts, running.as_ref(), &path, &report);
        server.stop().unwrap();
    }
}

#[cfg(feature = "beam-runtime")]
fn assert_dev_outcome(
    case: &str,
    facts: &serde_json::Value,
    running: Option<&WorkspaceSnapshot>,
    path: &Path,
    report: &str,
) {
    match case {
        "valid" => {
            assert_eq!(facts["a"], 2);
            assert_eq!(facts["b"], 2);
            assert_eq!(facts["changed"], true);
            assert_eq!(facts["version"], "next");
            assert_eq!(running, Some(&snapshot(4)));
            assert!(path.exists());
            assert!(report.starts_with("reloaded 2 modules"), "{report}");
        }
        "postcommit" => {
            assert_eq!(facts["a"], 2);
            assert_eq!(facts["b"], 2);
            assert_eq!(facts["changed"], true);
            assert!(running.is_none());
            assert!(!path.exists());
            assert!(report.contains("may have changed"), "{report}");
            assert!(!report.contains("unchanged"), "{report}");
        }
        _ => {
            assert_eq!(facts["a"], 1);
            assert_eq!(facts["b"], 1);
            assert_eq!(facts["changed"], false);
            assert_eq!(facts["version"], "base");
            assert_eq!(running, Some(&snapshot(3)));
            assert_eq!(read_snapshot(path).unwrap(), snapshot(3));
            assert!(report.contains("before code was replaced"), "{report}");
        }
    }
}
