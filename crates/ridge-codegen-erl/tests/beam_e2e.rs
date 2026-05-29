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
//! Pre-existing failures (unchanged from baseline):
//! - `log_analyzer` — CLI args flow via `plain_args` (-extra flag) works
//!   but stdout mismatch persists (encoding issue).
//! - `url_shortener` — `std.map` BEAM module not on code path.
//! - `game_of_life` — `std.list` BEAM module not on code path.
//! - `rate_limiter` — passes (1/4 baseline unchanged).

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

/// `rate_limiter` — erlc rejects emitted Core Erlang in all actor modules.
///
/// erlc subprocess stderr (verbatim):
///   ridge_module_0: illegal expression in main/0 (×1)
///   ridge_module_0_limiter: unbound variable 'V_Capacity', 'V_LastRefill',
///     'V_RefillRate', 'V_State2', 'V_State4', 'V_Tokens' in handle_call/3, handle_cast/2
///   ridge_module_0_collector: illegal expression in handle_call/3, handle_cast/2 (×8)
///   ridge_module_0_collector: unbound variable 'V_Received', 'V_State3',
///     'V_TotalAllowed', 'V_TotalDenied' in handle_call/3, handle_cast/2
///   ridge_module_0_worker: unbound variable 'V_Collector', 'V_Limiter',
///     'V_SendRequests', 'V_State3' in handle_call/3, handle_cast/2
///
/// Root cause analysis:
/// - "illegal expression in main/0": `apply ~{}~ ('ok')` — Ok() constructor codegen bug.
/// - "illegal expression in collector handle_call/3, handle_cast/2": same Ok() bug
///   inside actor handler bodies.
/// - "unbound V_Capacity, V_Tokens, V_LastRefill, V_RefillRate in handlers":
///   state-field reads emitted as bare variable references instead of
///   `call 'maps':'get'('field', V_State)`.
/// - "unbound V_State2, V_State4 in handlers": SSA state vars scoped incorrectly
///   — state-thread index mismatch.
/// - Unbound V_Cap, V_Rate, V_Col, V_Id, V_L in init/1: RESOLVED — init body
///   now wrapped in `case V_Args of [P1|[P2|[]]] -> end`.
/// - Undefined function requestsPerWorker/0 in handle_call/3: RESOLVED — codegen
///   now emits `call 'ridge_module_0':'requestsPerWorker' ()` for parent-module
///   const refs in actor handlers.
///
/// **Remaining errors** (Ok/Err ctor in actor handlers, state-field reads,
/// SSA state-thread index mismatch, tuple-param lambda destructure in
/// collectors/workers) are in IR/lowering (`ridge-lower`) — out of scope here.
/// These involve frozen-crate semantics and would require a plan-level decision
/// before any source change to `ridge-codegen-erl` or `ridge-lower`.
///
/// **What is deferred:** Full e2e BEAM execution of `rate_limiter.ridge`.
/// **Why:** Multi-actor codegen requires IR/lowering fixes in `ridge-lower`.
/// **Where the follow-up lives:** `rate_limiter` codegen is a backlog item
/// unless a future change explicitly reopens it.
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
//
// The companion case — mailboxSize on a dead actor returns None — needs a
// way to terminate an actor from Ridge source without taking the spawning
// process down with it. gen_server:start_link links the spawner, so the
// natural "crash on a malformed message" recipe collapses main too. That
// case is left to a future test once a non-linking spawn or a Ridge-level
// exit-trap primitive lands; pure-Erlang coverage of mailbox_size/1's
// `undefined` branch is already in the runtime.

/// Helper that compiles, runs, and asserts a zero exit code, returning
/// `(stdout, stderr)`.
fn run_inline_actor_test(name: &str, source: &str) -> (String, String) {
    let (workspace_root, _td) = make_example_workspace(name, source);
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
    let (stdout, stderr, exit_code) = run_erl(&beam_dir, &module, &[]);
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
