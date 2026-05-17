//! `ridge test` — discover and run `pub fn test_*` functions in the workspace.
//!
//! ## Surface
//!
//! ```text
//! ridge test [--member <name>] [--filter <glob-pattern>]
//! ```
//!
//! ## Algorithm
//!
//! 1. Locate workspace root via `ridge_manifest::find_workspace_root`.
//! 2. Typecheck workspace via `ridge_driver::check_workspace_typed` (no erlc
//!    needed at this stage).  Render diagnostics and exit non-zero on errors.
//! 3. Discover every `pub fn test_*` (arity 0) across all workspace members
//!    and classify each:
//!    - Arity != 0         → `C301 TestArityInvalid` (skip, count as failure).
//!    - `ffi` cap declared → `C302 TestCapabilityForbidden` (skip, count as failure).
//!    - Return type `Bool` → `C303 BoolTestDeprecated` warning (still run).
//!    - Return type `Result Unit Text` → run.
//!    - Anything else      → failure (invalid test return type).
//! 4. Apply `--filter` glob matching against `Module.test_fn_name`.
//! 5. If no tests survive filtering → print "no tests discovered" and exit 0.
//! 6. If every surviving test is a validation failure (C301/C302/etc.) → report
//!    them and exit 1 WITHOUT invoking erlc.  This is what lets the unit tests
//!    `test_arity_invalid` / `test_ffi_rejection` run in environments where OTP
//!    is not installed.
//! 7. Otherwise compile the workspace via `ridge_driver::compile_workspace`
//!    (produces `.beam` files via erlc — requires OTP 26+ on PATH).
//! 8. Run each surviving canonical/bool test in a fresh BEAM child:
//!    `erl -pa <beam_dir> -pa <runtime_dir> -s ridge_test_runner run <module> <fn> -s init stop -noshell`
//! 9. Tally (passed, failed, skipped) and print summary.
//! 10. Exit 0 if all tests pass (or no tests found); exit 1 on any failure.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use clap::Parser;
use ridge_driver::{
    check_workspace_typed, compile_workspace, AstCapability, AstItem, AstType, CheckOptions,
    CompileOptions, ModuleMetadata, PrimitiveType, TypedModule, TypedWorkspace, Visibility,
    WorkspaceGraph,
};
use ridge_manifest::find_workspace_root;

use crate::error::CliError;
use crate::render::render_diagnostics;

// ── Argument struct ───────────────────────────────────────────────────────────

/// Run the test suite for a Ridge workspace.
///
/// Discovers every `pub fn test_*` function across the workspace (or the named
/// member), compiles them, runs each in a fresh BEAM child process, and reports
/// pass / fail per test.
#[derive(Debug, Parser)]
pub struct TestArgs {
    /// Only test the named workspace member.
    #[arg(long, value_name = "NAME")]
    pub member: Option<String>,

    /// Filter tests by glob pattern matched against `Module.test_fn_name`.
    ///
    /// Supports `*` (any sequence) and `?` (any single character).
    /// Example: `--filter "*.test_arith*"`
    #[arg(long, value_name = "PATTERN")]
    pub filter: Option<String>,
}

// ── Execute ───────────────────────────────────────────────────────────────────

