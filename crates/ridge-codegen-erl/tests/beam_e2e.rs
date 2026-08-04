//! End-to-end BEAM run on the four Ridge examples.
//!
//! Gated on `--features beam-runtime` (requires OTP installation with `erlc`
//! and `erl` on PATH).  For each of the four examples:
//!
//! 1. Compile the example workspace via `ridge_driver::compile_workspace`.
//! 2. The driver runs the full Ridge pipeline and invokes `erlc` to produce
//!    `.beam` files.
//! 3. Invoke `erl -noshell -pa <beam_dir> -s <module> main -s init stop` as a
//!    subprocess with a 60-second timeout.
//! 4. Compare normalised stdout against `tests/expected/<name>.txt`.
//!
//! `DoD`: all four tests green ↔ spec §11.3 `DoD` satisfied.
//!
//! Example status:
//! - `log_analyzer`, `game_of_life`, `rate_limiter` — compile and run end to
//!   end; stdout matches the curated `tests/expected/<name>.txt`.
//! - `url_shortener` — `#[ignore]`d: `Http.listen` enters an accept loop that
//!   never returns under a healthy server, so `main` cannot complete inside the
//!   batch-mode harness. See the test's own note for the deferred follow-up.
//!
//! Alongside the four examples this file carries focused regression tests that
//! pin individual codegen/parse fixes end to end (destructuring lambda params,
//! `List.groupBy`, else-less `if` as a statement, actor cross-module calls,
//! bounded mailboxes, and more).

#![cfg(feature = "beam-runtime")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]
// Doc comments in the ignored tests quote raw erlc stderr (variable names like
// 'V_Foo' and module names like ridge_module_0) which trigger doc_markdown.
#![allow(clippy::doc_markdown)]

use ridge_driver::{compile_workspace, CompileOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

// ── Constants ────────────────────────────────────────────────────────────────

/// Hard timeout for each `erl` invocation (60 s per plan §9).
const ERL_TIMEOUT_SECS: u64 = 60;

/// Directory containing curated expected-output files.
const EXPECTED_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/expected");

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Normalise line endings: replace `\r\n` with `\n`, trim trailing whitespace
/// from each line, and strip a trailing newline from the whole string.
///
/// Windows `erl.exe` emits `\r\n`; expected files are Unix-style.
fn normalise(s: &str) -> String {
    let unified = s.replace("\r\n", "\n");
    let trimmed_lines: Vec<&str> = unified.lines().map(str::trim_end).collect();
    trimmed_lines.join("\n")
}

/// Best-effort pipe drain for the timeout panic path.
///
/// The pipe's writer (the killed child) has been terminated by `child.kill()`,
/// so the OS read syscall reaches EOF promptly.  Any read error or partial
/// read is recorded in-band; we never re-panic from this helper.
///
/// **Hardening candidate:**
/// This helper has "may not capture all" semantics — on a heavily-loaded
/// system, the killed child may not have fully flushed its write buffer before
/// `read_to_end` completes.  The resulting panic message may show truncated
/// stdout/stderr.  This is acceptable for a contributor-facing diagnostic
/// (§1.3 #10 exemption); hardening to a bounded-timeout drain (e.g. via a
/// thread + `recv_timeout`) is deferred to a future release alongside the
/// bounded-server testing work.  Do NOT attempt to add a `sleep` or retry
/// loop here without a design decision.
fn drain_pipe<R: std::io::Read>(pipe: Option<R>) -> String {
    let Some(mut p) = pipe else {
        return String::from("<no pipe>");
    };
    let mut buf = Vec::new();
    match p.read_to_end(&mut buf) {
        Ok(_) => String::from_utf8_lossy(&buf).into_owned(),
        Err(e) => format!("<drain failed: {e}>"),
    }
}

/// Run `erl -noshell -pa <beam_dir> -s <module> main -s init stop [-extra plain_args...]`
/// with a hard timeout.  Returns `(stdout, stderr, exit_code)`.
///
/// `plain_args` are appended after `-extra` so that `init:get_plain_arguments/0`
/// (used by `ridge_rt:cli_args/1`) returns them as binary strings.
///
/// If `erl` is not on PATH, the test panics with a clear message.
/// If the process exceeds `ERL_TIMEOUT_SECS`, it is killed and the test panics.
fn run_erl(beam_dir: &Path, module: &str, plain_args: &[&str]) -> (String, String, i32) {
    let erl_path = which::which("erl")
        .expect("erl not found on PATH — install OTP or run without --features beam-runtime");

    let mut cmd = Command::new(&erl_path);
    // Use -noinput instead of -noshell: on Windows, -noshell inhibits the IO
    // subsystem's flush-on-write behaviour, causing stdout to be buffered and
    // never drained before the process exits.  -noinput leaves the IO
    // subsystem in a more flush-friendly state for batch programs.
    //
    // Prepend an -eval that sets the group_leader encoding to unicode,
    // ensuring io:format/2 flushes correctly for all Ridge programs under
    // -noinput on Windows.
    cmd.arg("-noinput")
        .arg("-eval")
        .arg("io:setopts(group_leader(), [{encoding, unicode}])")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-s")
        .arg(module)
        .arg("main")
        .arg("-s")
        .arg("init")
        .arg("stop");
    // Plain args passed via -extra are accessible via init:get_plain_arguments/0.
    if !plain_args.is_empty() {
        cmd.arg("-extra");
        for arg in plain_args {
            cmd.arg(arg);
        }
    }

    wait_and_capture(cmd, module)
}

/// Start a program through `ridge_main_runner`, the way `ridge run` and a
/// released escript do, rather than calling `<module>:main/0` from the boot
/// process. The entry point is part of the behaviour under test whenever
/// crashes, exit codes, or diagnostic streams are involved.
fn run_erl_via_runner(beam_dir: &Path, module: &str) -> (String, String, i32) {
    let erl_path = which::which("erl")
        .expect("erl not found on PATH — install OTP or run without --features beam-runtime");

    let mut cmd = Command::new(&erl_path);
    cmd.arg("-noinput")
        .arg("-eval")
        .arg("io:setopts(group_leader(), [{encoding, unicode}])")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-s")
        .arg("ridge_main_runner")
        .arg("run")
        .arg(module)
        .arg("main")
        .arg("-s")
        .arg("init")
        .arg("stop");

    wait_and_capture(cmd, module)
}

/// Run `cmd` to completion with a timeout, returning `(stdout, stderr, code)`.
fn wait_and_capture(mut cmd: Command, module: &str) -> (String, String, i32) {
    // Spawn and wait with a manual timeout using a thread.
    let timeout = Duration::from_secs(ERL_TIMEOUT_SECS);

    // Use std::process::Command::output() which waits for completion.
    // For timeout we use a separate thread approach.
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn erl process");

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout_bytes = {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    if let Some(mut s) = child.stdout.take() {
                        let _ = s.read_to_end(&mut buf);
                    }
                    buf
                };
                let stderr_bytes = {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    if let Some(mut s) = child.stderr.take() {
                        let _ = s.read_to_end(&mut buf);
                    }
                    buf
                };
                let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
                let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
                let exit_code = status.code().unwrap_or(-1);
                return (stdout, stderr, exit_code);
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();

                    // Drain the captured pipes BEFORE panicking so the panic
                    // message surfaces what the BEAM-side actually emitted.
                    // Best-effort: if draining fails, proceed with a placeholder.
                    let stdout = drain_pipe(child.stdout.take());
                    let stderr = drain_pipe(child.stderr.take());
                    panic!(
                        "erl process timed out after {ERL_TIMEOUT_SECS} seconds for module {module}\n\
                         --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                panic!("error waiting for erl process: {e}");
            }
        }
    }
}

/// Read the curated expected output for an example.
fn read_expected(name: &str) -> String {
    let path = Path::new(EXPECTED_DIR).join(format!("{name}.txt"));
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read expected file {}: {e}", path.display()))
}

/// Build a temporary single-member workspace for the given example source.
///
/// Returns `(workspace_path, TempDir)` — keep `TempDir` alive for the test
/// duration so the workspace isn't deleted prematurely.
fn make_example_workspace(name: &str, source: &str) -> (PathBuf, tempfile::TempDir) {
    let td = tempfile::TempDir::new().expect("create temp workspace dir");
    let root = td.path().to_owned();
    let write = |rel: &str, content: &str| {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        fs::write(&full, content).expect("write file");
    };
    write(
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write(
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write(&format!("apps/demo/src/{name}.ridge"), source);
    (root, td)
}

/// Core helper: compile workspace via the driver → erl → compare stdout.
///
/// Derives beam_dir from `CompileArtefacts.beam_files[0].parent()`.
fn run_example_e2e(name: &str, extra_erl_args: &[&str]) -> (String, String, i32) {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let example_path = Path::new(manifest_dir)
        .join("../../examples")
        .join(format!("{name}.ridge"));

    let source = fs::read_to_string(&example_path)
        .unwrap_or_else(|e| panic!("could not read example {}: {e}", example_path.display()));

    // ── 1. Compile via ridge-driver ───────────────────────────────────────────
    let (workspace_root, _td) = make_example_workspace(name, &source);
    let opts = CompileOptions::new(workspace_root);
    let artefacts = compile_workspace(opts)
        .unwrap_or_else(|e| panic!("compile_workspace failed for example {name}: {e}"));

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced for example {name}\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    // ── 2. Locate beam dir and module name ────────────────────────────────────
    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file
        .parent()
        .unwrap_or_else(|| panic!("beam_path has no parent dir for {name}"))
        .to_owned();

    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_else(|| panic!("beam file stem is not valid UTF-8 for {name}"))
        .to_owned();

    // ── 3. Run erl ────────────────────────────────────────────────────────────
    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, extra_erl_args);

    assert_eq!(
        exit_code, 0,
        "erl exited with code {exit_code} for example {name}\n\
         --- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}"
    );

    let actual = normalise(&stdout);
    let expected = normalise(&read_expected(name));
    assert_eq!(
        actual, expected,
        "stdout mismatch for example {name}\n--- expected ---\n{expected}\n--- actual ---\n{actual}\n--- stderr ---\n{stderr}"
    );

    (stdout, stderr, exit_code)
}

// ── Example tests ─────────────────────────────────────────────────────────────

/// `log_analyzer` — codegen bugs affecting record construction, lambda
/// destructuring, and partial application were resolved in a prior pass.
/// The example now compiles and runs; it requires CLI arguments
/// `<log_file> <MIN_LEVEL>` passed via `-extra`.
///
/// Fixture: `tests/fixtures/sample.log`, threshold: `WARN`.
#[test]
fn beam_e2e_log_analyzer() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.log");
    let _ = run_example_e2e("log_analyzer", &[fixture, "WARN"]);
}

/// `url_shortener` — all codegen bugs resolved.
///
/// Previously blocked by unbound variable errors in main/0 and handle_call/3:
///   ridge_module_0: unbound variable 'V_Url' in main/0
///   ridge_module_0_store: unbound variable 'V_Code' in handle_call/3
///
/// RESOLVED via fixes to Ok/Err constructor codegen, state-field reads,
/// cross-module refs, SSA state threading, and the try/catch printer.
///
/// ---
///
/// # Deferred
///
/// **What is deferred:**
/// Testability of `url_shortener` under the BEAM e2e batch harness.
/// NOTE: `Http.listen` itself is NOT deferred — it works correctly in
/// production. What is deferred is the ability to run this example end-to-end
/// inside a batch-mode test harness.
///
/// **Why:**
/// Structural — `Http.listen` (implemented at
/// `crates/ridge-codegen-erl/runtime/ridge_rt.erl:341-364`) enters
/// `http_accept_loop/2` which only exits on `{error, closed}` (i.e., never
/// under a healthy server). The BEAM e2e harness is batch-only
/// (`erl -noinput -s <mod> main -s init stop`); `main` never returns under
/// any non-pathological execution, so the 60-second harness timeout is the
/// inevitable outcome of any healthy run. This is a test-design mismatch,
/// not a bug in `url_shortener.ridge`.
///
/// **Where the follow-up lives:**
/// A bounded-server testing harness for long-running Ridge examples.
/// The `#[ignore]` is trivially reversible — un-mark it once a bounded-server
/// harness integration is in place.
///
/// **Rejected alternatives (do NOT revisit):**
/// - Option (ii) runtime-side env-var-driven test-aware accept-timeout:
///   rejected — env-var-gated runtime semantics is a capability side-channel
///   that breaks "runtime behaves identically in test and prod".
/// - Option (iii) example-side bounded-server rewrite: rejected — corrupts
///   the canonical pedagogy of `url_shortener.ridge` as a faithful HTTP server
///   demonstration.
///
/// These options were considered and declined; they SHALL NOT be
/// reopened without a new plan-level decision.
#[ignore = "Http.listen blocks BEAM accept loop; testability deferred"]
#[test]
fn beam_e2e_url_shortener() {
    let _ = run_example_e2e("url_shortener", &[]);
}

/// `game_of_life` — all codegen bugs resolved.
///
/// Previously blocked by a bad-map error in setCell/3:
///   {badmap, ok} in setCell/3 — `map_get(rows, ok)` — `with` update
///   expression was returning `ok` instead of the updated map.
///
/// RESOLVED via fixes to Ok/Err constructor codegen, tuple-param lambda
/// destructuring, partial-app eta-wrapping, zero-arity constant apply, and
/// the try/catch printer.
#[test]
fn beam_e2e_game_of_life() {
    let _ = run_example_e2e("game_of_life", &[]);
}

