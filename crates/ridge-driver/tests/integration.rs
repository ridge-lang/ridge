//! Ridge-driver integration tests — T2 §3.0 test surface (16 tests).
//!
//! Test plan:
//! - compile + check + run on each of the four canonical examples (12 tests).
//! - workspace with multiple members (1 test).
//! - forbid-rule violation (1 test).
//! - missing-erlang probe (1 test, PATH manipulation).
//! - emit-Core-only mode (1 test).
//!
//! Tests gated on `#[cfg(feature = "beam-runtime")]` spawn real `erl` processes
//! and require OTP on PATH.  The remaining tests run in all environments.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::redundant_clone
)]

mod common;
use common::{
    make_forbid_workspace, make_multi_member_workspace, make_workspace, read_example, write_file,
    TempWorkspace,
};

use ridge_driver::{
    check_workspace, compile_workspace, run_workspace, CheckOptions, CompileOptions, EmitArtefacts,
    RunError, RunOptions,
};

/// Serialises tests that depend on the value of `$PATH` at the moment `erl`
/// is resolved.
///
/// `run_missing_erlang` mutates the process-wide PATH to suppress `erl`
/// discovery, and `cargo test` runs unit tests from the same binary in
/// parallel. Without this lock, any happy-path test that resolves `erl` (the
/// four `run_*` tests on the canonical examples) can race with the
/// PATH-clearing test and fail with a spurious `ErlangNotFound` — or worse,
/// the PATH-clearing test can pass while a parallel sibling silently uses an
/// already-spawned child that captured the empty PATH at spawn time.
///
/// Acquire the lock in any test that either reads PATH (calls `erl`) or
/// writes it. Other tests in this file can stay parallel.
static PATH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Normalise line endings and trim trailing whitespace per line.
#[cfg(feature = "beam-runtime")]
fn normalise(s: &str) -> String {
    let unified = s.replace("\r\n", "\n");
    let trimmed: Vec<&str> = unified.lines().map(str::trim_end).collect();
    trimmed.join("\n")
}

/// Read the expected output baseline from `ridge-codegen-erl/tests/expected/`.
#[cfg(feature = "beam-runtime")]
fn read_expected(name: &str) -> String {
    // CARGO_MANIFEST_DIR is `crates/ridge-driver`; expected files live in
    // `crates/ridge-codegen-erl/tests/expected/<name>.txt`.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir)
        .join("..")
        .join("ridge-codegen-erl")
        .join("tests")
        .join("expected")
        .join(format!("{name}.txt"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read expected/{name}.txt: {e}"))
}

// ── Tests 1–3: log_analyzer — compile, check, run ────────────────────────────

/// T2-01: compile the `log_analyzer` example with `compile_workspace`.
#[test]
fn compile_log_analyzer() {
    let source = read_example("log_analyzer");
    let tw = make_workspace("log_analyzer", &source);
    let opts = CompileOptions::new(tw.path.clone());
    let result = compile_workspace(opts);
    assert!(result.is_ok(), "compile failed: {result:?}");
}

/// T2-02: check the `log_analyzer` example with `check_workspace`.
#[test]
fn check_log_analyzer() {
    let source = read_example("log_analyzer");
    let tw = make_workspace("log_analyzer", &source);
    let opts = CheckOptions::new(tw.path.clone());
    let result = check_workspace(opts);
    assert!(result.is_ok(), "check returned fatal error: {result:?}");
}

/// T2-03: run the `log_analyzer` example on the BEAM runtime.
///
/// Requires OTP on PATH (`--features beam-runtime`).
#[test]
#[cfg(feature = "beam-runtime")]
fn run_log_analyzer() {
    let _guard = PATH_ENV_LOCK.lock().expect("PATH_ENV_LOCK not poisoned");

    let source = read_example("log_analyzer");
    let tw = make_workspace("log_analyzer", &source);

    // Compile first so we have beam files.
    let compile_opts = CompileOptions::new(tw.path.clone());
    let artefacts = compile_workspace(compile_opts).expect("compile failed");
    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced for log_analyzer"
    );

    // log_analyzer needs a log file and a level as CLI args.
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("ridge-codegen-erl")
        .join("tests")
        .join("fixtures")
        .join("sample.log");
    let fixture_str = fixture.to_str().expect("fixture path is valid UTF-8");

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, code) = run_erl_direct(&beam_dir, &module_name, &[fixture_str, "WARN"]);
    assert_eq!(
        code, 0,
        "erl exited with {code}\nstdout: {stdout}\nstderr: {stderr}"
    );

    let actual = normalise(&stdout);
    let expected = normalise(&read_expected("log_analyzer"));
    assert_eq!(
        actual, expected,
        "stdout mismatch for log_analyzer\nexpected:\n{expected}\nactual:\n{actual}"
    );
}

