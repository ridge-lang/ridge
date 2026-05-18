//! Shared test helpers for `ridge-cli` integration tests.
//!
//! Ported and extended from `crates/ridge-driver/tests/common/mod.rs`.  The
//! two copies are intentionally kept loosely coupled — this avoids a dev-dep
//! cycle and lets each test suite evolve independently.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items,
    dead_code
)]

use std::fs;
use std::path::{Path, PathBuf};

// ── TempWorkspace ─────────────────────────────────────────────────────────────

/// A temporary workspace directory that cleans up on drop.
pub struct TempWorkspace {
    /// The absolute path to the workspace root.
    pub path: PathBuf,
    // Kept alive so the directory is removed on drop.
    _tempdir: tempfile::TempDir,
}

impl TempWorkspace {
    /// Create a new unique temporary directory.
    pub fn new() -> Self {
        let td = tempfile::TempDir::new().expect("create tempdir");
        let path = td.path().to_owned();
        Self { path, _tempdir: td }
    }
}

// ── File helpers ──────────────────────────────────────────────────────────────

/// Write `content` to `dir.join(relative_path)`, creating parent directories.
pub fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(&full, content).expect("write file");
}

// ── Workspace builders ────────────────────────────────────────────────────────

/// Absolute path to the repo root `examples/` directory.
///
/// Works from `crates/ridge-cli/` — walks up two levels.
pub fn examples_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .join("..")
        .join("..")
        .join("examples")
}

/// Read one of the four canonical Ridge example source files.
pub fn read_example(name: &str) -> String {
    let path = examples_dir().join(format!("{name}.ridge"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read example {name}: {e}"))
}

/// Build a minimal single-member **library** workspace.
///
/// Layout:
/// ```text
/// <root>/
///   ridge.toml           (workspace manifest, members = ["apps/*"])
///   apps/demo/
///     ridge.toml         (project manifest, kind = "library")
///     src/<module>.ridge
/// ```
pub fn make_workspace(module_name: &str, source: &str) -> TempWorkspace {
    let tw = TempWorkspace::new();
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

/// Build a minimal single-member **app** workspace.
///
/// Layout:
/// ```text
/// <root>/
///   ridge.toml           (workspace manifest, members = ["apps/*"])
///   apps/demo/
///     ridge.toml         (project manifest, kind = "app", entry = "src/<module>.ridge")
///     src/<module>.ridge
/// ```
///
/// The source must define `pub fn main()` (zero-argument) or
/// `pub fn main(args: List Text) -> Unit` (one-argument).
pub fn make_app_workspace(module_name: &str, source: &str) -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        &format!(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/{module_name}.ridge\"\n"
        ),
    );
    write_file(&tw.path, &format!("apps/demo/src/{module_name}.ridge"), source);
    tw
}

/// Build a two-member workspace with `apps/api` (library) and `apps/core` (library).
pub fn make_multi_member_workspace() -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"multi-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    // Canonical Ridge surface: `fn name (params) -> Type = expr`.  Multiple
    // params are curried: `(a: T) (b: T)` — comma-separated tuple-style is
    // not valid syntax.
    let api_src = "pub fn greet -> Text = \"hello\"\n";
    let core_src = "pub fn add (a: Int) (b: Int) -> Int = a + b\n";
    write_file(
        &tw.path,
        "apps/api/ridge.toml",
        "[project]\nname = \"api\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(&tw.path, "apps/api/src/Api.ridge", api_src);
    write_file(
        &tw.path,
        "apps/core/ridge.toml",
        "[project]\nname = \"core\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(&tw.path, "apps/core/src/Core.ridge", core_src);
    tw
}

/// Build a two-member workspace with one `app` member (`apps/app`) and one
/// `library` member (`apps/lib`).
///
/// Useful for testing `--member` selection in `ridge run`.
pub fn make_mixed_workspace(app_source: &str) -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"mixed-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/myapp/ridge.toml",
        "[project]\nname = \"myapp\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    );
    write_file(&tw.path, "apps/myapp/src/Main.ridge", app_source);
    write_file(
        &tw.path,
        "apps/mylib/ridge.toml",
        "[project]\nname = \"mylib\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        &tw.path,
        "apps/mylib/src/Lib.ridge",
        "pub fn helper -> Int = 42\n",
    );
    tw
}

/// Build an example workspace around one of the four canonical examples.
///
/// The example is placed at `apps/demo/src/<name>.ridge` with `kind = "app"`.
pub fn make_example_app_workspace(name: &str) -> TempWorkspace {
    let source = read_example(name);
    make_app_workspace(name, &source)
}

/// Build an example workspace around one of the four canonical examples as a library.
///
/// Uses `kind = "library"` so `ridge build` works without requiring an
/// executable entry point.
pub fn make_example_workspace(name: &str) -> TempWorkspace {
    let source = read_example(name);
    make_workspace(name, &source)
}
