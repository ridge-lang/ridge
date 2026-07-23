//! Runtime bridge installer — copies the bundled `ridge_rt.erl` and
//! `ridge_test_runner.erl` into the output directory and compiles them to
//! `.beam` so they are available on the BEAM code path at runtime.

use crate::CodegenError;
use std::path::{Path, PathBuf};

/// The bundled `ridge_rt.erl` source, embedded at compile time.
const RIDGE_RT_SOURCE: &str = include_str!("../runtime/ridge_rt.erl");

/// The bundled `ridge_test_runner.erl` source, embedded at compile time.
const RIDGE_TEST_RUNNER_SOURCE: &str = include_str!("../runtime/ridge_test_runner.erl");

/// The bundled `ridge_main_runner.erl` source, embedded at compile time.
const RIDGE_MAIN_RUNNER_SOURCE: &str = include_str!("../runtime/ridge_main_runner.erl");

/// The bundled `ridge_pg.erl` source, embedded at compile time.
///
/// The first-party `PostgreSQL` client backing the `std.data` Postgres adapter.
/// Installed and compiled on every build so its `.beam` is on the code path
/// whenever a program opens a Postgres connection.
const RIDGE_PG_SOURCE: &str = include_str!("../runtime/ridge_pg.erl");

/// The bundled `ridge_sup.erl` source, embedded at compile time.
///
/// The OTP supervisor callback module `ridge_rt:start_supervisor/4` starts
/// through. Installed and compiled on every build so its `.beam` is on
/// the code path whenever a program calls `std.actor.supervise`.
const RIDGE_SUP_SOURCE: &str = include_str!("../runtime/ridge_sup.erl");

/// The bundled `ridge_bench_runner.erl` source, embedded at compile time.
///
/// Unlike the other runners this is *not* installed on every build — it is only
/// needed when running Layer B micro-benchmarks, so the benchmark harness opts
/// in via [`install_bench_runner`] / [`compile_bench_runner`].
const RIDGE_BENCH_RUNNER_SOURCE: &str = include_str!("../runtime/ridge_bench_runner.erl");

/// The bundled `ridge_sqlite.erl` source — the SQLite adapter runtime that loads
/// and drives the native bridge. Installed and compiled only under the
/// `beam-runtime` feature, alongside the baked NIF object it loads.
#[cfg(feature = "beam-runtime")]
const RIDGE_SQLITE_SOURCE: &str = include_str!("../runtime/ridge_sqlite.erl");

/// The SQLite NIF object, compiled by `build.rs` from the vendored amalgamation
/// and baked in so that running a Ridge program never needs a C toolchain. Only
/// present under `beam-runtime`, the same gate `build.rs` compiles it behind.
#[cfg(feature = "beam-runtime")]
const SQLITE_NIF_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ridge_sqlite_nif.bin"));

/// Information about the installed runtime.
#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    /// Absolute (or output-root-relative) path to the installed `ridge_rt.erl`.
    pub erl_path: PathBuf,
    /// Absolute path to the compiled `ridge_rt.beam` (produced by `erlc`).
    /// `None` if erlc was not invoked or compilation failed.
    pub beam_path: Option<PathBuf>,
}

/// Install the bundled `ridge_rt.erl` runtime under `<out_root>/runtime/`.
///
/// Idempotent — if the destination file already exists and its contents match
/// the embedded bytes, the file is not rewritten (mtime is preserved).
///
/// I/O failures surface as [`CodegenError::OutputDirNotWritable`].
pub fn install_runtime(out_root: &Path) -> Result<RuntimeInfo, CodegenError> {
    let runtime_dir = out_root.join("runtime");
    std::fs::create_dir_all(&runtime_dir).map_err(|e| CodegenError::OutputDirNotWritable {
        path: runtime_dir.clone(),
        io_err: e.to_string(),
    })?;

    let erl_path = runtime_dir.join("ridge_rt.erl");
    let embedded = RIDGE_RT_SOURCE.as_bytes();

    // Idempotent: skip write if existing content matches.
    if erl_path.exists() {
        match std::fs::read(&erl_path) {
            Ok(existing) if existing == embedded => {
                return Ok(RuntimeInfo {
                    erl_path,
                    beam_path: None,
                });
            }
            _ => {}
        }
    }

    std::fs::write(&erl_path, embedded).map_err(|e| CodegenError::OutputDirNotWritable {
        path: erl_path.clone(),
        io_err: e.to_string(),
    })?;

    // Also install ridge_test_runner.erl (T9 test runner bridge).
    install_runner_source(
        &runtime_dir,
        "ridge_test_runner.erl",
        RIDGE_TEST_RUNNER_SOURCE,
    )?;

    // And ridge_main_runner.erl, used by `ridge run` to project main()'s
    // Result return into an exit code (added 0.2.2).
    install_runner_source(
        &runtime_dir,
        "ridge_main_runner.erl",
        RIDGE_MAIN_RUNNER_SOURCE,
    )?;

    // And ridge_pg.erl, the first-party PostgreSQL client backing the
    // std.data Postgres adapter.
    install_runner_source(&runtime_dir, "ridge_pg.erl", RIDGE_PG_SOURCE)?;

    // And ridge_sup.erl, the OTP supervisor callback module behind
    // std.actor.supervise.
    install_runner_source(&runtime_dir, "ridge_sup.erl", RIDGE_SUP_SOURCE)?;

    // And ridge_sqlite.erl, the SQLite adapter runtime — only when SQLite
    // support is built in (the baked NIF exists only under `beam-runtime`).
    #[cfg(feature = "beam-runtime")]
    install_runner_source(&runtime_dir, "ridge_sqlite.erl", RIDGE_SQLITE_SOURCE)?;

    Ok(RuntimeInfo {
        erl_path,
        beam_path: None,
    })
}

