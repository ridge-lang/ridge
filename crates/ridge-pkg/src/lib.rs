//! `ridge-pkg` — package manager for the Ridge language toolchain.
//!
//! Resolves `path`, `git`, `workspace`, and `workspace-member` dependencies
//! declared in a project's `ridge.toml` into a flat list of [`ResolvedDep`]
//! values that downstream tools (e.g. `ridge-driver`) can consume.
//!
//! # Error codes
//!
//! All errors are in the `P###` namespace (§1.3 #3).  See [`PkgError`].
//!
//! # Hard constraints (§1.3)
//!
//! - No `panic!`, `unwrap`, or `expect` on user-reachable paths (§1.3 #4).
//! - Cross-platform path handling via `Path::join` only (§1.3 #5).
//! - HTTPS-only for git deps.
//! - Shared cache via `directories` crate.
//! - System `git` binary via `std::process::Command`.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod cache;
pub mod error;
pub mod git;
pub mod path;

mod resolver;

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use cache::cache_root;
pub use error::{PkgError, PkgWarning};
pub use resolver::{resolve_dependencies, DepKind, ResolvedDep};
