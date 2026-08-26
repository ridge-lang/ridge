//! Shared test helpers for `ridge-codegen-erl` integration tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items,
    dead_code
)]

use ridge_ir::LoweredWorkspace;
use ridge_lower::lower_workspace;
use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::typecheck_workspace;
use std::fs;
use std::path::{Path, PathBuf};

/// A temporary directory that cleans itself up on drop.
///
/// Unique per instance: `id` labels the directory, it does not name it, so two
/// tests passing the same `id` still get a directory each. A fixed name is
/// shared by every test that writes it, and under the runner CI and
/// `scripts/gate.sh` both use, every test is its own process — see the note in
/// `crates/ridge-lower/tests/common/mod.rs` for the measurement.
pub struct TempWorkspace {
    pub path: PathBuf,
    /// Owns the directory; removed when this value is dropped.
    _dir: tempfile::TempDir,
}

impl TempWorkspace {
    pub fn new(id: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(&format!("ridge_codegen_erl_test_{id}_"))
            .tempdir()
            .expect("create temp workspace dir");
        let path = dir.path().to_owned();
        Self { path, _dir: dir }
    }
}

fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(&full, content).expect("write file");
}

pub fn make_workspace(id: &str, module_name: &str, source: &str) -> TempWorkspace {
    let tw = TempWorkspace::new(id);
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        &tw.path,
        &format!("apps/demo/src/{module_name}.ridge"),
        source,
    );
    tw
}

pub struct PipelineResult {
    pub lowered: LoweredWorkspace,
}

pub fn run_pipeline(workspace_path: &Path) -> PipelineResult {
    let disc = discover_workspace(workspace_path);
    let ws_graph = disc.graph.expect("workspace graph must be present");
    let resolved = resolve_workspace(ws_graph);
    let typecheck_result = typecheck_workspace(&resolved);
    let lowered = lower_workspace(&typecheck_result.typed, &resolved).workspace;
    PipelineResult { lowered }
}
