//! How each Ridge stdlib symbol is resolved, for every codegen backend.
//!
//! This module exposes [`lookup`], [`all_entries`], and [`StdlibTarget`] — the
//! single source of truth for stdlib symbol resolution shared across backends.
//!
//! ## What a backend is being told
//!
//! A stdlib declaration answers one of three different questions about where
//! its implementation comes from, and [`StdlibTarget`] keeps them apart so a
//! backend does not have to guess:
//!
//! - [`StdlibTarget::Foreign`] — the declaration named a function of the host
//!   runtime (`@ffi`).  A backend that is not that host has to shim it or say
//!   plainly that it cannot.
//! - [`StdlibTarget::RidgeModule`] — an ordinary Ridge body, compiled into the
//!   stdlib module of that name.  Every backend that compiles Ridge has it.
//! - [`StdlibTarget::Primitive`] — an operation of the language (`@primitive`).
//!   The declaration named nothing; each backend supplies the instruction.
//!
//! This used to be one struct with a `beam_module` field, which held a BEAM
//! module for the first case and a Ridge module for the second, and left the
//! third with nowhere to go — so arithmetic was declared as foreign, and the
//! shared table handed every backend `erlang` as the meaning of `+`.
//!
//! ## Generation
//!
//! The table is generated at build time by `crates/ridge-stdlib/build.rs` from
//! the declarations in the `stdlib/*.ridge` sources.  Consumers adapt
//! [`StdlibTarget`] into their own representation at the seam — `BridgeTarget`
//! stays inside `ridge-codegen-erl`, keeping this crate target-neutral in fact
//! and not only in its doc comments.

/// Where one Ridge stdlib symbol's implementation comes from.
///
/// Returned by [`lookup`].  Consumers are responsible for adapting this into
/// their target-specific representation (e.g. `BridgeTarget` in
/// `ridge-codegen-erl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdlibTarget {
    /// The declaration named a function of the host runtime, via
    /// `@ffi("module", "name", arity)`.
    Foreign {
        /// The host module the declaration named (e.g. `"lists"`, `"ridge_rt"`).
        module: String,
        /// The function name inside that module.
        fn_name: String,
        /// Arity, as declared in the attribute.
        arity: u32,
    },
    /// An ordinary Ridge body, compiled into the stdlib module named here.
    ///
    /// The function name is unchanged from the Ridge source; the arity counts
    /// the value parameters plus one dictionary parameter per `where`
    /// constraint, matching what call sites pass.
    RidgeModule {
        /// The dotted Ridge module (e.g. `"std.list"`), which is also the
        /// compiled module's name.
        module: String,
        /// The Ridge function name.
        fn_name: String,
        /// Arity including dictionary parameters.
        arity: u32,
    },
    /// An operation of the language itself, declared `@primitive`.
    ///
    /// There is deliberately nothing here but the arity: naming an
    /// implementation is the backend's job, and a symbol it has no answer for
    /// is an error it should raise rather than a call it should invent.
    Primitive {
        /// Arity — the declared parameter count.
        arity: u32,
    },
}

impl StdlibTarget {
    /// The arity every variant carries.
    #[must_use]
    pub const fn arity(&self) -> u32 {
        match self {
            Self::Foreign { arity, .. }
            | Self::RidgeModule { arity, .. }
            | Self::Primitive { arity } => *arity,
        }
    }

    /// Whether this symbol is a language primitive.
    #[must_use]
    pub const fn is_primitive(&self) -> bool {
        matches!(self, Self::Primitive { .. })
    }
}

// Include the build-script-generated lookup table.
// The generated file defines `build_target_map`, `TARGET_MAP`, `TargetMap`, and
// `lookup`, all of which reference `StdlibTarget` from this module.
include!(concat!(env!("OUT_DIR"), "/stdlib_targets.rs"));

/// Iterate over every generated stdlib entry.
///
/// Yields `(key, target)` pairs where `key` is `"ridge_module::fn_name"`.
/// This enables consumers to build their own adapter maps without requiring
/// repeated `lookup` calls with known keys.
///
/// The iterator borrows the `'static` backing map; iteration order is
/// unspecified (hash map order).
pub fn all_entries() -> impl Iterator<Item = (&'static str, &'static StdlibTarget)> {
    let map: &'static TargetMap = TARGET_MAP.get_or_init(build_target_map);
    map.iter().map(|(k, v)| (k.as_str(), v))
}

/// Every symbol the standard library declares `@primitive`, as
/// `(ridge_module, ridge_fn, arity)`.
///
/// This is the list a backend has to be able to answer in full.  It is derived
/// from the generated table rather than written down a second time, so it
/// cannot drift from what the stdlib sources actually declare — which is the
/// point: the set of primitive operations is a language decision, and each
/// backend's table is checked against this one in both directions.
///
/// Sorted, so a failure reads the same way every run.
#[must_use]
pub fn primitive_symbols() -> Vec<(&'static str, &'static str, u32)> {
    let mut out: Vec<(&'static str, &'static str, u32)> = all_entries()
        .filter(|(_, t)| t.is_primitive())
        .filter_map(|(key, t)| {
            let (module, name) = key.split_once("::")?;
            Some((module, name, t.arity()))
        })
        .collect();
    out.sort_unstable();
    out
}