/// Execute `ridge test`.
///
/// # Errors
///
/// Returns a [`CliError`] for workspace-structure problems.  Test failures and
/// compile errors are handled internally via [`process::exit`].
pub fn execute(args: &TestArgs, cwd: &Path) -> Result<(), CliError> {
    // ── 1. Locate workspace root ──────────────────────────────────────────────
    let workspace_root = find_workspace_root(cwd).ok_or(CliError::NoWorkspaceRoot)?;

    // ── 2. Typecheck workspace (no erlc needed yet) ────────────────────────────
    let mut check_opts = CheckOptions::new(workspace_root.clone());
    if let Some(ref name) = args.member {
        check_opts.members = Some(vec![name.clone()]);
    }

    let typed_artefacts = match check_workspace_typed(check_opts) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    if !typed_artefacts.diagnostics.is_empty() {
        render_diagnostics(&typed_artefacts.diagnostics, &typed_artefacts.sources);
        process::exit(1);
    }

    // ── 3. Discover test functions ─────────────────────────────────────────────
    let tests = discover_tests(&typed_artefacts.typed, &typed_artefacts.graph);

    if tests.is_empty() {
        println!("notice: no tests discovered");
        return Ok(());
    }

    // ── 4. Apply --filter ─────────────────────────────────────────────────────
    let tests: Vec<DiscoveredTest> = if let Some(ref pattern) = args.filter {
        tests
            .into_iter()
            .filter(|t| glob_match(pattern, &t.qualified_name))
            .collect()
    } else {
        tests
    };

    if tests.is_empty() {
        println!("notice: no tests discovered");
        return Ok(());
    }

    // ── 5. Early-exit if no test can actually run ─────────────────────────────
    // If every surviving test is a validation failure (C301/C302/InvalidReturn),
    // there is nothing to spawn — report them and exit 1 WITHOUT invoking erlc.
    // This keeps `ridge test` usable on agents where OTP is not installed
    // (cf. test_cmd::test_arity_invalid / test_ffi_rejection).
    let needs_runtime = tests.iter().any(|t| {
        matches!(
            t.classification,
            TestClassification::Canonical | TestClassification::BoolDeprecated
        )
    });

    if !needs_runtime {
        // run_tests_and_report's loop `continue`s on every invalid classification
        // before touching `erl_path`/`beam_dir`/`runtime_dir`, so the dummy
        // paths below are never dereferenced.
        let unused = Path::new("");
        run_tests_and_report(&tests, unused, unused, unused);
        return Ok(()); // unreachable: run_tests_and_report calls process::exit
    }

    // ── 6. Compile workspace (needs erlc on PATH) ──────────────────────────────
    let mut compile_opts = CompileOptions::new(workspace_root.clone());
    if let Some(ref name) = args.member {
        compile_opts.members = Some(vec![name.clone()]);
    }

    let artefacts = match compile_workspace(compile_opts) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    if !artefacts.diagnostics.is_empty() {
        render_diagnostics(&artefacts.diagnostics, &artefacts.sources);
        process::exit(1);
    }

    // ── 7. Discover .beam output dir and runtime dir ───────────────────────────
    let beam_dir = beam_dir_from_artefacts(&artefacts.beam_files);
    let runtime_dir = beam_dir.parent().map_or_else(
        || {
            workspace_root
                .join("target")
                .join("ridge")
                .join("debug")
                .join("runtime")
        },
        |p| p.join("runtime"),
    );

    // ── 8. Locate erl binary ───────────────────────────────────────────────────
    let Ok(erl_path) = which::which("erl") else {
        eprintln!("error: C004 ErlangNotFound: erl not found on PATH (install OTP 26+)");
        process::exit(1);
    };

    // ── 9. Run tests and tally results ────────────────────────────────────────
    run_tests_and_report(&tests, &erl_path, &beam_dir, &runtime_dir);
    Ok(())
}

