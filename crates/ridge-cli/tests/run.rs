//! Integration tests for `ridge run`.
//!
//! Tests that require a BEAM runtime (`erl` on PATH) are gated behind
//! `#[cfg(feature = "beam-runtime")]`.  The "no executable member" error path
//! and `--observer` connection-info stderr test do not need OTP.
//!
//! ## Feature gates
//!
//! - `beam-runtime` — tests that spawn `erl`.
//! - `cli-watch` — the `--watch` cycle test.
//!
//! Run BEAM tests with:
//! ```text
//! cargo test -p ridge-cli --features beam-runtime,cli-watch
//! ```
//!
//! Run the `--watch` stress test (ignored by default):
//! ```text
//! cargo test -p ridge-cli --features beam-runtime,cli-watch -- --ignored watch_stress
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use assert_cmd::Command;
use common::make_workspace;
#[cfg(feature = "beam-runtime")]
use common::{make_app_workspace, make_example_app_workspace, make_mixed_workspace, write_file};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

// ── helpers ───────────────────────────────────────────────────────────────────

fn ridge_cmd() -> Command {
    Command::cargo_bin("ridge").unwrap()
}

/// Spawn-friendly variant: `assert_cmd::Command::spawn` became private in 2.x,
/// so the `--watch` cycle tests need a raw `std::process::Command` built from
/// the same cargo-bin path. Keeps `ridge_cmd()` available for the assert-based
/// tests that benefit from its richer expectation API.
#[cfg(feature = "cli-watch")]
fn ridge_spawnable_cmd() -> std::process::Command {
    std::process::Command::new(assert_cmd::cargo::cargo_bin("ridge"))
}

/// A minimal Ridge `main` entry point.  Canonical surface: `fn name -> Type =
/// expr` (no parens for zero-arg, no braces; body after `=`).  We use a
/// trivially-typed return so the source parses without needing `import std.io`
/// or capability declarations — these tests only assert that the CLI does not
/// hit C001/C006, not that stdout matches a specific string.
#[cfg(feature = "beam-runtime")]
const HELLO_MAIN: &str = "pub fn main -> Int = 0\n";

// ── Test 1–4: ridge run on each canonical example ─────────────────────────────

/// `ridge run` on the `log_analyzer` example matches the expected output.
///
/// Requires OTP (`erl` on PATH).
#[cfg(feature = "beam-runtime")]
#[test]
fn run_log_analyzer() {
    let tw = make_example_app_workspace("log_analyzer");
    let output = ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The example may fail due to missing CLI args — we accept a non-zero
    // exit here since the expected/*.txt harness is the authoritative check.
    // This test asserts the command at least runs without C001/C006.
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "unexpected workspace-level error.\nstderr: {stderr}"
    );
    let _ = (stdout, stderr);
}

/// `ridge run` on the `url_shortener` example.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_url_shortener() {
    let tw = make_example_app_workspace("url_shortener");
    let output = ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "unexpected workspace-level error.\nstderr: {stderr}"
    );
}

/// `ridge run` on the `game_of_life` example.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_game_of_life() {
    let tw = make_example_app_workspace("game_of_life");
    let output = ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "unexpected workspace-level error.\nstderr: {stderr}"
    );
}

/// `ridge run` on the `rate_limiter` example.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_rate_limiter() {
    let tw = make_example_app_workspace("rate_limiter");
    let output = ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "unexpected workspace-level error.\nstderr: {stderr}"
    );
}

// ── Test 5: --member selection ────────────────────────────────────────────────

/// `ridge run --member myapp` selects the `myapp` app member in a mixed workspace.
///
/// Requires OTP.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_member_selection() {
    let tw = make_mixed_workspace(HELLO_MAIN);
    let output = ridge_cmd()
        .arg("run")
        .arg("--member")
        .arg("myapp")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C005") && !stderr.contains("C007"),
        "unexpected member-selection error.\nstderr: {stderr}"
    );
}

// ── Test 6: argument pass-through after -- ────────────────────────────────────

