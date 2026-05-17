//! `ridge build` — compile a Ridge workspace.
//!
//! ## Surface
//!
//! ```text
//! ridge build [--member <name>] [--release] [--emit beam|core|both] [--bin <member>]
//! ```
//!
//! Loads the workspace root via [`ridge_manifest::find_workspace_root`] and
//! delegates to [`ridge_driver::compile_workspace`].
//!
//! When `--bin <member>` is specified, the workspace is compiled to `.beam`
//! and then packaged into a self-contained `escript` artefact at
//! `target/ridge/<profile>/<member>.escript`.  On Windows users invoke it as
//! `escript <name>.escript`; on POSIX the file is marked executable (0755).

use std::path::Path;
use std::time::Instant;

use clap::Parser;
use ridge_codegen_erl::escript::package_escript_from_beam_dir;
use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts, Profile};
use ridge_manifest::find_workspace_root;

use crate::error::CliError;
use crate::render::render_diagnostics;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Compile the current workspace.
///
/// Produces `.beam` files (default) or `.core` files in
/// `target/ridge/<profile>/`.  Use `--bin <member>` to additionally package
/// the named member as a self-contained `escript` artefact.
#[derive(Debug, Parser)]
pub struct BuildArgs {
    /// Only compile the named workspace member.
    #[arg(long, value_name = "NAME")]
    pub member: Option<String>,

    /// Compile in release mode (enables BEAM optimisations).
    #[arg(long)]
    pub release: bool,

    /// Choose which artefacts to emit.
    ///
    /// `beam` (default) — `.beam` files only.
    /// `core` — Core Erlang text files only, no BEAM compilation.
    /// `both` — `.beam` and `.core` files.
    #[arg(long, value_name = "beam|core|both", default_value = "beam")]
    pub emit: EmitChoice,

    /// Package the named workspace member as a self-contained `escript` artefact.
    ///
    /// Writes `target/ridge/<profile>/<member>.escript`.  On POSIX the file is
    /// marked executable (0755); on Windows invoke it as `escript <name>.escript`.
    ///
    /// The member must have an `entry` (`kind = "app"` or `kind = "service"`)
    /// and its `main` function must accept 0 or 1 argument.
    #[arg(long, value_name = "MEMBER")]
    pub bin: Option<String>,
}

/// User-facing emit choice for `--emit`.
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum EmitChoice {
    /// Produce `.beam` files only (default).
    Beam,
    /// Produce Core Erlang `.core` files only.
    Core,
    /// Produce both `.beam` and `.core` files.
    Both,
}

