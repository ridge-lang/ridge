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

/// The bundled `ridge_loader.erl` source, embedded at compile time.
///
/// The hot code upgrade orchestrator `ridge run --reload` drives through
/// `rpc:call`. Installed and compiled on every build so its `.beam` is on
/// the code path of every dev node.
const RIDGE_LOADER_SOURCE: &str = include_str!("../runtime/ridge_loader.erl");

/// The bundled `ridge_bench_runner.erl` source, embedded at compile time.
///
/// Unlike the other runners this is *not* installed on every build — it is only
/// needed when running Layer B micro-benchmarks, so the benchmark harness opts
/// in via [`install_bench_runner`] / [`compile_bench_runner`].
const RIDGE_BENCH_RUNNER_SOURCE: &str = include_str!("../runtime/ridge_bench_runner.erl");

/// The bundled `ridge_repl_runner.erl` source, embedded at compile time.
///
/// Like the bench runner this is opt-in rather than installed on every build:
/// only `ridge repl` needs it, and it is the only runner whose session is a
/// throwaway workspace rather than a project.
const RIDGE_REPL_RUNNER_SOURCE: &str = include_str!("../runtime/ridge_repl_runner.erl");

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

    // Idempotent: every install_runner_source call skips the write when the
    // destination already matches, preserving mtimes (the staleness check at
    // compile time keys off them). No early return here: a matching
    // ridge_rt.erl must not skip the other shims — one of them may be the
    // piece that changed.
    install_runner_source(&runtime_dir, "ridge_rt.erl", RIDGE_RT_SOURCE)?;

    // Also install ridge_test_runner.erl (test runner bridge).
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

    // And ridge_loader.erl, the hot code upgrade orchestrator behind
    // `ridge run --reload`.
    install_runner_source(&runtime_dir, "ridge_loader.erl", RIDGE_LOADER_SOURCE)?;

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
    compile_runner_if_stale(
        erlc_path,
        out_root,
        &beam_out_dir,
        "ridge_bench_runner.erl",
        "ridge_bench_runner.beam",
    )?;
    Ok(beam_out_dir.join("ridge_bench_runner.beam"))
}

/// Install the bundled `ridge_repl_runner.erl` under `<out_root>/runtime/`.
///
/// Separate from [`install_runtime`] for the same reason as the bench runner:
/// only `ridge repl` loads it. Idempotent — skips the write when the
/// destination already matches.
///
/// I/O failures surface as [`CodegenError::OutputDirNotWritable`].
pub fn install_repl_runner(out_root: &Path) -> Result<PathBuf, CodegenError> {
    let runtime_dir = out_root.join("runtime");
    std::fs::create_dir_all(&runtime_dir).map_err(|e| CodegenError::OutputDirNotWritable {
        path: runtime_dir.clone(),
        io_err: e.to_string(),
    })?;
    install_runner_source(
        &runtime_dir,
        "ridge_repl_runner.erl",
        RIDGE_REPL_RUNNER_SOURCE,
    )?;
    Ok(runtime_dir.join("ridge_repl_runner.erl"))
}

/// Compile the installed `ridge_repl_runner.erl` to `ridge_repl_runner.beam`.
///
/// Companion to [`install_repl_runner`]; the `.beam` lands in `<out_root>/beam/`
/// so `erl -pa <beam_dir>` can load it. Idempotent.
///
/// Errors during `erlc` surface as [`CodegenError::ErlcRejectedInput`].
pub fn compile_repl_runner(erlc_path: &Path, out_root: &Path) -> Result<PathBuf, CodegenError> {
    let beam_out_dir = out_root.join("beam");
    std::fs::create_dir_all(&beam_out_dir).map_err(|e| CodegenError::OutputDirNotWritable {
        path: beam_out_dir.clone(),
        io_err: e.to_string(),
    })?;
    compile_runner_if_stale(
        erlc_path,
        out_root,
        &beam_out_dir,
        "ridge_repl_runner.erl",
        "ridge_repl_runner.beam",
    )?;
    Ok(beam_out_dir.join("ridge_repl_runner.beam"))
}

