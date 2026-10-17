//! Result propagation converts only errors, using the exact source/destination dictionary.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import app.Conversions as Conv

class Fallible a =
    inspect (x: a) -> Result Int Text

type Inner = LocalFault | Other
type Outer = Wrapped Inner | Unknown
type ConversionBox a = BoxedConversion a | EmptyConversion
type ConversionShell a = ShelledConversion a | NoConversion

instance ErrorFrom (ConversionBox a) Text where ToText a =
    errorFrom (x: ConversionBox a) -> Text = match x
        BoxedConversion value -> toText value
        EmptyConversion -> "empty"

instance ErrorFrom (ConversionShell e) (ConversionShell f) where ErrorFrom e f =
    errorFrom (x: ConversionShell e) -> ConversionShell f = match x
        ShelledConversion error -> ShelledConversion (errorFrom error)
        NoConversion -> NoConversion

instance ErrorFrom Inner Outer =
    errorFrom (x: Inner) -> Outer = Wrapped x

instance ErrorFrom Inner Text =
    errorFrom (x: Inner) -> Text = "text"

-- Equality must bypass even an explicitly supplied reflexive conversion.
instance ErrorFrom Inner Inner =
    errorFrom (x: Inner) -> Inner = Other

fn read (good: Bool) -> Result Int Inner =
    if good then Ok 42 else Err LocalFault

fn outer (good: Bool) -> Result Int Outer =
    let n = read good ?
    Ok n

fn textResult () -> Result Int Text =
    let n = read false ?
    Ok n

fn sameResult () -> Result Int Inner =
    let n = read false ?
    Ok n

fn lift (r: Result a e) -> Result a f where ErrorFrom e f =
    let n = r ?
    Ok n

fn genericResult () -> Result Int Outer = lift (read false)

fn fan (r: Result Int e) -> (Result Int f, Result Int g) where ErrorFrom e f, ErrorFrom e g =
    (lift r, lift r)

fn fanPair () -> (Result Int Outer, Result Int Text) = fan (read false)

pub fn fanout () -> Text = match fanPair ()
    (Err (Wrapped LocalFault), Err "text") -> "fan-ok"
    _ -> "wrong"


pub fn converted () -> Text = match outer false
    Err (Wrapped LocalFault) -> "wrapped"
    _ -> "wrong"

pub fn success () -> Int = match outer true
    Ok n -> n
    _ -> 0

pub fn otherDestination () -> Text = match textResult ()
    Err text -> text
    _ -> "wrong"

pub fn equality () -> Text = match sameResult ()
    Err LocalFault -> "unchanged"
    _ -> "wrong"

pub fn generic () -> Text = match genericResult ()
    Err (Wrapped LocalFault) -> "generic"
    _ -> "wrong"

pub fn explicit () -> Text = errorFrom LocalFault

fn importedResult () -> Result Int Outer = Conv.lift (read false)

pub fn imported () -> Text = match importedResult ()
    Err (Wrapped LocalFault) -> "imported"
    _ -> "wrong"

fn tryResult () -> Result Int Text =
    try
        let n = read false ?
        n

pub fn tryLabel () -> Text = match tryResult ()
    Err text -> text
    _ -> "wrong"

fn invoke (callback: fn Unit -> Result Int Outer) -> Result Int Outer = callback ()

fn callbackResult () -> Result Int Outer =
    invoke (fn _ ->
        let n = read false ?
        Ok n)

pub fn callback () -> Text = match callbackResult ()
    Err (Wrapped LocalFault) -> "callback"
    _ -> "wrong"

fn localResult () -> Result Int Outer =
    fn local (r: Result a e) -> Result a f where ErrorFrom e f =
        let n = r ?
        Ok n
    local (read false)

pub fn localHelper () -> Text = match localResult ()
    Err (Wrapped LocalFault) -> "local"
    _ -> "wrong"

instance Fallible Inner =
    inspect (x: Inner) -> Result Int Text =
        let n = read false ?
        Ok n

pub fn instanceBody () -> Text = match inspect LocalFault
    Err text -> text
    _ -> "wrong"

fn boxedResult () -> Result Int Text =
    let result: Result Int (ConversionBox Int) = Err (BoxedConversion 17)
    let n = result ?
    Ok n

pub fn boxed () -> Text = match boxedResult ()
    Err text -> text
    _ -> "wrong"

fn shelledResult () -> Result Int (ConversionShell Text) =
    let result: Result Int (ConversionShell Inner) = Err (ShelledConversion LocalFault)
    shelledLift result

fn shelledLift (result: Result a (ConversionShell e)) -> Result a (ConversionShell f) where ErrorFrom e f =
    let n = result ?
    Ok n

pub fn shelled () -> Text = match shelledResult ()
    Err (ShelledConversion text) -> text
    _ -> "wrong"
"#;

#[test]
fn error_conversion_preserves_values_and_dispatches_by_both_types() {
    // A runtime-enabled invocation must execute the runtime oracle.
    which::which("erl").expect("erl required for BEAM runtime test");
    which::which("erlc").expect("erlc required for BEAM runtime test");
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname = \"errors\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), SOURCE).unwrap();
    std::fs::write(dir.path().join("app/src/Conversions.ridge"),
        "pub fn lift (r: Result a e) -> Result a f where ErrorFrom e f =\n    let n = r ?\n    Ok n\n").unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile conversion fixture");
    let user = artifacts
        .beam_files
        .iter()
        .find(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with("_Main"))
        })
        .unwrap_or_else(|| {
            panic!(
                "user module missing: {:?}; diagnostics: {:?}",
                artifacts.beam_files, artifacts.diagnostics
            )
        });
    let module = user.file_stem().unwrap().to_str().unwrap();
    let expression = format!(
        "io:format(\"~s~n\",[{module}:converted()]),io:format(\"~p~n\",[{module}:success()]),io:format(\"~s~n\",[{module}:otherDestination()]),io:format(\"~s~n\",[{module}:equality()]),io:format(\"~s~n\",[{module}:generic()]),io:format(\"~s~n\",[{module}:explicit()]),io:format(\"~s~n\",[{module}:imported()]),io:format(\"~s~n\",[{module}:tryLabel()]),io:format(\"~s~n\",[{module}:callback()]),io:format(\"~s~n\",[{module}:localHelper()]),io:format(\"~s~n\",[{module}:instanceBody()]),io:format(\"~s~n\",[{module}:boxed()]),io:format(\"~s~n\",[{module}:shelled()]),io:format(\"~s~n\",[{module}:fanout()]),halt()."
    );
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(user.parent().unwrap())
        .args(["-eval", &expression])
        .output()
        .expect("execute conversion fixture");
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .replace("\r\n", "\n"),
        "wrapped\n42\ntext\nunchanged\ngeneric\ntext\nimported\ntext\ncallback\nlocal\ntext\n17\ntext\nfan-ok\n"
    );
}