/// Run the discovered tests, print per-test results, and exit with the
/// appropriate code.  Separated from `execute` to keep line counts under the
/// Clippy limit.
#[allow(clippy::too_many_lines)]
fn run_tests_and_report(
    tests: &[DiscoveredTest],
    erl_path: &Path,
    beam_dir: &Path,
    runtime_dir: &Path,
) {
    let mut passed: usize = 0;
    let mut failed: usize = 0;
    let mut skipped: usize = 0;
    let mut bool_tests: usize = 0;

    let wall_start = Instant::now();

    for test in tests {
        match &test.classification {
            TestClassification::ArityInvalid => {
                eprintln!(
                    "error: C301 TestArityInvalid: '{}' must have zero parameters",
                    test.qualified_name
                );
                failed += 1;
                skipped += 1;
                continue;
            }
            TestClassification::CapabilityForbidden => {
                eprintln!(
                    "error: C302 TestCapabilityForbidden: '{}' declares the 'ffi' capability; \
                     ffi tests are not permitted in ridge test 0.1.0",
                    test.qualified_name
                );
                failed += 1;
                skipped += 1;
                continue;
            }
            TestClassification::InvalidReturnType => {
                eprintln!(
                    "error: '{}' has an unsupported return type; \
                     test functions must return 'Result Unit Text' or 'Bool'",
                    test.qualified_name
                );
                failed += 1;
                skipped += 1;
                continue;
            }
            TestClassification::BoolDeprecated => {
                // Emit per-test C303 warning.
                eprintln!(
                    "warning: C303 BoolTestDeprecated: '{}' returns Bool (deprecated); \
                     -- migrate: change return type to Result Unit Text; \
                     replace 'true' with 'Ok ()' and 'false' with 'Err \"<reason>\"'",
                    test.qualified_name
                );
                bool_tests += 1;
            }
            TestClassification::Canonical => {}
        }

        // Run the test in a fresh BEAM child.
        let outcome = run_test(
            erl_path,
            beam_dir,
            runtime_dir,
            &test.beam_module,
            &test.fn_name,
        );

        match outcome {
            TestOutcome::Pass => {
                println!("ok  {}", test.qualified_name);
                passed += 1;
            }
            TestOutcome::Fail { stderr } => {
                println!("FAIL {}", test.qualified_name);
                if !stderr.is_empty() {
                    eprintln!("{}", stderr.trim_end());
                }
                failed += 1;
            }
            TestOutcome::Timeout => {
                println!("FAIL {} (timeout after 60s)", test.qualified_name);
                failed += 1;
            }
            TestOutcome::SpawnFailed { message } => {
                eprintln!(
                    "error: could not spawn erl for '{}': {message}",
                    test.qualified_name
                );
                failed += 1;
            }
        }
    }

    let elapsed_ms = wall_start.elapsed().as_millis();

    // ── 8. Print summary ───────────────────────────────────────────────────────
    println!("Tests: {passed} passed, {failed} failed, {skipped} skipped ({elapsed_ms}ms)");

    if bool_tests > 0 {
        println!(
            "{bool_tests} test(s) returned Bool; see migration snippets above. \
             Bool acceptance is removed in 0.2.0."
        );
    }

    // ── 9. Exit code ──────────────────────────────────────────────────────────
    if failed > 0 {
        process::exit(1);
    }
}

// ── Test discovery ────────────────────────────────────────────────────────────

/// Classification of a discovered test function.
#[derive(Debug, Clone)]
enum TestClassification {
    /// `Result Unit Text` return type — canonical test contract.
    Canonical,
    /// `Bool` return type — accepted but deprecated (C303 warning).
    BoolDeprecated,
    /// Arity != 0 — C301 error (skip, count as failure).
    ArityInvalid,
    /// `ffi` capability declared — C302 error (skip, count as failure).
    CapabilityForbidden,
    /// Return type is neither `Result Unit Text` nor `Bool`.
    InvalidReturnType,
}

/// A test function discovered in the workspace.
#[derive(Debug)]
struct DiscoveredTest {
    /// Qualified name shown in output: `ModuleName.fn_name`.
    qualified_name: String,
    /// BEAM module name (Erlang atom, e.g. `ridge_module_0`).
    beam_module: String,
    /// The Ridge function name (e.g. `test_arith`).
    fn_name: String,
    /// How this test is classified (determines whether / how it runs).
    classification: TestClassification,
}