/// Compile the installed `ridge_rt.erl` to `ridge_rt.beam` using `erlc`.
///
/// Also compiles `ridge_test_runner.erl` → `ridge_test_runner.beam`.
/// The resulting `.beam` files are placed in `<out_root>/beam/` alongside
/// user modules.  This ensures `erl -pa <beam_dir>` can find both at runtime.
///
/// Idempotent — each bundled shim is recompiled only when its installed
/// `.erl` is newer than its `.beam` (or the beam is missing).
///
/// Errors during `erlc` invocation are returned as [`CodegenError::ErlcRejectedInput`].
pub fn compile_runtime(erlc_path: &Path, out_root: &Path) -> Result<PathBuf, CodegenError> {
    let beam_out_dir = out_root.join("beam");
    std::fs::create_dir_all(&beam_out_dir).map_err(|e| CodegenError::OutputDirNotWritable {
        path: beam_out_dir.clone(),
        io_err: e.to_string(),
    })?;

    // One compiler process for the stale set, in the original runner order.
    // The optional bench and REPL runners still compile only on request.
    compile_runners_if_stale(
        erlc_path,
        out_root,
        &beam_out_dir,
        &[
            ("ridge_rt.erl", "ridge_rt.beam"),
            ("ridge_test_runner.erl", "ridge_test_runner.beam"),
            ("ridge_main_runner.erl", "ridge_main_runner.beam"),
            ("ridge_pg.erl", "ridge_pg.beam"),
            ("ridge_sup.erl", "ridge_sup.beam"),
            ("ridge_loader.erl", "ridge_loader.beam"),
            #[cfg(feature = "beam-runtime")]
            ("ridge_sqlite.erl", "ridge_sqlite.beam"),
        ],
    )?;

    // Install the same baked NIF after compilation succeeds, even on a no-op.
    #[cfg(feature = "beam-runtime")]
    write_bytes_if_changed(&beam_out_dir.join(sqlite_nif_filename()), SQLITE_NIF_BYTES)?;

    Ok(beam_out_dir.join("ridge_rt.beam"))
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

/// True when the `.erl` source is newer than the compiled `.beam` (or the
/// beam is missing) — i.e. the bundled shim must be (re)compiled.
fn beam_is_stale(erl_path: &Path, beam_path: &Path) -> bool {
    if !beam_path.exists() {
        return true;
    }
    let erl_mtime = std::fs::metadata(erl_path).and_then(|m| m.modified()).ok();
    let beam_mtime = std::fs::metadata(beam_path).and_then(|m| m.modified()).ok();
    matches!((erl_mtime, beam_mtime), (Some(e), Some(b)) if e > b)
}

/// Compile a single bundled runner under `runtime/` to `beam/`.
///
/// Recompiles when the installed `.erl` is newer than the `.beam` (the
/// installer rewrites the source whenever its content changed, so this
/// catches shim updates across compiler versions).
fn compile_runner_if_stale(
    erlc_path: &Path,
    out_root: &Path,
    beam_out_dir: &Path,
    erl_name: &str,
    beam_name: &str,
) -> Result<(), CodegenError> {
    compile_runners_if_stale(erlc_path, out_root, beam_out_dir, &[(erl_name, beam_name)])
}

/// Select the same missing/outdated outputs as the individual runner path.
fn compile_runners_if_stale(
    erlc_path: &Path,
    out_root: &Path,
    beam_out_dir: &Path,
    runners: &[(&str, &str)],
) -> Result<(), CodegenError> {
    let runtime_dir = out_root.join("runtime");
    let sources: Vec<_> = runners
        .iter()
        .filter_map(|(erl_name, beam_name)| {
            let source = runtime_dir.join(erl_name);
            (source.exists() && beam_is_stale(&source, &beam_out_dir.join(beam_name)))
                .then_some(source)
        })
        .collect();

    compile_runner_sources(&runtime_dir, &sources, |inputs| {
        std::process::Command::new(erlc_path)
            .arg("-o")
            .arg(beam_out_dir)
            .args(inputs)
            .output()
            .map_err(|_| CodegenError::ErlcNotFound {
                searched_paths: vec![],
            })
    })
}

/// Fast success path; on failure retain the individual input's diagnostic.
fn compile_runner_sources(
    runtime_dir: &Path,
    sources: &[PathBuf],
    mut compile: impl FnMut(&[PathBuf]) -> Result<std::process::Output, CodegenError>,
) -> Result<(), CodegenError> {
    if sources.is_empty() {
        return Ok(());
    }
    let output = compile(sources)?;
    if output.status.success() {
        return Ok(());
    }

    if sources.len() > 1 {
        // Do not re-check staleness: a failed batch can have written a prefix
        // of its outputs. Retry the original set to identify the rejected file
        // without parsing compiler text or blaming the first input by default.
        for source in sources {
            let retry = compile(std::slice::from_ref(source))?;
            if !retry.status.success() {
                return Err(runner_rejection(source, &retry));
            }
        }
    }

    // A transient batch failure must remain a failure even if every retry
    // succeeds. In that case the rejected input is the set, not a known file.
    let rejected = match sources {
        [source] => source.as_path(),
        _ => runtime_dir,
    };
    Err(runner_rejection(rejected, &output))
}

/// Preserve the existing error code, compiler stderr and exit status.
fn runner_rejection(source: &Path, output: &std::process::Output) -> CodegenError {
    CodegenError::ErlcRejectedInput {
        core_path: source.to_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Controlled compiler results keep failure-path coverage independent of OTP.
    fn compiler_output(success: bool, stderr: &str) -> std::process::Output {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        #[cfg(unix)]
        let status = std::process::ExitStatus::from_raw(if success { 0 } else { 9 << 8 });
        #[cfg(windows)]
        let status = std::process::ExitStatus::from_raw(if success { 0 } else { 9 });
        std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn runtime_batch_empty_and_success_launch_only_required_commands() {
        let runtime = Path::new("runtime with spaces");
        compile_runner_sources(runtime, &[], |_| panic!("empty set launched erlc")).unwrap();
        let sources = vec![runtime.join("a.erl"), runtime.join("b.erl")];
        let mut calls = Vec::new();
        compile_runner_sources(runtime, &sources, |inputs| {
            calls.push(inputs.to_vec());
            Ok(compiler_output(true, ""))
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![sources],
            "one call must contain the complete set"
        );
    }

    #[test]
    fn runtime_batch_retries_in_order_and_reports_rejected_source() {
        let runtime = Path::new("runtime");
        let sources = vec![
            runtime.join("a.erl"),
            runtime.join("b.erl"),
            runtime.join("c.erl"),
        ];
        for bad in 0..sources.len() {
            let mut calls = Vec::new();
            let error = compile_runner_sources(runtime, &sources, |inputs| {
                calls.push(inputs.to_vec());
                if inputs.len() > 1 {
                    Ok(compiler_output(false, "batch rejected"))
                } else {
                    Ok(compiler_output(
                        inputs[0] != sources[bad],
                        "individual rejection",
                    ))
                }
            })
            .unwrap_err();
            let CodegenError::ErlcRejectedInput {
                core_path,
                stderr,
                exit_code,
            } = error
            else {
                panic!("expected compiler rejection");
            };
            assert_eq!(core_path, sources[bad]);
            assert_eq!(stderr, "individual rejection");
            assert_eq!(exit_code, 9);
            let expected: Vec<_> = std::iter::once(sources.clone())
                .chain(sources[..=bad].iter().map(|p| vec![p.clone()]))
                .collect();
            assert_eq!(calls, expected, "must stop at the first rejected input");
        }
    }

    #[test]
    fn runtime_batch_does_not_hide_unrepeatable_failure() {
        let runtime = Path::new("runtime");
        let sources = vec![runtime.join("a.erl"), runtime.join("b.erl")];
        let mut calls = Vec::new();
        let error = compile_runner_sources(runtime, &sources, |inputs| {
            calls.push(inputs.to_vec());
            Ok(compiler_output(inputs.len() == 1, "original batch failure"))
        })
        .unwrap_err();
        let CodegenError::ErlcRejectedInput {
            core_path,
            stderr,
            exit_code,
        } = error
        else {
            panic!("expected original batch failure");
        };
        assert_eq!(core_path, runtime, "no individual file was rejected");
        assert_eq!(stderr, "original batch failure");
        assert_eq!(exit_code, 9);
        assert_eq!(
            calls,
            vec![
                sources.clone(),
                vec![sources[0].clone()],
                vec![sources[1].clone()]
            ]
        );
    }

    #[test]
    fn runtime_single_runner_and_spawn_errors_stay_failures() {
        let runtime = Path::new("runtime");
        let sources = vec![runtime.join("only.erl")];
        let mut calls = 0;
        let error = compile_runner_sources(runtime, &sources, |_| {
            calls += 1;
            Ok(compiler_output(false, "single rejection"))
        })
        .unwrap_err();
        assert_eq!(calls, 1, "a singleton must not be retried");
        assert!(
            matches!(error, CodegenError::ErlcRejectedInput { core_path, .. } if core_path == sources[0])
        );
        let error = compile_runner_sources(runtime, &sources, |_| {
            Err(CodegenError::ErlcNotFound {
                searched_paths: Vec::new(),
            })
        })
        .unwrap_err();
        assert!(matches!(error, CodegenError::ErlcNotFound { .. }));
    }

    #[cfg(feature = "beam-runtime")]
    #[test]
    fn runtime_batch_matches_single_outputs_and_preserves_current_files() {
        use std::collections::BTreeMap;
        let tmp = tempfile::Builder::new()
            .prefix("ridge runtime batch ")
            .tempdir()
            .unwrap();
        let erlc = which::which("erlc").expect("beam-runtime requires erlc");
        install_runtime(tmp.path()).unwrap();
        let beam = tmp.path().join("beam");
        compile_runtime(&erlc, tmp.path()).unwrap();
        let baseline = tmp.path().join("individual");
        std::fs::create_dir(&baseline).unwrap();
        let names = [
            "ridge_rt",
            "ridge_test_runner",
            "ridge_main_runner",
            "ridge_pg",
            "ridge_sup",
            "ridge_loader",
            "ridge_sqlite",
        ];
        for name in names {
            let output = std::process::Command::new(&erlc)
                .arg("-o")
                .arg(&baseline)
                .arg(tmp.path().join("runtime").join(format!("{name}.erl")))
                .output()
                .unwrap();
            assert!(output.status.success(), "{name}: {output:?}");
            assert_eq!(
                std::fs::read(beam.join(format!("{name}.beam"))).unwrap(),
                std::fs::read(baseline.join(format!("{name}.beam"))).unwrap(),
                "{name}"
            );
        }
        let snapshot = || -> BTreeMap<_, _> {
            std::fs::read_dir(&beam)
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (
                        path.file_name().unwrap().to_owned(),
                        (
                            std::fs::read(&path).unwrap(),
                            std::fs::metadata(&path).unwrap().modified().unwrap(),
                        ),
                    )
                })
                .collect()
        };
        let before = snapshot();
        let missing_compiler = tmp.path().join("no-such-erlc");
        install_runtime(tmp.path()).unwrap();
        compile_runtime(&missing_compiler, tmp.path()).expect("current set must not launch erlc");
        assert_eq!(
            snapshot(),
            before,
            "current beams and NIF must retain bytes and mtimes"
        );
        let nif = beam.join(sqlite_nif_filename());
        assert_eq!(std::fs::read(&nif).unwrap(), SQLITE_NIF_BYTES);
        std::fs::remove_file(&nif).unwrap();
        compile_runtime(&missing_compiler, tmp.path())
            .expect("missing NIF is restored without erlc");
        assert_eq!(std::fs::read(nif).unwrap(), SQLITE_NIF_BYTES);

        let before = snapshot();
        std::fs::remove_file(beam.join("ridge_pg.beam")).unwrap();
        compile_runtime(&erlc, tmp.path()).unwrap();
        let after = snapshot();
        for (name, state) in &before {
            if name != "ridge_pg.beam" {
                assert_eq!(
                    after.get(name),
                    Some(state),
                    "unselected output {name:?} changed"
                );
            }
        }
        assert_eq!(
            after[std::ffi::OsStr::new("ridge_pg.beam")].0,
            before[std::ffi::OsStr::new("ridge_pg.beam")].0
        );
    }

    #[cfg(feature = "beam-runtime")]
    #[test]
    fn runtime_batch_attributes_real_syntax_errors() {
        let erlc = which::which("erlc").expect("beam-runtime requires erlc");
        for bad in 0..3 {
            let tmp = tempfile::tempdir().unwrap();
            let runtime = tmp.path().join("runtime");
            let beam = tmp.path().join("beam");
            std::fs::create_dir(&runtime).unwrap();
            std::fs::create_dir(&beam).unwrap();
            let names = [
                ("a.erl", "a.beam"),
                ("b.erl", "b.beam"),
                ("c.erl", "c.beam"),
            ];
            for (index, (source, _)) in names.iter().enumerate() {
                let name = Path::new(source).file_stem().unwrap().to_str().unwrap();
                let body = if index == bad {
                    format!("-module({name}).\ninvalid syntax !!!\n")
                } else {
                    format!("-module({name}).\n-export([value/0]).\nvalue() -> ok.\n")
                };
                std::fs::write(runtime.join(source), body).unwrap();
            }
            let error = compile_runners_if_stale(&erlc, tmp.path(), &beam, &names).unwrap_err();
            assert!(
                matches!(error, CodegenError::ErlcRejectedInput { core_path, exit_code, .. } if core_path == runtime.join(names[bad].0) && exit_code != 0)
            );
            for (index, (_, output)) in names.iter().enumerate() {
                assert_eq!(
                    beam.join(output).exists(),
                    index < bad,
                    "partial output {output}"
                );
            }
        }
    }

    #[test]
    fn install_runtime_writes_ridge_loader_source() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        install_runtime(tmp.path()).expect("install_runtime");
        let installed =
            std::fs::read_to_string(tmp.path().join("runtime").join("ridge_loader.erl"))
                .expect("ridge_loader.erl must be installed");
        assert!(
            installed.contains("-module(ridge_loader)."),
            "installed ridge_loader.erl must declare the ridge_loader module"
        );
    }

    #[test]
    fn install_runtime_is_idempotent_and_preserves_matching_files() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        install_runtime(tmp.path()).expect("first install");
        let loader_path = tmp.path().join("runtime").join("ridge_loader.erl");
        let first = std::fs::metadata(&loader_path)
            .and_then(|m| m.modified())
            .expect("mtime");
        install_runtime(tmp.path()).expect("second install");
        let second = std::fs::metadata(&loader_path)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(first, second, "matching sources must not be rewritten");
    }

    /// The REPL runner installs on request, and only on request.
    ///
    /// Both halves matter. It has to be installable from here — that is the
    /// whole point of moving it out of the CLI — and it must stay out of the
    /// ordinary build, because every project would otherwise carry a runner
    /// only an interactive session ever loads.
    #[test]
    fn the_repl_runner_installs_on_request_and_not_before() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let installed = tmp.path().join("runtime").join("ridge_repl_runner.erl");

        install_runtime(tmp.path()).expect("install_runtime");
        assert!(
            !installed.exists(),
            "an ordinary build must not install the REPL runner"
        );

        install_repl_runner(tmp.path()).expect("install_repl_runner");
        let source = std::fs::read_to_string(&installed).expect("ridge_repl_runner.erl");
        assert!(
            source.contains("-module(ridge_repl_runner)."),
            "the installed source must declare the ridge_repl_runner module"
        );
    }

    /// Every runner that reports a crash reports it through the one place.
    ///
    /// Four of them each held their own copy of the `RIDGE_BACKTRACE` bargain,
    /// and a runner that forgot half of it would still have looked right: the
    /// sentence would print, and only the way out would be missing.
    #[test]
    fn every_runner_reports_a_crash_through_ridge_rt() {
        for (name, source) in [
            ("ridge_main_runner.erl", RIDGE_MAIN_RUNNER_SOURCE),
            ("ridge_test_runner.erl", RIDGE_TEST_RUNNER_SOURCE),
            ("ridge_bench_runner.erl", RIDGE_BENCH_RUNNER_SOURCE),
            ("ridge_repl_runner.erl", RIDGE_REPL_RUNNER_SOURCE),
        ] {
            assert!(
                source.contains("ridge_rt:print_failure("),
                "{name} must report crashes through ridge_rt:print_failure/4"
            );
            assert!(
                !source.contains("os:getenv(\"RIDGE_BACKTRACE\")"),
                "{name} must not read RIDGE_BACKTRACE itself — naming it in a                  comment is fine, deciding on it is not"
            );
        }
        assert!(
            RIDGE_RT_SOURCE.contains("os:getenv(\"RIDGE_BACKTRACE\")"),
            "and the one place that does decide must still be ridge_rt"
        );
    }

    #[test]
    fn beam_is_stale_only_when_source_is_newer_or_beam_missing() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let erl = tmp.path().join("x.erl");
        let beam = tmp.path().join("x.beam");
        std::fs::write(&erl, b"-module(x).").expect("write erl");
        assert!(beam_is_stale(&erl, &beam), "missing beam is stale");
        std::fs::write(&beam, b"BEAM").expect("write beam");
        assert!(
            !beam_is_stale(&erl, &beam),
            "beam newer than source is fresh"
        );
    }
}
