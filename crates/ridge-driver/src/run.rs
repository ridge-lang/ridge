//! Implementation of [`run_workspace`].
//!
//! Compiles the workspace then invokes the BEAM runtime via
//! `ridge_main_runner:run/1`, which pattern-matches the user's `main()`
//! return value and exits with a code that respects the conventional
//! `Result Unit Text` shape: `Err _` → exit 1 + stderr, anything else → exit 0.
//!
//! BEAM invocation via `erl`, not `escript`.
//! Beam dir: `<workspace_root>/target/ridge/<profile>/beam/`.

use std::io::Read;
use std::path::Path;
use std::process::Command;

use crate::compile::compile_workspace;
use crate::error::{ProcessExitCode, RunError};
use crate::options::{CompileOptions, EmitArtefacts, RunOptions};

/// Timeout in seconds for the `erl` child process (60 s per plan §9).
const ERL_TIMEOUT_SECS: u64 = 60;

/// Compile and run a Ridge workspace on the BEAM runtime.
///
/// ## Flow
///
/// 1. Call [`compile_workspace`] with a [`CompileOptions`] derived from
///    `options`.
/// 2. Probe `erl` via `PATH`; surface `C004` if not found.
/// 3. Resolve the BEAM module name from `options.main_module` or the first
///    `.beam` file produced.
/// 4. Invoke `erl -noshell -pa <beam_dir> -s <module> start -s init stop`.
/// 5. Return `Ok(ProcessExitCode(0))` on exit-0 or `RunError::ErlExitNonZero`
///    on non-zero.
///
/// ## Errors
///
/// - [`RunError::CompileFailed`] — upstream compile error.
/// - [`RunError::ErlangNotFound`] — `erl` not on PATH (C004).
/// - [`RunError::NoBeamModule`] — codegen produced no `.beam` output.
/// - [`RunError::ErlExitNonZero`] — BEAM node exited non-zero.
/// - [`RunError::SpawnFailed`] — OS could not spawn `erl`.
#[allow(clippy::needless_pass_by_value)]
pub fn run_workspace(options: RunOptions) -> Result<ProcessExitCode, RunError> {
    // ── 1. Compile ────────────────────────────────────────────────────────────
    let compile_opts = CompileOptions {
        workspace_root: options.workspace_root.clone(),
        members: None,
        profile: options.profile,
        emit: EmitArtefacts::Beam,
        cache_root: None,
    };
    let artefacts = compile_workspace(compile_opts)?;

    // ── 2. Probe erl ─────────────────────────────────────────────────────────
    // C004 ErlangNotFound is probed once here, not in compile_workspace
    // (compile does not need `erl`; run does).
    let erl_path = probe_erl()?;

    // ── 3. Resolve beam dir and module name ───────────────────────────────────
    if artefacts.beam_files.is_empty() {
        return Err(RunError::NoBeamModule);
    }

    // All beam files land in `<workspace_root>/target/ridge/<profile>/beam/`.
    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_owned();

    let module_name = options.main_module.as_ref().map_or_else(
        || {
            beam_file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("ridge_module_0")
                .to_owned()
        },
        std::clone::Clone::clone,
    );

    // ── 4. Invoke erl ────────────────────────────────────────────────────────
    //
    // The entry function is dispatched through `ridge_main_runner:run/1`,
    // which calls `<module>:<entry_fn>()` and projects the return value:
    // an `Err _` causes a non-zero exit + stderr; anything else exits 0.
    // The runner is installed alongside `ridge_rt.beam` at compile time
    // (see `ridge_codegen_erl::runtime`).  Without it the BEAM would ignore
    // `main`'s return value and `ridge run && next-step` would proceed even
    // after an `Err` from the program.
    let entry_fn = options.entry_fn.as_deref().unwrap_or("main");

    let mut cmd = Command::new(&erl_path);
    cmd.arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-s")
        .arg("ridge_main_runner")
        .arg("run")
        .arg(&module_name)
        .arg(entry_fn)
        .arg("-s")
        .arg("init")
        .arg("stop");

    if !options.extra_args.is_empty() {
        cmd.arg("-extra");
        for arg in &options.extra_args {
            cmd.arg(arg);
        }
    }

    // Inherit stdout so the BEAM program's `Io.println` calls reach the
    // user's terminal as they happen, rather than being buffered into a pipe
    // and dumped at exit. Stderr stays piped so crash dumps remain available
    // for inclusion in `RunError::ErlExitNonZero` on failure.
    let mut child = cmd
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| RunError::SpawnFailed {
            message: e.to_string(),
        })?;

    // ── 5. Wait with timeout ──────────────────────────────────────────────────
    let timeout = std::time::Duration::from_secs(ERL_TIMEOUT_SECS);
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr = drain_pipe(child.stderr.take());
                let code = status.code().unwrap_or(-1);
                if code == 0 {
                    // Stderr is still piped; relay anything that landed there
                    // (warnings, etc.) so it isn't dropped on success.
                    use std::io::Write;
                    let _ = std::io::stderr().write_all(stderr.as_bytes());
                    return Ok(ProcessExitCode(0));
                }
                return Err(RunError::ErlExitNonZero {
                    code,
                    // Stdout was inherited and already on the user's terminal;
                    // nothing to surface in the error struct.
                    stdout: String::new(),
                    stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    return Err(RunError::ErlExitNonZero {
                        code: -1,
                        stdout: String::new(),
                        stderr: format!("erl process timed out after {ERL_TIMEOUT_SECS} seconds"),
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                return Err(RunError::SpawnFailed {
                    message: format!("error waiting for erl process: {e}"),
                });
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Probe `erl` on `PATH`.
///
/// Returns the absolute path to `erl` or `RunError::ErlangNotFound` (C004).
fn probe_erl() -> Result<std::path::PathBuf, RunError> {
    which::which("erl").map_err(|_| RunError::ErlangNotFound)
}

/// Drain a child process stdio pipe into a `String`.
fn drain_pipe(pipe: Option<impl Read>) -> String {
    let Some(mut r) = pipe else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}
