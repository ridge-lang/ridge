//! Regression test for stdlib BEAM bundling.
//!
//! Lives in its own test binary, isolating it from `run_missing_erlang` in
//! `integration.rs` (which mutates the process-wide PATH). `integration.rs`
//! now serialises PATH-dependent tests via a module-level mutex, so the
//! file-level split is defence-in-depth rather than the only thing keeping
//! these two tests apart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::make_workspace;

use ridge_driver::{compile_workspace, CompileOptions};

/// A fresh workspace build must emit the stdlib `.beam` files into
/// `target/.../beam/`. v0.2.0 shipped a binary that resolved the stdlib
/// source directory from a compile-time `env!("CARGO_MANIFEST_DIR")`, which
/// only exists on the build machine. On other machines the bundling pass
/// silently produced zero BEAMs, and any program calling a Ridge-bodied
/// stdlib function (`List.head`, `Option.withDefault`, …) crashed at boot
/// with `undef`. The fix embeds the stdlib sources via `include_str!`.
#[test]
fn stdlib_beams_emitted_on_fresh_build() {
    // Trivial source — the stdlib bundling pass runs regardless of what the
    // user code imports, so a successful compile is all we need.
    let source = "pub fn answer () -> Int = 42\n";
    let tw = make_workspace("Main", source);
    let opts = CompileOptions::new(tw.path);
    let artefacts = compile_workspace(opts).expect("compile workspace");

    // Locate the beam dir from any produced artefact.
    let beam_file = artefacts
        .beam_files
        .first()
        .expect("at least one .beam file produced");
    let beam_dir = beam_file.parent().expect("beam file has a parent dir");

    // Spot-check a few canonical stdlib modules. `std.list` is the one users
    // hit first (it powers `List.head`/`List.drop`/`Option.withDefault` chains).
    for module in &["std.list", "std.option", "std.result", "std.text"] {
        let path = beam_dir.join(format!("{module}.beam"));
        assert!(
            path.exists(),
            "expected stdlib BEAM at {} but it was not emitted; \
             Ridge-bodied stdlib functions would crash at runtime",
            path.display()
        );
    }
}

/// A build directory missing one stdlib module has to be repaired, and used to
/// be left as it was.
///
/// The reuse test was whether `std.list.beam` existed, which took one file as
/// proof of the whole set. So any other module could be absent — an interrupted
/// build, a concurrent one writing the same directory, or a compiler upgrade
/// that added a module to the standard library — and nothing put it back:
/// `check` passed, `build` reported success, and the program died at run time on
/// `'std.text':split/2` with nothing pointing at the build directory.
#[test]
fn a_missing_stdlib_module_is_re_emitted() {
    let source = "pub fn answer () -> Int = 42\n";
    let tw = make_workspace("Main", source);

    let artefacts = compile_workspace(CompileOptions::new(tw.path.clone())).expect("first build");
    let beam_dir = artefacts
        .beam_files
        .first()
        .expect("at least one .beam file produced")
        .parent()
        .expect("beam file has a parent dir")
        .to_path_buf();

    // Delete a module that is not the one the old sentinel looked at, so the
    // test fails on the real defect rather than on the sentinel's own file.
    let victim = beam_dir.join("std.text.beam");
    assert!(victim.exists(), "std.text.beam must exist to be removed");
    std::fs::remove_file(&victim).expect("remove one stdlib beam");
    assert!(
        beam_dir.join("std.list.beam").exists(),
        "the sentinel file is deliberately left in place"
    );

    compile_workspace(CompileOptions::new(tw.path)).expect("second build");

    assert!(
        victim.exists(),
        "a stdlib module deleted from the build directory must be re-emitted; \
         leaving it absent gives a build that reports success and then fails at \
         run time with `undef` on a stdlib function"
    );
}

/// The reused set is only reused when a manifest says what it contains.
///
/// Without one there is no way to tell a complete emission from a partial one
/// short of recompiling the standard library to find out, which is the cost the
/// old single-file check was avoiding.
#[test]
fn the_emitted_set_is_recorded() {
    let source = "pub fn answer () -> Int = 42\n";
    let tw = make_workspace("Main", source);
    let artefacts = compile_workspace(CompileOptions::new(tw.path)).expect("build");

    let beam_dir = artefacts
        .beam_files
        .first()
        .expect("at least one .beam file produced")
        .parent()
        .expect("beam file has a parent dir")
        .to_path_buf();
    let manifest = beam_dir
        .parent()
        .expect("beam dir has a parent")
        .join(".stdlib-manifest");

    let body = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
    let mut lines = body.lines();
    assert_eq!(
        lines.next(),
        Some(env!("CARGO_PKG_VERSION")),
        "the first line records the compiler that wrote the set, so a version \
         whose standard library gained a module does not reuse the old one"
    );
    let modules: Vec<&str> = lines.filter(|l| !l.trim().is_empty()).collect();
    for expected in &["std.list", "std.text", "std.option", "std.result"] {
        assert!(
            modules.contains(expected),
            "{expected} missing from the recorded set: {modules:?}"
        );
    }
}
