//! Track-A tests for `std.crypto` — `constantTimeEq` bridged to `crypto:hash_equals/2`.
//!
//! `crypto.ridge` is `@ffi`-only. These tests pin two things:
//!   1. the module compiles through the T4 build pipeline and is discovered, and
//!   2. its `@ffi` target is present in the generated `ffi_targets` table.
//!
//! The second assertion is the regression guard: `std.crypto` lived in the
//! `build_driver` tier table but was missing from `STDLIB_MODULES` in `build.rs`,
//! so its target never reached the generated table and `constantTimeEq` failed
//! codegen with `E002 StdlibBridgeMissing`. Keeping the two lists in sync is
//! what this guards.
//!
//! The discover-and-build test serializes around a process-level mutex because
//! `build_all` writes to a temp directory keyed by process ID — parallel
//! invocations within the same test binary would race on the same path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Mutex;

use ridge_stdlib::build_driver::{build_all, discover};
use ridge_stdlib::stdlib_targets::{self, StdlibTarget};

static BUILD_LOCK: Mutex<()> = Mutex::new(());

fn stdlib_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("stdlib")
}

#[test]
fn std_crypto_is_discovered_and_built() {
    let _guard = BUILD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = stdlib_dir();

    let modules = discover(&dir);
    assert!(
        modules.iter().any(|m| m.name == "std.crypto"),
        "discover() must find std.crypto; found: {:?}",
        modules.iter().map(|m| &m.name).collect::<Vec<_>>()
    );

    let summary = build_all(&dir).unwrap_or_else(|e| panic!("build_all failed: {e}"));
    assert!(
        summary.modules_built.iter().any(|m| m == "std.crypto"),
        "std.crypto must appear in modules_built; got: {:?}",
        summary.modules_built
    );
}

/// The declared host target for `std.crypto::{name}`, or a failure naming it.
fn foreign_target(name: &str) -> (String, String, u32) {
    let target = stdlib_targets::lookup("std.crypto", name)
        .unwrap_or_else(|| panic!("std.crypto::{name} must resolve in the stdlib target table"));
    match target {
        StdlibTarget::Foreign {
            module,
            fn_name,
            arity,
        } => (module.clone(), fn_name.clone(), *arity),
        other => panic!("std.crypto::{name} must be a host function, got {other:?}"),
    }
}

#[test]
fn constant_time_eq_is_in_the_stdlib_target_table() {
    assert_eq!(
        foreign_target("constantTimeEq"),
        ("crypto".to_owned(), "hash_equals".to_owned(), 2)
    );
}

#[test]
fn digests_and_base64_are_in_the_stdlib_target_table() {
    let cases = [
        ("sha256", "crypto_sha256", 1),
        ("hmacSha256", "crypto_hmac_sha256", 2),
        ("base64Encode", "base64_encode", 1),
        ("base64Decode", "base64_decode", 1),
    ];
    for (name, target_fn, arity) in cases {
        assert_eq!(
            foreign_target(name),
            ("ridge_rt".to_owned(), target_fn.to_owned(), arity),
            "{name}"
        );
    }
}
