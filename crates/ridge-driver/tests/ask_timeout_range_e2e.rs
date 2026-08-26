//! A millisecond count outside the platform's range is normalised, not a crash.
//!
//! Every wait on this backend is a `receive ... after`, whose argument is a
//! 32-bit millisecond count. Ridge's `Int` is 64-bit and signed, so a program
//! can name a duration nothing can wait for — in both directions — and four
//! surfaces used to die on one: `?>`'s `timeout`, `Actor.tryAsk`,
//! `Actor.await` and `Time.sleep`.
//!
//! Two of those deaths were worse than unhelpful. `gen_server:call/3` raises
//! `function_clause` out of `gen:call/4`, whose argument list holds the whole
//! message — every argument the handler was sent — and the crash reporter
//! prints that term verbatim. An ask carrying a password printed the password.
//! So the first test below runs the program the way a person runs it, through
//! `ridge_main_runner`, and reads what lands on stderr.
//!
//! The literal spelling is rejected at compile time instead (`T060`), which is
//! why every count here is computed: a test that wrote `timeout -1` would be
//! testing the diagnostic, and the runtime path would go unvisited.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

/// The string that must never reach a crash report.
///
/// It is an ordinary handler argument, which is the point: nothing marks it as
/// secret, and nothing could — any ask can carry a credential.
const SECRET: &str = "hunter2-correct-horse";

const SOURCE: &str = r#"
import std.actor as Actor
import std.time as Time

actor Vault =
    state opened: Int = 0

    on unlock (secret: Text) (pin: Int) -> Int =
        opened + pin

-- A deadline already in the past, computed rather than written. `?>` raises on
-- a missed deadline, so this one is read from the runner's stderr.
pub fn spawn time askNegative () -> Int =
    let v = spawn Vault
    let ms = 0 - 1
    v ?> unlock "hunter2-correct-horse" 4242 timeout ms

-- `tryAsk` promises a `Result` instead of a raise. It used to die anyway, which
-- is the one failure its whole contract exists to rule out.
pub fn spawn time tryAskNegative () -> Text =
    let v = spawn Vault
    let ms = 0 - 1
    match Actor.tryAsk v (unlock "hunter2-correct-horse" 4242) ms
        Ok _ -> "ok"
        Err _ -> "err"

-- Above the ceiling the wait is capped, so a handler that replies at once still
-- replies rather than the program dying with `timeout_value`.
pub fn spawn time askHuge () -> Int =
    let v = spawn Vault
    let ms = 9223372036854775000 + 807
    v ?> unlock "hunter2-correct-horse" 4242 timeout ms

-- The same rule on a monitor's deadline: nothing is down yet, and a negative
-- wait is a wait that has already elapsed.
pub fn spawn time awaitNegative () -> Text =
    let v = spawn Vault
    let m = Actor.monitor v
    match Actor.await m (0 - 1)
        Some _ -> "down"
        None -> "alive"

-- And on a plain pause, which reaches `timer:sleep/1` through the same guard.
pub fn time sleepNegative () -> Text =
    Time.sleep (0 - 1)
    "returned"
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"ask-timeout-range-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = [\"spawn\", \"time\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

/// Compile the workspace and return `(beam_dir, user_module)`.
fn build(dir: &std::path::Path, cache: &std::path::Path) -> (std::path::PathBuf, String) {
    let artefacts = compile_workspace(
        CompileOptions::new(dir.to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.to_path_buf()),
    )
    .expect("compile to BEAM");

    assert!(
        artefacts.diagnostics.is_empty(),
        "no compile errors expected; got {:?}",
        artefacts.diagnostics
    );

    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|stem| {
            stem.starts_with("ridge_")
                && !matches!(
                    *stem,
                    "ridge_rt"
                        | "ridge_main_runner"
                        | "ridge_test_runner"
                        | "ridge_pg"
                        | "ridge_sup"
                        | "ridge_sqlite"
                        | "ridge_bench_runner"
                )
        })
        .expect("a user module")
        .to_owned();
    (beam_dir, module)
}

/// The security half: the report a person actually sees carries no payload.
///
/// Run through `ridge_main_runner` rather than by calling the function from
/// `erl -eval`, because the leak was never in the raise — it was in what the
/// reporter printed when the reason was one it did not recognise. Reading the
/// term from inside a `catch` would have shown a clean-looking tuple and proved
/// nothing about the sentence on the terminal.
#[test]
fn a_negative_ask_deadline_reports_without_the_payload() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping a_negative_ask_deadline_reports_without_the_payload"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-ask-timeout-range-report-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-ask-timeout-range-report-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());
    let (beam_dir, module) = build(dir.path(), cache.path());

    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-s")
        .arg("ridge_main_runner")
        .arg("run")
        .arg(&module)
        .arg("askNegative")
        .arg("-s")
        .arg("init")
        .arg("stop")
        .output()
        .expect("run erl");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let both = format!("{stdout}{stderr}");

    // The positive half is also the control for the negative one: if the run
    // never happened, or the reason changed shape, this fails rather than
    // leaving "the secret is absent" true for the wrong reason.
    assert!(
        both.contains("no answer to `unlock`"),
        "expected the missed-deadline report in Ridge's voice\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !both.contains("function_clause"),
        "the OTP guard failure must not reach the reader\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !both.contains(SECRET),
        "the handler's arguments must not appear in a crash report\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// The four surfaces that return rather than raise, in one run.
#[test]
fn an_out_of_range_wait_is_normalised_at_every_surface() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping an_out_of_range_wait_is_normalised_at_every_surface"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-ask-timeout-range-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-ask-timeout-range-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());
    let (beam_dir, module) = build(dir.path(), cache.path());

    // `askNegative` is read through its reason rather than its return: the
    // deadline in the raised term is the one that was actually waited, so
    // `{deadline,0}` says both that it did not die in `gen:call/4` and that the
    // negative count arrived as an already-passed deadline.
    let expr = format!(
        "Neg = try {module}:askNegative() of V -> {{returned, V}} \
           catch error:{{ridge_rt_ask_timeout, _, D}} -> {{deadline, D}}; C:R -> {{other, C, R}} end, \
         io:format(\"askNegative=~p~n\",[Neg]), \
         io:format(\"tryAskNegative=~s~n\",[{module}:tryAskNegative()]), \
         io:format(\"askHuge=~p~n\",[{module}:askHuge()]), \
         io:format(\"awaitNegative=~s~n\",[{module}:awaitNegative()]), \
         io:format(\"sleepNegative=~s~n\",[{module}:sleepNegative()]), \
         halt()."
    );
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    for (probe, why) in [
        (
            "askNegative={deadline,0}",
            "a computed negative deadline arrives as an already-passed one, not as a guard failure in gen:call/4",
        ),
        (
            "tryAskNegative=err",
            "`tryAsk` returns its Result instead of raising, which is the whole of its contract",
        ),
        (
            "askHuge=4242",
            "a count above the platform's ceiling waits the longest it can express, and the handler still replies",
        ),
        (
            "awaitNegative=alive",
            "a negative wait on a monitor has already elapsed and nothing is down",
        ),
        (
            "sleepNegative=returned",
            "a negative pause has already elapsed and returns at once",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