/// `rate_limiter` — a three-actor program (a token-bucket limiter, a stats
/// collector, and worker drivers) that exercises multi-actor codegen end to
/// end: `spawn`, message sends, `state` field reads and writes, and cross-actor
/// calls all run on the BEAM.
///
/// Getting here meant clearing a cluster of multi-actor codegen bugs: Ok/Err
/// constructor emission inside actor handlers, `state` field reads emitted as
/// bare variables instead of `maps:get`, and SSA state-thread index mismatches
/// that left handler-local vars unbound. Those are resolved; the program now
/// compiles and its stdout matches `tests/expected/rate_limiter.txt` — the
/// assertion (beam produced, exit 0, stdout equals expected) lives in
/// `run_example_e2e`.
#[test]
fn beam_e2e_rate_limiter() {
    let _ = run_example_e2e("rate_limiter", &[]);
}

// ── Regression: actor handler reaches into parent-module fns ─────────────────
//
// An actor compiles to its own BEAM module (`<parent>_<actor>`), so any call
// from inside a handler (or an inner lambda nested in one) to a top-level
// fn of the source file must be emitted as a qualified
// `call 'parent':'fn' (args…)` AND the target must appear in the parent
// module's export list.
//
// Two failure modes used to surface in practice:
//
//   1. `lower_lambda` dropped `actor_parent` when it created the per-lambda
//      scope, so any inner `fn helper = ...` that called a parent-module fn
//      emitted a bare `apply 'fn'/n (...)` → erlc rejected the actor's .core
//      with `undefined function fn/n in handle_cast/2`.
//   2. Private (non-`pub`) parent-module fns were never added to the BEAM
//      export list, so even after the qualified call was emitted, the actor
//      module saw `undefined function 'parent':'fn'/n` at runtime.
//
// This regression compiles + runs a program that exercises both shapes —
// a direct call from a handler body AND a call from an inner fn — and
// checks the BEAM exits 0.  The previous failure mode was a runtime crash
// with a `function_clause` / `undef` exit reason and a non-zero exit code.

const ACTOR_CROSS_MODULE_CALL_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time

-- Private parent-module fn called from inside an actor handler body.
-- Before the export-widening fix this was missing from the BEAM exports
-- and the qualified call from the actor failed at runtime.
fn double (n: Int) -> Int =
    n + n

-- Private parent-module fn called from inside an inner lambda nested
-- in an actor handler — the path lower_lambda used to drop actor_parent
-- on, producing a bare unqualified call rejected by erlc.
fn triple (n: Int) -> Int =
    n + n + n

actor Reach =
    state result: Int = 0

    on io tick (n: Int) -> Unit =
        let direct = double n
        fn nested (x: Int) -> Int =
            triple x
        let viaInner = nested n
        result <- direct + viaInner
        Io.println $"reach ${Int.toText n} = ${Int.toText result}"

fn spawn io time main () -> Result Unit Text =
    let r = spawn Reach
    r ! tick 5
    Time.sleep 200
    Ok ()
"#;

