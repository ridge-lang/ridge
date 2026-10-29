//! Checked replies preserve state and reach consumers with their declared types.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

fn workspace(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lib/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"replies\"\nversion=\"0.1.0\"\nmembers=[\"lib\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/src/Replies.ridge"), source).unwrap();
    dir
}

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

#[test]
fn explicit_calls_preserve_function_values_until_invocation() {
    assert!(which::which("erl").is_ok() && which::which("erlc").is_ok());
    let source = r#"import lib.Helpers as H
import std.io as Io
type Callback = fn io () -> Bool
type UnitCallback = fn io Unit -> Bool
const callback: Callback = H.bare
const unitCallback: UnitCallback = H.unit
fn identity f = f
pub fn io exercise () -> Text =
    let first = H.bare
    var second = H.paren
    let passed = identity first
    Io.println "start"
    let _ = passed ()
    let _ = second ()
    let _ = callback ()
    let _ = unitCallback ()
    let _ = List.map (fn (f: Callback) -> f ()) [first, second]
    Io.println "end"
    "done"
"#;
    let dir = workspace(source);
    std::fs::write(
        dir.path().join("lib/src/Helpers.ridge"),
        r#"import std.io as Io
pub fn io bare -> Bool =
    Io.println "bare"
    true
pub fn io paren () -> Bool =
    Io.println "paren"
    false
pub fn io unit (_: Unit) -> Bool = true
"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"io\"]\n").unwrap();
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
        .find(|name| name.to_ascii_lowercase().ends_with("_replies"))
        .unwrap();
    let eval = format!("io:format(\"~ts~n\",[{module}:exercise()]),halt().");
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(beam_dir)
        .args(["-eval", &eval])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    // Taking each reference above must print nothing before `start`.
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        "start\nbare\nparen\nbare\nbare\nparen\nend\ndone\n"
    );
}

const SOURCE: &str = r#"
import std.actor (AskError)
type Book = { value: Int }

actor Source =
    on value () -> Int = 7

fn time readSource (h: Handle Source) -> Result Int AskError =
    let n = h ?> value () ?
    Ok n

fn readInt (r: Result Int AskError) -> Int =
    match r
        Ok n -> n
        Err _ -> -1000

actor Counter =
    state n: Int = 0
    on add (delta: Int) -> Int =
        n <- n + delta
        n
    on choose (b: Bool) -> Result Int Text =
        if b then
            n <- n + 10
            Ok n
        else
            n <- n + 100
            Err "declined"
    on book () -> Book = Book { value = n }
    on viaLambda () -> Int =
        let f = fn (x: Int) -> Int = return x + 1
        f n
    on time viaHelper (h: Handle Source) -> Result Int AskError = readSource h
    on value () -> Int = n

pub fn spawn time exercise () -> Text =
    let h = spawn Counter
    h ! add 1
    let first = readInt (h ?> value ())
    let second = readInt (h ?> add 2)
    let chosen = match h ?> choose true
        Ok (Ok n) -> n
        Ok (Err _) -> -1000
        Err _ -> -2000
    let _ = h ?> choose false
    let after = readInt (h ?> value ())
    let book = match h ?> book ()
        Ok b -> b
        Err _ -> Book { value = -1000 }
    let fromLambda = readInt (h ?> viaLambda ())
    let source = spawn Source
    let fromHelper = match h ?> viaHelper source
        Ok (Ok n) -> n
        Ok (Err _) -> -1000
        Err _ -> -2000
    $"${first},${second},${chosen},${after},${book.value},${fromLambda},${fromHelper}"
"#;

