//! Regression test for stdlib BEAM bundling.
//!
//! Lives in its own test binary so it does not race with `run_missing_erlang`
//! in `integration.rs`, which mutates the process-wide PATH to suppress
//! `erl` discovery (see comment at the top of that test).

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
    let opts = CompileOptions::new(tw.path.clone());
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
