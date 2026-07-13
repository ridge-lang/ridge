//! Regression: `compile_workspace` records the module that defines `fn main`
//! as the entry point, even when it is not the first module by fully-qualified
//! name.
//!
//! Before this, `ridge run` launched `beam_files[0]` — the alphabetically-first
//! module — so any multi-file program whose entry module did not sort first
//! crashed at runtime with `error:undef`. These tests pin the entry-point
//! bookkeeping without needing OTP (they emit `.core`, not `.beam`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{write_file, TempWorkspace};
use ridge_driver::{compile_workspace, select_entry_beam, CompileOptions, EmitArtefacts, Profile};

/// A two-module app whose entry module (`Main`) sorts *after* the other module
/// (`Aaa`) records `demo.Main` as its single entry point.
#[test]
fn entry_module_is_the_one_with_main_not_the_first_by_name() {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        // Combined workspace + project manifest (the single-project layout).
        "[workspace]\nname = \"demo\"\nversion = \"0.1.0\"\nmembers = [\".\"]\n\n\
         [project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n\
         [project.src]\nroot = \"src\"\n",
    );
    // `Aaa` sorts before `Main`, so it is compiled as module 0. `Main` carries
    // the entry point.
    write_file(&tw.path, "src/Aaa.ridge", "pub fn helper () -> Int = 1\n");
    write_file(&tw.path, "src/Main.ridge", "fn main () -> Int = 0\n");

    // Move `tw.path` into the options: the temp dir stays on disk until `tw`'s
    // `TempDir` field drops at end of scope, so the compile still sees it.
    let opts = CompileOptions::new(tw.path)
        .with_profile(Profile::Debug)
        .with_emit(EmitArtefacts::Core);
    let artefacts = compile_workspace(opts).expect("compile should succeed");

    assert_eq!(
        artefacts.entry_modules.len(),
        1,
        "exactly one module defines `main`; got: {:?}",
        artefacts.entry_modules
    );
    let entry = &artefacts.entry_modules[0];
    assert_eq!(entry.module_fqn, "demo.Main");
    assert_eq!(entry.project_name, "demo");

    // The launcher picks that module's atom, not `beam_files[0]`.
    assert_eq!(
        select_entry_beam(&artefacts.entry_modules, "demo").as_deref(),
        Some(entry.beam_module.as_str())
    );
}

/// A library-only workspace (no `fn main`) records no entry points.
#[test]
fn library_only_workspace_has_no_entry_modules() {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"lib-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/lib/ridge.toml",
        "[project]\nname = \"lib\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        &tw.path,
        "apps/lib/src/Lib.ridge",
        "pub fn add (a: Int) (b: Int) -> Int = a + b\n",
    );

    // Move `tw.path` into the options: the temp dir stays on disk until `tw`'s
    // `TempDir` field drops at end of scope, so the compile still sees it.
    let opts = CompileOptions::new(tw.path)
        .with_profile(Profile::Debug)
        .with_emit(EmitArtefacts::Core);
    let artefacts = compile_workspace(opts).expect("compile should succeed");

    assert!(
        artefacts.entry_modules.is_empty(),
        "a library workspace has no entry point; got: {:?}",
        artefacts.entry_modules
    );
}
