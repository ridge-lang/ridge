//! Cross-module type seeding — imported type names resolve in the consumer.
//!
//! Runs the full `discover -> resolve -> typecheck` pipeline over a two-module
//! project where `proj.Lib` declares a record and `proj.Main` annotates a
//! parameter with the imported type and accesses its fields. Before type-name
//! seeding these annotations fell through to a fresh type var and every field
//! access was silently absorbed.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;

use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::{typecheck_workspace, TypeError};
use tempfile::TempDir;

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// Build a two-module project `proj` (`Main.ridge` + `Lib.ridge`) and run the
/// full pipeline. Returns every `T###` error across the workspace.
fn typecheck_two_modules(main_src: &str, lib_src: &str) -> Vec<TypeError> {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"**\"]\n",
    );
    write_file(td.path(), "libs/proj/src/Main.ridge", main_src);
    write_file(td.path(), "libs/proj/src/Lib.ridge", lib_src);

    let disc = discover_workspace(td.path());
    let resolved = resolve_workspace(disc.graph.expect("workspace graph"));
    let result = typecheck_workspace(&resolved);
    result.errors.into_iter().map(|(_, e)| e).collect()
}

fn count_code(errors: &[TypeError], code: &str) -> usize {
    errors.iter().filter(|e| e.code() == code).count()
}

const LIB_PLAIN: &str = "pub type Plain = { x: Int }\n";

#[test]
fn imported_type_annotation_resolves_unknown_field_is_t005() {
    // `(p: Plain)` must resolve to the producer's record so `p.nope` is a real
    // unknown-field error rather than being silently absorbed.
    let main = "import proj.Lib (Plain)\nfn f (p: Plain) -> Int = p.nope\n";
    let errors = typecheck_two_modules(main, LIB_PLAIN);
    assert_eq!(
        count_code(&errors, "T005"),
        1,
        "expected one T005 for unknown field on imported record; got {errors:?}"
    );
}

#[test]
fn imported_type_field_type_flows_t001() {
    // `p.x` is Int; returning it as Text must mismatch — proving the field type
    // crossed the module boundary.
    let main = "import proj.Lib (Plain)\nfn f (p: Plain) -> Text = p.x\n";
    let errors = typecheck_two_modules(main, LIB_PLAIN);
    assert_eq!(
        count_code(&errors, "T001"),
        1,
        "expected one T001 for Int field returned as Text; got {errors:?}"
    );
}

#[test]
fn imported_type_correct_field_use_is_clean() {
    let main = "import proj.Lib (Plain)\nfn f (p: Plain) -> Int = p.x\n";
    let errors = typecheck_two_modules(main, LIB_PLAIN);
    assert!(
        errors.is_empty(),
        "correct cross-module field access must type-check clean; got {errors:?}"
    );
}