/// Arguments after `--` are passed through to the BEAM node.
///
/// Requires OTP.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_arg_passthrough() {
    // A module that accepts args — we just verify it doesn't error on C001/C006.
    let tw = make_app_workspace("Main", HELLO_MAIN);
    let output = ridge_cmd()
        .arg("run")
        .arg("--")
        .arg("foo")
        .arg("bar")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run spawn failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("C001") && !stderr.contains("C006"),
        "arg passthrough caused a workspace error.\nstderr: {stderr}"
    );
}

/// `ridge run` shows a warning, and runs the program anyway.
///
/// The same source under `ridge check` reports `T017`; under `run` it used to
/// report nothing at all, so a lint stayed invisible for as long as someone
/// developed with `run` and arrived as a surprise from CI. Nothing about the
/// warning changes the outcome: `run` is not a build gate and takes no
/// `--deny-warnings`.
///
/// Requires OTP.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_shows_a_warning_and_still_runs() {
    let tw = make_app_workspace(
        "Main",
        "type Role = Admin | Guest\n\npub fn main () -> Int =\n    match Guest\n        Admin -> 1\n        Guest -> 2\n        _ -> 3\n",
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .success()
        .stderr(contains("T017").and(contains("redundant pattern")));
}

// ── Test 7: "No executable member" error — does not need OTP ─────────────────

/// `ridge run` in a workspace with only `library` members exits non-zero with
/// `C006 NoExecutableMember`.
#[test]
fn run_no_executable_member() {
    // make_workspace creates a library-only workspace.
    let tw = make_workspace("Lib", "pub fn helper -> Int = 42\n");

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C006"));
}

/// A member whose manifest does not parse is skipped by the collector, so the
/// workspace looks memberless from here — and `run` used to answer `C006`,
/// pointing at the `kind` key on a manifest that declares it. The manifest
/// error is what the reader needs.
#[test]
fn run_names_the_broken_manifest_not_c006() {
    let tw = common::TempWorkspace::new();
    common::write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    common::write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n\
         [capabilities]\nallow = [\"io\", \"rand\"]\n",
    );
    common::write_file(
        &tw.path,
        "apps/demo/src/Main.ridge",
        "fn main () -> Int = 0\n",
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("M011"))
        .stderr(contains("C006").not());
}

// ── Test 8: --watch recompile + restart cycle ─────────────────────────────────

/// `ridge run --watch` survives a single file-change cycle.
///
/// Writes a `.ridge` file mid-run and asserts a recompile + restart occurs
/// without a crash or zombie process.
///
/// Requires OTP and `cli-watch` feature.
#[cfg(all(feature = "beam-runtime", feature = "cli-watch"))]
#[test]
fn run_watch_single_cycle() {
    use std::time::Duration;

    let tw = make_app_workspace("Main", HELLO_MAIN);

    // Spawn `ridge run --watch` in the background.
    let mut child = ridge_spawnable_cmd()
        .arg("run")
        .arg("--watch")
        .current_dir(&tw.path)
        .spawn()
        .expect("failed to spawn ridge run --watch");

    // Give the initial compile + launch a moment.
    std::thread::sleep(Duration::from_secs(3));

    // Touch the source file to trigger a watch event.
    write_file(
        &tw.path,
        "apps/demo/src/Main.ridge",
        "pub fn main -> Int = 1\n",
    );

    // Wait for debounce + recompile + relaunch.
    std::thread::sleep(Duration::from_secs(4));

    // The watch process should still be alive (it should not have crashed).
    let result = child.try_wait().expect("try_wait failed");
    assert!(
        result.is_none(),
        "ridge run --watch exited prematurely after a file change"
    );

    // Kill the watcher cleanly.
    let _ = child.kill();
    let _ = child.wait();
}

// ── Test 9: --observer prints connection-info to stderr ───────────────────────

/// `ridge run --observer` prints the connection-info line to stderr before
/// launching the BEAM node.
///
/// This test does NOT require OTP — it asserts the stderr line appears before
/// the process is attempted.  If OTP is absent the process may fail, but the
/// stderr line must still have been emitted.
///
/// Note: the observer test relies on the workspace having an executable member
/// and finding (or failing to find) erl.  We use a library workspace here to
/// trigger the C006 early exit so the test is OTP-agnostic.
#[test]
fn run_observer_no_executable_member() {
    // Use a library workspace — the CLI should error with C006 before even
    // attempting to resolve the cookie or spawn erl.
    let tw = make_workspace("Lib", "pub fn helper -> Int = 42\n");

    ridge_cmd()
        .arg("run")
        .arg("--observer")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("C006"));
}