/// Walk every module in the typed workspace and collect `pub fn test_*` entries.
fn discover_tests(typed: &TypedWorkspace, graph: &WorkspaceGraph) -> Vec<DiscoveredTest> {
    let mut tests = Vec::new();

    for module in &typed.modules {
        let module_name = module_display_name(module, graph);
        let beam_module = beam_module_name(module);

        for item in &module.ast.items {
            let AstItem::Fn(f) = item else { continue };

            // Only public functions.
            if f.vis != Visibility::Pub {
                continue;
            }
            // Only `test_*` prefix.
            if !f.name.text.starts_with("test_") {
                continue;
            }

            let qualified_name = format!("{module_name}.{}", f.name.text);
            let fn_name = f.name.text.clone();

            // Check arity.
            if !f.params.is_empty() {
                tests.push(DiscoveredTest {
                    qualified_name,
                    beam_module: beam_module.clone(),
                    fn_name,
                    classification: TestClassification::ArityInvalid,
                });
                continue;
            }

            // Check for ffi capability.
            if f.caps.contains(&AstCapability::Ffi) {
                tests.push(DiscoveredTest {
                    qualified_name,
                    beam_module: beam_module.clone(),
                    fn_name,
                    classification: TestClassification::CapabilityForbidden,
                });
                continue;
            }

            // Classify by return type.
            let classification = match return_type_classification(f.ret.as_ref()) {
                ReturnTypeKind::ResultUnitText => TestClassification::Canonical,
                ReturnTypeKind::Bool => TestClassification::BoolDeprecated,
                ReturnTypeKind::Other => TestClassification::InvalidReturnType,
            };

            tests.push(DiscoveredTest {
                qualified_name,
                beam_module: beam_module.clone(),
                fn_name,
                classification,
            });
        }
    }

    tests
}

/// How a test function's declared return type maps to the runner contract.
enum ReturnTypeKind {
    /// `Result Unit Text` — canonical contract (OQ-C004).
    ResultUnitText,
    /// `Bool` — transitional; accepted with C303 warning.
    Bool,
    /// Anything else — rejected.
    Other,
}

/// Classify the return type of a test function.
fn return_type_classification(ret: Option<&AstType>) -> ReturnTypeKind {
    let Some(ty) = ret else {
        // No declared return type — treat as Other (cannot validate).
        return ReturnTypeKind::Other;
    };

    match ty {
        AstType::Primitive {
            name: PrimitiveType::Bool,
            ..
        } => ReturnTypeKind::Bool,
        AstType::App { head, args, .. } => {
            // Result Unit Text  →  App { head: "Result", args: [Unit, Text] }
            if head.text == "Result" && args.len() == 2 && is_unit(&args[0]) && is_text(&args[1]) {
                ReturnTypeKind::ResultUnitText
            } else {
                ReturnTypeKind::Other
            }
        }
        _ => ReturnTypeKind::Other,
    }
}

/// Returns `true` if `ty` is the `Unit` primitive.
const fn is_unit(ty: &AstType) -> bool {
    matches!(
        ty,
        AstType::Primitive {
            name: PrimitiveType::Unit,
            ..
        }
    )
}

/// Returns `true` if `ty` is the `Text` primitive.
const fn is_text(ty: &AstType) -> bool {
    matches!(
        ty,
        AstType::Primitive {
            name: PrimitiveType::Text,
            ..
        }
    )
}

// ── Module naming ─────────────────────────────────────────────────────────────

/// Look up the `ModuleMetadata` for a `TypedModule` in the workspace graph.
fn module_meta<'g>(module: &TypedModule, graph: &'g WorkspaceGraph) -> Option<&'g ModuleMetadata> {
    graph.modules.iter().find(|m| m.id == module.id)
}

/// Return a human-readable module name for display in test output.
///
/// Uses the Pascal-case file stem from the workspace graph, e.g.
/// `apps/demo/src/Demo.rg` → `"Demo"`.  Falls back to `Module<N>` when
/// the module id is not found in the graph (e.g. stdlib built-ins).
fn module_display_name(module: &TypedModule, graph: &WorkspaceGraph) -> String {
    module_meta(module, graph).map_or_else(
        || format!("Module{}", module.id.0),
        |meta| {
            meta.file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_owned()
        },
    )
}

