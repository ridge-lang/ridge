//! Ridge standard library — compiled artefacts and public facade.
//!
//! The build script (`build.rs`) discovers the `.ridge` source files under
//! `stdlib/`, drives the Ridge pipeline over them in tier order, and emits
//! generated tables into `OUT_DIR`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod build_driver;
pub mod codegen_ffi_targets;
pub mod codegen_manifest;
pub mod ffi_caps_audit;
pub mod ffi_targets;
pub mod ffi_validator;

use std::path::PathBuf;

/// Absolute path to the `stdlib/` source directory embedded at compile time.
///
/// Consumed by `ridge-driver`'s `compile_workspace` to locate the Ridge stdlib
/// sources for on-demand `.beam` compilation.
/// The path is always valid on the machine that compiled `ridge-stdlib`.
///
/// The driver calls this function, compiles the stdlib sources into the user's
/// `target/ridge/<profile>/beam/` directory, and therefore makes
/// `'std.list'`, `'std.option'`, etc. available at BEAM runtime.
#[must_use]
pub fn stdlib_sources_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("stdlib")
}
