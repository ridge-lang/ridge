//! Local reassignment preserves previous values, binding identity and actor state.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

fn assert_runs(source: &str, expected: &str) {
    assert!(
        which::which("erl").is_ok() && which::which("erlc").is_ok(),
        "runtime gate requires Erlang"
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"actor-assignment\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/src/Main.ridge"), source).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_owned())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_owned()),
    )
    .unwrap();
    assert!(
        artefacts.diagnostics.is_empty(),
        "{:?}",
        artefacts.diagnostics
    );
    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .unwrap();
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|name| name.ends_with("_Main"))
        .unwrap();
    let eval = format!("Actual={module}:exercise(),case Actual of {expected} -> io:put_chars(\"assignment-ok\\n\"); _ -> io:format(\"unexpected: ~p~n\",[Actual]),halt(5) end,halt().");
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(beam_dir)
        .args(["-eval", &eval])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout, "assignment-ok\n");
}

const UPDATES: &str = r"
import std.actor (AskError)

fn read (r: Result Int AskError) -> Int =
    match r
        Ok n -> n
        Err _ -> -1000

actor Counter =
    state n: Int = 0
    init () =
        var seed = 1
        seed <- seed + 1
        n <- seed
    on value () -> Int = n
    on add (delta: Int) -> Int =
        var x = n
        x <- x + delta
        n <- x
        n
    on branch (enabled: Bool) -> Int =
        var x = n
        if enabled then
            x <- x + 1
            n <- x
            x
        else
            x <- x + 10
            n <- x
            x
    on simple () -> Int =
        var x = 1
        x <- x + 1
        x
    on literal () -> Int =
        var x = 1
        x <- 7
        x
    on other () -> Int =
        var x = 1
        let y = 4
        x <- y + 1
        x
    on repeated () -> Int =
        var x = 1
        x <- 2
        x <- x + 3
        x
    on conditional () -> Int =
        var x = 1
        x <- if true then x + 1 else x + 2
        x
    on inBranch () -> Int =
        var x = 1
        if true then
            x <- x + 2
            x
        else x
    on helper () -> Int =
        var x = 1
        let next = fn (y: Int) -> Int = y + 1
        x <- next x
        x
    on initializer () -> Int =
        var x = 1
        let ignored =
            x <- 7
            x
        x
    on shadow () -> Int =
        var x = 1
        let inner =
            var x = x + 2
            x
        inner * 10 + x
    on shadowLiteral () -> Int =
        var x = 1
        let inner =
            var x = 7
            x
        inner * 10 + x
    on aliasCallback () -> Int =
        var f = fn (y: Int) -> Int = y + 1
        let old = f
        f <- fn (y: Int) -> Int = old y + 2
        f 3
    on recursiveFunction () -> Int =
        fn factorial (input: Int) -> Int = if input == 0 then 1 else input * factorial (input - 1)
        factorial 4

pub fn spawn time exercise () -> List Int =
    let h = spawn Counter
    let initial = read (h ?> value ())
    h ! add 3
    let afterSend = read (h ?> value ())
    let afterAsk = read (h ?> add 2)
    let firstBranch = read (h ?> branch true)
    let secondBranch = read (h ?> branch false)
    let snapshot = read (h ?> value ())
    [initial, afterSend, afterAsk, firstBranch, secondBranch, snapshot, read (h ?> simple ()), read (h ?> literal ()), read (h ?> other ()), read (h ?> repeated ()), read (h ?> conditional ()), read (h ?> inBranch ()), read (h ?> helper ()), read (h ?> initializer ()), read (h ?> shadow ()), read (h ?> shadowLiteral ()), read (h ?> aliasCallback ()), read (h ?> recursiveFunction ())]
";

#[test]
fn actor_local_updates_preserve_values_bindings_and_state() {
    assert_runs(UPDATES, "[2,5,7,8,18,18,2,7,5,5,2,3,2,7,31,71,6,24]");
}

#[test]
fn reassigned_actor_callback_captures_the_previous_value() {
    assert_runs(
        r"
import std.actor (AskError)
actor Counter =
    on value () -> Int =
        var f = fn (n: Int) -> Int = n + 1
        f <- fn (n: Int) -> Int = if n == 0 then 10 else f 0 + n
        f 2
pub fn spawn time exercise () -> Int =
    let h = spawn Counter
    match h ?> value ()
        Ok n -> n
        Err _ -> -1000
",
        "3",
    );
}

const LIFECYCLE: &str = r"
import std.actor as Actor
import std.actor (AskError, ExitReason)
import std.time as Time

actor Sink =
    state terminated: Int = 0
    state down: Int = 0
    on recordTermination (value: Int) -> Unit = terminated <- value
    on recordDown (value: Int) -> Unit = down <- value
    on snapshot () -> (Int, Int) = (terminated, down)

actor Worker =
    state sink: Handle Sink
    init (target: Handle Sink) =
        sink <- target
    on value () -> Int = 0
    terminate time (reason: ExitReason) =
        var x = 1
        x <- x + 1
        match sink ?> recordTermination x
            Ok _ -> ()
            Err _ -> ()

actor Watcher =
    state sink: Handle Sink
    state notifications: Int = 0
    init (target: Handle Sink) =
        sink <- target
    on watch (worker: Handle Worker) -> Unit =
        let _ = Actor.monitor worker
        ()
    on count () -> Int = notifications
    onDown time (m: Monitor) (reason: ExitReason) =
        var x = 1
        x <- x + 2
        notifications <- x
        match sink ?> recordDown x
            Ok _ -> ()
            Err _ -> ()

fn time waitForDown (h: Handle Watcher) (attempts: Int) -> Int =
    if attempts == 0 then -1000 else
        match h ?> count ()
            Ok 3 -> 3
            Ok _ ->
                Time.sleep 10
                waitForDown h (attempts - 1)
            Err _ -> -2000

pub fn spawn time exercise () -> (Int, Int, Int) =
    let sink = spawn Sink
    let worker = spawn Worker sink
    let watcher = spawn Watcher sink
    let _ = watcher ?> watch worker
    Actor.stop worker
    let observed = waitForDown watcher 500
    match sink ?> snapshot ()
        Ok (terminated, down) -> (terminated, down, observed)
        Err _ -> (-1000, -1000, observed)
";

#[test]
fn lifecycle_local_updates_reach_their_consumers() {
    assert_runs(LIFECYCLE, "{2,3,3}");
}
