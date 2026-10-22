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
//! Callers constructing `RunArgs`, `CheckArgs` or `TestArgs` directly must set
//! their optional `file` field; `None` retains workspace discovery. Use
//! [`Cli::try_parse_invocation_from`] to include the bare-file run shorthand.
//!
//! # Hard constraints
//!
//! - No `panic!` / `unwrap` / `expect` on user-input paths.
//! - Every `pub` item carries rustdoc.
//! - Cross-platform paths via [`std::path::PathBuf::join`] only.

#![warn(missing_docs)]

pub mod cmd;
pub mod error;
pub mod render;
pub mod scaffold;

pub use error::CliError;

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

impl Cli {
    /// Parse an invocation, including `ridge script.ridge [script arguments]`.
    ///
    /// The shorthand also supports `#!/usr/bin/env ridge`: arguments following
    /// the source belong to the program. Explicit subcommands retain their flags.
    ///
    /// # Errors
    /// Returns clap's usual usage or argument error.
    pub fn try_parse_invocation_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let mut args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
        if args.get(1).is_some_and(|arg| {
            !arg.to_string_lossy().starts_with('-')
                && std::path::Path::new(arg)
                    .extension()
                    .is_some_and(|ext| ext == "ridge")
        }) {
            args.insert(1, "run".into());
            args.insert(3, "--".into());
        }
        <Self as Parser>::try_parse_from(args)
    }
}

/// Available `ridge` subcommands.
#[derive(Debug, Subcommand)]
pub enum RidgeCommand {
    /// Compile the current workspace.
    Build(cmd::build::BuildArgs),
    /// Compile and run a workspace or source file on the BEAM runtime.
    Run(cmd::run::RunArgs),
    /// Type-check a workspace or source file without producing output files.
    Check(cmd::check::CheckArgs),
    /// Format Ridge source files according to the standard style.
    Fmt(cmd::fmt::FmtArgs),
    /// Scaffold a new Ridge project in `<name>/`.
    New(cmd::new::NewArgs),
    /// Scaffold a Ridge project in the current directory.
    Init(cmd::init::InitArgs),
    /// Discover and run tests in a workspace or source file.
    ///
    /// Compiles the workspace, then runs each test function in a fresh BEAM
    /// child process.  Reports pass / fail per test.  Exit code is 0 if all
    /// tests pass (or no tests found), 1 otherwise.
    Test(cmd::test::TestArgs),
    /// Manage schema migrations generated from an entity model.
    ///
    /// `ridge migrate add <name>` is the Ridge analogue of EF Core's
    /// `Add-Migration`: it diffs the model against the last snapshot and
    /// writes a new migration file plus a refreshed snapshot.
    Migrate(cmd::migrate::MigrateArgs),
    /// Source-level reload tooling.
    ///
    /// `ridge reload --check` diffs the last build's snapshot against the
    /// current source and reports, per public symbol, whether a reload would
    /// be compatible, need a generated migration, or be incompatible. Exit
    /// code is 0 only when the report is reloadable with no scaffold holes.
    /// `ridge reload --node` applies the upgrade to a running node over
    /// Erlang distribution.
    Reload(cmd::reload::ReloadArgs),
    /// Start an interactive REPL session.
    ///
    /// Reads expressions from stdin, evaluates each one, and prints the result.
    /// Bracket-counting auto-continuation: lines with unbalanced `(`, `[`, `{`
    /// are continued automatically.  Type `:q` to quit.
    ///
    /// Capabilities allowed: `io`, `fs`, `net`, `time`, `random`, `env`,
    /// `proc`, `spawn` (all except `ffi`).
    Repl(cmd::repl::ReplArgs),
    /// Say what a diagnostic code means.
    ///
    /// `ridge explain T031` prints the one-line meaning of a code and the crate
    /// that reports it; `ridge explain --list` prints every code there is.
    /// Needs no workspace — a code means the same thing everywhere.
    Explain(cmd::explain::ExplainArgs),
}
