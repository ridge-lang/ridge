//! Ridge compiler CLI entry point — thin binary that delegates to the
//! `ridge_cli` library crate.
//!
//! All real work lives in `ridge_cli::cmd::*`.  This binary only:
//! 1. Parses CLI arguments via `clap`.
//! 2. Resolves the current working directory.
//! 3. Dispatches to the matching `execute` function.
//! 4. Translates any `CliError` into a non-zero process exit.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process;

use clap::Parser;
use ridge_cli::{cmd, Cli, RidgeCommand};

fn main() {
    let cli = Cli::parse();

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: could not determine current directory: {e}");
            process::exit(1);
        }
    };

    let result = match &cli.command {
        RidgeCommand::Build(args) => cmd::build::execute(args, &cwd),
        RidgeCommand::Run(args) => cmd::run::execute(args, &cwd),
        RidgeCommand::Check(args) => cmd::check::execute(args, &cwd),
        RidgeCommand::Fmt(args) => cmd::fmt::execute(args, &cwd),
        RidgeCommand::New(args) => cmd::new::execute(args, &cwd),
        RidgeCommand::Init(args) => cmd::init::execute(args, &cwd),
        RidgeCommand::Test(args) => cmd::test::execute(args, &cwd),
        RidgeCommand::Repl(args) => cmd::repl::execute(args, &cwd),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