// ── Tests 4–6: url_shortener ──────────────────────────────────────────────────

/// T2-04: compile the `url_shortener` example.
#[test]
fn compile_url_shortener() {
    let source = read_example("url_shortener");
    let tw = make_workspace("url_shortener", &source);
    let opts = CompileOptions::new(tw.path.clone());
    let result = compile_workspace(opts);
    assert!(result.is_ok(), "compile failed: {result:?}");
}

/// T2-05: check the `url_shortener` example.
#[test]
fn check_url_shortener() {
    let source = read_example("url_shortener");
    let tw = make_workspace("url_shortener", &source);
    let opts = CheckOptions::new(tw.path.clone());
    let result = check_workspace(opts);
    assert!(result.is_ok(), "check returned fatal error: {result:?}");
}

/// T2-06: run the `url_shortener` example on the BEAM runtime.
#[test]
#[cfg(feature = "beam-runtime")]
fn run_url_shortener() {
    let _guard = PATH_ENV_LOCK.lock().expect("PATH_ENV_LOCK not poisoned");

    let source = read_example("url_shortener");
    let tw = make_workspace("url_shortener", &source);

    let compile_opts = CompileOptions::new(tw.path.clone());
    let artefacts = compile_workspace(compile_opts).expect("compile failed");
    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files for url_shortener"
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem")
        .to_owned();

    let (stdout, stderr, code) = run_erl_direct(&beam_dir, &module_name, &[]);
    assert_eq!(
        code, 0,
        "erl exited {code}\nstdout: {stdout}\nstderr: {stderr}"
    );

    let actual = normalise(&stdout);
    let expected = normalise(&read_expected("url_shortener"));
    assert_eq!(actual, expected, "stdout mismatch for url_shortener");
}

// ── Tests 7–9: game_of_life ───────────────────────────────────────────────────

/// T2-07: compile the `game_of_life` example.
#[test]
fn compile_game_of_life() {
    let source = read_example("game_of_life");
    let tw = make_workspace("game_of_life", &source);
    let opts = CompileOptions::new(tw.path.clone());
    let result = compile_workspace(opts);
    assert!(result.is_ok(), "compile failed: {result:?}");
}

/// T2-08: check the `game_of_life` example.
#[test]
fn check_game_of_life() {
    let source = read_example("game_of_life");
    let tw = make_workspace("game_of_life", &source);
    let opts = CheckOptions::new(tw.path.clone());
    let result = check_workspace(opts);
    assert!(result.is_ok(), "check returned fatal error: {result:?}");
}

/// T2-09: run the `game_of_life` example on the BEAM runtime.
#[test]
#[cfg(feature = "beam-runtime")]
fn run_game_of_life() {
    let _guard = PATH_ENV_LOCK.lock().expect("PATH_ENV_LOCK not poisoned");

    let source = read_example("game_of_life");
    let tw = make_workspace("game_of_life", &source);

    let compile_opts = CompileOptions::new(tw.path.clone());
    let artefacts = compile_workspace(compile_opts).expect("compile failed");
    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files for game_of_life"
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem")
        .to_owned();

    let (stdout, stderr, code) = run_erl_direct(&beam_dir, &module_name, &[]);
    assert_eq!(
        code, 0,
        "erl exited {code}\nstdout: {stdout}\nstderr: {stderr}"
    );

    let actual = normalise(&stdout);
    let expected = normalise(&read_expected("game_of_life"));
    assert_eq!(actual, expected, "stdout mismatch for game_of_life");
}

// ── Tests 10–12: rate_limiter ─────────────────────────────────────────────────

/// T2-10: compile the `rate_limiter` example.
#[test]
fn compile_rate_limiter() {
    let source = read_example("rate_limiter");
    let tw = make_workspace("rate_limiter", &source);
    let opts = CompileOptions::new(tw.path.clone());
    let result = compile_workspace(opts);
    assert!(result.is_ok(), "compile failed: {result:?}");
}

/// T2-11: check the `rate_limiter` example.
#[test]
fn check_rate_limiter() {
    let source = read_example("rate_limiter");
    let tw = make_workspace("rate_limiter", &source);
    let opts = CheckOptions::new(tw.path.clone());
    let result = check_workspace(opts);
    assert!(result.is_ok(), "check returned fatal error: {result:?}");
}