/// Install a bundled runner `.erl` source under `<runtime_dir>/<name>`.
///
/// Idempotent — skips the write when the destination already matches.
fn install_runner_source(
    runtime_dir: &Path,
    name: &str,
    embedded_source: &str,
) -> Result<(), CodegenError> {
    let dest = runtime_dir.join(name);
    let embedded = embedded_source.as_bytes();
    if dest.exists() {
        if let Ok(existing) = std::fs::read(&dest) {
            if existing == embedded {
                return Ok(());
            }
        }
    }
    std::fs::write(&dest, embedded).map_err(|e| CodegenError::OutputDirNotWritable {
        path: dest,
        io_err: e.to_string(),
    })
}

/// Install the bundled `ridge_bench_runner.erl` under `<out_root>/runtime/`.
///
/// Separate from [`install_runtime`] because the bench runner is only needed by
/// the Layer B benchmark harness, not by ordinary `ridge build` / `ridge run`.
/// Idempotent — skips the write when the destination already matches.
///
/// I/O failures surface as [`CodegenError::OutputDirNotWritable`].
pub fn install_bench_runner(out_root: &Path) -> Result<PathBuf, CodegenError> {
    let runtime_dir = out_root.join("runtime");
    std::fs::create_dir_all(&runtime_dir).map_err(|e| CodegenError::OutputDirNotWritable {
        path: runtime_dir.clone(),
        io_err: e.to_string(),
    })?;
    install_runner_source(
        &runtime_dir,
        "ridge_bench_runner.erl",
        RIDGE_BENCH_RUNNER_SOURCE,
    )?;
    Ok(runtime_dir.join("ridge_bench_runner.erl"))
}

/// Compile the installed `ridge_bench_runner.erl` to `ridge_bench_runner.beam`.
///
/// Companion to [`install_bench_runner`]; the `.beam` lands in
/// `<out_root>/beam/` so `erl -pa <beam_dir>` can load it. Idempotent.
///
/// Errors during `erlc` surface as [`CodegenError::ErlcRejectedInput`].
pub fn compile_bench_runner(erlc_path: &Path, out_root: &Path) -> Result<PathBuf, CodegenError> {
    let beam_out_dir = out_root.join("beam");
    std::fs::create_dir_all(&beam_out_dir).map_err(|e| CodegenError::OutputDirNotWritable {
        path: beam_out_dir.clone(),
        io_err: e.to_string(),
    })?;
    compile_runner_if_missing(
        erlc_path,
        out_root,
        &beam_out_dir,
        "ridge_bench_runner.erl",
        "ridge_bench_runner.beam",
    )?;
    Ok(beam_out_dir.join("ridge_bench_runner.beam"))
}