#[test]
fn declared_replies_run_with_state_result_and_nominal_values() {
    assert!(
        which::which("erl").is_ok() && which::which("erlc").is_ok(),
        "runtime gate requires Erlang"
    );
    let workspace = workspace(SOURCE);
    let cache = tempfile::tempdir().unwrap();
    let artefacts = compile_workspace(
        CompileOptions::new(workspace.path().to_owned())
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
        .find(|name| name.to_ascii_lowercase().ends_with("_replies"))
        .unwrap();
    let eval = format!("io:format(\"~ts~n\",[{module}:exercise()]),halt().");
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
    assert_eq!(stdout, "1,3,13,113,113,114,7\n");
}

#[test]
fn incompatible_reply_is_rejected_before_artifact_emission() {
    let source = "actor Counter =\n    on value () -> Int = \"wrong\"\n";
    let workspace = workspace(source);
    let cache = tempfile::tempdir().unwrap();
    let artefacts = compile_workspace(
        CompileOptions::new(workspace.path().to_owned())
            .with_emit(EmitArtefacts::Both)
            .with_cache_root(cache.path().to_owned()),
    )
    .unwrap();
    assert!(
        artefacts.diagnostics.iter().any(|d| d.code == "T001"),
        "{:?}",
        artefacts.diagnostics
    );
    assert!(
        artefacts.beam_files.is_empty() && artefacts.core_files.is_empty(),
        "invalid reply reached code generation"
    );
}

const UNIT_SOURCE: &str = r#"
import lib.Actors as A
import lib.Helpers as H
import std.actor as Mail
import std.actor (AskError)
import std.io as Io

fn value (r: Result Int AskError) -> Int =
    match r
        Ok n -> n
        Err _ -> -1000

fn io unitValue () -> Unit = Io.println "unit"

pub fn io spawn time exercise () -> Text =
    let h = A.counter ()
    let alias = h
    h ! mark ()
    let one = value (alias ?> reply ())
    let _ = Mail.send alias (mark ())
    let two = value (Mail.tryAsk alias (reply ()) 2000)
    H.mark alias
    let three = value (H.read alias)
    let _ = H.send alias
    let four = value (H.tryRead alias)
    let u = ()
    h ! mark u
    let five = value (h ?> reply u)
    h ! mark (unitValue ())
    let six = value (h ?> reply ())
    let _ = Mail.send h (mark (unitValue ()))
    let seven = value (h ?> reply ())
    h ! mark (())
    let eight = value (h ?> reply ())
    h ! ping ()
    let _ = Mail.send h (naked ())
    let after = value (h ?> get ())
    let proxy = A.proxy ()
    let forwarded = value (proxy ?> forward h)
    let zero = A.zero ()
    let nullary = value (proxy ?> nullary zero)
    List.forEach (fn target -> target ! ping ()) [h]
    List.forEach (fn target -> target ! mark ()) [h]
    let read = fn () -> Int = value (alias ?> reply ())
    let captured = read ()
    $"${one},${two},${three},${four},${five},${six},${seven},${eight},${after},${forwarded},${nullary},${captured}"
"#;
const UNIT_ACTORS: &str = r"
import std.actor as Mail
pub actor Counter =
    state n: Int = 0
    on mark (_: Unit) = n <- n + 1
    on reply (_: Unit) -> Int = n
    on get () -> Int = n
    on ping () = n <- n + 10
    on naked = n <- n + 100

pub actor Zero =
    on reply () -> Int = 17

pub actor Proxy =
    on time forward (target: Handle Counter) -> Int =
        target ! mark ()
        let _ = Mail.send target (mark ())
        target ! ping ()
        let _ = Mail.send target (naked ())
        match Mail.tryAsk target (reply ()) 2000
            Ok n -> n
            Err _ -> -1000
    on time nullary (target: Handle Zero) -> Int =
        let first = match target ?> reply ()
            Ok n -> n
            Err _ -> -1000
        let second = match Mail.tryAsk target (reply ()) 2000
            Ok n -> n
            Err _ -> -1000
        first + second

pub fn spawn counter () -> Handle Counter = spawn Counter
pub fn spawn proxy () -> Handle Proxy = spawn Proxy
pub fn spawn zero () -> Handle Zero = spawn Zero
";
const UNIT_HELPERS: &str = r"
import lib.Actors (Counter)
import std.actor as Mail
import std.actor (AskError, SendError)
type CounterAlias = Counter
pub fn mark (h: Handle CounterAlias) -> Unit = List.forEach (fn target -> target ! mark ()) [h]
pub fn time read (h: Handle Counter) -> Result Int AskError = h ?> reply ()
pub fn send (h: Handle Counter) -> Result Unit SendError = Mail.send h (mark ())
pub fn time tryRead (h: Handle Counter) -> Result Int AskError = Mail.tryAsk h (reply ()) 2000
";
#[test]
fn unit_payloads_and_nullary_markers_execute_on_all_message_routes() {
    assert!(which::which("erl").is_ok() && which::which("erlc").is_ok());
    let workspace = workspace(UNIT_SOURCE);
    std::fs::write(workspace.path().join("lib/src/Actors.ridge"), UNIT_ACTORS).unwrap();
    std::fs::write(workspace.path().join("lib/src/Helpers.ridge"), UNIT_HELPERS).unwrap();
    std::fs::write(
        workspace.path().join("lib/ridge.toml"),
        "[project]\nname=\"lib\"\nversion=\"0.1.0\"\nkind=\"library\"\n[capabilities]\nallow=[\"io\",\"spawn\",\"time\"]\n",
    )
    .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let artefacts = compile_workspace(
        CompileOptions::new(workspace.path().to_owned())
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
        .find(|name| name.to_ascii_lowercase().ends_with("_replies"))
        .unwrap();
    let eval = format!("io:format(\"~ts~n\",[{module}:exercise()]),halt().");
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(beam_dir)
        .args(["-eval", &eval])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
        "unit\nunit\n1,2,3,4,5,6,7,8,118,230,34,241\n"
    );
}
