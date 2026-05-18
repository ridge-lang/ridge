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

pub struct TempWorkspace {
    pub path: PathBuf,
}

impl TempWorkspace {
    pub fn new(id: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ridge_codegen_erl_test_{id}"));
        if path.exists() {
            let _ = fs::remove_dir_all(&path);
        }
        fs::create_dir_all(&path).expect("create temp workspace dir");
        Self { path }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
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
    write_file(&tw.path, &format!("apps/demo/src/{module_name}.ridge"), source);
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
    let lowered = lower_workspace(&typecheck_result.typed, &resolved);
    PipelineResult { lowered }
}
