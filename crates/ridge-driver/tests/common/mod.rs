//! Shared helpers for `ridge-driver` integration tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items,
    dead_code
)]

use std::fs;
use std::path::{Path, PathBuf};

/// A temporary workspace that is cleaned up on drop.
pub struct TempWorkspace {
    pub path: PathBuf,
    // Keep the TempDir alive so it is cleaned up on Drop.
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

fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(&full, content).expect("write file");
}

/// Absolute path to the repository root `examples/` directory.
pub fn examples_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` for `ridge-driver` is `.../crates/ridge-driver`.
    // Two levels up is the repo root; then `examples/`.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir)
        .join("..")
        .join("..")
        .join("examples")
}

/// Read one of the four canonical Ridge example files.
pub fn read_example(name: &str) -> String {
    let path = examples_dir().join(format!("{name}.rg"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("could not read example {name}: {e}"))
}

/// Build a minimal single-member workspace from a Ridge source file.
///
/// Layout:
/// ```text
/// <root>/
///   ridge.toml           (workspace manifest, members = ["apps/*"])
///   apps/demo/
///     ridge.toml         (project manifest)
///     src/<module>.rg    (source file)
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
    write_file(&tw.path, &format!("apps/demo/src/{module_name}.rg"), source);
    tw
}

/// Build a two-member workspace: `apps/api` and `apps/core`.
///
/// Both members contain a minimal Ridge module.
pub fn make_multi_member_workspace() -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"multi-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    // Canonical Ridge surface: `fn name (params) -> Type = expr`.  Multi-param
    // is curried: `(a: T) (b: T)` — tuple-style commas are not valid syntax.
    let api_src = "pub fn greet -> Text = \"hello\"\n";
    let core_src = "pub fn add (a: Int) (b: Int) -> Int = a + b\n";
    write_file(
        &tw.path,
        "apps/api/ridge.toml",
        "[project]\nname = \"api\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(&tw.path, "apps/api/src/Api.rg", api_src);
    write_file(
        &tw.path,
        "apps/core/ridge.toml",
        "[project]\nname = \"core\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(&tw.path, "apps/core/src/Core.rg", core_src);
    tw
}

/// Build a workspace with a forbid rule that will be violated by an import.
///
/// The rule forbids `acme.ui.**` from importing `acme.db.**`.  The `Ui` module
/// imports from `Db`, which triggers `R013 ForbidViolation`.
///
/// The project names use the dotted-prefix convention (`acme.ui`, `acme.db`)
/// so that the module FQNs (`acme.ui.Ui`, `acme.db.Db`) match the forbid
/// glob patterns (`acme.ui.**`, `acme.db.**`) — matching the `acme_forbid`
/// fixture pattern from `ridge-resolve`'s own tests.
pub fn make_forbid_workspace() -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\n\
         name = \"forbid-ws\"\n\
         version = \"0.1.0\"\n\
         members = [\"apps/*\"]\n\
         \n\
         [workspace.rules]\n\
         forbid = [{ from = \"acme.ui.**\", to = \"acme.db.**\" }]\n",
    );
    write_file(
        &tw.path,
        "apps/ui/ridge.toml",
        "[project]\nname = \"acme.ui\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"**\"]\n",
    );
    write_file(
        &tw.path,
        "apps/db/ridge.toml",
        "[project]\nname = \"acme.db\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"**\"]\n",
    );
    let db_src = "pub fn query() -> Text { \"result\" }";
    let ui_src = "import acme.db.Db\npub fn show() -> Text { Db.query() }";
    write_file(&tw.path, "apps/db/src/Db.rg", db_src);
    write_file(&tw.path, "apps/ui/src/Ui.rg", ui_src);
    tw
}