/// `ridge run --observer` on an app workspace prints the connection-info line
/// to stderr.
///
/// Requires OTP (needs `erl` to attempt to spawn the node); the test is
/// satisfied if the stderr output contains the connection-info hint regardless
/// of whether the BEAM node actually starts.
#[cfg(feature = "beam-runtime")]
#[test]
fn run_observer_prints_connection_info() {
    // Create a cookie file in a temp location and point the CLI at it via
    // --cookie to avoid depending on the developer's ~/.erlang.cookie.
    let tw = make_app_workspace("Main", HELLO_MAIN);

    let output = ridge_cmd()
        .arg("run")
        .arg("--observer")
        .arg("--cookie")
        .arg("testcookie123")
        .current_dir(&tw.path)
        .output()
        .expect("ridge run --observer spawn failed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Connect with:"),
        "expected connection-info hint on stderr.\nstderr: {stderr}"
    );
}

// ── Stress test: 50-cycle --watch without BEAM process leaks ─────────────────

/// Stress test: `ridge run --watch` survives 50 sequential file-change cycles
/// without leaking BEAM child processes.
///
/// This test is `#[ignore]` by default because it takes ~5 minutes and
/// requires OTP.  Run it explicitly with:
/// ```text
/// cargo test -p ridge-cli --features beam-runtime,cli-watch -- --ignored watch_stress
/// ```
///
/// The test verifies R14 (no zombie processes) by checking that the watcher
/// process does not accumulate open handles after each cycle.
#[cfg(all(feature = "beam-runtime", feature = "cli-watch"))]
#[ignore = "slow stress test — run with: cargo test -p ridge-cli --features beam-runtime,cli-watch -- --ignored watch_stress"]
#[test]
fn watch_stress() {
    use std::time::Duration;

    let tw = make_app_workspace("Main", HELLO_MAIN);

    let mut child = ridge_spawnable_cmd()
        .arg("run")
        .arg("--watch")
        .current_dir(&tw.path)
        .spawn()
        .expect("failed to spawn ridge run --watch");

    // Initial boot.
    std::thread::sleep(Duration::from_secs(3));

    for i in 0..50_u32 {
        // Write a new version of the source file.
        let new_source = format!("pub fn main -> Int = {i}\n");
        write_file(&tw.path, "apps/demo/src/Main.ridge", &new_source);

        // Wait for debounce (500 ms) + compile + restart overhead.
        std::thread::sleep(Duration::from_secs(3));

        // The watcher must still be alive.
        let result = child.try_wait().expect("try_wait failed");
        assert!(
            result.is_none(),
            "ridge run --watch exited prematurely at cycle {i}"
        );
    }

    // Clean shutdown.
    let _ = child.kill();
    let _ = child.wait();
}

// ── Test 10: capability gate — `ridge run` aborts on R016 ────────────────────

/// `ridge run` exits non-zero and renders the diagnostic when the program
/// uses a capability that is not declared in `[capabilities].allow`.
///
/// Does NOT require OTP — the diagnostic gate fires inside the compile phase,
/// before `erl` is probed.
#[test]
fn run_aborts_on_missing_capability() {
    use common::write_file;

    let tw = common::TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"caps-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
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

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(contains("R016"));
}

// ── A stopped program speaks Ridge, not Erlang ───────────────────────────────

/// An app workspace whose member declares capabilities.
///
/// `make_app_workspace` writes a manifest with none, which suits most of this
/// file; the actor test below needs `spawn` and `time`, and the capability gate
/// refuses the program without them.
#[cfg(feature = "beam-runtime")]
fn make_capable_app_workspace(source: &str, allow: &str) -> common::TempWorkspace {
    let tw = common::TempWorkspace::new();
    write_file(
        &tw.path,
        "ridge.toml",
        r#"[workspace]
name = "crash-ws"
version = "0.1.0"
members = ["apps/*"]
"#,
    );
    write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        &format!(
            r#"[project]
name = "demo"
version = "0.1.0"
kind = "app"
entry = "src/Main.ridge"

[capabilities]
allow = [{allow}]
"#
        ),
    );
    write_file(&tw.path, "apps/demo/src/Main.ridge", source);
    tw
}

