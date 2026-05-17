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
use common::{make_forbid_workspace, make_multi_member_workspace, make_workspace, read_example};

use ridge_driver::{
    check_workspace, compile_workspace, run_workspace, CheckOptions, CompileOptions, EmitArtefacts,
    RunOptions,
};

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
    // Ridge source files always have the `.rg` extension (lower-case by
    // convention).  Use a path-based check to satisfy clippy's
    // case-sensitive-file-extension-comparisons lint.
    let path_ok = std::path::Path::new(sid)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rg"));
    assert!(
        path_ok,
        "R013 diagnostic source_id should be a .rg file path; got: {sid:?}"
    );
}

// ── Test 15: missing Erlang — C004 ErlangNotFound ────────────────────────────

/// T2-15: `run_workspace` when `erl` is not on `PATH` produces `C004 ErlangNotFound`.
///
/// We mock the PATH by temporarily overriding it to an empty temp directory,
/// ensuring `which("erl")` fails.  We restore it after the test.
#[test]
fn run_missing_erlang() {
    let source = read_example("url_shortener");
    let tw = make_workspace("url_shortener", &source);

    // Override PATH to a directory that definitely does not contain `erl`.
    let empty_dir = tempfile::TempDir::new().expect("create empty tempdir");
    let original_path = std::env::var_os("PATH");
    // Note: setting env vars is not thread-safe across tests that run in parallel
    // in the same process.  Integration tests run in separate test binaries so
    // this is safe here (each integration test file is its own process).
    std::env::set_var("PATH", empty_dir.path());

    let result = run_workspace(RunOptions::new(tw.path.clone(), "demo".to_owned()));

    // Restore PATH before any assertion so the process remains usable.
    match original_path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }

    // Compile succeeded (no erl needed for compile), but run must return C004.
    assert!(
        result.is_err(),
        "expected error when erl is missing, got Ok"
    );
    let err = result.unwrap_err();
    let err_str = format!("{err}");
    assert!(
        err_str.contains("C004") || err_str.contains("ErlangNotFound"),
        "expected C004/ErlangNotFound, got: {err_str}"
    );
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