/// Regression: an actor handler reaches into the parent module's private
/// top-level fns both directly and through an inner fn (lambda).  Both
/// paths must produce qualified cross-module calls AND the parent module
/// must export the targets, regardless of Ridge `pub` visibility.
#[test]
fn beam_e2e_actor_reaches_parent_module_fns() {
    let (workspace_root, _td) = make_example_workspace("Reach", ACTOR_CROSS_MODULE_CALL_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts = compile_workspace(opts)
        .expect("compile_workspace failed for actor cross-module regression");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // tick 5 → double 5 = 10, triple 5 = 15, result = 25.
    assert!(
        stdout.contains("reach 5 = 25"),
        "expected 'reach 5 = 25' in stdout, got:\n{stdout}"
    );
}

// ── HOF-over-self-recursive-fn regression coverage ────────────────────────────

/// Source: stresses three angles of passing a self-recursive top-level fn as a
/// first-class value to a stdlib HOF.  Earlier dx-test work (lisp, json-xml,
/// parser-combinators) avoided this pattern with explicit-recursion
/// workarounds against a `{badfun, ok}` BEAM crash that no longer
/// reproduces.  These cases pin the working behaviour so the pattern cannot
/// silently regress under future codegen refactors.
const HOF_OVER_RECURSIVE_FN_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List

-- 1. Simple self-recursive Int -> Int passed to List.map.
fn countdown (n: Int) -> Int =
    if n <= 0 then 0
    else countdown (n - 1)

-- 2. Self-recursive fn returning a union, passed to List.map and then
--    threaded through List.filter with another fn-value.
type R = ROk Int | RErr Text

fn evalLeaf (n: Int) -> R =
    if n < 0 then RErr "neg"
    else if n == 0 then ROk 0
    else evalLeaf (n - 1)

fn isOk (r: R) -> Bool =
    match r
        ROk _ -> true
        RErr _ -> false

-- 3. Tree-recursive walk over a self-referential union via List.map.
type Tree = TLeaf Int | TNode (List Tree)

fn sumTree (t: Tree) -> Int =
    match t
        TLeaf n -> n
        TNode kids ->
            let parts = List.map sumTree kids
            List.fold (fn acc x -> acc + x) 0 parts

fn io main () -> Result Unit Text =
    let ns = [3, 2, 1, 0]
    let downs = List.map countdown ns
    let okCount = List.length (List.filter isOk (List.map evalLeaf ns))
    let t = TNode [TLeaf 1, TNode [TLeaf 2, TLeaf 3], TLeaf 4]
    let total = sumTree t
    Io.println $"countdown=${Int.toText (List.length downs)} ok=${Int.toText okCount} tree=${Int.toText total}"
    Ok ()
"#;

/// Regression: passing a self-recursive top-level fn (or one that calls itself
/// indirectly through a sibling) to `List.map` / `List.filter` / `List.fold`
/// must run end-to-end on the BEAM and produce the expected results.  Without
/// this coverage the historical `{badfun, ok}` crash could resurface
/// undetected and apps would have to bring back the explicit-recursion
/// workaround documented in the Tier 5 dx-tests.
#[test]
fn beam_e2e_hof_over_self_recursive_fn() {
    let (workspace_root, _td) = make_example_workspace("Hof", HOF_OVER_RECURSIVE_FN_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts =
        compile_workspace(opts).expect("compile_workspace failed for HOF-recursive regression");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // countdown over [3,2,1,0] → 4 results.
    // evalLeaf over [3,2,1,0] → 4 ROk → 4.
    // sumTree TNode[TLeaf 1, TNode[TLeaf 2, TLeaf 3], TLeaf 4] → 1+2+3+4 = 10.
    assert!(
        stdout.contains("countdown=4 ok=4 tree=10"),
        "expected 'countdown=4 ok=4 tree=10' in stdout, got:\n{stdout}"
    );
}

// ── Destructuring lambda params ──────────────────────────────────────────────
//
// A lambda param that is not a plain variable binds its variables during
// type-checking but used to lose them at lowering: a constructor pattern
// (`fn (Some n) -> ...`) dropped the binding entirely, and a nested tuple
// (`fn ((p, q), r) -> ...`) degraded its inner elements to wildcards. Both
// produced Core Erlang the backend rejected with `unbound variable`, even
// though `ridge check` reported success. This pins the end-to-end behaviour of
// the generalised destructuring-param lowering route.

const LAMBDA_DESTRUCTURING_PARAMS_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List

fn io main () -> Result Unit Text =
    -- Constructor-pattern param: `n` was bound in the checker but dropped at
    -- lowering, so erlc rejected the body with `unbound variable 'V_N'`.
    let inc = fn (Some n) -> n + 1
    let a = inc (Some 41)
    -- Nested tuple param: the inner `p`/`q` used to degrade to wildcards, so
    -- only the outer arity survived.
    let sum3 = fn ((p, q), r) -> p + q + r
    let b = sum3 ((10, 20), 30)
    -- Tuple param through a stdlib HOF (the historically working path) — now
    -- shares the same lowering route, so it must keep working.
    let sums = List.map (fn (k, v) -> k + v) [(1, 2), (3, 4)]
    let c = List.fold (fn acc x -> acc + x) 0 sums
    Io.println $"a=${Int.toText a} b=${Int.toText b} c=${Int.toText c}"
    Ok ()
"#;

/// Regression: destructuring lambda params must bind their variables end to
/// end.  Before the fix a constructor-pattern or nested-tuple param passed
/// `ridge check` but the backend rejected the lowered body with `unbound
/// variable`.  Compiles + runs the program and checks the computed output.
#[test]
fn beam_e2e_lambda_destructuring_params() {
    let (workspace_root, _td) =
        make_example_workspace("Destructure", LAMBDA_DESTRUCTURING_PARAMS_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts = compile_workspace(opts)
        .expect("compile_workspace failed for lambda-destructuring regression");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // inc (Some 41) = 42; sum3 ((10,20),30) = 60; map+fold of (k+v) over
    // [(1,2),(3,4)] = 3 + 7 = 10.
    assert!(
        stdout.contains("a=42 b=60 c=10"),
        "expected 'a=42 b=60 c=10' in stdout, got:\n{stdout}"
    );
}

// ── std.list::groupBy regression ──────────────────────────────────────────────
//
// `List.groupBy` used to be a dead stub returning an empty map, so any program
// that grouped a list silently lost every element. The real implementation
// folds each element into its key's bucket and must preserve the input order
// within a bucket. The signature `List.fold (\acc x -> acc*10 + x) 0` encodes
// both membership and order in a single integer: bucket [1,2,3] -> 123, and a
// reversed bucket (the classic left-fold-prepend mistake) would show 321.

const GROUP_BY_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List
import std.map as Map

-- Order-sensitive fold: [1,2,3] -> 123, [4,5,6] -> 456. A reversed bucket
-- would surface as 321 / 654, so this pins ordering as well as membership.
fn sigOf (k: Int) (m: Map Int (List Int)) -> Int =
    match Map.get k m
        Some g -> List.fold (fn acc x -> acc * 10 + x) 0 g
        None   -> 0 - 1

fn io main () -> Result Unit Text =
    let groups = List.groupBy (fn x -> if x > 3 then 1 else 0) [1, 2, 3, 4, 5, 6]
    Io.println $"n=${Int.toText (Map.length groups)} lo=${Int.toText (sigOf 0 groups)} hi=${Int.toText (sigOf 1 groups)}"
    Ok ()
"#;

/// Regression: `List.groupBy` partitions a list by key into a `Map key
/// (List elem)`, preserving encounter order inside each bucket. Before the fix
/// it was a stub returning an empty map, so grouping dropped every element.
#[test]
fn beam_e2e_list_group_by() {
    let (workspace_root, _td) = make_example_workspace("GroupBy", GROUP_BY_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts =
        compile_workspace(opts).expect("compile_workspace failed for groupBy regression");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // Two buckets (key 0 for x<=3, key 1 for x>3), each in input order.
    assert!(
        stdout.contains("n=2 lo=123 hi=456"),
        "expected 'n=2 lo=123 hi=456' in stdout, got:\n{stdout}"
    );
}

// ── else-less `if` as a non-final statement ──────────────────────────────────
//
// `parse_if` used to consume the newline that separates an else-less `if` from
// the statement that follows it, eaten while probing for an absent `else`. That
// newline is the statement separator the enclosing block relies on, so the two
// statements fused: the source below would either miscompile or drop the
// trailing statements. This pins the end-to-end behaviour for both layout
// shapes — a single-line then-branch (`if c then e`) and an indented multi-line
// then-branch — used as non-final statements, over both a taken and a skipped
// guard.

const IF_NO_ELSE_STATEMENT_SOURCE: &str = r#"
import std.io as Io
import std.int as Int

fn io main () -> Result Unit Text =
    let a = 5
    -- Single-line then-branch, else-less, followed by another statement.
    if a > 3 then Io.println "a-big"
    let b = 1
    -- Indented multi-line then-branch, else-less, guard false, followed by more.
    if b > 3 then
        Io.println "b-big"
    let total = a + b
    Io.println $"total=${Int.toText total}"
    Ok ()
"#;

/// Regression: an else-less `if` used as a non-final statement must not swallow
/// the statement that follows it. Before the fix `parse_if` ate the newline
/// separator while probing for an absent `else`, fusing the two statements.
/// The taken guard prints `a-big`, the skipped guard prints nothing, and the
/// trailing `let total`/`println` must still run — proving the statements stay
/// separate over both branch outcomes and both layout shapes.
#[test]
fn beam_e2e_if_no_else_as_statement() {
    let (workspace_root, _td) = make_example_workspace("IfNoElse", IF_NO_ELSE_STATEMENT_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts = compile_workspace(opts)
        .expect("compile_workspace failed for else-less-if statement regression");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // a=5 > 3 → "a-big"; b=1 > 3 is false → no "b-big"; total = 5 + 1 = 6, which
    // only holds if `let b`/`let total`/the final println stayed separate stmts.
    assert!(
        stdout.contains("a-big"),
        "expected 'a-big' (taken guard) in stdout, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("b-big"),
        "did not expect 'b-big' (skipped guard) in stdout, got:\n{stdout}"
    );
    assert!(
        stdout.contains("total=6"),
        "expected 'total=6' (trailing statements ran) in stdout, got:\n{stdout}"
    );
}

// ── `Ordering` constructors are in scope without an import ────────────────────
//
// `Ordering = Less | Equal | Greater` is a prelude builtin (the result type of
// `compare`), available in every module without an import. Its type name and
// constructors are registered for type checking, but the resolver did not seed
// the constructor names, so hand-matching a comparison result — `match compare
// a b { Less -> … }` — or naming a bare `Less`/`Equal`/`Greater` value failed
// name resolution with `R010` before type checking ever ran. Nothing in the
// standard library matches on an `Ordering` (it flows straight into `List.sort`
// and friends), so no test exercised the path.
//
// The constructors lower to the bare atoms `'Less'`/`'Equal'`/`'Greater'` — the
// exact terms the runtime comparator returns — so a value built here matches a
// value produced by `compare`. This runs both directions end to end on the BEAM.

const ORDERING_CONSTRUCTORS_SOURCE: &str = r#"
import std.io as Io

-- Hand-match the result of `compare` (an `Ordering`) without importing anything.
fn classify (a: Int) (b: Int) -> Text =
    match compare a b
        Less    -> "lt"
        Equal   -> "eq"
        Greater -> "gt"

-- Round-trip a bare `Ordering` value through a match: the constructor built here
-- must match the same atom the comparator above produces.
fn name (o: Ordering) -> Text =
    match o
        Less    -> "less"
        Equal   -> "equal"
        Greater -> "greater"

fn io main () -> Result Unit Text =
    Io.println (classify 1 2)
    Io.println (classify 5 5)
    Io.println (classify 9 4)
    Io.println (name Less)
    Io.println (name Equal)
    Io.println (name Greater)
    Ok ()
"#;

/// Regression: the prelude `Ordering` constructors must resolve in user code so a
/// comparison result can be matched by hand and a bare `Less`/`Equal`/`Greater`
/// can be constructed. Both must agree on the runtime representation, so a value
/// built in `name` matches the atom `compare` returns in `classify`.
#[test]
fn beam_e2e_ordering_constructors_resolve_and_match() {
    let (workspace_root, _td) =
        make_example_workspace("OrderingCtors", ORDERING_CONSTRUCTORS_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts =
        compile_workspace(opts).expect("compile_workspace failed for Ordering constructors");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines,
        vec!["lt", "eq", "gt", "less", "equal", "greater"],
        "unexpected output; full stdout:\n{stdout}"
    );
}

// ── `Float.toText` renders ordinary magnitudes without exponent notation ──────
//
// Erlang's shortest round-trip float form uses exponent notation for values
// like 5600.0 (`5.6e3`), which leaked through `Float.toText` and made printed
// floats (averages, prices, counts) jarring and inconsistent in the same column
// of output. `Float.toText` now re-renders the shortest digits positionally for
// a readable exponent range, keeping the round-trip digits intact, so a whole
// or near-whole float prints as a plain decimal.

const FLOAT_TOTEXT_SOURCE: &str = r#"
import std.io as Io
import std.float as Float

fn io main () -> Result Unit Text =
    Io.println (Float.toText 5600.0)
    Io.println (Float.toText 90000.0)
    Io.println (Float.toText 1200.0)
    Io.println (Float.toText 2965.0)
    Io.println (Float.toText 0.001)
    Io.println (Float.toText 3.14159)
    Io.println (Float.toText (0.0 - 5600.0))
    Io.println (Float.toText 0.0)
    Ok ()
"#;

/// Regression: `Float.toText` must render ordinary-magnitude floats as plain
/// decimals rather than Erlang's shortest exponent form (`5.6e3`), while keeping
/// the exact round-trip digits — negative and fractional values included.
#[test]
fn beam_e2e_float_totext_avoids_exponent_notation() {
    let (workspace_root, _td) = make_example_workspace("FloatToText", FLOAT_TOTEXT_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts =
        compile_workspace(opts).expect("compile_workspace failed for Float.toText rendering");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines,
        vec!["5600.0", "90000.0", "1200.0", "2965.0", "0.001", "3.14159", "-5600.0", "0.0"],
        "unexpected output; full stdout:\n{stdout}"
    );
}

// `std.list.foldRight` is a direct `@ffi("lists", "foldr", 3)` bridge with no
// argument-adapting wrapper, so its correctness rests on two facts that only a
// real BEAM run can prove: the uncurried callback the type system requires is
// handed to `lists:foldr` as a native 2-arity fun, and the elements arrive in
// the `(elem, acc)` order the Ridge signature `fn a -> b -> b` promises. Erlang
// calls its foldr callback as `Fun(Elem, Acc)`, which happens to match, so no
// wrapper is needed — but nothing pinned that, and a stray swap would go
// unnoticed by every compile-only test. The module-level `foldRight` compile
// checks never invoke it, and the `std.list` unit suite exercises a local
// pure-Ridge reimplementation rather than the FFI bridge, so this is the only
// test that runs the real thing.

const FOLD_RIGHT_FFI_SOURCE: &str = r#"
import std.io as Io
import std.list as List
import std.int as Int

fn cons (x: Int) (acc: List Int) -> List Int = x :: acc

fn io main () -> Result Unit Text =
    -- Idiomatic uncurried callback.
    let sum = List.foldRight (fn x acc -> x + acc) 0 [1, 2, 3]
    -- Partial application of a named 2-arg fn: right-folding cons over a list
    -- rebuilds it, so `rebuilt` must equal the input [1, 2, 3].
    let rebuilt = List.foldRight cons [] [1, 2, 3]
    -- Non-commutative fold pins the callback argument order. A right fold of
    -- subtraction is 1 - (2 - (3 - 0)) = 2; had the FFI handed the callback its
    -- arguments swapped as (acc, elem), it would compute -6 instead.
    let rsub = List.foldRight (fn x acc -> x - acc) 0 [1, 2, 3]
    let first = match rebuilt
        x :: _ -> Int.toText x
        [] -> "empty"
    let _ = Io.println $"sum=${Int.toText sum}"
    let _ = Io.println $"rsub=${Int.toText rsub}"
    let _ = Io.println $"rebuilt_first=${first} rebuilt_len=${Int.toText (List.length rebuilt)}"
    Ok ()
"#;

/// Regression: `std.list.foldRight` must be callable through its real
/// `lists:foldr` FFI bridge with the correct element/accumulator ordering.
/// `sum=6` proves the uncurried callback reaches `lists:foldr`; `rsub=2`
/// (not `-6`) proves the callback receives `(elem, acc)` and not the swapped
/// pair; `rebuilt` equal to the input proves partial application of a named
/// 2-arg fn folds right the way a hand-written cons would.
#[test]
fn beam_e2e_fold_right_ffi_arg_order() {
    let (workspace_root, _td) = make_example_workspace("FoldRightFfi", FOLD_RIGHT_FFI_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts =
        compile_workspace(opts).expect("compile_workspace failed for foldRight FFI regression");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("sum=6"),
        "expected 'sum=6' (uncurried callback reached lists:foldr), got:\n{stdout}"
    );
    // The distinguishing check: correct (elem, acc) order yields 2, a swap yields -6.
    assert!(
        stdout.contains("rsub=2"),
        "expected 'rsub=2' (correct elem/acc order); a swap would print -6, got:\n{stdout}"
    );
    assert!(
        stdout.contains("rebuilt_first=1 rebuilt_len=3"),
        "expected right-folded cons to rebuild [1, 2, 3], got:\n{stdout}"
    );
}

// ── Bounded mailbox + observability tests ─────────────────────────────────────
//
// Six tests pin the bounded-mailbox runtime end to end:
//
//   1. Unbounded actors keep their 0.1.0 / 0.2.6 behaviour (regression).
//   2. drop_newest below the cap delivers every message.
//   3. drop_newest above the cap caps the queue at N.
//   4. error    below the cap delivers every message.
//   5. error    above the cap raises {mailbox_full, _} in the sender, so the
//      BEAM exits non-zero with that reason in stderr.
//   6. Actor.mailboxSize reports Some for a freshly-spawned (live) actor.
//   7. Actor.mailboxSize reports None once that actor has crashed.
//
// Case 7 was blocked for two releases: killing an actor from Ridge source
// meant crashing it, and `gen_server:start_link` took the spawner down with
// it, so the recipe collapsed the program instead of leaving a dead handle to
// read. `spawn` no longer links, so the case is finally expressible.

/// Compiles an inline program and returns `(beam_dir, module, tempdir guard)`.
/// The guard must stay alive while the beams are used.
fn compile_inline_actor_test(name: &str, source: &str) -> (PathBuf, String, tempfile::TempDir) {
    let (workspace_root, td) = make_example_workspace(name, source);
    let opts = CompileOptions::new(workspace_root);
    let artefacts = compile_workspace(opts)
        .unwrap_or_else(|e| panic!("compile_workspace failed for {name}: {e}"));
    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files for {name}\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );
    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam parent").to_owned();
    let module = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem utf-8")
        .to_owned();
    (beam_dir, module, td)
}

/// Helper that compiles, runs, and asserts a zero exit code, returning
/// `(stdout, stderr)`.
fn run_inline_actor_test(name: &str, source: &str) -> (String, String) {
    let (beam_dir, module, _td) = compile_inline_actor_test(name, source);
    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code} for {name}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    (stdout, stderr)
}

/// Same, but entered through `ridge_main_runner` — the path `ridge run` and a
/// released escript take. Use it whenever the behaviour under test depends on
/// the entry point: `-s <module> main` starts the program in Erlang's boot
/// process, which traps exits and leaves the default logger on stdout, so it
/// reports neither fatal signals nor diagnostics the way a real run does.
fn run_inline_actor_test_via_runner(name: &str, source: &str) -> (String, String) {
    let (beam_dir, module, _td) = compile_inline_actor_test(name, source);
    let (stdout, stderr, exit_code) = run_erl_via_runner(&beam_dir, &module);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code} for {name}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    (stdout, stderr)
}

// ── 1. unbounded baseline ────────────────────────────────────────────────────

const MAILBOX_UNBOUNDED_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List
import std.time as Time

actor Counter =
    state n: Int = 0
    on tick = n <- n + 1

fn spawn io time main () -> Result Unit Text =
    let c = spawn Counter
    let _ = List.map (fn _ -> c ! tick) (List.range 1 100)
    -- Let the actor drain the 100 casts before we exit.
    Time.sleep 200
    Io.println "unbounded ok"
    Ok ()
"#;

/// Sending 100 messages to an actor with no `mailbox` member must succeed
/// end to end with no overflow signalling — this is the 0.1.0 / 0.2.6
/// behaviour the cut promises to preserve.
#[test]
fn beam_e2e_mailbox_unbounded_unchanged() {
    let (stdout, _) = run_inline_actor_test("MailboxUnbounded", MAILBOX_UNBOUNDED_SOURCE);
    assert!(
        stdout.contains("unbounded ok"),
        "expected 'unbounded ok' in stdout, got:\n{stdout}"
    );
}

// ── 2. drop_newest under cap delivers every message ─────────────────────────

const MAILBOX_DROP_NEWEST_UNDER_CAP_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List
import std.time as Time

actor Counter =
    mailbox bounded 100 drop newest
    state n: Int = 0
    on tick = n <- n + 1

fn spawn io time main () -> Result Unit Text =
    let c = spawn Counter
    let _ = List.map (fn _ -> c ! tick) (List.range 1 50)
    Time.sleep 200
    Io.println "drop newest under cap ok"
    Ok ()
"#;

/// 50 messages to a `drop newest` actor bounded at 100 must all be
/// delivered — overflow logic must not fire while the queue stays under
/// the cap.
#[test]
fn beam_e2e_mailbox_drop_newest_under_cap() {
    let (stdout, _) = run_inline_actor_test(
        "MailboxDropNewestUnder",
        MAILBOX_DROP_NEWEST_UNDER_CAP_SOURCE,
    );
    assert!(
        stdout.contains("drop newest under cap ok"),
        "expected 'drop newest under cap ok' in stdout, got:\n{stdout}"
    );
}

// ── 3. drop_newest over cap caps the queue length ───────────────────────────

const MAILBOX_DROP_NEWEST_OVERFLOW_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List
import std.time as Time
import std.actor as Actor

actor Slow =
    mailbox bounded 5 drop newest
    state n: Int = 0
    on tick =
        Time.sleep 100
        n <- n + 1

fn spawn io time main () -> Result Unit Text =
    let s = spawn Slow
    -- Saturate the bounded mailbox while the actor is stuck inside the
    -- first handler's 100 ms sleep. The queue caps at 5; the rest are
    -- silently dropped.
    let _ = List.map (fn _ -> s ! tick) (List.range 1 100)
    match Actor.mailboxSize s
        Some n -> Io.println $"size=${Int.toText n}"
        None -> Io.println "dead"
    Ok ()
"#;

/// Saturating a `drop newest` actor bounded at 5 with 100 sends must cap
/// the queue at 5 — the rest of the messages are silently dropped, so
/// `Actor.mailboxSize` reports a small number bounded by the declared cap.
#[test]
fn beam_e2e_mailbox_drop_newest_overflow_caps_queue() {
    let (stdout, _) = run_inline_actor_test(
        "MailboxDropNewestOverflow",
        MAILBOX_DROP_NEWEST_OVERFLOW_SOURCE,
    );
    let size_line = stdout
        .lines()
        .find(|l| l.starts_with("size="))
        .unwrap_or_else(|| panic!("expected a `size=...` line, got:\n{stdout}"));
    let size: u32 = size_line
        .trim_start_matches("size=")
        .parse()
        .unwrap_or_else(|e| panic!("could not parse size in {size_line:?}: {e}"));
    assert!(
        size <= 5,
        "drop_newest bound is 5; observed mailboxSize {size} exceeds the cap.\nstdout:\n{stdout}"
    );
}

// ── 4. error under cap delivers every message ───────────────────────────────

const MAILBOX_ERROR_UNDER_CAP_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List
import std.time as Time

actor Counter =
    mailbox bounded 100 error
    state n: Int = 0
    on tick = n <- n + 1

fn spawn io time main () -> Result Unit Text =
    let c = spawn Counter
    let _ = List.map (fn _ -> c ! tick) (List.range 1 50)
    Time.sleep 200
    Io.println "error under cap ok"
    Ok ()
"#;

/// 50 messages to an `error` actor bounded at 100 must all be delivered —
/// the sender never sees an overflow signal while the queue stays under
/// the cap.
#[test]
fn beam_e2e_mailbox_error_under_cap() {
    let (stdout, _) = run_inline_actor_test("MailboxErrorUnder", MAILBOX_ERROR_UNDER_CAP_SOURCE);
    assert!(
        stdout.contains("error under cap ok"),
        "expected 'error under cap ok' in stdout, got:\n{stdout}"
    );
}

// ── 5. error overflow via `!` crashes the sender ────────────────────────────

const MAILBOX_ERROR_OVERFLOW_SOURCE: &str = r#"
import std.io as Io
import std.list as List
import std.time as Time

actor Slow =
    mailbox bounded 5 error
    state n: Int = 0
    on tick =
        Time.sleep 200
        n <- n + 1

fn spawn io time main () -> Result Unit Text =
    let s = spawn Slow
    -- Saturate the bounded mailbox. The 6th cast onward raises
    -- {mailbox_full, _} in the sender (this process); main never reaches
    -- the println below, the linked BEAM exits non-zero, and erl prints
    -- the reason on stderr.
    let _ = List.map (fn _ -> s ! tick) (List.range 1 100)
    Io.println "should not reach"
    Ok ()
"#;

/// Saturating an `error` actor bounded at 5 must raise `{mailbox_full, _}`
/// in the caller. With main as the caller, the BEAM exits non-zero and the
/// reason is visible in `erl`'s stderr.
#[test]
fn beam_e2e_mailbox_error_overflow_crashes_sender() {
    let (workspace_root, _td) =
        make_example_workspace("MailboxErrorOverflow", MAILBOX_ERROR_OVERFLOW_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts = compile_workspace(opts).expect("compile error-overflow source");
    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam parent").to_owned();
    let module = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem utf-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module, &[]);
    assert_ne!(
        exit_code, 0,
        "expected non-zero exit code (sender crash via `!` overflow), got 0\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("mailbox_full"),
        "expected 'mailbox_full' in stdout+stderr, got:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        !stdout.contains("should not reach"),
        "main should crash before reaching the println; stdout was:\n{stdout}"
    );
}

// ── 6. mailboxSize reports Some for a live actor ────────────────────────────

const MAILBOX_SIZE_ALIVE_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.actor as Actor

actor Quiet =
    state n: Int = 0
    on tick = n <- n + 1

fn spawn io main () -> Result Unit Text =
    let q = spawn Quiet
    match Actor.mailboxSize q
        Some n -> Io.println $"size=${Int.toText n}"
        None -> Io.println "dead"
    Ok ()
"#;

/// A freshly-spawned actor with no pending messages must report `Some 0`
/// from `ridge_rt:mailbox_size/1`. The match arm proves the runtime hands
/// the Ridge program an `Option` tuple; the digit proves the queue length
/// round-trips through the FFI.
#[test]
fn beam_e2e_mailbox_size_reports_some_for_live_actor() {
    let (stdout, _) = run_inline_actor_test("MailboxSizeAlive", MAILBOX_SIZE_ALIVE_SOURCE);
    assert!(
        stdout.contains("size=0"),
        "expected 'size=0' for an idle live actor, got:\n{stdout}"
    );
}

const MAILBOX_SIZE_DEAD_SOURCE: &str = r#"
import std.io    as Io
import std.time  as Time
import std.actor as Actor

actor Fragile =
    state n: Int = 0

    on io boom (d: Int) -> Unit =
        n <- 10 / d
        Io.println "fragile-survived"

fn spawn io time main () -> Result Unit Text =
    let f = spawn Fragile
    f ! boom 0
    Time.sleep 300
    match Actor.mailboxSize f
        Some _ -> Io.println "reads-live"
        None -> Io.println "reads-dead"
    Ok ()
"#;

/// A handle whose actor has crashed must read `None`, so a program can tell an
/// absent actor from an idle one. The companion to the `Some 0` case above,
/// and the half that could not be written while `spawn` linked.
#[test]
fn beam_e2e_mailbox_size_reports_none_for_dead_actor() {
    let (stdout, _) = run_inline_actor_test("MailboxSizeDead", MAILBOX_SIZE_DEAD_SOURCE);
    assert!(
        stdout.contains("reads-dead"),
        "expected 'reads-dead' after the actor crashed, got:\n{stdout}"
    );
}

// ── Actor crash isolation ────────────────────────────────────────────────────

const ACTOR_CRASH_ISOLATION_SOURCE: &str = r#"
import std.io    as Io
import std.time  as Time
import std.actor as Actor

actor Child =
    state n: Int = 0

    on io explode (d: Int) -> Unit =
        n <- 10 / d
        Io.println "child-survived"

actor Parent =
    state started: Int = 0

    on spawn io time start () -> Unit =
        let c = spawn Child
        started <- started + 1
        c ! explode 0
        Time.sleep 300
        Io.println "parent-ran-past-child-crash"

fn spawn io time main () -> Result Unit Text =
    let p = spawn Parent
    p ! start ()
    Time.sleep 900
    match Actor.mailboxSize p
        Some _ -> Io.println "parent-handle-live"
        None -> Io.println "parent-handle-dead"
    Ok ()
"#;

/// An actor that crashes must not take down the actor that spawned it.
///
/// Written actor-to-actor on purpose. `run_erl` launches `main` with `-s`, so
/// main runs in Erlang's boot process — and that process traps exits, which
/// masks a cascading crash. A version of this test that spawned the crashing
/// actor straight from `main` would pass even if `spawn` went back to linking,
/// i.e. it could never fail. A spawning actor is an ordinary `gen_server` with
/// no `trap_exit`, so the link (when there was one) really did kill it: before
/// `ridge_rt:spawn_actor/3` switched to an unlinked start, this program printed
/// neither line and the handle read dead.
///
/// Two independent signals: the parent keeps executing its own handler after
/// the child dies, and its handle still reads live afterwards.
#[test]
fn beam_e2e_actor_crash_does_not_kill_its_spawner() {
    let (stdout, stderr) =
        run_inline_actor_test_via_runner("ActorCrashIsolation", ACTOR_CRASH_ISOLATION_SOURCE);
    assert!(
        stdout.contains("parent-ran-past-child-crash"),
        "the spawning actor died with its child, got:\n{stdout}"
    );
    assert!(
        stdout.contains("parent-handle-live"),
        "the spawning actor's handle reads dead after the child crashed, got:\n{stdout}"
    );
    // Isolation must not mean silence: the crash is still reported.
    assert!(
        stderr.contains("badarith"),
        "expected the child's crash on stderr, got:\n{stderr}"
    );
}

/// A crash report must land on stderr, never in the program's own output.
///
/// The BEAM's default logger writes to stdout, so an actor dying mid-run used
/// to inject its crash report into whatever the program was printing — enough
/// to corrupt the output of anything piped into another tool. `Io.println`
/// owns stdout; diagnostics go to stderr.
#[test]
fn beam_e2e_actor_crash_report_goes_to_stderr_not_stdout() {
    let (stdout, stderr) =
        run_inline_actor_test_via_runner("CrashStream", ACTOR_CRASH_ISOLATION_SOURCE);
    assert!(
        stderr.contains("badarith"),
        "expected the crash report on stderr, got:\n{stderr}"
    );
    assert!(
        !stdout.contains("badarith") && !stdout.contains("CRASH REPORT"),
        "the crash report leaked into the program's stdout:\n{stdout}"
    );
}

// ── Typed supervision ────────────────────────────────────────────────────────
//
// The supervision API — `child` specs, `Actor.supervise`, dynamic children,
// `Actor.tryAsk` — runs on a real OTP supervisor underneath. These tests pin
// its semantics end to end from Ridge source: each program is compiled to
// BEAM and run, and the restart behaviour is observed through the program's
// own stdout and exit code.
//
// Entry points mirror the crash-isolation tests above: programs that must
// run to completion go through `run_inline_actor_test_via_runner` (asserts a
// zero exit and keeps crash reports on stderr); programs whose `main` is
// expected to DIE — the loud-failure cases — drive `run_erl_via_runner`
// directly so the non-zero exit code and the structured reason can be
// asserted. Restarts are asynchronous, so a crashed child is always given a
// bounded sleep before the next ask, exactly like the mailbox tests give a
// crash time to land.

// ── 1. A supervised handle survives its child's restart ──────────────────────

const SUPERVISION_RESTART_TRANSPARENCY_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (OneForOne)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

    on die (d: Int) =
        count <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let c = Actor.startChild sup (child Counter)?
    c ! bump
    c ! bump
    let n1 = c ?> get timeout 2000
    Io.println $"before=${Int.toText n1}"
    c ! die 0
    -- The restart is asynchronous: give the supervisor a moment to bring the
    -- replacement up before asking through the same handle again.
    Time.sleep 500
    let n2 = c ?> get timeout 2000
    Io.println $"after=${Int.toText n2}"
    Io.println "main-still-running"
    Ok ()
"#;

/// The headline supervision property: a child crashes, the supervisor
/// restarts it with fresh state, and the handle handed out by `startChild`
/// keeps working without the caller noticing the pid changed underneath it.
/// `after=0` proves the restart (state re-initialised); the ask succeeding
/// at all proves the handle resolved the replacement process; the sentinel
/// proves `main` was never at risk.
#[test]
fn beam_e2e_supervised_handle_survives_restart() {
    let (stdout, _) =
        run_inline_actor_test_via_runner("SupRestart", SUPERVISION_RESTART_TRANSPARENCY_SOURCE);
    assert!(
        stdout.contains("before=2"),
        "expected 'before=2' (two bumps before the crash), got:\n{stdout}"
    );
    assert!(
        stdout.contains("after=0"),
        "expected 'after=0' (restarted child has fresh state), got:\n{stdout}"
    );
    assert!(
        stdout.contains("main-still-running"),
        "main died with the supervised child, got:\n{stdout}"
    );
}

// ── 2. A supervised child's crash stays inside the supervisor ───────────────

const SUPERVISION_CRASH_ISOLATION_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (OneForOne)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

    on die (d: Int) =
        count <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let c = Actor.startChild sup (child Counter)?
    c ! die 0
    Time.sleep 500
    Io.println "main-ran-past-child-crash"
    let n = c ?> get timeout 2000
    Io.println $"child-back=${Int.toText n}"
    Ok ()
"#;

/// Crash isolation must hold under supervision too: starting a supervisor
/// links no fate to the starter, so a child dying takes down neither the
/// supervisor nor `main` — and the crash is still reported, on stderr.
/// Guards the unlinked-`spawn` semantics against regressions introduced by
/// the supervision runtime.
#[test]
fn beam_e2e_supervised_child_crash_leaves_main_running() {
    let (stdout, stderr) =
        run_inline_actor_test_via_runner("SupIsolation", SUPERVISION_CRASH_ISOLATION_SOURCE);
    assert!(
        stdout.contains("main-ran-past-child-crash"),
        "main died with the supervised child, got:\n{stdout}"
    );
    assert!(
        stdout.contains("child-back=0"),
        "the supervisor did not restart its child, got:\n{stdout}"
    );
    assert!(
        stderr.contains("badarith"),
        "expected the child's crash report on stderr, got:\n{stderr}"
    );
}

// ── 3. one_for_all restarts the siblings too ────────────────────────────────

const SUPERVISION_ONE_FOR_ALL_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (OneForAll)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

    on die (d: Int) =
        count <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForAll 3 5000 []?
    let a = Actor.startChild sup (Actor.childId "a" (child Counter))?
    let b = Actor.startChild sup (Actor.childId "b" (child Counter))?
    a ! bump
    a ! bump
    b ! bump
    let na = a ?> get timeout 2000
    let nb = b ?> get timeout 2000
    Io.println $"pre=${Int.toText na},${Int.toText nb}"
    a ! die 0
    Time.sleep 500
    let ra = a ?> get timeout 2000
    let rb = b ?> get timeout 2000
    Io.println $"post=${Int.toText ra},${Int.toText rb}"
    Ok ()
"#;

/// Under `one_for_all`, one child's crash restarts EVERY child. Only `a` is
/// sent `die`, yet `b`'s state resets too — `post=0,0` is the strategy's
/// signature. (`pre=2,1` first proves both counters accumulated distinct
/// state, so the zeros afterwards cannot be a startup artifact.)
#[test]
fn beam_e2e_supervisor_one_for_all_restarts_siblings() {
    let (stdout, _) =
        run_inline_actor_test_via_runner("SupOneForAll", SUPERVISION_ONE_FOR_ALL_SOURCE);
    assert!(
        stdout.contains("pre=2,1"),
        "expected 'pre=2,1' before the crash, got:\n{stdout}"
    );
    assert!(
        stdout.contains("post=0,0"),
        "one_for_all must restart the sibling as well — expected 'post=0,0', got:\n{stdout}"
    );
}

// ── 4. rest_for_one restarts only the tail ──────────────────────────────────

const SUPERVISION_REST_FOR_ONE_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (RestForOne)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

    on die (d: Int) =
        count <- 10 / d

fn spawn io time main () -> Result Unit Text =
    -- Phase 1: crashing the FIRST child restarts everything started after
    -- it as well — under rest_for_one that is the whole tail.
    let sup1 = Actor.supervise RestForOne 3 5000 []?
    let a1 = Actor.startChild sup1 (Actor.childId "a" (child Counter))?
    let b1 = Actor.startChild sup1 (Actor.childId "b" (child Counter))?
    a1 ! bump
    a1 ! bump
    b1 ! bump
    a1 ! die 0
    Time.sleep 500
    let p1a = a1 ?> get timeout 2000
    let p1b = b1 ?> get timeout 2000
    Io.println $"p1-post=${Int.toText p1a},${Int.toText p1b}"
    -- Phase 2: crashing the SECOND child leaves the earlier one alone.
    let sup2 = Actor.supervise RestForOne 3 5000 []?
    let a2 = Actor.startChild sup2 (Actor.childId "a" (child Counter))?
    let b2 = Actor.startChild sup2 (Actor.childId "b" (child Counter))?
    a2 ! bump
    a2 ! bump
    b2 ! bump
    b2 ! die 0
    Time.sleep 500
    let p2a = a2 ?> get timeout 2000
    let p2b = b2 ?> get timeout 2000
    Io.println $"p2-post=${Int.toText p2a},${Int.toText p2b}"
    Ok ()
"#;

/// `rest_for_one` scoped both ways, with a fresh supervisor per phase so the
/// two readings cannot contaminate each other. Crashing the first child
/// resets the second (`p1-post=0,0`); crashing the second leaves the first's
/// accumulated state intact (`p2-post=2,0`).
#[test]
fn beam_e2e_supervisor_rest_for_one_restarts_tail_only() {
    let (stdout, _) =
        run_inline_actor_test_via_runner("SupRestForOne", SUPERVISION_REST_FOR_ONE_SOURCE);
    assert!(
        stdout.contains("p1-post=0,0"),
        "crashing the first child must restart the tail — expected 'p1-post=0,0', got:\n{stdout}"
    );
    assert!(
        stdout.contains("p2-post=2,0"),
        "crashing the second child must spare the first — expected 'p2-post=2,0', got:\n{stdout}"
    );
}

// ── 5. Restart-intensity exhaustion takes the supervisor down, loudly ───────

const SUPERVISION_INTENSITY_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (OneForOne)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

    on die (d: Int) =
        count <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 1 60000 []?
    let c = Actor.startChild sup (child Counter)?
    c ! die 0
    Time.sleep 500
    let n = c ?> get timeout 2000
    Io.println $"back-once=${Int.toText n}"
    c ! die 0
    Time.sleep 500
    -- The second restart inside the 60 s window exceeds intensity 1: the
    -- supervisor gives up and terminates. whichChildren on a dead supervisor
    -- must fail loudly rather than answer an empty list.
    let _ = Actor.whichChildren sup
    Io.println "should-not-reach"
    Ok ()
"#;

/// More than `intensity` restarts within `period` kills the supervisor
/// itself. The first crash is absorbed (`back-once=0`); the second inside
/// the window exhausts the budget, and the subsequent `whichChildren` raises
/// `ridge_sup_noproc` in `main` — a dead supervisor reads as a crash of the
/// caller, never as a silent empty answer.
#[test]
fn beam_e2e_supervisor_intensity_exhaustion_fails_loudly() {
    let (beam_dir, module, _td) =
        compile_inline_actor_test("SupIntensity", SUPERVISION_INTENSITY_SOURCE);
    let (stdout, stderr, exit_code) = run_erl_via_runner(&beam_dir, &module);
    assert_ne!(
        exit_code, 0,
        "expected a non-zero exit (whichChildren on a dead supervisor), got 0\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stderr.contains("ridge_sup_noproc"),
        "expected 'ridge_sup_noproc' on stderr, got:\n{stderr}"
    );
    assert!(
        stdout.contains("back-once=0"),
        "the first restart should have succeeded before exhaustion, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("should-not-reach"),
        "main should crash at whichChildren, before the println; stdout was:\n{stdout}"
    );
}

// ── 6. Dynamic children: startChild / whichChildren / stopChild ─────────────

const SUPERVISION_DYNAMIC_SOURCE: &str = r#"
import std.io as Io
import std.actor as Actor
import std.actor (OneForOne)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

fn io main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let _ = Actor.startChild sup (Actor.childId "a" (child Counter))?
    let _ = Actor.startChild sup (Actor.childId "b" (child Counter))?
    match Actor.whichChildren sup
        [("b", true), ("a", true)] -> Io.println "both-alive"
        _ -> Io.println "unexpected-children"
    match Actor.stopChild sup "a"
        Ok _ -> Io.println "stop-a-ok"
        Err e -> Io.println $"stop-a-failed=${e}"
    match Actor.whichChildren sup
        [("b", true)] -> Io.println "only-b-left"
        _ -> Io.println "unexpected-after-stop"
    match Actor.stopChild sup "a"
        Ok _ -> Io.println "second-stop-unexpectedly-ok"
        Err _ -> Io.println "second-stop-err"
    Ok ()
"#;

/// The dynamic-child surface over an empty static list: two `startChild`
/// calls register under their `childId`s, `whichChildren` reports both alive
/// (newest first, OTP's order for dynamically started children), `stopChild`
/// removes one (terminate + delete, so the id is gone — not merely down),
/// and stopping the same id twice is an `Err` rather than a crash or a lie.
#[test]
fn beam_e2e_supervisor_dynamic_children() {
    let (stdout, _) = run_inline_actor_test("SupDynamic", SUPERVISION_DYNAMIC_SOURCE);
    for want in ["both-alive", "stop-a-ok", "only-b-left", "second-stop-err"] {
        assert!(
            stdout.contains(want),
            "expected '{want}' in stdout, got:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("unexpected") && !stdout.contains("stop-a-failed"),
        "a dynamic-child operation misbehaved, got:\n{stdout}"
    );
}

// ── 7. tryAsk: Ok on a live child, Timeout on a slow one, Noproc on a dead one

const SUPERVISION_TRY_ASK_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (OneForOne, Noproc, Timeout)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

actor Slow =
    state n: Int = 0

    on time ponder () -> Int =
        Time.sleep 2000
        n

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let c = Actor.startChild sup (Actor.childId "c" (child Counter))?
    c ! bump
    c ! bump
    match Actor.tryAsk c get 1000
        Ok n -> Io.println $"live=${Int.toText n}"
        Err Noproc -> Io.println "live-noproc"
        Err Timeout -> Io.println "live-timeout"
    -- Slow needs its own supervisor: Supervisor a is homogeneous, so a
    -- Counter supervisor cannot take a Slow spec.
    let sup2 = Actor.supervise OneForOne 3 5000 []?
    let s = Actor.startChild sup2 (child Slow)?
    match Actor.tryAsk s ponder 200
        Ok n -> Io.println $"slow-ok=${Int.toText n}"
        Err Noproc -> Io.println "slow-noproc"
        Err Timeout -> Io.println "slow-timeout"
    let _ = Actor.stopChild sup "c"?
    match Actor.tryAsk c get 1000
        Ok n -> Io.println $"stopped-ok=${Int.toText n}"
        Err Noproc -> Io.println "stopped-noproc"
        Err Timeout -> Io.println "stopped-timeout"
    Ok ()
"#;

/// `tryAsk` is the `Result`-returning ask: a live child answers `Ok` with
/// the handler's reply, a handler that outruns the deadline reads as
/// `Err Timeout` (the 200 ms budget against a 2 s ponder), and a child that
/// was stopped reads as `Err Noproc` — all matched in-language, so the
/// program itself decides what each failure means instead of dying.
#[test]
fn beam_e2e_supervised_tryask_outcomes() {
    let (stdout, _) = run_inline_actor_test("SupTryAsk", SUPERVISION_TRY_ASK_SOURCE);
    for want in ["live=2", "slow-timeout", "stopped-noproc"] {
        assert!(
            stdout.contains(want),
            "expected '{want}' in stdout, got:\n{stdout}"
        );
    }
    for unwanted in [
        "live-noproc",
        "live-timeout",
        "slow-ok",
        "slow-noproc",
        "stopped-ok",
        "stopped-timeout",
    ] {
        assert!(
            !stdout.contains(unwanted),
            "unexpected '{unwanted}' in stdout, got:\n{stdout}"
        );
    }
}

// ── 8. `?>` on a stopped child fails fast ───────────────────────────────────

const SUPERVISION_ASK_STOPPED_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.actor as Actor
import std.actor (OneForOne)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let c = Actor.startChild sup (Actor.childId "c" (child Counter))?
    let _ = Actor.stopChild sup "c"?
    -- The child is gone: ask has no answer to give, so it raises the
    -- structured ridge_ask_noproc reason in the caller instead of surfacing
    -- a raw gen_server exit.
    let n = c ?> get timeout 1000
    Io.println $"should-not-reach=${Int.toText n}"
    Ok ()
"#;

/// Asking a stopped supervised child with `?>` must fail fast with the
/// structured `ridge_ask_noproc` reason — the caller crashes with a
/// recognisable cause rather than hanging or surfacing an opaque
/// `exit:{noproc,_}`. The `Err`-returning alternative for callers that want
/// to handle the absence is `tryAsk` (previous test).
#[test]
fn beam_e2e_ask_on_stopped_child_fails_loudly() {
    let (beam_dir, module, _td) =
        compile_inline_actor_test("SupAskStopped", SUPERVISION_ASK_STOPPED_SOURCE);
    let (stdout, stderr, exit_code) = run_erl_via_runner(&beam_dir, &module);
    assert_ne!(
        exit_code, 0,
        "expected a non-zero exit (ask on a stopped child), got 0\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stderr.contains("ridge_ask_noproc"),
        "expected 'ridge_ask_noproc' on stderr, got:\n{stderr}"
    );
    assert!(
        !stdout.contains("should-not-reach"),
        "main should crash at the ask, before the println; stdout was:\n{stdout}"
    );
}

// ── 9. `?>` on a dead unsupervised actor, from a `?`-using main ─────────────

const ASK_DEAD_PLAIN_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (OneForOne)

actor Fragile =
    state n: Int = 0

    on get () -> Int =
        n

    on die (d: Int) =
        n <- 10 / d

fn spawn io time main () -> Result Unit Text =
    -- Any use of `?` wraps the body in the propagation try/catch, and an
    -- exception crossing that frame must keep its class and reason: a
    -- previous codegen bug re-raised with an invalid stacktrace and turned
    -- the crash into a silent `badarg` return with exit code 0.
    let _ = Actor.supervise OneForOne 3 5000 []?
    let f = spawn Fragile
    f ! die 0
    Time.sleep 300
    let n = f ?> get timeout 1000
    Io.println $"should-not-reach=${Int.toText n}"
    Ok ()
"#;

/// `?>` on a dead actor must raise `ridge_ask_noproc` in the caller even
/// when the calling function uses `?` somewhere in its body — the
/// propagation wrapper around such bodies used to swallow the exit and
/// report success. This is the plain-handle twin of the stopped-child case
/// above: same structured reason, no supervisor involved.
#[test]
fn beam_e2e_ask_on_dead_plain_actor_fails_loudly() {
    let (beam_dir, module, _td) = compile_inline_actor_test("AskDeadPlain", ASK_DEAD_PLAIN_SOURCE);
    let (stdout, stderr, exit_code) = run_erl_via_runner(&beam_dir, &module);
    assert_ne!(
        exit_code, 0,
        "expected a non-zero exit (ask on a dead actor), got 0\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stderr.contains("ridge_ask_noproc"),
        "expected 'ridge_ask_noproc' on stderr, got:\n{stderr}"
    );
    assert!(
        !stdout.contains("should-not-reach"),
        "main should crash at the ask, before the println; stdout was:\n{stdout}"
    );
}

// ── 10. Static children: the supervisor starts its own spec list ────────────

const SUPERVISION_STATIC_CHILDREN_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List
import std.actor as Actor
import std.actor (OneForOne)

actor Seeded =
    state count: Int = 0

    init (start: Int) =
        count <- start

    on get () -> Int =
        count

fn spawn io main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 [child Seeded (7)]?
    let kids = Actor.whichChildren sup
    Io.println $"kids=${Int.toText (List.length kids)}"
    match List.all (fn (_, alive) -> alive) kids
        True -> Io.println "all-alive"
        False -> Io.println "some-dead"
    Ok ()
"#;

/// Every other supervision test builds its children dynamically (`[]` +
/// `startChild`). The static-list path — `supervise … [child Seeded (7)]` —
/// must start the children itself: the supervisor comes up with the child
/// already running (`kids=1`, `all-alive`), and the init argument crossed
/// through the spec's start tuple.
#[test]
fn beam_e2e_supervisor_static_children_start_with_the_supervisor() {
    let (stdout, _) =
        run_inline_actor_test_via_runner("SupStatic", SUPERVISION_STATIC_CHILDREN_SOURCE);
    assert!(
        stdout.contains("kids=1"),
        "the static child should be running under the supervisor, got:\n{stdout}"
    );
    assert!(
        stdout.contains("all-alive"),
        "the static child should be alive, got:\n{stdout}"
    );
}

const SUPERVISION_STATIC_INIT_CRASH_SOURCE: &str = r#"
import std.io as Io
import std.actor as Actor
import std.actor (OneForOne)

actor Doomed =
    state n: Int = 0

    init (d: Int) =
        n <- 10 / d

fn spawn io main () -> Result Unit Text =
    match Actor.supervise OneForOne 3 5000 [child Doomed (0)]
        Ok _ -> Io.println "unexpected-ok"
        Err _ -> Io.println "start-failed"
    Io.println "main-survived"
    Ok ()
"#;

/// A static child whose init fails takes the supervisor's start down with
/// it: `supervise` must answer `Err` (OTP's start_link propagates the child
/// start failure), not crash `main` and not answer a half-built supervisor.
#[test]
fn beam_e2e_supervisor_static_child_init_crash_returns_err() {
    let (stdout, _) = run_inline_actor_test_via_runner(
        "SupStaticInitCrash",
        SUPERVISION_STATIC_INIT_CRASH_SOURCE,
    );
    assert!(
        stdout.contains("start-failed"),
        "supervise should return Err when a static child's init crashes, got:\n{stdout}"
    );
    assert!(
        stdout.contains("main-survived"),
        "main should continue past the failed supervise, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("unexpected-ok"),
        "supervise answered Ok for a child that could never start, got:\n{stdout}"
    );
}

// ── 11. transient: normal stop stays down, abnormal exit restarts ───────────

const SUPERVISION_TRANSIENT_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (OneForOne, Transient, Noproc, Timeout)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

    on die (d: Int) =
        count <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let a = Actor.startChild sup (Actor.childId "a" (Actor.childRestart Transient (child Counter)))?
    a ! bump
    -- A NORMAL stop must not restart a transient child: the ask afterwards
    -- finds nothing to answer.
    Actor.stop a
    Time.sleep 300
    match Actor.tryAsk a get 1000
        Err Noproc -> Io.println "normal-stop-stayed-down"
        Ok n -> Io.println $"unexpected-reply=${Int.toText n}"
        Err Timeout -> Io.println "unexpected-timeout"
    -- An abnormal exit MUST restart a transient child, with fresh state:
    -- bumped once before the crash, the restarted counter reads 0, not 1.
    let b = Actor.startChild sup (Actor.childId "b" (Actor.childRestart Transient (child Counter)))?
    b ! bump
    b ! die 0
    Time.sleep 500
    let n = b ?> get timeout 2000
    Io.println $"crash-restarted=${Int.toText n}"
    Ok ()
"#;

/// `Transient` is the middle restart policy and the only one that
/// distinguishes exit reasons: a normal stop leaves the child down
/// (`normal-stop-stayed-down`), a crash brings it back with re-run init
/// (`crash-restarted=0` — bumped once before dying, so 0 proves the reset).
/// The `Restart` variant crosses the FFI boundary as a verbatim atom that
/// `ridge_rt` maps to OTP's lowercase one; a mapping bug would show up here
/// as a restarted normal-stop or a never-restarted crash.
#[test]
fn beam_e2e_supervisor_transient_restarts_only_on_abnormal_exit() {
    let (stdout, _) =
        run_inline_actor_test_via_runner("SupTransient", SUPERVISION_TRANSIENT_SOURCE);
    assert!(
        stdout.contains("normal-stop-stayed-down"),
        "a transient child stopped normally must NOT be restarted, got:\n{stdout}"
    );
    assert!(
        stdout.contains("crash-restarted=0"),
        "a transient child that crashed must restart with fresh state, got:\n{stdout}"
    );
}

// ── 12. temporary: a crash leaves the child down ────────────────────────────

const SUPERVISION_TEMPORARY_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.time as Time
import std.actor as Actor
import std.actor (OneForOne, Temporary, Noproc, Timeout)

actor Counter =
    state count: Int = 0

    on bump =
        count <- count + 1

    on get () -> Int =
        count

    on die (d: Int) =
        count <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let c = Actor.startChild sup (Actor.childId "c" (Actor.childRestart Temporary (child Counter)))?
    c ! bump
    c ! die 0
    Time.sleep 500
    match Actor.tryAsk c get 1000
        Err Noproc -> Io.println "crash-stayed-down"
        Ok n -> Io.println $"unexpected-reply=${Int.toText n}"
        Err Timeout -> Io.println "unexpected-timeout"
    Io.println "main-still-running"
    Ok ()
"#;

/// `Temporary` children are never restarted — not even on a crash. The
/// bumped counter dies and stays dead (`crash-stayed-down`), while the
/// supervisor and `main` carry on (`main-still-running`). Without this pin
/// a `temporary → transient` mapping slip at the runtime boundary would be
/// invisible: the child would silently come back.
#[test]
fn beam_e2e_supervisor_temporary_never_restarts() {
    let (stdout, _) =
        run_inline_actor_test_via_runner("SupTemporary", SUPERVISION_TEMPORARY_SOURCE);
    assert!(
        stdout.contains("crash-stayed-down"),
        "a temporary child must NOT be restarted after a crash, got:\n{stdout}"
    );
    assert!(
        stdout.contains("main-still-running"),
        "main died with the temporary child, got:\n{stdout}"
    );
}

// ── terminate callback ─────────────────────────────────────────────────────

const TERMINATE_ON_STOP_SOURCE: &str = r#"
import std.io    as Io
import std.time  as Time
import std.actor as Actor
import std.actor (OneForOne, ExitReason, Normal, NotRunning, Shutdown, Crashed)

actor Worker =
    state n: Int = 0

    on work () -> Unit =
        n <- n + 1

    terminate io (reason: ExitReason) =
        match reason
            Normal -> Io.println "terminate-normal"
            NotRunning -> Io.println "terminate-unreachable"
            Shutdown -> Io.println "terminate-shutdown"
            Crashed m -> Io.println "terminate-crashed"

fn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let _ = Actor.startChild sup (Actor.childId "w" (child Worker))?
    match Actor.stopChild sup "w"
        Ok _ -> Io.println "stop-ok"
        Err e -> Io.println $"stop-failed=${e}"
    Time.sleep 300
    Io.println "main-done"
    Ok ()
"#;

/// An ordered stop from the supervisor must reach the callback: `stopChild`
/// fires `terminate` with `Shutdown`, and the side effect is observed.
#[test]
fn beam_e2e_terminate_fires_on_supervisor_stop() {
    let (stdout, _) = run_inline_actor_test("TerminateOnStop", TERMINATE_ON_STOP_SOURCE);
    assert!(stdout.contains("stop-ok"), "got:\n{stdout}");
    assert!(
        stdout.contains("terminate-shutdown"),
        "the callback did not observe Shutdown, got:\n{stdout}"
    );
    assert!(stdout.contains("main-done"), "got:\n{stdout}");
}

const TERMINATE_ON_CRASH_SOURCE: &str = r#"
import std.io    as Io
import std.int   as Int
import std.time  as Time
import std.actor as Actor
import std.actor (OneForOne, ExitReason, Normal, NotRunning, Shutdown, Crashed)

actor Worker =
    state n: Int = 0

    on explode (d: Int) -> Unit =
        n <- 10 / d

    on get () -> Int =
        n

    terminate io (reason: ExitReason) =
        match reason
            Normal -> Io.println "terminate-normal"
            NotRunning -> Io.println "terminate-unreachable"
            Shutdown -> Io.println "terminate-shutdown"
            Crashed m -> Io.println "terminate-crashed"

fn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let w = Actor.startChild sup (Actor.childId "w" (child Worker))?
    w ! explode 0
    Time.sleep 500
    let n = w ?> get ()
    Io.println $"handle-replied=${Int.toText n}"
    Ok ()
"#;

/// A handler crash fires `terminate` with the `Crashed` payload, and the
/// internal trap_exit changes nothing about restart transparency: the
/// supervisor restarts the child and the handle re-resolves (state reset).
#[test]
fn beam_e2e_terminate_fires_on_crash_and_restart_still_works() {
    let (stdout, _) = run_inline_actor_test("TerminateOnCrash", TERMINATE_ON_CRASH_SOURCE);
    assert!(
        stdout.contains("terminate-crashed"),
        "the callback did not observe the crash, got:\n{stdout}"
    );
    assert!(
        stdout.contains("handle-replied=0"),
        "the restarted child should re-run init (state reset to 0), got:\n{stdout}"
    );
}

const INIT_CRASH_NO_TERMINATE_SOURCE: &str = r#"
import std.io    as Io
import std.actor (ExitReason, Normal, NotRunning, Shutdown, Crashed)

actor Worker =
    state n: Int = 0

    init (d: Int) =
        n <- 10 / d

    terminate io (reason: ExitReason) =
        match reason
            Normal -> Io.println "terminate-normal"
            NotRunning -> Io.println "terminate-unreachable"
            Shutdown -> Io.println "terminate-fired"
            Crashed m -> Io.println "terminate-fired"

fn spawn io main () -> Result Unit Text =
    let _w = spawn Worker 0
    Io.println "spawn-returned"
    Ok ()
"#;

/// OTP never calls terminate/2 when init itself fails — the callback must
/// NOT fire on an init crash, and the spawn failure still surfaces.
#[test]
fn beam_e2e_init_crash_does_not_fire_terminate() {
    let (beam_dir, module, _td) =
        compile_inline_actor_test("InitCrashNoTerminate", INIT_CRASH_NO_TERMINATE_SOURCE);
    let (stdout, _stderr, exit_code) = run_erl_via_runner(&beam_dir, &module);
    assert!(
        !stdout.contains("terminate-fired"),
        "terminate must not run when init crashes (OTP semantics), got:\n{stdout}"
    );
    assert_ne!(
        exit_code, 0,
        "the init crash must surface as a failure, got exit 0\n--- stdout ---\n{stdout}"
    );
}

// ── Process-monitor BEAM e2e tests ────────────────────────────────────────────

/// `await` observes a crash: the DOWN arrives with the `Crashed` payload.
const MONITOR_AWAIT_CRASH_SOURCE: &str = r#"
import std.io    as Io
import std.actor as Actor
import std.actor (ExitReason, Normal, NotRunning, Shutdown, Crashed)

actor Worker =
    state n: Int = 0

    on explode (d: Int) -> Unit =
        n <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let w = spawn Worker
    let m = Actor.monitor w
    w ! explode 0
    match Actor.await m 3000
        Some (Crashed r) -> Io.println "await-crashed"
        Some Normal -> Io.println "await-normal"
        Some Shutdown -> Io.println "await-shutdown"
        Some NotRunning -> Io.println "await-notrunning"
        None -> Io.println "await-timeout"
    Ok ()
"#;

#[test]
fn beam_e2e_await_observes_crash() {
    let (stdout, _) = run_inline_actor_test("MonitorAwaitCrash", MONITOR_AWAIT_CRASH_SOURCE);
    assert!(stdout.contains("await-crashed"), "got:\n{stdout}");
}

/// `await` on a live actor times out and returns `None`.
const MONITOR_AWAIT_TIMEOUT_SOURCE: &str = r#"
import std.io    as Io
import std.actor as Actor

actor Worker =
    state n: Int = 0

    on ping () -> Int =
        n

fn spawn io time main () -> Result Unit Text =
    let w = spawn Worker
    let m = Actor.monitor w
    match Actor.await m 300
        Some r -> Io.println "await-unexpected-down"
        None -> Io.println "await-timeout"
    let _ = Actor.unmonitor m
    Ok ()
"#;

#[test]
fn beam_e2e_await_times_out_on_live_actor() {
    let (stdout, _) = run_inline_actor_test("MonitorAwaitTimeout", MONITOR_AWAIT_TIMEOUT_SOURCE);
    assert!(stdout.contains("await-timeout"), "got:\n{stdout}");
}

/// `Actor.stop` asks a handle to stop normally: the actor's `terminate`
/// callback runs with `Normal`, and a monitor's DOWN carries `Normal` too.
const ACTOR_STOP_NORMAL_SOURCE: &str = r#"
import std.io    as Io
import std.actor as Actor
import std.actor (ExitReason, Normal, NotRunning, Shutdown, Crashed)

actor Worker =
    state n: Int = 0

    on tick =
        n <- n + 1

    terminate io (reason: ExitReason) =
        match reason
            Normal -> Io.println "terminate-normal"
            NotRunning -> Io.println "terminate-unreachable"
            Shutdown -> Io.println "terminate-shutdown"
            Crashed _ -> Io.println "terminate-crashed"

fn spawn io time main () -> Result Unit Text =
    let w = spawn Worker
    let m = Actor.monitor w
    Actor.stop w
    match Actor.await m 3000
        Some Normal -> Io.println "await-normal"
        Some _ -> Io.println "await-other"
        None -> Io.println "await-timeout"
    Ok ()
"#;

#[test]
fn beam_e2e_actor_stop_delivers_normal() {
    let (stdout, _) = run_inline_actor_test("ActorStopNormal", ACTOR_STOP_NORMAL_SOURCE);
    assert!(stdout.contains("await-normal"), "got:\n{stdout}");
    assert!(stdout.contains("terminate-normal"), "got:\n{stdout}");
}

// ── Actor.send BEAM e2e tests ─────────────────────────────────────────────────

/// `Actor.send` to a running actor answers `Ok ()` and the message lands.
const ACTOR_SEND_OK_SOURCE: &str = r#"
import std.io    as Io
import std.actor as Actor
import std.actor (MailboxFull)

actor Worker =
    state n: Int = 0

    on io work (x: Int) =
        n <- n + x
        Io.println "handler-ran"

fn spawn io main () -> Result Unit Text =
    let w = spawn Worker
    match Actor.send w (work 1)
        Ok _ -> Io.println "send-ok"
        Err MailboxFull -> Io.println "send-full"
    Ok ()
"#;

#[test]
fn beam_e2e_actor_send_unbounded_returns_ok() {
    let (stdout, _) = run_inline_actor_test("ActorSendOk", ACTOR_SEND_OK_SOURCE);
    assert!(stdout.contains("send-ok"), "got:\n{stdout}");
}

/// A full bounded `error`-policy mailbox answers `Err MailboxFull` instead
/// of raising in the caller like `!` would.
const ACTOR_SEND_FULL_SOURCE: &str = r#"
import std.io    as Io
import std.actor as Actor
import std.actor (MailboxFull)
import std.time  as Time

actor Slow =
    state n: Int = 0

    mailbox bounded 1 error

    on time work (x: Int) =
        Time.sleep 1000
        n <- n + x

fn spawn io time main () -> Result Unit Text =
    let w = spawn Slow
    match Actor.send w (work 1)
        Ok _ -> Io.println "first-ok"
        Err MailboxFull -> Io.println "first-full"
    Time.sleep 100
    match Actor.send w (work 2)
        Ok _ -> Io.println "second-ok"
        Err MailboxFull -> Io.println "second-full"
    match Actor.send w (work 3)
        Ok _ -> Io.println "third-ok"
        Err MailboxFull -> Io.println "third-full"
    Ok ()
"#;

#[test]
fn beam_e2e_actor_send_bounded_full_returns_mailbox_full() {
    let (stdout, _) = run_inline_actor_test("ActorSendFull", ACTOR_SEND_FULL_SOURCE);
    assert!(stdout.contains("first-ok"), "got:\n{stdout}");
    assert!(stdout.contains("second-ok"), "got:\n{stdout}");
    assert!(stdout.contains("third-full"), "got:\n{stdout}");
}

/// `Actor.send` to a dead handle is a no-op answering `Ok ()`, matching the
/// `!` operator's dead-target semantics.
const ACTOR_SEND_DEAD_SOURCE: &str = r#"
import std.io    as Io
import std.actor as Actor
import std.actor (MailboxFull)

actor Worker =
    state n: Int = 0

    on work (x: Int) =
        n <- n + x

fn spawn io main () -> Result Unit Text =
    let w = spawn Worker
    Actor.stop w
    match Actor.send w (work 1)
        Ok _ -> Io.println "send-dead-ok"
        Err MailboxFull -> Io.println "send-full"
    Ok ()
"#;

#[test]
fn beam_e2e_actor_send_dead_handle_returns_ok() {
    let (stdout, _) = run_inline_actor_test("ActorSendDead", ACTOR_SEND_DEAD_SOURCE);
    assert!(stdout.contains("send-dead-ok"), "got:\n{stdout}");
}

/// std.crypto digests and base64 round-trip end to end on the BEAM.
const CRYPTO_DIGEST_SOURCE: &str = r#"
import std.io    as Io
import std.bytes as Bytes
import std.crypto as Crypto

fn io checkRoundTrip (b: Bytes) -> Unit =
    match Bytes.toUtf8 b
        Ok s -> Io.println s
        Err _ -> Io.println "decode-failed"

fn io main () -> Unit =
    let digest = Crypto.sha256 "abc"
    Io.println (Bytes.toHex digest)
    match Crypto.base64Decode (Crypto.base64Encode (Bytes.fromUtf8 "hello"))
        Ok b -> checkRoundTrip b
        Err _ -> Io.println "b64-failed"
"#;

#[test]
fn beam_e2e_crypto_sha256_and_base64_roundtrip() {
    let (beam_dir, module, _td) = compile_inline_actor_test("CryptoDigest", CRYPTO_DIGEST_SOURCE);
    let (stdout, _stderr, exit_code) = run_erl_via_runner(&beam_dir, &module);
    assert_eq!(exit_code, 0, "got:\n{stdout}");
    assert!(
        stdout.contains("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        "sha256 hex mismatch, got:\n{stdout}"
    );
    assert!(
        stdout.contains("hello"),
        "base64 round-trip failed:\n{stdout}"
    );
}

/// Monitoring an already-dead handle fires an immediate `NotRunning` DOWN.
const MONITOR_DEAD_SOURCE: &str = r#"
import std.io    as Io
import std.time  as Time
import std.actor as Actor
import std.actor (ExitReason, Normal, NotRunning, Shutdown, Crashed)

actor Worker =
    state n: Int = 0

    on explode (d: Int) -> Unit =
        n <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let w = spawn Worker
    w ! explode 0
    Time.sleep 500
    let m = Actor.monitor w
    match Actor.await m 1000
        Some NotRunning -> Io.println "await-notrunning"
        Some (Crashed r) -> Io.println "await-crashed"
        Some Normal -> Io.println "await-normal"
        Some Shutdown -> Io.println "await-shutdown"
        None -> Io.println "await-timeout"
    Ok ()
"#;

#[test]
fn beam_e2e_monitor_dead_handle_reports_not_running() {
    let (stdout, _) = run_inline_actor_test("MonitorDead", MONITOR_DEAD_SOURCE);
    assert!(stdout.contains("await-notrunning"), "got:\n{stdout}");
}

/// Async delivery: a watcher actor's `onDown` member receives the DOWN and
/// mutates its state.
const WATCHER_SOURCE: &str = r#"
import std.io    as Io
import std.int   as Int
import std.time  as Time
import std.actor as Actor
import std.actor (ExitReason)

actor Worker =
    state n: Int = 0

    on explode (d: Int) -> Unit =
        n <- 10 / d

actor Watcher =
    state deaths: Int = 0

    on io watch (w: Handle Worker) -> Unit =
        let _ = Actor.monitor w
        Io.println "watching"

    on count () -> Int =
        deaths

    onDown (m: Monitor) (reason: ExitReason) =
        deaths <- deaths + 1

fn spawn io time main () -> Result Unit Text =
    let w = spawn Worker
    let watcher = spawn Watcher
    watcher ! watch w
    Time.sleep 300
    w ! explode 0
    Time.sleep 500
    let n = watcher ?> count ()
    Io.println $"deaths=${Int.toText n}"
    Ok ()
"#;

#[test]
fn beam_e2e_watcher_actor_receives_down_in_on_down() {
    let (stdout, _) = run_inline_actor_test("Watcher", WATCHER_SOURCE);
    assert!(stdout.contains("watching"), "got:\n{stdout}");
    assert!(stdout.contains("deaths=1"), "got:\n{stdout}");
}

/// Monitors observe pids, not logical handles: a supervised child that
/// crashes and is restarted fires DOWN for the old pid.
const MONITOR_SUPERVISED_RESTART_SOURCE: &str = r#"
import std.io    as Io
import std.actor as Actor
import std.actor (OneForOne, ExitReason, Normal, NotRunning, Shutdown, Crashed)

actor Worker =
    state n: Int = 0

    on explode (d: Int) -> Unit =
        n <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let sup = Actor.supervise OneForOne 3 5000 []?
    let w = Actor.startChild sup (Actor.childId "w" (child Worker))?
    let m = Actor.monitor w
    w ! explode 0
    match Actor.await m 3000
        Some (Crashed r) -> Io.println "down-for-old-pid"
        Some Normal -> Io.println "await-normal"
        Some Shutdown -> Io.println "await-shutdown"
        Some NotRunning -> Io.println "await-notrunning"
        None -> Io.println "await-timeout"
    Ok ()
"#;

#[test]
fn beam_e2e_supervised_restart_fires_down_for_old_pid() {
    let (stdout, _) = run_inline_actor_test(
        "MonitorSupervisedRestart",
        MONITOR_SUPERVISED_RESTART_SOURCE,
    );
    assert!(stdout.contains("down-for-old-pid"), "got:\n{stdout}");
}

/// `unmonitor` suppresses the DOWN: a later `await` on the same ref legally
/// returns `None` after the timeout — no stale DOWN is flushed into it.
const UNMONITOR_SOURCE: &str = r#"
import std.io    as Io
import std.actor as Actor
import std.actor (ExitReason)

actor Worker =
    state n: Int = 0

    on explode (d: Int) -> Unit =
        n <- 10 / d

fn spawn io time main () -> Result Unit Text =
    let w = spawn Worker
    let m = Actor.monitor w
    let _ = Actor.unmonitor m
    w ! explode 0
    match Actor.await m 800
        None -> Io.println "no-down-after-unmonitor"
        Some r -> Io.println "down-leaked"
    Ok ()
"#;

#[test]
fn beam_e2e_unmonitor_suppresses_down() {
    let (stdout, _) = run_inline_actor_test("Unmonitor", UNMONITOR_SOURCE);
    assert!(stdout.contains("no-down-after-unmonitor"), "got:\n{stdout}");
}

// ── Slice pattern BEAM e2e tests (D258) ──────────────────────────────────────

/// Suffix rest `[.., last]`: print the last element of a list.
const SLICE_SUFFIX_SOURCE: &str = r#"
import std.io as Io
import std.int as Int

fn io main () -> Result Unit Text =
    let xs = [10, 20, 30, 40, 50]
    match xs
        [] -> Io.println "empty"
        [.., last] -> Io.println $"last=${Int.toText last}"
    Ok ()
"#;

/// Runs `[.., last]` on a 5-element list; expects the last element (50).
#[test]
fn beam_e2e_slice_suffix_last_element() {
    let (stdout, _) = run_inline_actor_test("SliceSuffix", SLICE_SUFFIX_SOURCE);
    assert!(
        stdout.contains("last=50"),
        "expected 'last=50' for [10,20,30,40,50], got:\n{stdout}"
    );
}

/// Middle rest `[first, .., last]`: print first and last elements.
const SLICE_MIDDLE_SOURCE: &str = r#"
import std.io as Io
import std.int as Int

fn io main () -> Result Unit Text =
    let xs = [1, 2, 3, 4, 5]
    match xs
        [] -> Io.println "empty"
        [first, .., last] -> Io.println $"first=${Int.toText first} last=${Int.toText last}"
    Ok ()
"#;

/// Runs `[first, .., last]` on a 5-element list; expects first=1 and last=5.
#[test]
fn beam_e2e_slice_middle_first_and_last() {
    let (stdout, _) = run_inline_actor_test("SliceMiddle", SLICE_MIDDLE_SOURCE);
    assert!(
        stdout.contains("first=1"),
        "expected 'first=1' in output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("last=5"),
        "expected 'last=5' in output, got:\n{stdout}"
    );
}

/// Middle-bound rest `[a, mid @ .., b]`: print the middle slice.
const SLICE_MID_BIND_SOURCE: &str = r#"
import std.io as Io
import std.int as Int
import std.list as List

fn io main () -> Result Unit Text =
    let xs = [1, 2, 3, 4, 5]
    match xs
        [] -> Io.println "empty"
        [a, mid @ .., b] ->
            let len = List.length mid
            Io.println $"a=${Int.toText a} b=${Int.toText b} mid_len=${Int.toText len}"
    Ok ()
"#;

/// Runs `[a, mid @ .., b]` on a 5-element list; middle = [2,3,4] (length 3).
#[test]
fn beam_e2e_slice_mid_bind() {
    let (stdout, _) = run_inline_actor_test("SliceMidBind", SLICE_MID_BIND_SOURCE);
    assert!(
        stdout.contains("a=1"),
        "expected 'a=1' in output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("b=5"),
        "expected 'b=5' in output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("mid_len=3"),
        "expected 'mid_len=3' for middle [2,3,4], got:\n{stdout}"
    );
}

/// Empty list falls through to `[]` arm with suffix/middle rest present.
const SLICE_EMPTY_FALLTHROUGH_SOURCE: &str = r#"
import std.io as Io
import std.list as List

fn make_empty () -> List Int =
    List.filter (fn _ -> false) [1]

fn io main () -> Result Unit Text =
    let xs = make_empty ()
    match xs
        [.., last] -> Io.println "non-empty"
        [] -> Io.println "empty"
    Ok ()
"#;

/// An empty list must fall through the `[.., last]` arm (length guard fails)
/// and match the `[]` arm.
#[test]
fn beam_e2e_slice_empty_list_falls_through() {
    let (stdout, _) =
        run_inline_actor_test("SliceEmptyFallthrough", SLICE_EMPTY_FALLTHROUGH_SOURCE);
    assert!(
        stdout.contains("empty"),
        "expected 'empty' for [], got:\n{stdout}"
    );
    assert!(
        !stdout.contains("non-empty"),
        "unexpected 'non-empty' for [], got:\n{stdout}"
    );
}

// ── String literals (commit 1) ───────────────────────────────────────────────

/// Triple-quoted string: dedent against the closing delimiter and drop the
/// opening/closing newlines, then print the value.
const STRING_MULTILINE_SOURCE: &str = r##"
import std.io as Io

fn io main () -> Result Unit Text =
    let s = """
        line one
        line two
        """
    Io.println s
    Ok ()
"##;

/// `"""` must dedent to `line one\nline two` (no leading margin spaces).
#[test]
fn beam_e2e_string_multiline_dedent() {
    let (stdout, _) = run_inline_actor_test("StringMultiline", STRING_MULTILINE_SOURCE);
    assert!(
        stdout.contains("line one\nline two"),
        "expected dedented 'line one\\nline two', got:\n{stdout:?}"
    );
    assert!(
        !stdout.contains("    line one"),
        "margin was not stripped, got:\n{stdout:?}"
    );
}

/// Raw string: backslash escapes are NOT decoded — `r"a\nb"` is the literal
/// four characters a, backslash, n, b.
const STRING_RAW_NO_DECODE_SOURCE: &str = r##"
import std.io as Io

fn io main () -> Result Unit Text =
    Io.println r"a\nb"
    Ok ()
"##;

#[test]
fn beam_e2e_string_raw_no_escape_decode() {
    let (stdout, _) = run_inline_actor_test("StringRawNoDecode", STRING_RAW_NO_DECODE_SOURCE);
    assert!(
        stdout.contains(r"a\nb"),
        "raw string must keep the literal backslash-n, got:\n{stdout:?}"
    );
}

/// Raw string with one hash embeds a plain double-quote: `r#"say "hi""#`.
const STRING_RAW_HASH_SOURCE: &str = r##"
import std.io as Io

fn io main () -> Result Unit Text =
    Io.println r#"say "hi""#
    Ok ()
"##;

#[test]
fn beam_e2e_string_raw_hash_embeds_quote() {
    let (stdout, _) = run_inline_actor_test("StringRawHash", STRING_RAW_HASH_SOURCE);
    assert!(
        stdout.contains("say \"hi\""),
        "raw `#` string must keep the embedded quote, got:\n{stdout:?}"
    );
}

// ── Prefix rest, fixed and record patterns (commits 2a) ──────────────────────

/// `[first, rest @ ..]` binds `first` to the head and `rest` to the tail.
const LIST_PREFIX_REST_SOURCE: &str = r##"
import std.io as Io
import std.int as Int
import std.list as List

fn io main () -> Result Unit Text =
    let xs = [10, 20, 30, 40]
    match xs
        [first, rest @ ..] -> Io.println $"first=${Int.toText first} restLen=${Int.toText (List.length rest)}"
        [] -> Io.println "empty"
    Ok ()
"##;

#[test]
fn beam_e2e_list_prefix_rest_binds_tail() {
    let (stdout, _) = run_inline_actor_test("ListPrefixRest", LIST_PREFIX_REST_SOURCE);
    assert!(
        stdout.contains("first=10"),
        "expected first=10, got:\n{stdout}"
    );
    assert!(
        stdout.contains("restLen=3"),
        "expected restLen=3 for tail [20,30,40], got:\n{stdout}"
    );
}

/// Fixed `[a, b, c]` binds positionally and matches a length-3 list exactly.
const LIST_FIXED_SOURCE: &str = r##"
import std.io as Io
import std.int as Int

fn io main () -> Result Unit Text =
    let xs = [1, 2, 3]
    match xs
        [a, b, c] -> Io.println $"sum=${Int.toText (a + b + c)}"
        _ -> Io.println "other"
    Ok ()
"##;

#[test]
fn beam_e2e_list_fixed_binds_positionally() {
    let (stdout, _) = run_inline_actor_test("ListFixed", LIST_FIXED_SOURCE);
    assert!(stdout.contains("sum=6"), "expected sum=6, got:\n{stdout}");
}

/// Record rest `User { name, .. }` matches and binds `name`, ignoring `age`.
const RECORD_REST_SOURCE: &str = r##"
import std.io as Io

type User = { name: Text, age: Int }

fn io main () -> Result Unit Text =
    let u = User { name = "Ada", age = 42 }
    match u
        User { name, .. } -> Io.println name
    Ok ()
"##;

#[test]
fn beam_e2e_record_rest_ignores_other_fields() {
    let (stdout, _) = run_inline_actor_test("RecordRest", RECORD_REST_SOURCE);
    assert!(
        stdout.contains("Ada"),
        "expected the bound name 'Ada', got:\n{stdout}"
    );
}

// ── Or-pattern BEAM e2e ──────────────────────────────────────────────────────

/// Literal or-pattern `0 | 1 | 2 -> …`: every alternative shares the arm body.
const OR_PATTERN_LITERAL_SOURCE: &str = r##"
import std.io as Io

fn classify (n: Int) -> Text =
    match n
        0 | 1 | 2 -> "low"
        _ -> "high"

fn io main () -> Result Unit Text =
    let _ = Io.println (classify 1)
    let _ = Io.println (classify 9)
    Ok ()
"##;

#[test]
fn beam_e2e_or_pattern_literal_alternatives() {
    let (stdout, _) = run_inline_actor_test("OrPatternLiteral", OR_PATTERN_LITERAL_SOURCE);
    assert!(
        stdout.contains("low"),
        "expected 'low' for classify 1 (matches `1` alternative), got:\n{stdout}"
    );
    assert!(
        stdout.contains("high"),
        "expected 'high' for classify 9 (falls through), got:\n{stdout}"
    );
}

/// Binding or-pattern `Plus x | Minus x -> x`: both alternatives bind `x` (same
/// type), so the shared body can use it regardless of which alternative matched.
const OR_PATTERN_BINDING_SOURCE: &str = r##"
import std.io as Io
import std.int as Int

type Token = Plus Int | Minus Int

fn amount (t: Token) -> Int =
    match t
        Plus x | Minus x -> x

fn io main () -> Result Unit Text =
    Io.println $"a=${Int.toText (amount (Plus 5))} b=${Int.toText (amount (Minus 7))}"
    Ok ()
"##;

#[test]
fn beam_e2e_or_pattern_shared_binding() {
    let (stdout, _) = run_inline_actor_test("OrPatternBinding", OR_PATTERN_BINDING_SOURCE);
    assert!(
        stdout.contains("a=5"),
        "expected 'a=5' from `Plus x -> x` binding x=5, got:\n{stdout}"
    );
    assert!(
        stdout.contains("b=7"),
        "expected 'b=7' from `Minus x -> x` binding x=7, got:\n{stdout}"
    );
}

// ── Multi-line interpolation BEAM e2e ────────────────────────────────────────

/// A `$"""..."""` block dedents to the closing margin and evaluates its `${…}`
/// holes at runtime, producing a value that carries the interior newline.
const MULTILINE_INTERP_SOURCE: &str = r##"
import std.io as Io
import std.int as Int

fn io main () -> Result Unit Text =
    let n = 42
    let msg = $"""
        Value is ${Int.toText n}.
        Second line.
        """
    Io.println msg
"##;

#[test]
fn beam_e2e_multiline_interpolation_dedents_and_evaluates_holes() {
    let (stdout, _) = run_inline_actor_test("MultilineInterp", MULTILINE_INTERP_SOURCE);
    // The 8-space margin is stripped and the hole prints the integer.
    assert!(
        stdout.contains("Value is 42."),
        "expected the dedented, interpolated first line, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Second line."),
        "expected the second interior line, got:\n{stdout}"
    );
    // The interior newline survives: the two lines are on separate output lines.
    assert!(
        stdout.contains("Value is 42.\nSecond line."),
        "expected the interior newline to survive between the two lines, got:\n{stdout:?}"
    );
}

// ── Local `var` reassignment ─────────────────────────────────────────────────
//
// A `var`-bound local reassigned with `<-` inside a plain (non-actor) fn used
// to emit Core Erlang whose right-hand side read the *new* SSA name the binder
// was about to introduce — `let V_Acc1 = V_Acc1 + 10` — instead of the prior
// version. `ridge check` passed, but the backend rejected the body with
// `unbound variable`, so the feature was unusable outside actors. The fix
// lowers the RHS against the pre-assignment scope, so the read resolves to the
// previous binding and only the new binder advances the index.

const LOCAL_VAR_REASSIGN_SOURCE: &str = r#"
import std.io as Io
import std.int as Int

-- A single `<-` in a non-actor fn: the minimal case that used to fail at erlc.
fn once () -> Int =
    var acc = 0
    acc <- acc + 10
    acc

-- A chain of reassignments. Each RHS reads the accumulator's current value, so
-- `acc + acc` must resolve both sides to the same prior binding.
fn running () -> Int =
    var acc = 1
    acc <- acc + 4
    acc <- acc + acc
    acc <- acc - 2
    acc

fn io main () -> Result Unit Text =
    Io.println $"once=${Int.toText (once ())} running=${Int.toText (running ())}"
    Ok ()
"#;

/// Regression: a `var` local reassigned with `<-` in a non-actor fn must run
/// end to end. The RHS of each `<-` reads the accumulator's current SSA
/// version; before the fix it read the binder's about-to-be-introduced version
/// and the backend rejected the body with `unbound variable`.
#[test]
fn beam_e2e_local_var_reassignment() {
    let (workspace_root, _td) = make_example_workspace("LocalVar", LOCAL_VAR_REASSIGN_SOURCE);
    let opts = CompileOptions::new(workspace_root);
    let artefacts =
        compile_workspace(opts).expect("compile_workspace failed for local-var regression");

    assert!(
        !artefacts.beam_files.is_empty(),
        "no .beam files produced\ndiagnostics: {:#?}",
        artefacts.diagnostics
    );

    let beam_file = &artefacts.beam_files[0];
    let beam_dir = beam_file.parent().expect("beam file has parent").to_owned();
    let module_name = beam_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("beam stem is UTF-8")
        .to_owned();

    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module_name, &[]);
    assert_eq!(
        exit_code, 0,
        "erl exited {exit_code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // once = 0 + 10 = 10; running: 1 -> +4=5 -> +acc(5+5)=10 -> -2 = 8.
    assert!(
        stdout.contains("once=10 running=8"),
        "expected 'once=10 running=8' in stdout, got:\n{stdout}"
    );
}

// ── Shutdown barriers (spec §7.2.2) ─────────────────────────────────────────
//
// A send is asynchronous and carries no delivery guarantee once the program
// ends: messages still queued when `main` returns are discarded with the
// actor. Mailboxes are FIFO, so any synchronous interaction with that actor
// acts as a barrier for everything sent to it earlier. These pin both forms
// through `ridge_main_runner` — the path `ridge run` takes, and therefore the
// one where shutdown actually races the handler.
//
// The handler sleeps before printing so a passing test means the barrier
// waited, not that the handler happened to win the race.

const ASK_BARRIER_SOURCE: &str = r#"
import std.io as Io
import std.time as Time

actor Worker =
    state n: Int = 0
    on io slow (msg: Text) =
        Time.sleep 150
        Io.println $"handled: ${msg}"
    on ping (n: Int) -> Int = n

fn spawn io time main () -> Result Unit Text =
    let w = spawn Worker
    w ! slow "one"
    w ! slow "two"
    let _ = w ?> ping 1
    Io.println "done"
    Ok ()
"#;

/// An ask drains everything sent earlier to the same actor, and the actor is
/// still alive afterwards.
#[test]
fn beam_e2e_ask_is_a_barrier_for_earlier_sends() {
    let (stdout, _) = run_inline_actor_test_via_runner("AskBarrier", ASK_BARRIER_SOURCE);
    let handled_one = stdout.find("handled: one");
    let handled_two = stdout.find("handled: two");
    let done = stdout.find("done");
    assert!(
        handled_one.is_some() && handled_two.is_some(),
        "both sends must be handled before the ask returns, got:\n{stdout}"
    );
    assert!(
        handled_one < handled_two && handled_two < done,
        "FIFO order must hold and both must precede `done`, got:\n{stdout}"
    );
}

const STOP_BARRIER_SOURCE: &str = r#"
import std.io as Io
import std.time as Time
import std.actor as Actor

actor Worker =
    state n: Int = 0
    on io slow (msg: Text) =
        Time.sleep 150
        Io.println $"handled: ${msg}"

fn spawn io time main () -> Result Unit Text =
    let w = spawn Worker
    w ! slow "one"
    w ! slow "two"
    let _ = Actor.stop w
    Io.println "done"
    Ok ()
"#;

/// `Actor.stop` queues behind the sends already in the mailbox, so they are
/// handled before the actor goes down — the barrier to reach for at the end
/// of a program.
#[test]
fn beam_e2e_stop_is_a_barrier_for_earlier_sends() {
    let (stdout, _) = run_inline_actor_test_via_runner("StopBarrier", STOP_BARRIER_SOURCE);
    let handled_one = stdout.find("handled: one");
    let handled_two = stdout.find("handled: two");
    let done = stdout.find("done");
    assert!(
        handled_one.is_some() && handled_two.is_some(),
        "both sends must be handled before stop returns, got:\n{stdout}"
    );
    assert!(
        handled_one < handled_two && handled_two < done,
        "FIFO order must hold and both must precede `done`, got:\n{stdout}"
    );
}