/// T2-12: run the `rate_limiter` example on the BEAM runtime.
#[test]
#[cfg(feature = "beam-runtime")]
fn run_rate_limiter() {
    let _guard = PATH_ENV_LOCK.lock().expect("PATH_ENV_LOCK not poisoned");

    let source = read_example("rate_limiter");
    let tw = make_workspace("rate_limiter", &source);

    let compile_opts = CompileOptions::new(tw.path.clone());
    let artefacts = compile_workspace(compile_opts).expect("compile failed");
    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files for rate_limiter"
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem")
        .to_owned();

    let (stdout, stderr, code) = run_erl_direct(&beam_dir, &module_name, &[]);
    assert_eq!(
        code, 0,
        "erl exited {code}\nstdout: {stdout}\nstderr: {stderr}"
    );

    let actual = normalise(&stdout);
    let expected = normalise(&read_expected("rate_limiter"));
    assert_eq!(actual, expected, "stdout mismatch for rate_limiter");
}

// ── Test 13: multi-member workspace ──────────────────────────────────────────

/// T2-13: compile a workspace with two members (`apps/api` and `apps/core`).
///
/// Asserts that `compile_workspace` succeeds without a fatal error.
#[test]
fn compile_multi_member_workspace() {
    let tw = make_multi_member_workspace();
    let opts = CompileOptions::new(tw.path.clone());
    let result = compile_workspace(opts);
    assert!(result.is_ok(), "multi-member compile failed: {result:?}");
    // Compilation succeeded — codegen may or may not produce .beam depending
    // on erlc availability and parallel-test concurrency; we only assert Ok.
    let artefacts = result.unwrap();
    // At minimum, source_maps should exist for each discovered module.
    // (multi-member → 2+ modules, so source_maps should be non-empty)
    assert!(
        !artefacts.source_maps.is_empty(),
        "expected source maps for both members, got none"
    );
}

// ── Test 14: forbid-rule violation ────────────────────────────────────────────

/// T2-14: compile a workspace with a `[workspace.rules.forbid]` rule that is
/// violated.
///
/// The driver returns `Ok(artefacts)` but `artefacts.diagnostics` is non-empty
/// (contains an `R013 ForbidViolation`).  This tests that the driver surfaces
/// forbid violations as diagnostics rather than panicking.
#[test]
fn compile_forbid_rule_violation() {
    let tw = make_forbid_workspace();
    let opts = CompileOptions::new(tw.path.clone());
    let result = compile_workspace(opts);
    // The driver must not return a fatal CompileError for forbid violations —
    // they are non-fatal resolve diagnostics.
    assert!(
        result.is_ok(),
        "compile returned fatal error for forbid violation: {result:?}"
    );
    let artefacts = result.unwrap();
    // There should be at least one diagnostic describing the violation.
    assert!(
        !artefacts.diagnostics.is_empty(),
        "expected diagnostics for forbid-rule violation, got none"
    );
    let has_forbid = artefacts.diagnostics.iter().any(|d| {
        d.code == "R013"
            || d.primary_message.contains("ForbidViolation")
            || d.primary_message.to_lowercase().contains("forbid")
    });
    assert!(
        has_forbid,
        "expected R013/ForbidViolation in diagnostics, got: {:#?}",
        artefacts.diagnostics
    );

    // Smoke: the R013 diagnostic must carry a real source_id
    // (workspace-relative file path) instead of "<unknown>".  This verifies
    // that resolve errors are attributed to the importing module's file.
    let r013_diag = artefacts
        .diagnostics
        .iter()
        .find(|d| d.code == "R013")
        .expect("expected an R013 diagnostic");
    let sid = r013_diag.source_id.as_str();
    assert!(
        !sid.contains("<unknown>"),
        "R013 diagnostic must NOT carry <unknown> source_id; got: {sid:?}"
    );
    // Ridge source files always have the `.ridge` extension (lower-case by
    // convention).  Use a path-based check to satisfy clippy's
    // case-sensitive-file-extension-comparisons lint.
    let path_ok = std::path::Path::new(sid)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("ridge"));
    assert!(
        path_ok,
        "R013 diagnostic source_id should be a .ridge file path; got: {sid:?}"
    );
}

// ── Test 15: missing Erlang — C004 ErlangNotFound ────────────────────────────

