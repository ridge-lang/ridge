//! Regression: the `entry` key names the module a program starts from.
//!
//! It was parsed, required for an `app` or a `service` (`M006` without it), and
//! then dropped — `ridge_resolve::manifest::Project` had no field to keep it in.
//! Nothing downstream could tell which module the manifest meant, so `ridge run`
//! launched whichever module defined `main` first by name, an entry naming a
//! file that was not there passed `check`, and an app with no `main` at all
//! compiled and then died at startup.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{write_file, TempWorkspace};
use ridge_driver::{
    check_workspace, compile_workspace, select_entry_beam, CheckOptions, CompileOptions,
    EmitArtefacts, Profile,
};

/// A single-project app whose manifest declares `entry`, with the sources the
/// caller supplies.
fn app_workspace(entry: &str, sources: &[(&str, &str)]) -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        &format!(
            "[workspace]\nname = \"demo\"\nversion = \"0.1.0\"\nmembers = [\".\"]\n\n\
             [project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"{entry}\"\n\n\
             [project.src]\nroot = \"src\"\n\n\
             [capabilities]\nallow = [\"io\"]\n"
        ),
    );
    for (path, source) in sources {
        write_file(&tw.path, path, source);
    }
    tw
}

fn codes(tw: &TempWorkspace) -> Vec<&'static str> {
    check_workspace(CheckOptions::new(tw.path.clone()))
        .expect("check runs")
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect()
}

/// An entry that names a file the project does not contain is a typo in the
/// manifest, and nothing else in the pipeline can notice it: the file was never
/// walked, so it produces no modules and no errors of its own.
#[test]
fn entry_naming_a_missing_file_reports_m021() {
    let tw = app_workspace(
        "src/Nope.ridge",
        &[("src/Main.ridge", "pub fn main () -> Int = 0\n")],
    );
    assert!(codes(&tw).contains(&"M021"), "got: {:?}", codes(&tw));
}

/// An `app` is started by calling `main` on its entry module. Without one there
/// is nothing to call — a compile error, not a startup crash.
#[test]
fn entry_without_main_reports_m022() {
    let tw = app_workspace(
        "src/Main.ridge",
        &[("src/Main.ridge", "pub fn helper () -> Int = 1\n")],
    );
    assert!(codes(&tw).contains(&"M022"), "got: {:?}", codes(&tw));
}

/// A `main` in some other module does not stand in for the declared entry.
#[test]
fn main_in_another_module_does_not_satisfy_the_entry() {
    let tw = app_workspace(
        "src/Main.ridge",
        &[
            ("src/Main.ridge", "pub fn helper () -> Int = 1\n"),
            ("src/Other.ridge", "pub fn main () -> Int = 0\n"),
        ],
    );
    assert!(codes(&tw).contains(&"M022"), "got: {:?}", codes(&tw));
}

/// The declared entry wins over the module that merely sorts first.
///
/// `Aardvark` is compiled as module 0 and also defines `main`; the manifest
/// names `Main`, so `Main` is the single entry point.
#[test]
fn the_declared_entry_is_the_entry_point() {
    let tw = app_workspace(
        "src/Main.ridge",
        &[
            ("src/Main.ridge", "pub fn main () -> Int = 0\n"),
            ("src/Aardvark.ridge", "pub fn main () -> Int = 1\n"),
        ],
    );

    let opts = CompileOptions::new(tw.path)
        .with_profile(Profile::Debug)
        .with_emit(EmitArtefacts::Core);
    let artefacts = compile_workspace(opts).expect("compile runs");

    assert_eq!(
        artefacts.entry_modules.len(),
        1,
        "the manifest names one entry: {:?}",
        artefacts.entry_modules
    );
    assert_eq!(artefacts.entry_modules[0].module_fqn, "demo.Main");
    assert_eq!(
        select_entry_beam(&artefacts.entry_modules, "demo").as_deref(),
        Some(artefacts.entry_modules[0].beam_module.as_str())
    );
}

/// A library declares no entry, so every module carrying a `main` stays a
/// candidate — the rule that applied before any entry was honoured.
#[test]
fn a_library_keeps_the_has_a_main_rule() {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"demo\"\nversion = \"0.1.0\"\nmembers = [\".\"]\n\n\
         [project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n\
         [project.src]\nroot = \"src\"\n",
    );
    write_file(&tw.path, "src/Main.ridge", "pub fn main () -> Int = 0\n");

    let opts = CompileOptions::new(tw.path)
        .with_profile(Profile::Debug)
        .with_emit(EmitArtefacts::Core);
    let artefacts = compile_workspace(opts).expect("compile runs");
    assert_eq!(artefacts.entry_modules.len(), 1);
}

/// A module that does not parse has an empty or partial AST, so `main` is
/// missing from it whether or not the source declares one. Reporting M022 there
/// names the manifest for a fault that is in the source, and sends the reader
/// to the wrong file.
#[test]
fn a_parse_error_in_the_entry_does_not_report_m022() {
    let tw = app_workspace(
        "src/Main.ridge",
        &[(
            "src/Main.ridge",
            "pub fn main () -> Int =\n    let x = 1 @@@\n    x\n",
        )],
    );
    let got = codes(&tw);
    assert!(!got.contains(&"M022"), "got: {got:?}");
    assert!(
        got.iter().any(|c| c.starts_with('P') || c.starts_with('L')),
        "the parse failure itself must still be reported: {got:?}"
    );
}

/// The suppression is scoped to modules that failed to parse: an entry that
/// parses cleanly and genuinely has no `main` still reports.
#[test]
fn a_clean_entry_without_main_still_reports_m022() {
    let tw = app_workspace(
        "src/Main.ridge",
        &[("src/Main.ridge", "pub fn helper () -> Int = 1\n")],
    );
    assert!(codes(&tw).contains(&"M022"), "got: {:?}", codes(&tw));
}

/// An app whose manifest and sources agree stays quiet.
#[test]
fn a_coherent_app_reports_nothing() {
    let tw = app_workspace(
        "src/Main.ridge",
        &[("src/Main.ridge", "pub fn main () -> Int = 0\n")],
    );
    assert!(codes(&tw).is_empty(), "got: {:?}", codes(&tw));
}
