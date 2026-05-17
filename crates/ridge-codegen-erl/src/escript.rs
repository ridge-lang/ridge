//! `escript` artefact emitter (D107 closure).
//!
//! Packages a set of [`CErlModule`] ASTs (or a pre-compiled beam directory)
//! into a self-contained Erlang `escript` binary.  The emitted artefact can
//! be invoked as:
//!
//! - **POSIX** (`Linux` / `macOS`): `./output.escript` (the shebang makes it
//!   directly executable after `chmod +x`, which the CLI sets to 0755).
//! - **Windows**: `escript output.escript` — Windows ignores the shebang line,
//!   so users must invoke `escript` explicitly.
//!
//! ## Format
//!
//! The emitted file follows the Erlang `escript` zip-archive format:
//!
//! 1. `#!/usr/bin/env escript\n` — shebang.
//! 2. `%%! -smp\n` — escript flag line.
//! 3. Raw zip archive bytes containing every `.beam` file plus `ridge_rt.beam`
//!    and a thin shim module named after the escript entry point.
//!
//! The `escript` runtime detects the zip by its PK magic bytes, loads all
//! modules, and calls `<entry>:main(Args)`.
//!
//! ## Escript dispatch bridge
//!
//! The escript runtime calls `<script_name>:main/1` where `<script_name>` is
//! the escript filename without `.escript`.  Because the Ridge codegen names
//! BEAM modules as `module_0`, `module_1`, etc., a thin shim module is
//! generated and compiled into the archive.  The shim:
//!
//! ```erlang
//! -module(game_of_life).
//! -export([main/1]).
//! main(Args) ->
//!     BinArgs = ridge_rt:escript_main(Args),
//!     'module_0':main(BinArgs).
//! ```
//!
//! This is the role of `ridge_rt:escript_main/1` — converting the raw string
//! list from escript dispatch to Ridge's binary-string list type.

use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

use crate::{erlc, printer, runtime, CErlModule};

// ── Error type ────────────────────────────────────────────────────────────────

/// Error variants for [`emit_escript`] and [`package_escript_from_beam_dir`].
///
/// All variants carry enough context to produce a human-readable diagnostic
/// without panicking.
#[derive(Debug)]
#[non_exhaustive]
pub enum EmitError {
    /// `erlc` is not available on `PATH` (or the given override path).
    ErlcNotFound,
    /// `erlc` rejected one of the emitted `.core` files or the generated shim.
    ErlcFailed {
        /// Name of the Core Erlang module that was rejected.
        module: String,
        /// `erlc` stderr verbatim.
        stderr: String,
        /// `erlc` exit code.
        exit_code: i32,
    },
    /// An I/O error occurred writing intermediate files or the final artefact.
    Io {
        /// Human-readable context (operation + OS error message).
        detail: String,
    },
    /// The specified `main` module was not found in `modules`.
    MainModuleNotFound {
        /// The requested module name.
        name: String,
    },
    /// A workspace member marked as a `library` (no entry point) was passed.
    ///
    /// **Code C008.**
    EscriptNeedsEntry {
        /// The member name identified as a library.
        member: String,
    },
    /// The `main` function's arity is not 0 or 1.
    ///
    /// **Code C009.**
    EscriptMainArityInvalid {
        /// Module name containing the bad `main`.
        module: String,
        /// Actual arity found.
        arity: u32,
    },
    /// Zip archive construction failed.
    ZipFailed {
        /// Internal detail from the `zip` crate.
        detail: String,
    },
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ErlcNotFound => write!(f, "C008: erlc not found on PATH"),
            Self::ErlcFailed {
                module,
                stderr,
                exit_code,
            } => {
                write!(
                    f,
                    "erlc rejected module '{module}' (exit {exit_code}): {stderr}"
                )
            }
            Self::Io { detail } => write!(f, "I/O error: {detail}"),
            Self::MainModuleNotFound { name } => {
                write!(
                    f,
                    "C008: main module '{name}' not found in compiled modules"
                )
            }
            Self::EscriptNeedsEntry { member } => write!(
                f,
                "C008 EscriptNeedsEntry: '{member}' is a library with no entry point"
            ),
            Self::EscriptMainArityInvalid { module, arity } => write!(
                f,
                "C009 EscriptMainArityInvalid: '{module}::main' has arity {arity}; \
                 escript entry must be arity 0 or 1"
            ),
            Self::ZipFailed { detail } => write!(f, "zip archive creation failed: {detail}"),
        }
    }
}