impl From<EmitChoice> for EmitArtefacts {
    fn from(c: EmitChoice) -> Self {
        match c {
            EmitChoice::Beam => Self::Beam,
            EmitChoice::Core => Self::Core,
            EmitChoice::Both => Self::Both,
        }
    }
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge build`.
///
/// When `args.bin` is `Some(member)`, also emits a self-contained `escript`
/// artefact at `target/ridge/<profile>/<member>.escript` after compilation.
///
/// # Errors
///
/// Returns `1` (via process exit) on build failure.  Diagnostics are printed
/// to stderr before returning.
pub fn execute(args: &BuildArgs, cwd: &Path) -> Result<(), CliError> {
    // ── 1. Locate workspace root ──────────────────────────────────────────────
    let workspace_root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;

    // ── 2. Build options ──────────────────────────────────────────────────────
    let profile = if args.release {
        Profile::Release
    } else {
        Profile::Debug
    };

    // When --bin is set, we need .beam output regardless of --emit.
    let emit = if args.bin.is_some() {
        EmitArtefacts::Beam
    } else {
        EmitArtefacts::from(args.emit.clone())
    };

    // If --bin specifies a member, restrict compilation to that member.
    let member_filter = args.bin.as_ref().or(args.member.as_ref());
    let mut opts = CompileOptions::new(workspace_root.clone())
        .with_profile(profile)
        .with_emit(emit);
    opts.members = member_filter.map(|m| vec![m.clone()]);

    // ── 3. Compile ────────────────────────────────────────────────────────────
    let start = Instant::now();
    let artefacts = compile_workspace(opts).map_err(|e| {
        eprintln!("error: {e}");
        // Return a NoWorkspaceRoot so the caller can propagate; the real
        // error is already printed.
        CliError::NoWorkspaceRoot
    })?;

    // ── 4. Render non-fatal diagnostics ───────────────────────────────────────
    if !artefacts.diagnostics.is_empty() {
        render_diagnostics(&artefacts.diagnostics, &artefacts.sources);
        return Err(CliError::NoWorkspaceRoot); // non-zero exit on warnings/errors
    }

    // ── 5. Emit escript artefact (--bin path) ─────────────────────────────────
    if let Some(bin_member) = &args.bin {
        emit_escript_artefact(&workspace_root, bin_member, profile, &artefacts.beam_files)?;
    }

    // ── 6. Success banner ─────────────────────────────────────────────────────
    let n = artefacts.beam_files.len() + artefacts.core_files.len();
    let elapsed = start.elapsed().as_millis();
    println!("Compiled {n} module(s) in {elapsed}ms");

    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Package the compiled workspace as an escript and write it to
/// `<workspace_root>/target/ridge/<profile>/<member>.escript`.
///
/// On POSIX systems the file is marked 0755; on Windows the mode is not set
/// (users invoke `escript <name>.escript`).
///
/// # Errors
///
/// Returns [`CliError`] if no `.beam` files were produced, the escript
/// packaging fails, or writing the output file fails.
fn emit_escript_artefact(
    workspace_root: &Path,
    member: &str,
    profile: Profile,
    beam_files: &[std::path::PathBuf],
) -> Result<(), CliError> {
    if beam_files.is_empty() {
        eprintln!("error: no .beam files produced for member '{member}'");
        return Err(CliError::NoWorkspaceRoot);
    }

    // Locate the beam directory from the first beam file.
    let beam_dir = beam_files[0].parent().ok_or_else(|| {
        eprintln!("error: beam file has no parent directory");
        CliError::NoWorkspaceRoot
    })?;

    // Derive the main BEAM module name from the first beam file stem.
    // The member's primary module is beam_files[0] (driver orders it first).
    let main_module = beam_files[0]
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(member);

    // Package the escript payload.
    // main_module is the internal BEAM atom (e.g. "module_0").
    // member is the user-visible escript entry name.
    let payload = package_escript_from_beam_dir(beam_dir, main_module, member).map_err(|e| {
        eprintln!("error: escript packaging failed: {e}");
        CliError::NoWorkspaceRoot
    })?;

    // Determine output path: target/ridge/<profile>/<member>.escript
    // The `Profile::Debug` and `_` arms both produce "debug" on purpose:
    // `Profile` is `#[non_exhaustive]` and future variants default to the
    // debug layout.  Disable `match_same_arms` (otherwise it suggests
    // collapsing the explicit `Debug` arm into the wildcard, which would
    // remove the documentation of the intent).
    #[allow(clippy::match_same_arms)]
    let profile_dir = match profile {
        Profile::Debug => "debug",
        Profile::Release => "release",
        // Profile is #[non_exhaustive]; future variants default to debug layout.
        _ => "debug",
    };
    let out_dir = workspace_root
        .join("target")
        .join("ridge")
        .join(profile_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| {
        eprintln!(
            "error: could not create output directory {}: {e}",
            out_dir.display()
        );
        CliError::NoWorkspaceRoot
    })?;

    let out_path = out_dir.join(format!("{member}.escript"));
    std::fs::write(&out_path, &payload).map_err(|e| {
        eprintln!("error: could not write escript {}: {e}", out_path.display());
        CliError::NoWorkspaceRoot
    })?;

    // On POSIX: set file mode 0755 so it is directly executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&out_path, perms).map_err(|e| {
            eprintln!("error: could not set escript permissions: {e}");
            CliError::NoWorkspaceRoot
        })?;
    }

    println!("Wrote escript: {}", out_path.display());
    Ok(())
}