/// T2-15: `run_workspace` when `erl` is not on `PATH` produces `C004 ErlangNotFound`.
///
/// We mock the PATH by temporarily overriding it to an empty temp directory,
/// ensuring `which("erl")` fails.  We restore it after the test.
#[test]
fn run_missing_erlang() {
    // Other tests in this binary call `erl`/`erlc`; hold PATH_ENV_LOCK so the
    // PATH mutation below cannot leak to them.
    let _guard = PATH_ENV_LOCK.lock().expect("PATH_ENV_LOCK not poisoned");

    // Use a trivial source that compiles cleanly so the test exercises the
    // missing-`erl` error path and not the diagnostic gate added to guard the
    // capability contract.  `url_shortener` is unsuitable here because it
    // imports several stdlib modules and the bare `make_workspace` helper
    // does not declare matching capabilities, which (correctly) surfaces an
    // `R016` diagnostic before runtime probing.
    let source = "fn main () -> Unit =\n    ()\n";
    let tw = make_workspace("Main", source);

    // Override PATH to a directory that definitely does not contain `erl`.
    let empty_dir = tempfile::TempDir::new().expect("create empty tempdir");
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", empty_dir.path());

    let result = run_workspace(RunOptions::new(tw.path.clone(), "demo".to_owned()));

    // Restore PATH before any assertion so the process remains usable.
    match original_path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }

    // Run must surface a failure.  Two surfaces are acceptable here:
    //   * `C004 ErlangNotFound` — clearing PATH only removes `erl`; the
    //     stdlib BEAMs were already cached so compile reaches the run probe.
    //   * `CompileDiagnostics` — PATH-clearing also removes `erlc`, the
    //     stdlib build needs it, and the resulting stdlib-load error
    //     surfaces as a diagnostic before the run probe.
    // Both are valid "no OTP on PATH" surfaces.
    assert!(
        result.is_err(),
        "expected error when erl is missing, got Ok"
    );
    let err = result.unwrap_err();
    if let RunError::CompileDiagnostics(payload) = &err {
        assert!(
            !payload.diagnostics.is_empty(),
            "expected non-empty CompileDiagnostics, got empty payload"
        );
    } else {
        let err_str = format!("{err}");
        assert!(
            err_str.contains("C004") || err_str.contains("ErlangNotFound"),
            "expected C004/ErlangNotFound, got: {err_str}"
        );
    }
}

// ── Test 16: emit-Core-only mode ──────────────────────────────────────────────

/// T2-16: `compile_workspace` with `emit = EmitArtefacts::Core` produces `.core`
/// files but no `.beam` files.
#[test]
fn compile_emit_core_only() {
    let source = read_example("url_shortener");
    let tw = make_workspace("url_shortener", &source);

    let opts = CompileOptions::new(tw.path.clone()).with_emit(EmitArtefacts::Core);
    let result = compile_workspace(opts);
    assert!(result.is_ok(), "compile (core-only) failed: {result:?}");
    let artefacts = result.unwrap();

    // No .beam files should be produced in Core-only mode.
    assert!(
        artefacts.beam_files.is_empty(),
        "expected no .beam files in Core-only mode, got: {:?}",
        artefacts.beam_files
    );

    // At least one .core file should be present.
    assert!(
        !artefacts.core_files.is_empty(),
        "expected at least one .core file in Core-only mode"
    );

    // Verify the .core files actually exist on disk.
    for f in &artefacts.core_files {
        assert!(
            f.exists(),
            ".core file does not exist on disk: {}",
            f.display()
        );
    }
}

// ── Beam-runtime helper (not a test) ─────────────────────────────────────────

/// Run `erl -noshell -pa <beam_dir> -s <module> main -s init stop [-extra args...]`.
///
/// Used by beam-runtime tests; mirrors the `run_erl` helper in `beam_e2e.rs`.
#[cfg(feature = "beam-runtime")]
fn run_erl_direct(
    beam_dir: &std::path::Path,
    module: &str,
    plain_args: &[&str],
) -> (String, String, i32) {
    use std::io::Read;
    use std::process::Command;
    use std::time::Duration;

    let erl_path = which::which("erl")
        .expect("erl not found on PATH — install OTP or run without --features beam-runtime");

    let mut cmd = Command::new(&erl_path);
    cmd.arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-s")
        .arg(module)
        .arg("main")
        .arg("-s")
        .arg("init")
        .arg("stop");
    if !plain_args.is_empty() {
        cmd.arg("-extra");
        for arg in plain_args {
            cmd.arg(arg);
        }
    }

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn erl");

    let timeout = Duration::from_secs(60);
    let start = std::time::Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout_bytes = Vec::new();
                let mut stderr_bytes = Vec::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_end(&mut stdout_bytes);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_end(&mut stderr_bytes);
                }
                let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
                let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
                let code = status.code().unwrap_or(-1);
                return (stdout, stderr, code);
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    panic!("erl timed out for module {module}");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("error waiting for erl: {e}"),
        }
    }
}

