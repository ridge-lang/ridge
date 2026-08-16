//! A build that fails analysis must not reach codegen.
//!
//! Carrying on past an error to collect more diagnostics is deliberate, and the
//! artefact struct documents the pass as best-effort. The same decision used to
//! govern codegen's *side effects* as well: a rejected program was lowered,
//! written into `target/` over the output of the last build that succeeded, and
//! handed to `erlc`. The command reported the error and exited non-zero, so
//! nothing suggested the working artefact was gone.
//!
//! `.core` emission is enough to test this — the question is whether anything
//! is written at all, not which backend writes it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{make_workspace, write_file, TempWorkspace};
use ridge_driver::{compile_workspace, CompileArtefacts, CompileOptions, EmitArtefacts};

/// Same shape either way, so a rejected version still lowers to something —
/// which is what made the overwrite possible in the first place.
const GOOD: &str = "pub fn answer () -> Int = 42\n";
const ILL_TYPED: &str = "pub fn answer () -> Int =\n    let x: Int = \"text\"\n    99\n";

/// A redundant arm is a warning, not an error: the program is sound and its
/// artefacts must still be produced.
const WARNS: &str = "\
pub fn classify (x: Int) -> Int =
    match x
        0 -> 1
        _ -> 2
        5 -> 3
";

fn compile(tw: &TempWorkspace) -> CompileArtefacts {
    compile_workspace(CompileOptions::new(tw.path.clone()).with_emit(EmitArtefacts::Core))
        .expect("compile ran")
}

fn error_codes(a: &CompileArtefacts) -> Vec<&str> {
    a.diagnostics
        .iter()
        .filter(|d| matches!(d.severity, ridge_resolve::Severity::Error))
        .map(|d| d.code)
        .collect()
}

#[test]
fn a_build_that_fails_to_type_check_writes_nothing() {
    let tw = make_workspace("Main", ILL_TYPED);
    let artefacts = compile(&tw);

    assert!(
        error_codes(&artefacts).contains(&"T001"),
        "expected the type error to be reported; got: {:?}",
        error_codes(&artefacts)
    );
    assert!(
        artefacts.core_files.is_empty() && artefacts.beam_files.is_empty(),
        "a rejected program must not reach the output directory; wrote {:?} {:?}",
        artefacts.core_files,
        artefacts.beam_files
    );
}

/// The symptom that made this worth fixing: the artefact on disk was not stale,
/// it was the rejected program, and it ran.
#[test]
fn a_failed_build_leaves_the_previous_artefact_untouched() {
    let tw = make_workspace("Main", GOOD);
    let first = compile(&tw);
    let artefact = first
        .core_files
        .first()
        .expect("the good build produced an artefact")
        .clone();
    let before = std::fs::read(&artefact).expect("read the good artefact");

    write_file(&tw.path, "apps/demo/src/Main.ridge", ILL_TYPED);
    let second = compile(&tw);
    assert!(
        !error_codes(&second).is_empty(),
        "the second build was supposed to fail"
    );

    let after = std::fs::read(&artefact).expect("the previous artefact is still there");
    assert_eq!(
        before, after,
        "a failed build replaced the artefact of the last build that succeeded"
    );
}

/// The boundary the gate keys on. A warning is advisory — the artefacts it
/// describes are sound — so it must not stop output the way an error does.
#[test]
fn a_warning_does_not_stop_codegen() {
    let tw = make_workspace("Main", WARNS);
    let artefacts = compile(&tw);

    let warned = artefacts
        .diagnostics
        .iter()
        .any(|d| d.code == "T017" && matches!(d.severity, ridge_resolve::Severity::Warning));
    assert!(
        warned,
        "expected the redundant-arm warning; got: {:?}",
        artefacts
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>()
    );
    assert!(
        error_codes(&artefacts).is_empty(),
        "a redundant arm is not an error; got: {:?}",
        error_codes(&artefacts)
    );
    assert!(
        !artefacts.core_files.is_empty(),
        "a build with only warnings must still emit"
    );
}

/// Analysis still runs to completion — the gate decides what happens to the
/// output, and must not shrink what the user is told.
#[test]
fn the_gate_does_not_swallow_diagnostics() {
    let tw = make_workspace(
        "Main",
        "pub fn answer () -> Int =\n    let x: Int = \"text\"\n    let y: Bool = 1\n    99\n",
    );
    let artefacts = compile(&tw);

    assert!(
        error_codes(&artefacts).len() >= 2,
        "both type errors must survive the gate; got: {:?}",
        error_codes(&artefacts)
    );
}
