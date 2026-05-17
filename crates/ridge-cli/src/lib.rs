//! Ridge CLI — library crate.
//!
//! Mirrors the binary surface (`ridge build`, `ridge run`, `ridge check`,
//! `ridge new`, `ridge init`) but exposes each subcommand's `execute` plus
//! the shared `scaffold` and `error` modules as `pub` items, so that
//! integration tests can drive the real code paths without spawning the
//! binary.
//!
//! The binary at `src/main.rs` is a thin shim that parses CLI arguments via
//! `clap` and dispatches to these `execute` functions.
//!
//! # Hard constraints (§1.3)
//!
//! - No `panic!` / `unwrap` / `expect` on user-input paths.
//! - Every `pub` item carries rustdoc.
//! - Cross-platform paths via [`std::path::PathBuf::join`] only.

#![warn(missing_docs)]

pub mod cmd;
pub mod error;
pub mod render;
pub mod scaffold;

use clap::{Parser, Subcommand};

// ── Top-level CLI ─────────────────────────────────────────────────────────────

/// The `ridge` compiler and toolchain.
///
/// Run `ridge <COMMAND> --help` for subcommand-specific usage.
#[derive(Debug, Parser)]
#[command(name = "ridge", version, about, long_about = None)]
pub struct Cli {
    /// The subcommand to execute.
    #[command(subcommand)]
    pub command: RidgeCommand,
}

/// Available `ridge` subcommands.
#[derive(Debug, Subcommand)]
pub enum RidgeCommand {
    /// Compile the current workspace.
    Build(cmd::build::BuildArgs),
    /// Compile and run the current workspace on the BEAM runtime.
    Run(cmd::run::RunArgs),
    /// Type-check the current workspace without producing any output files.
    Check(cmd::check::CheckArgs),
    /// Format Ridge source files according to the standard style.
    Fmt(cmd::fmt::FmtArgs),
    /// Scaffold a new Ridge project in `<name>/`.
    New(cmd::new::NewArgs),
    /// Scaffold a Ridge project in the current directory.
    Init(cmd::init::InitArgs),
    /// Discover and run `pub fn test_*` functions in the workspace.
    ///
    /// Compiles the workspace, then runs each test function in a fresh BEAM
    /// child process.  Reports pass / fail per test.  Exit code is 0 if all
    /// tests pass (or no tests found), 1 otherwise.
    Test(cmd::test::TestArgs),
    /// Start an interactive REPL session.
    ///
    /// Reads expressions from stdin, evaluates each one, and prints the result.
    /// Bracket-counting auto-continuation: lines with unbalanced `(`, `[`, `{`
    /// are continued automatically.  Type `:q` to quit.
    ///
    /// Capabilities allowed: `io`, `fs`, `net`, `time`, `random`, `env`,
    /// `proc`, `spawn` (all except `ffi`).
    Repl(cmd::repl::ReplArgs),
}