// ── Tests 17–19: ridge_main_runner Err projection ─────────────────────────────
//
// Regression coverage for the bug where `ridge run` ignored main's return
// value: `Err msg` silently produced exit-0 with no stderr, so any pipeline
// like `ridge run && next-step` proceeded after a logical failure.
//
// The fix wraps the BEAM entry through `ridge_main_runner:run/1` which
// pattern-matches the return value: `{error, _}` halts non-zero with the
// message on stderr; anything else (including `{ok, _}` and bare `Unit`)
// halts zero.  These three tests cover the three shapes mains take in
// practice.

/// T2-17: `run_workspace` halts non-zero and surfaces the message on stderr
/// when main returns `Err _`.
#[test]
#[cfg(feature = "beam-runtime")]
fn run_err_main_returns_nonzero_with_stderr() {
    let _guard = PATH_ENV_LOCK.lock().expect("PATH_ENV_LOCK not poisoned");

    let source = "fn main () -> Result Unit Text =\n    Err \"boom\"\n";
    let tw = make_workspace("Main", source);

    let result = run_workspace(RunOptions::new(tw.path.clone(), "demo".to_owned()));
    assert!(
        result.is_err(),
        "expected non-zero exit for Err main, got Ok"
    );
    let err_str = format!("{:?}", result.unwrap_err());
    assert!(
        err_str.contains("boom"),
        "expected the Err message 'boom' to surface (on stderr), got: {err_str}"
    );
}

/// T2-18: `run_workspace` exits zero when main returns `Ok ()` — happy-path
/// sanity check that the runner's pattern-match doesn't break working code.
#[test]
#[cfg(feature = "beam-runtime")]
fn run_ok_main_returns_zero() {
    let _guard = PATH_ENV_LOCK.lock().expect("PATH_ENV_LOCK not poisoned");

    let source = "fn main () -> Result Unit Text =\n    Ok ()\n";
    let tw = make_workspace("Main", source);

    let result = run_workspace(RunOptions::new(tw.path.clone(), "demo".to_owned()));
    assert!(
        result.is_ok(),
        "expected exit 0 for Ok main, got: {result:?}"
    );
}

/// T2-19: `run_workspace` exits zero when main returns bare `Unit` — keeps
/// the pre-runner behaviour for programs that don't opt into the Result
/// convention (e.g. `fn io main () -> Unit = Io.println "..."`).
#[test]
#[cfg(feature = "beam-runtime")]
fn run_unit_main_returns_zero() {
    let _guard = PATH_ENV_LOCK.lock().expect("PATH_ENV_LOCK not poisoned");

    let source = "fn main () -> Unit =\n    ()\n";
    let tw = make_workspace("Main", source);

    let result = run_workspace(RunOptions::new(tw.path.clone(), "demo".to_owned()));
    assert!(
        result.is_ok(),
        "expected exit 0 for Unit main, got: {result:?}"
    );
}

// ── Test 20: capability gate — run aborts on R016 ─────────────────────────────

/// `run_workspace` returns [`RunError::CompileDiagnostics`] when the compile
/// pipeline emits error-severity diagnostics (e.g. `R016` capability not
/// declared in `ridge.toml`).
///
/// Without this gate `ridge run` would either execute a stale `.beam` from a
/// previous good compile or run partially-emitted output that bypasses the
/// capability contract declared in the manifest.  Does not require `erl`
/// because the gate fires before runtime probing.
#[test]
fn run_aborts_on_capability_diagnostic() {
    let tw = make_app_workspace_io_no_caps();

    let result = run_workspace(RunOptions::new(tw.path.clone(), "demo".to_owned()));

    let err = result.expect_err("expected CompileDiagnostics, got Ok");
    match err {
        RunError::CompileDiagnostics(payload) => {
            assert!(
                payload.diagnostics.iter().any(|d| d.code == "R016"),
                "expected R016 in diagnostics, got codes: {:?}",
                payload
                    .diagnostics
                    .iter()
                    .map(|d| d.code)
                    .collect::<Vec<_>>(),
            );
        }
        other => panic!("expected RunError::CompileDiagnostics, got: {other:?}"),
    }
}

/// Build a single-app workspace whose source uses `io` but whose manifest
/// declares an empty capability allow-list, guaranteeing an `R016` diagnostic.
fn make_app_workspace_io_no_caps() -> TempWorkspace {
    let tw = TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    );
    write_file(
        &tw.path,
        "apps/demo/src/Main.ridge",
        "import std.io as Io\n\nfn io main () -> Result Unit Text =\n    Io.println \"should not reach\"\n    Ok ()\n",
    );
    tw
}
