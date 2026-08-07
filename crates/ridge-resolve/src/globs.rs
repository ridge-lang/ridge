//! Glob matching for member and export patterns, delegated to `ridge-manifest`.
//!
//! This module was a copy of `ridge_manifest::globs` — identical code, and the
//! only place either copy's tests lived. The tests moved to `ridge-manifest`
//! alongside the parser they support; nothing is left to duplicate.

pub use ridge_manifest::globs::{CompiledGlob, GlobError, GlobPattern};
