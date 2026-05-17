//! Per-error-enum `From<&XError> for Diagnostic` adapters.
//!
//! Each sub-module handles one upstream error type.  The adapters are the
//! **only** construction path for [`crate::Diagnostic`] outside of tests.
//!
//! # Adapters hosted here
//!
//! - [`lex`] — `LexError → Diagnostic`
//! - [`parse`] — `ParseError → Diagnostic`
//! - [`resolve`] — `ResolveError → Diagnostic`
//! - [`manifest`] — `ManifestError → Diagnostic`
//!
//! # Adapters hosted in `ridge-driver`
//!
//! `TypeError` and `CodegenError` adapters live in
//! `ridge_driver::diag_adapters` (`diag_from_typecheck`, `diag_from_codegen`)
//! because `ridge-typecheck` and `ridge-codegen-erl` already depend on
//! `ridge-diagnostics` (for the [`crate::HasErrorCode`] trait), so hosting
//! the adapters here would create a dependency cycle.  `ridge-driver`
//! depends on all four crates and is the natural meeting point.
//!
//! A future refactor (post-0.1.0) could extract `HasErrorCode` into a
//! leaf trait crate and reunite all six adapters here; see D167.

pub mod lex;
pub mod manifest;
pub mod parse;
pub mod resolve;