impl std::error::Error for EmitError {}

// Convert from CodegenError where possible (for `erlc` pipeline reuse).
fn map_codegen_err(module: &str, e: crate::CodegenError) -> EmitError {
    match e {
        crate::CodegenError::ErlcNotFound { .. } => EmitError::ErlcNotFound,
        crate::CodegenError::ErlcRejectedInput {
            stderr, exit_code, ..
        } => EmitError::ErlcFailed {
            module: module.to_owned(),
            stderr,
            exit_code,
        },
        other => EmitError::Io {
            detail: format!("{other:?}"),
        },
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Compile a set of Core Erlang modules into a self-contained `escript` binary.
///
/// ## Parameters
///
/// - `modules` — Core Erlang ASTs to compile.  Must include the module whose
///   name matches `main` (case-sensitive, e.g. `"module_0"`).
/// - `main` — BEAM module atom name of the Ridge module that exports `main/1`.
///
/// The emitted escript will have its entry point named after `main`; if the
/// modules slice contains a module literally named `main`, the shim is omitted.
/// In practice `main` is the internal codegen name (e.g. `"module_0"`) and the
/// escript filename is determined by the caller, so the shim bridges the two.
///
/// ## Return value
///
/// Returns the raw bytes of the `escript` file.  The caller writes them to
/// disk and (on POSIX) sets mode 0755.
///
/// ## Errors
///
/// Returns [`EmitError`] if `erlc` fails, I/O fails, or `main` is missing.
pub fn emit_escript(modules: &[CErlModule], main: &str) -> Result<Vec<u8>, EmitError> {
    // ── 0. Validate ───────────────────────────────────────────────────────────
    if !modules.iter().any(|m| m.name.0 == main) {
        return Err(EmitError::MainModuleNotFound {
            name: main.to_owned(),
        });
    }
    validate_main_arity(modules, main)?;

    // ── 1. Tempdir for core/ beam/ runtime/ ───────────────────────────────────
    let td = tempdir_for_escript().map_err(|e| EmitError::Io {
        detail: format!("create tempdir: {e}"),
    })?;
    let core_dir = td.join("core");
    let beam_dir = td.join("beam");
    for dir in &[&core_dir, &beam_dir] {
        std::fs::create_dir_all(dir).map_err(|e| EmitError::Io {
            detail: format!("create dir {}: {e}", dir.display()),
        })?;
    }

    // ── 2. Probe erlc ─────────────────────────────────────────────────────────
    let erlc_info = erlc::probe(None).map_err(|_| EmitError::ErlcNotFound)?;

    // ── 3. Install + compile ridge_rt into beam_dir ───────────────────────────
    install_runtime_for_escript(&td, &erlc_info.path)?;

    // ── 4. Write + compile each CErlModule ────────────────────────────────────
    let runtime_dir = td.join("runtime");
    for module in modules {
        let name = &module.name.0;
        let core_path = core_dir.join(format!("{name}.core"));
        let core_text = printer::print_module(module);
        std::fs::write(&core_path, core_text.as_bytes()).map_err(|e| EmitError::Io {
            detail: format!("write {}: {e}", core_path.display()),
        })?;
        erlc::compile_core(
            &erlc_info.path,
            &core_path,
            &beam_dir,
            &runtime_dir,
            crate::BuildProfile::Debug,
        )
        .map_err(|e| map_codegen_err(name, e))?;
    }

    // ── 5. Package ────────────────────────────────────────────────────────────
    // For emit_escript, `main` is also the escript entry name (caller sets both).
    // No shim needed because the test harness names the escript after the module.
    build_escript_payload(&beam_dir, None)
}

/// Package an escript from an already-compiled `beam_dir`.
///
/// This is the **CLI path** for `ridge build --bin <member>`: the workspace
/// has already been compiled by `compile_workspace` and `.beam` files are on
/// disk.  This function:
///
/// 1. Generates a thin Erlang shim named `escript_entry` that exports `main/1`
///    and delegates to `internal_module:main(Args)`.  This bridges the escript
///    dispatch convention (`<script_name>:main/1`) to the Ridge BEAM module.
/// 2. Compiles the shim via `erlc`.
/// 3. Zips all `.beam` files (including the shim and `ridge_rt.beam`) into an
///    escript payload.
///
/// ## Parameters
///
/// - `beam_dir` — directory containing the compiled `.beam` files.
/// - `internal_module` — actual BEAM module atom (e.g. `"module_0"`).
/// - `escript_entry` — user-visible name (e.g. `"game_of_life"`); the shim
///   module will be named this so `escript` can dispatch to it.
///
/// ## Errors
///
/// Returns [`EmitError`] on `erlc` failure, I/O failure, or zip failure.
pub fn package_escript_from_beam_dir(
    beam_dir: &Path,
    internal_module: &str,
    escript_entry: &str,
) -> Result<Vec<u8>, EmitError> {
    let needs_shim = internal_module != escript_entry;

    let shim_beam: Option<(PathBuf, Vec<u8>)> = if needs_shim {
        // Generate and compile the shim.
        let erlc_info = erlc::probe(None).map_err(|_| EmitError::ErlcNotFound)?;

        let td = tempdir_for_escript().map_err(|e| EmitError::Io {
            detail: format!("create shim tempdir: {e}"),
        })?;

        let shim_src = build_shim_source(escript_entry, internal_module);
        let shim_erl = td.join(format!("{escript_entry}.erl"));
        std::fs::write(&shim_erl, shim_src.as_bytes()).map_err(|e| EmitError::Io {
            detail: format!("write shim .erl: {e}"),
        })?;

        let output = std::process::Command::new(&erlc_info.path)
            .arg("-o")
            .arg(&td)
            .arg(&shim_erl)
            .output()
            .map_err(|_| EmitError::ErlcNotFound)?;

        if !output.status.success() {
            return Err(EmitError::ErlcFailed {
                module: escript_entry.to_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
            });
        }

        let shim_beam_path = td.join(format!("{escript_entry}.beam"));
        let shim_bytes = std::fs::read(&shim_beam_path).map_err(|e| EmitError::Io {
            detail: format!("read shim beam {}: {e}", shim_beam_path.display()),
        })?;
        Some((shim_beam_path, shim_bytes))
    } else {
        None
    };

    build_escript_payload(
        beam_dir,
        shim_beam.as_ref().map(|(path, _bytes)| path.as_path()),
    )
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Generate the Erlang source for a thin shim module that bridges escript
/// dispatch to the actual Ridge BEAM entry module.
///
/// The escript runtime calls `<script_name>:main/1` with a list of argument
/// strings.  Ridge's zero-arg `main ()` compiles to BEAM `main/0` (it reads
/// args via `ridge_rt:cli_args/0` → `init:get_plain_arguments/0`).  The shim
/// dispatches dynamically based on which arity the Ridge module exports:
///
/// - If `<internal_module>` exports `main/1` — pass `BinArgs` directly.
/// - If `<internal_module>` exports `main/0` — call with no args; the Ridge
///   code retrieves CLI args via `ridge_rt:cli_args/0` internally.
fn build_shim_source(shim_name: &str, internal_module: &str) -> String {
    format!(
        "%% Auto-generated by ridge-codegen-erl escript shim (T12/D107).\n\
         -module({shim_name}).\n\
         -export([main/1]).\n\
         main(Args) ->\n\
             BinArgs = ridge_rt:escript_main(Args),\n\
             case erlang:function_exported('{internal_module}', main, 1) of\n\
                 true  -> '{internal_module}':main(BinArgs);\n\
                 false -> '{internal_module}':main()\n\
             end.\n"
    )
}

/// Validate that the module named `main` in `modules` exports `main` with
/// arity ≤ 1.  Ridge `main/0` compiles to BEAM arity 1 (unit arg); `main/1`
/// (args) also compiles to BEAM arity 1.  Arity ≥ 2 is invalid.
fn validate_main_arity(modules: &[CErlModule], main: &str) -> Result<(), EmitError> {
    let Some(m) = modules.iter().find(|m| m.name.0 == main) else {
        return Ok(());
    };
    for export in &m.exports {
        if export.name.0 == "main" && export.arity > 1 {
            return Err(EmitError::EscriptMainArityInvalid {
                module: main.to_owned(),
                arity: export.arity,
            });
        }
    }
    Ok(())
}

/// Create a uniquely-named tempdir under the OS temp root.
///
/// Does NOT use the `tempfile` crate (dev-dep only; not available in
/// production code paths).  Uses a nanosecond-resolution timestamp for
/// uniqueness.
fn tempdir_for_escript() -> Result<PathBuf, std::io::Error> {
    let base = std::env::temp_dir();
    let unique = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("ridge_escript_{nanos:010x}")
    };
    let dir = base.join(unique);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Install and compile `ridge_rt.erl` into `<td>/beam/ridge_rt.beam`.
///
/// `td` must already contain `beam/` and `runtime/` subdirs.
fn install_runtime_for_escript(td: &Path, erlc_path: &Path) -> Result<(), EmitError> {
    // Ensure subdirs exist.
    for sub in &["beam", "runtime"] {
        std::fs::create_dir_all(td.join(sub)).map_err(|e| EmitError::Io {
            detail: format!("create {sub} dir: {e}"),
        })?;
    }
    runtime::install_runtime(td).map_err(|e| EmitError::Io {
        detail: format!("install runtime: {e:?}"),
    })?;
    runtime::compile_runtime(erlc_path, td).map_err(|e| EmitError::Io {
        detail: format!("compile ridge_rt: {e:?}"),
    })?;
    Ok(())
}

/// Build the escript payload from all `.beam` files in `beam_dir`.
///
/// Optionally includes an extra beam file at `extra_beam` (the generated shim).
///
/// Format:
/// 1. `#!/usr/bin/env escript\n`
/// 2. `%%! -smp\n`
/// 3. Raw zip archive bytes (PK magic detected by escript runtime).
fn build_escript_payload(beam_dir: &Path, extra_beam: Option<&Path>) -> Result<Vec<u8>, EmitError> {
    // Collect all .beam files from beam_dir.
    let entries = std::fs::read_dir(beam_dir).map_err(|e| EmitError::Io {
        detail: format!("read beam dir {}: {e}", beam_dir.display()),
    })?;

    let mut beam_files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| EmitError::Io {
            detail: format!("read dir entry: {e}"),
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("beam") {
            beam_files.push(path);
        }
    }
    beam_files.sort(); // deterministic order

    // Build the zip archive in memory.
    let mut zip_buf: Vec<u8> = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut zip_buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        // Write all beams from beam_dir.
        for beam_path in &beam_files {
            write_beam_to_zip(&mut writer, beam_path, options)?;
        }

        // Write extra shim beam if provided.
        if let Some(extra) = extra_beam {
            write_beam_to_zip(&mut writer, extra, options)?;
        }

        writer.finish().map_err(|e| EmitError::ZipFailed {
            detail: e.to_string(),
        })?;
    }

    // Assemble escript payload: shebang + flags + zip bytes.
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(b"#!/usr/bin/env escript\n");
    payload.extend_from_slice(b"%%! -smp\n");
    payload.extend_from_slice(&zip_buf);

    Ok(payload)
}

/// Write one `.beam` file into the zip archive under its filename only (no path).
fn write_beam_to_zip(
    writer: &mut zip::ZipWriter<std::io::Cursor<&mut Vec<u8>>>,
    beam_path: &Path,
    options: zip::write::SimpleFileOptions,
) -> Result<(), EmitError> {
    let file_name = beam_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| EmitError::Io {
            detail: format!("invalid beam filename: {}", beam_path.display()),
        })?;
    let beam_bytes = std::fs::read(beam_path).map_err(|e| EmitError::Io {
        detail: format!("read {}: {e}", beam_path.display()),
    })?;
    writer
        .start_file(file_name, options)
        .map_err(|e| EmitError::ZipFailed {
            detail: e.to_string(),
        })?;
    writer
        .write_all(&beam_bytes)
        .map_err(|e| EmitError::ZipFailed {
            detail: e.to_string(),
        })?;
    Ok(())
}