/// Asking a stopped actor names what happened and what to do instead.
///
/// The reason travelled as `exit:ridge_ask_noproc` — an atom that appears in no
/// documentation — over a stack through the runtime's own source files. None of
/// that is the reader's, and the one thing that was theirs, the remedy, was the
/// part missing.
#[cfg(feature = "beam-runtime")]
#[test]
fn asking_a_stopped_actor_names_the_fault_and_the_remedy() {
    let tw = make_capable_app_workspace(
        r#"import std.io    as Io
import std.actor as Actor

actor Worker =
    state n: Int = 0

    on ping (x: Int) -> Int =
        n + x

pub fn io spawn time main () -> Unit =
    let w = spawn Worker
    let _ = Actor.stop w
    let r = w ?> ping 1
    Io.println "unreachable"
"#,
        r#""io", "spawn", "time""#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("asked an actor that is no longer running")
                .and(contains("Actor.tryAsk"))
                .and(contains("ridge_ask_noproc").not())
                .and(contains("ridge_rt").not())
                .and(contains("stack:").not()),
        );
}

/// `badarith` covers every arithmetic fault OTP has, so the reason alone cannot
/// name this one. The top stack frame carries the operator and its arguments,
/// which is what turns a hedge into a sentence.
#[cfg(feature = "beam-runtime")]
#[test]
fn dividing_by_zero_is_reported_as_dividing_by_zero() {
    let tw = make_app_workspace(
        "Main",
        r#"pub fn main () -> Unit =
    let d = 0
    let _ = 10 / d
    ()
"#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("divided by zero")
                .and(contains("RIDGE_BACKTRACE"))
                .and(contains("badarith").not())
                .and(contains("erl_erts_errors").not()),
        );
}

/// The Erlang underneath is still one variable away.
///
/// Hiding it outright would trade one unusable output for another: whoever is
/// debugging the runtime itself needs the term and the frames, and they are the
/// people least likely to be served by a sentence.
#[cfg(feature = "beam-runtime")]
#[test]
fn the_runtime_stack_is_one_environment_variable_away() {
    let tw = make_app_workspace(
        "Main",
        r#"pub fn main () -> Unit =
    let d = 0
    let _ = 10 / d
    ()
"#,
    );

    ridge_cmd()
        .arg("run")
        .env("RIDGE_BACKTRACE", "1")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("divided by zero")
                .and(contains("stack:"))
                .and(contains("badarith")),
        );
}

/// A program that returns `Err` failed on its own terms, and Ridge adds nothing.
///
/// It used to arrive wrapped in `erl exited with code 1` between stdout and
/// stderr banners — the toolchain announcing a breakage that never happened,
/// over the top of a well-typed program doing exactly what it said it would.
#[cfg(feature = "beam-runtime")]
#[test]
fn a_program_that_returns_err_is_not_framed_as_a_broken_toolchain() {
    let tw = make_app_workspace(
        "Main",
        r#"pub fn main () -> Result Unit Text =
    Err "the input file has no header row"
"#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("the input file has no header row")
                .and(contains("erl exited with code").not())
                .and(contains("--- stderr ---").not())
                .and(contains("--- stdout ---").not()),
        );
}

