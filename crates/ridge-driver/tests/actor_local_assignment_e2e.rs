//! Local reassignment preserves previous values, binding identity and actor state.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

#[test]
fn initializers_and_defaults_resolve_module_callables() {
    assert_runs_with_modules(
        r"
import std.actor (AskError)
import lib.Helpers as H

fn initial (v: Int) -> Int = v + 1
fn zero () -> Int = 7
fn unit (u: Unit) -> Int = 7
const callback: fn Int -> Int = initial
class Seed a =
    seed (x: a) -> Int
instance Seed Int =
    seed (x: Int) -> Int = x
fn seeded (x: a) -> Int where Seed a = seed x

fn read (r: Result Int AskError) -> Int =
    match r
        Ok n -> n
        Err _ -> -1000

actor Direct =
    state n: Int = initial 6
    init (input: Int) =
        let f = initial
        let captured = fn (x: Int) -> Int = f x
        n <- n + captured input + zero () + unit () + callback 6
        n <- H.initial (seeded n)
    on value () -> Int = n

actor Defaults =
    state n: Int = callback 6
    on value () -> Int = n

actor FunctionDefault =
    state f: fn Int -> Int = initial
    on value () -> Int = f 6

actor Shadow =
    state n: Int = 0
    init (initial: fn Int -> Int) =
        n <- initial 6
        fn zero () -> Int = 9
        n <- n + zero ()
    on value () -> Int = n

pub fn spawn time exercise () =
    let direct = spawn Direct 6
    let defaults = spawn Defaults
    let functionDefault = spawn FunctionDefault
    let shadow = spawn Shadow (fn (n: Int) -> Int = n + 2)
    (read (direct ?> value ()), read (defaults ?> value ()), read (functionDefault ?> value ()), read (shadow ?> value ()))
",
        "{36,7,7,17}",
        &[("Helpers.ridge", "pub fn initial (n: Int) -> Int = n + 1\n")],
        "D=ridge_lib_Main_defaults:'__ridge_state_defaults'(), F=maps:get(f,ridge_lib_Main_functiondefault:'__ridge_state_defaults'()), maps:get(n,D)=:=7 andalso F(6)=:=7",
    );
}

fn assert_runs(source: &str, expected: &str) {
    assert_runs_with_modules(source, expected, &[], "true");
}