/// Derive the BEAM module name used when spawning `erl`.
///
/// The codegen crate (Phase 6) currently mangles the BEAM module name as
/// `ridge_module_<id>` (see `codegen_one_module` in `ridge-codegen-erl`).
/// This is the stable identifier in 0.1.0.  A future task will replace it
/// with the FQN-derived name once codegen uses full workspace metadata.
fn beam_module_name(module: &TypedModule) -> String {
    format!("ridge_module_{}", module.id.0)
}

// ── BEAM execution ────────────────────────────────────────────────────────────

/// Outcome of running a single test in a BEAM child.
enum TestOutcome {
    /// Test passed (child exited 0).
    Pass,
    /// Test failed (child exited non-zero).
    Fail {
        /// Captured stderr from the BEAM child.
        stderr: String,
    },
    /// The child ran longer than the 60 s hard limit.
    Timeout,
    /// The `erl` process could not be spawned.
    SpawnFailed {
        /// OS-level error message.
        message: String,
    },
}

/// Run a single test function in a fresh BEAM child process.
///
/// Invokes:
/// ```text
/// erl -pa <beam_dir> -pa <runtime_dir>
///     -s ridge_test_runner run <module> <fn_name>
///     -s init stop -noshell
/// ```
///
/// Waits up to 60 s then kills the child if it has not exited.
fn run_test(
    erl_path: &Path,
    beam_dir: &Path,
    runtime_dir: &Path,
    beam_module: &str,
    fn_name: &str,
) -> TestOutcome {
    let mut cmd = process::Command::new(erl_path);
    cmd.arg("-pa")
        .arg(beam_dir)
        .arg("-pa")
        .arg(runtime_dir)
        .arg("-s")
        .arg("ridge_test_runner")
        .arg("run")
        .arg(beam_module)
        .arg(fn_name)
        .arg("-s")
        .arg("init")
        .arg("stop")
        .arg("-noshell");

    cmd.stdout(process::Stdio::null());
    cmd.stderr(process::Stdio::piped());

    let Ok(mut child) = cmd.spawn() else {
        return TestOutcome::SpawnFailed {
            message: String::from("failed to spawn erl process"),
        };
    };

    // Wait up to 60 s.
    let timeout = std::time::Duration::from_secs(60);
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Collect stderr.
                let stderr = child.stderr.take().map_or_else(String::new, |mut r| {
                    let mut s = String::new();
                    let _ = r.read_to_string(&mut s);
                    s
                });

                if status.success() {
                    return TestOutcome::Pass;
                }
                return TestOutcome::Fail { stderr };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return TestOutcome::Timeout;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                return TestOutcome::Fail {
                    stderr: String::new(),
                };
            }
        }
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Extract the parent directory from the first beam file in the list.
///
/// Used to locate the `beam/` output directory produced by `compile_workspace`.
pub(crate) fn beam_dir_from_artefacts(beam_files: &[PathBuf]) -> PathBuf {
    beam_files
        .first()
        .and_then(|f| f.parent())
        .map_or_else(|| PathBuf::from("."), Path::to_owned)
}

// ── Glob matching (inline — no new dep) ──────────────────────────────────────

/// Match `text` against a simple glob `pattern`.
///
/// Supports `*` (match any sequence of characters) and `?` (match exactly one
/// character).  Case-sensitive.
fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_inner(pat: &[u8], text: &[u8]) -> bool {
    match (pat.first(), text.first()) {
        (None, None) => true,
        (Some(&b'*'), _) => {
            // Star: try matching 0 chars, then 1 char, … (greedy via recursion).
            glob_match_inner(&pat[1..], text)
                || (!text.is_empty() && glob_match_inner(pat, &text[1..]))
        }
        (Some(&b'?'), Some(_)) => glob_match_inner(&pat[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => glob_match_inner(&pat[1..], &text[1..]),
        _ => false,
    }
}