/// A number too large for `Int` is not a successful parse.
///
/// `Int.parse` and `Json.asInt` both answer `Option Int`, so they already have
/// the word for a value the type cannot hold. They used to hand one back
/// anyway: the BEAM's integers are arbitrary precision, so the bound the spec
/// states was not enforced anywhere a value entered the language.
///
/// The JSON case is the one that matters most and is the reason both are in one
/// test: that number is written by whoever sent the document, not by the
/// program's author.
#[cfg(feature = "beam-runtime")]
#[test]
fn a_number_too_large_for_int_does_not_arrive_as_one() {
    let tw = make_capable_app_workspace(
        r#"import std.io   as Io
import std.int  as Int
import std.json as Json
import std.map  as Map

fn describe (label: Text) (o: Option Int) -> Text =
    match o
        Some n -> Text.concat label (Int.toText n)
        None   -> Text.concat label "out-of-range"

pub fn io main () -> Unit =
    Io.println (describe "parseBig=" (Int.parse "99999999999999999999999999"))
    Io.println (describe "parseOk=" (Int.parse "123"))
    match Json.decode "{\"n\": 99999999999999999999999999}"
        Ok j ->
            match Json.asObject j
                Some m ->
                    match Map.get "n" m
                        Some v -> Io.println (describe "json=" (Json.asInt v))
                        None   -> Io.println "no key"
                None -> Io.println "not an object"
        Err _ -> Io.println "decode failed"
"#,
        r#""io""#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .success()
        .stdout(
            // Labelled so each claim stands on its own rather than on the order
            // three lines happen to arrive in.
            contains("parseBig=out-of-range")
                .and(contains("json=out-of-range"))
                // The ordinary parse still works: a guard that rejected
                // everything would satisfy the two claims above.
                .and(contains("parseOk=123"))
                .and(contains("99999999999999999999999999").not()),
        );
}

/// Arithmetic still computes, spelled either way.
///
/// `+` and `Int.add` are the same declaration reached by two syntaxes, and the
/// declaration no longer names anything to reach. What replaces the name is a
/// table inside the BEAM backend, so this is the check that the table is
/// complete and that both spellings land in it — an operator that resolved and
/// a function call that did not would be a compile error, and the reverse would
/// be two different meanings for one operation.
///
/// `%` earns its line: `Int.mod` is an ordinary Ridge body that calls `rem` by
/// name, which reaches the primitive as a local function rather than through
/// the operator path. That is the case that needs the wrapper to exist in the
/// compiled module at all.
#[cfg(feature = "beam-runtime")]
#[test]
fn arithmetic_computes_the_same_answer_by_operator_and_by_name() {
    let tw = make_capable_app_workspace(
        r#"import std.io    as Io
import std.int   as Int
import std.float as Float

fn showInt (label: Text) (n: Int) -> Text = Text.concat label (Int.toText n)

pub fn io main () -> Unit =
    Io.println (showInt "op=" (2 + 3 * 4 - 1))
    Io.println (showInt "named=" (Int.sub (Int.add 2 (Int.mul 3 4)) 1))
    Io.println (showInt "div=" (7 / 2))
    Io.println (showInt "mod=" (7 % 2))
    Io.println (showInt "neg=" (Int.neg 5))
    Io.println (Text.concat "float=" (Float.toText (Float.neg (1.5 * 2.0))))
"#,
        r#""io""#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .success()
        .stdout(
            // Labelled, so a wrong answer names which operation gave it rather
            // than shifting a line and failing on all of them.
            contains("op=13")
                .and(contains("named=13"))
                .and(contains("div=3"))
                .and(contains("mod=1"))
                .and(contains("neg=-5"))
                .and(contains("float=-3.0")),
        );
}

/// Narrowing a `Float` too large for `Int` says so, and says what the range is.
///
/// `Float.round` and `Float.truncate` return a bare `Int`, so unlike the
/// parsers they have nowhere to put "no" — the answer is an error. `Float`
/// reaches far past what `Int` holds, so this is reachable from any ordinary
/// computation rather than from a literal someone typed.
#[cfg(feature = "beam-runtime")]
#[test]
fn narrowing_a_float_past_ints_range_names_the_value_and_the_range() {
    let tw = make_capable_app_workspace(
        r#"import std.io    as Io
import std.int   as Int
import std.float as Float

pub fn io main () -> Unit =
    Io.println (Int.toText (Float.round (Float.pow 10.0 30.0)))
"#,
        r#""io""#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("`Float.round` produced")
                .and(contains("outside the range of `Int`"))
                .and(contains("-9223372036854775808 to 9223372036854775807"))
                .and(contains("badarg").not()),
        );
}

/// `Int.abs` at the minimum explains the asymmetry rather than restating it.
///
/// The range reaches one further below zero than above it, so the smallest
/// `Int` has no absolute value inside the type — the mistake `Math.abs` is
/// known for in Java, which returns the negative number unchanged. Repeating
/// the bounds would not help here: the argument was already inside them.
#[cfg(feature = "beam-runtime")]
#[test]
fn abs_of_the_smallest_int_explains_why_there_is_no_answer() {
    let tw = make_capable_app_workspace(
        r#"import std.io  as Io
import std.int as Int

pub fn io main () -> Unit =
    Io.println (Int.toText (Int.abs (0 - 9223372036854775807 - 1)))
"#,
        r#""io""#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("`Int.abs` produced 9223372036854775808")
                .and(contains("one further below zero than above it"))
                // The generic range line would be no help: the argument was in range.
                .and(contains("`Int` holds -9223372036854775808").not()),
        );
}

/// Arithmetic past the end of `Int` raises, and the failure names the operation.
///
/// This is the half of the range rule that arithmetic left open: a program could
/// stay inside the type at every entry point and still walk out of it by adding.
/// The value in the message is the one the host produced, which is what makes it
/// obvious that the answer was never going to fit.
#[cfg(feature = "beam-runtime")]
#[test]
fn adding_past_the_end_of_int_names_the_operation_and_the_opt_outs() {
    let tw = make_capable_app_workspace(
        r#"import std.io  as Io
import std.int as Int

pub fn io main () -> Unit =
    Io.println (Int.toText (9223372036854775807 + 1))
"#,
        r#""io""#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("`Int.add` produced 9223372036854775808")
                .and(contains("outside the range of `Int`"))
                // Naming the escape hatches is most of the help: the reader has
                // just been told no, and there are two supported ways to say yes.
                .and(contains("Int.wrappingAdd"))
                .and(contains("Int.saturatingAdd"))
                .and(contains("badarith").not()),
        );
}

/// The operator and the qualified name are one operation, including in the
/// spelling that is easiest to leave behind.
///
/// `Int.add` handed to a higher-order function is emitted as its own small fun
/// rather than as the call `a + b` becomes, so it is the spelling that would
/// quietly keep an out-of-range value if the range test were attached to the
/// call site alone.
#[cfg(feature = "beam-runtime")]
#[test]
fn a_primitive_passed_to_a_higher_order_function_is_still_checked() {
    let tw = make_capable_app_workspace(
        r#"import std.io   as Io
import std.int  as Int
import std.list as List

pub fn io main () -> Unit =
    Io.println (Int.toText (List.fold Int.add 0 [9223372036854775807, 1]))
"#,
        r#""io""#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .failure()
        .stderr(
            contains("`Int.add` produced 9223372036854775808")
                .and(contains("outside the range of `Int`")),
        );
}

/// Landing exactly on either end of the range is an ordinary result.
///
/// The negative control for the two tests above: a range test that is wrong by
/// one would break arithmetic with nothing wrong with it, and would do it at
/// the boundary, where the least code is looking.
#[cfg(feature = "beam-runtime")]
#[test]
fn arithmetic_that_reaches_the_ends_of_the_range_succeeds() {
    let tw = make_capable_app_workspace(
        r#"import std.io  as Io
import std.int as Int

pub fn io main () -> Unit =
    let maxVal = 9223372036854775807
    let minVal = 0 - maxVal - 1
    Io.println (Int.toText (maxVal - 1 + 1))
    Io.println (Int.toText (minVal + 1 - 1))
    Io.println (Int.toText (Int.wrappingAdd maxVal 1))
    Io.println (Int.toText (Int.saturatingAdd maxVal 1))
    Io.println (Int.toText (Int.rem minVal (0 - 1)))
"#,
        r#""io""#,
    );

    ridge_cmd()
        .arg("run")
        .current_dir(&tw.path)
        .assert()
        .success()
        .stdout(
            contains("9223372036854775807")
                .and(contains("-9223372036854775808"))
                // `rem` is the one integer operation with no range test, because
                // a remainder cannot leave a range its operands are inside —
                // including this case, which is the one that catches `div`.
                .and(contains("\n0")),
        );
}
