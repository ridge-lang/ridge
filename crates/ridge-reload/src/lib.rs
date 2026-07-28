//! Workspace snapshots and compatibility checking for source-level reloads.
//!
//! Pure functions only: extraction from compiler data structures, diffing two
//! snapshots, and classifying each change. No filesystem or target-runtime
//! access lives here — callers (driver, CLI) own all I/O.

#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

pub mod render;
pub mod snapshot;
pub mod diff;
