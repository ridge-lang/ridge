//! Target-neutral FFI lookup table for Ridge stdlib symbols (T14.5.3).
//!
//! This module exposes [`lookup`], [`all_entries`], and [`StdlibFfiTarget`] —
//! the single source of truth for path-B stdlib FFI resolution across all
//! codegen backends.
//!
//! ## Architecture (D141 / OQ-T14.5-04)
//!
//! The lookup table is generated at build time by `crates/ridge-stdlib/build.rs`
//! from the `@ffi`-decorated and pure-Ridge `pub fn` declarations in the
//! `stdlib/*.ridge` source files.  Consumers (e.g. `ridge-codegen-erl`) adapt
//! the returned [`StdlibFfiTarget`] into their own target representation at
//! the seam — `BridgeTarget` stays in `ridge-codegen-erl`, keeping
//! `ridge-stdlib` target-neutral.
//! forward-compat guarantee #2.

/// Target-neutral FFI descriptor for one Ridge stdlib symbol.
///
/// Returned by [`lookup`] for both `@ffi`-decorated stubs (where
/// `beam_module` / `fn_name` come from the attribute) and pure-Ridge
/// `pub fn` bodies (where `beam_module` is the compiled Ridge stdlib module
/// atom and `fn_name` is the Ridge function name).
///
/// Consumers are responsible for adapting this shape into their
/// target-specific representation (e.g.
/// `BridgeTarget::RidgeStdlibLocal` in `ridge-codegen-erl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdlibFfiTarget {
    /// BEAM module atom for this symbol's call site (e.g. `"lists"`, `"ridge_rt"`,
    /// or a Ridge dotted module name like `"std.list"` for pure-Ridge bodies).
    pub beam_module: String,
    /// BEAM (or Ridge) function name at the call site.
    pub fn_name: String,
    /// Arity.
    pub arity: u32,
}

// Include the build-script-generated lookup table.
// The generated file defines `build_ffi_map`, `FFI_MAP`, `FfiMap`, and `lookup`,
// all of which reference `StdlibFfiTarget` from this module.
include!(concat!(env!("OUT_DIR"), "/ffi_targets.rs"));

/// Iterate over all generated stdlib FFI entries.
///
/// Yields `(key, target)` pairs where `key` is `"ridge_module::fn_name"`.
/// This enables consumers to build their own adapter maps without requiring
/// repeated `lookup` calls with known keys.
///
/// The iterator borrows the `'static` backing map; iteration order is
/// unspecified (hash map order).
pub fn all_entries() -> impl Iterator<Item = (&'static str, &'static StdlibFfiTarget)> {
    let map: &'static FfiMap = FFI_MAP.get_or_init(build_ffi_map);
    map.iter().map(|(k, v)| (k.as_str(), v))
}