fn assert_runs_with_modules(source: &str, expected: &str, modules: &[(&str, &str)], extra: &str) {
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
    for (name, content) in modules {
        std::fs::write(dir.path().join("lib/src").join(name), content).unwrap();
    }
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
    let eval = format!("Actual={module}:exercise(),case Actual of {expected} -> ok; _ -> io:format(\"unexpected: ~p~n\",[Actual]),halt(5) end,case begin {extra} end of true -> io:put_chars(\"assignment-ok\\n\"); _ -> halt(6) end,halt().");
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

const TRY_STATE: &str = r#"
import std.actor (AskError)

fn fail () -> Result Int Text = Err "fail"
fn unwrap (r: Result Int Text) -> Int =
    match r
        Ok n -> n
        Err _ -> -1

actor Counter =
    state n: Int = 0
    init () =
        let r: Result Int Text = try
            n <- 3
            n
        match r
            Ok v -> n <- v + 2
            Err _ -> n <- -1000
    on current () -> Int = n
    on normal () -> Int =
        let r: Result Int Text = try
            n <- n + 1
            n
        unwrap r
    on failed () -> Int =
        let r: Result Int Text = try
            n <- n + 2
            let x = fail () ?
            n <- n + 100
            x
        unwrap r
    on branch (write: Bool) -> Int =
        let r: Result Int Text = try
            if write then
                n <- n + 3
                n
            else n
        unwrap r
    on nested () -> Int =
        let r: Result Int Text = try
            let inner: Result Int Text = try
                n <- n + 4
                let x = fail () ?
                n <- n + 100
                x
            n <- n + 1
            match inner
                Err _ -> n
                Ok _ -> -1000
        unwrap r
    on mixed () -> (Int, Int, Int) =
        var x = 10
        let r: Result Int Text = try
            n <- n + 1
            x <- x + 2
            n + x
        (unwrap r, x, n)
    on shadow () -> (Int, Int, Int) =
        var x = 10
        let r: Result Int Text = try
            x <- x + 2
            n <- n + 1
            var x = 50
            x <- x + 1
            x
        (unwrap r, x, n)
    on initializer () -> (Int, Int) =
        var r: Result Int Text = try
            n <- n + 1
            n
        let v = unwrap r
        (v, n)
    on choose (write: Bool) -> (Int, Int) =
        let r: Result Int Text =
            if write then try
                n <- n + 2
                n
            else try
                n <- n + 3
                n
        (unwrap r, n)
    on tick () -> Unit =
        let _: Result Unit Text = try
            n <- n + 1
            ()
        ()
    on mixedBranch (write: Bool) -> (Int, Int, Int) =
        var x = 10
        let r: Result Int Text = try
            if write then
                n <- n + 1
                x <- x + 1
                ()
            else ()
            x + n
        (unwrap r, x, n)
    on mixedError () -> (Int, Int, Int) =
        var x = 10
        let r: Result Int Text = try
            n <- n + 1
            x <- x + 2
            let v = fail () ?
            x <- 1000
            n <- 1000
            v
        (unwrap r, x, n)
    on beforeAnyWrite () -> (Int, Int) =
        let r: Result Int Text = try
            let v = fail () ?
            n <- 1000
            v
        (unwrap r, n)
    on unit () -> Result Unit Text =
        try
            n <- n + 1
    on directUnit () -> Unit = n <- n + 1

pub fn spawn time exercise () =
    let h = spawn Counter
    let initial = h ?> current ()
    let normal = h ?> normal ()
    let failure = h ?> failed ()
    let afterFailure = h ?> current ()
    let branch = h ?> branch true
    let noWrite = h ?> branch false
    let nested = h ?> nested ()
    let mixed = h ?> mixed ()
    let shadow = h ?> shadow ()
    let initializer = h ?> initializer ()
    let chosen = h ?> choose true
    let other = h ?> choose false
    h ! tick ()
    let final = h ?> current ()
    let mixedBranch = h ?> mixedBranch true
    let mixedNoWrite = h ?> mixedBranch false
    let mixedError = h ?> mixedError ()
    let beforeAnyWrite = h ?> beforeAnyWrite ()
    let unit = h ?> unit ()
    let afterUnit = h ?> current ()
    let directUnit = h ?> directUnit ()
    let afterDirectUnit = h ?> current ()
    (initial, normal, failure, afterFailure, branch, noWrite, nested, mixed, shadow, initializer, chosen, other, final, mixedBranch, mixedNoWrite, mixedError, beforeAnyWrite, unit, afterUnit, directUnit, afterDirectUnit)
"#;

#[test]
fn try_transports_actor_state_at_normal_and_error_exits() {
    assert_runs(TRY_STATE, "{{ok,5},{ok,6},{ok,-1},{ok,8},{ok,11},{ok,11},{ok,16},{ok,{29,12,17}},{ok,{51,12,18}},{ok,{19,19}},{ok,{21,21}},{ok,{24,24}},{ok,25},{ok,{37,11,26}},{ok,{36,10,26}},{ok,{-1,12,27}},{ok,{-1,27}},{ok,{ok,ok}},{ok,28},{ok,ok},{ok,29}}");
}

#[test]
fn try_transports_state_in_lifecycle_callbacks() {
    let source = LIFECYCLE
        .replace("state sink: Handle Sink", "state sink: Handle Sink\n    state n: Int = 0")
        .replace("sink <- target", "sink <- target\n        let _: Result Unit Text = try\n            n <- n + 1\n            ()")
        .replace("var x = 1\n        x <- x + 1", "let r: Result Int Text = try\n            n <- n + 2\n            n\n        let x = match r\n            Ok v -> v\n            Err _ -> -1000")
        .replace("var x = 1\n        x <- x + 2", "let r: Result Int Text = try\n            n <- n + 3\n            n\n        let x = match r\n            Ok v -> v\n            Err _ -> -1000")
        .replace("Ok 3 -> 3", "Ok 4 -> 4");
    // init adds one, then terminate adds two / onDown adds three.
    assert_runs(&source, "{3,4,4}");
}