/// Compile the installed `ridge_rt.erl` to `ridge_rt.beam` using `erlc`.
///
/// Also compiles `ridge_test_runner.erl` → `ridge_test_runner.beam` (T9).
/// The resulting `.beam` files are placed in `<out_root>/beam/` alongside
/// user modules.  This ensures `erl -pa <beam_dir>` can find both at runtime.
///
/// Idempotent — if `ridge_rt.beam` already exists in the beam dir, it is
/// returned immediately (no re-compilation).
///
/// Errors during `erlc` invocation are returned as [`CodegenError::ErlcRejectedInput`].
pub fn compile_runtime(erlc_path: &Path, out_root: &Path) -> Result<PathBuf, CodegenError> {
    let beam_out_dir = out_root.join("beam");

    // ── Compile ridge_rt.erl ──────────────────────────────────────────────────
    let rt_erl_path = out_root.join("runtime").join("ridge_rt.erl");
    let rt_beam_path = beam_out_dir.join("ridge_rt.beam");

    if !rt_beam_path.exists() {
        let output = std::process::Command::new(erlc_path)
            .arg("-o")
            .arg(&beam_out_dir)
            .arg(&rt_erl_path)
            .output()
            .map_err(|_| CodegenError::ErlcNotFound {
                searched_paths: vec![],
            })?;

        if !output.status.success() {
            return Err(CodegenError::ErlcRejectedInput {
                core_path: rt_erl_path,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
            });
        }
    }

    // ── Compile ridge_test_runner.erl (T9) ────────────────────────────────────
    compile_runner_if_missing(
        erlc_path,
        out_root,
        &beam_out_dir,
        "ridge_test_runner.erl",
        "ridge_test_runner.beam",
    )?;

    // ── Compile ridge_main_runner.erl (0.2.2 main() Err projection) ───────────
    compile_runner_if_missing(
        erlc_path,
        out_root,
        &beam_out_dir,
        "ridge_main_runner.erl",
        "ridge_main_runner.beam",
    )?;

    // ── Compile ridge_pg.erl (PostgreSQL client for the std.data adapter) ─────
    compile_runner_if_missing(
        erlc_path,
        out_root,
        &beam_out_dir,
        "ridge_pg.erl",
        "ridge_pg.beam",
    )?;

    // ── Compile ridge_sup.erl (OTP supervisor callback) ───────────────────────
    compile_runner_if_missing(
        erlc_path,
        out_root,
        &beam_out_dir,
        "ridge_sup.erl",
        "ridge_sup.beam",
    )?;

    // ── Compile ridge_sqlite.erl + write the baked NIF (SQLite runtime) ───────
    #[cfg(feature = "beam-runtime")]
    install_sqlite_runtime(erlc_path, out_root, &beam_out_dir)?;

    Ok(rt_beam_path)
}

/// Compile `ridge_sqlite.erl` and write the baked NIF object beside it, so
/// `erl -pa <beam_dir>` loads the glue and its `-on_load` finds the native
/// object as a real file next to the `.beam`.
#[cfg(feature = "beam-runtime")]
fn install_sqlite_runtime(
    erlc_path: &Path,
    out_root: &Path,
    beam_out_dir: &Path,
) -> Result<(), CodegenError> {
    compile_runner_if_missing(
        erlc_path,
        out_root,
        beam_out_dir,
        "ridge_sqlite.erl",
        "ridge_sqlite.beam",
    )?;
    let nif_path = beam_out_dir.join(sqlite_nif_filename());
    write_bytes_if_changed(&nif_path, SQLITE_NIF_BYTES)
}

/// The on-disk name `erlang:load_nif` resolves for the SQLite bridge: BEAM
/// appends `.dll` on Windows and `.so` everywhere else (including macOS).
#[cfg(feature = "beam-runtime")]
const fn sqlite_nif_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "ridge_sqlite.dll"
    } else {
        "ridge_sqlite.so"
    }
}

/// Write `bytes` to `dest` unless it already holds them — idempotent and
/// mtime-stable, the binary counterpart of [`install_runner_source`].
#[cfg(feature = "beam-runtime")]
fn write_bytes_if_changed(dest: &Path, bytes: &[u8]) -> Result<(), CodegenError> {
    if dest.exists() {
        if let Ok(existing) = std::fs::read(dest) {
            if existing == bytes {
                return Ok(());
            }
        }
    }
    std::fs::write(dest, bytes).map_err(|e| CodegenError::OutputDirNotWritable {
        path: dest.to_path_buf(),
        io_err: e.to_string(),
    })
}

/// Compile a single bundled runner under `runtime/` to `beam/`.
///
/// Idempotent — skips compilation if the `.beam` already exists.  Returns the
/// path of the produced `.beam` on success.
fn compile_runner_if_missing(
    erlc_path: &Path,
    out_root: &Path,
    beam_out_dir: &Path,
    erl_name: &str,
    beam_name: &str,
) -> Result<(), CodegenError> {
    let erl_path = out_root.join("runtime").join(erl_name);
    let beam_path = beam_out_dir.join(beam_name);

    if !erl_path.exists() || beam_path.exists() {
        return Ok(());
    }

    let output = std::process::Command::new(erlc_path)
        .arg("-o")
        .arg(beam_out_dir)
        .arg(&erl_path)
        .output()
        .map_err(|_| CodegenError::ErlcNotFound {
            searched_paths: vec![],
        })?;

    if !output.status.success() {
        return Err(CodegenError::ErlcRejectedInput {
            core_path: erl_path,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        });
    }

    Ok(())
}
