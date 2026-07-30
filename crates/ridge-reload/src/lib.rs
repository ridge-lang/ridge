//! Workspace snapshots and compatibility checking for source-level reloads.
//!
//! Pure functions only: extraction from compiler data structures, diffing two
//! snapshots, and classifying each change. No filesystem or target-runtime
//! access lives here — callers (driver, CLI) own all I/O.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod check;
pub mod diff;
pub mod render;
pub mod scaffold;
pub mod snapshot;

// The history a snapshot carries, re-exported so snapshot consumers (the
// driver's reload glue) can thread it into typechecking without depending on
// the shared-types crate directly.
pub use ridge_types::history::VersionHistory;
