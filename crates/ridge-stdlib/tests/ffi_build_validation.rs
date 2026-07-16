//! Regression coverage for the `@ffi` audit-table gate wired into the stdlib
//! build (T001 arity, T002 capability, T004 unknown target).
//!
//! The build refuses to compile a standard-library module whose `@ffi`
//! declaration drifts out of `ffi_caps_audit::AUDIT_TABLE`. These tests pin
//! that behaviour: the real stdlib still builds, and synthetic out-of-table or
//! wrong-arity declarations fail the build.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use ridge_stdlib::build_driver::{build_all, BuildError};
use tempfile::TempDir;

/// Write a single-module temp stdlib directory whose `int.ridge` holds `src`,
/// so `build_all` discovers it as `std.int` (tier 1) and validates its `@ffi`.
fn temp_stdlib_with_int(src: &str) -> TempDir {
    let td = TempDir::new().expect("tempdir");
    std::fs::write(td.path().join("int.ridge"), src).expect("write int.ridge");
    td
}

/// (a) The real standard library passes the gate — every `@ffi` it ships is in
/// the audit table. This is the regression guard against table drift.
#[test]
fn real_stdlib_passes_ffi_validation() {
    let stdlib_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib");
    build_all(&stdlib_dir).unwrap_or_else(|e| panic!("real stdlib failed the @ffi gate: {e}"));
}

/// (b) An `@ffi` pointing at a BEAM target absent from the audit table fails
/// the build with T004.
#[test]
fn out_of_table_target_fails_build_with_t004() {
    let td = temp_stdlib_with_int(
        "@ffi(\"some_unaudited_mod\", \"dangerous\", 1)\npub fn bad (x: Int) -> Int\n",
    );

    let err = build_all(td.path()).expect_err("out-of-table @ffi must fail the build");
    match err {
        BuildError::TierBuildFailed { source, .. } => {
            assert!(
                source.contains("T004"),
                "expected T004 in the failure, got: {source}"
            );
        }
        BuildError::CircularImport { .. } => {
            panic!("expected TierBuildFailed, got CircularImport")
        }
    }
}

/// (c) An in-table target whose declared Ridge arity disagrees with the `@ffi`
/// arity fails the build with T001.
#[test]
fn arity_mismatch_fails_build_with_t001() {
    // `erlang:abs/1` is in the audit table, but the Ridge signature declares
    // two parameters — a T001 arity mismatch.
    let td =
        temp_stdlib_with_int("@ffi(\"erlang\", \"abs\", 1)\npub fn bad (a: Int) (b: Int) -> Int\n");

    let err = build_all(td.path()).expect_err("arity-mismatched @ffi must fail the build");
    match err {
        BuildError::TierBuildFailed { source, .. } => {
            assert!(
                source.contains("T001"),
                "expected T001 in the failure, got: {source}"
            );
        }
        BuildError::CircularImport { .. } => {
            panic!("expected TierBuildFailed, got CircularImport")
        }
    }
}

/// (d) In a tier with several modules the failure names the module that
/// actually declares the bad `@ffi`, not the tier's first module.
///
/// Tier 2 lists `std.text` before `std.list`, but the module graph is sorted by
/// fully-qualified name, so `std.list` sorts first. Putting the out-of-table
/// `@ffi` in `std.list` (clean `std.text`) makes the two orders disagree: the
/// old code labelled every tier failure with the first *listed* module and
/// reported this against `std.text`, sending you to rebuild the wrong file.
#[test]
fn failure_names_the_owning_module_within_a_multi_module_tier() {
    let td = TempDir::new().expect("tempdir");
    // Clean module, listed first in the tier table, with a valid in-table @ffi.
    std::fs::write(
        td.path().join("text.ridge"),
        "@ffi(\"erlang\", \"abs\", 1)\npub fn absInt (x: Int) -> Int\n",
    )
    .expect("write text.ridge");
    // Offending module, whose @ffi points at a target absent from the table.
    std::fs::write(
        td.path().join("list.ridge"),
        "@ffi(\"some_unaudited_mod\", \"dangerous\", 1)\npub fn bad (x: Int) -> Int\n",
    )
    .expect("write list.ridge");

    let err = build_all(td.path()).expect_err("out-of-table @ffi must fail the build");
    match err {
        BuildError::TierBuildFailed { module, source, .. } => {
            assert!(
                source.contains("T004"),
                "expected T004 in the failure, got: {source}"
            );
            assert_eq!(
                module, "std.list",
                "the failure must name the module that declares the bad @ffi, not the first tier module"
            );
        }
        BuildError::CircularImport { .. } => {
            panic!("expected TierBuildFailed, got CircularImport")
        }
    }
}
