//! Constrained function values retain their evidence and source-level arity.
//! Function-valued constants load their value before applying source arguments.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const FUNCTION_CONSTANT_DEFS: &str = r"
import std.list as List
pub const increment: fn Int -> Int = fn (n: Int) -> Int = n + 1
pub const zero: fn () -> Int = fn () -> Int = 7
pub const unit: fn Unit -> Int = fn (_u: Unit) -> Int = 7
pub const subtract: fn Int Int -> Int = fn (a: Int) (b: Int) -> Int = a - b
pub const captured: fn Int -> Int =
    let offset = 5
    fn (n: Int) -> Int = n + offset
pub const sorted: fn (List Int) -> List Int = fn (xs: List Int) -> List Int = List.sort xs
pub fn ordinary () -> Int = 7
pub fn explicitUnit (_u: Unit) -> Int = 7
pub const number: Int = 7
";

const FUNCTION_CONSTANT_MAIN: &str = r#"
import app.Helpers as H
import std.io as Io
{definitions}
fn apply (f: fn Int -> Int) -> Int = f 6
pub fn check () -> Bool =
    let local = increment
    let imported = H.increment
    let z = H.zero
    let u = H.unit
    let reference = H.ordinary
    let partial = H.subtract 10
    let shadow = fn (increment: fn Int -> Int) -> Int = increment 6
    increment 6 == 7 && local 6 == 7 && H.increment 6 == 7 && imported 6 == 7 &&
        zero () == 7 && H.zero () == 7 && z () == 7 &&
        unit () == 7 && H.unit () == 7 && u () == 7 &&
        subtract 10 3 == 7 && H.subtract 10 3 == 7 && partial 3 == 7 &&
        captured 2 == 7 && H.captured 2 == 7 &&
        apply increment == 7 && apply H.increment == 7 &&
        ordinary () == 7 && H.ordinary () == 7 && reference () == 7 &&
        explicitUnit () == 7 && H.explicitUnit () == 7 &&
        number == 7 && H.number == 7 && shadow (fn n -> n + 3) == 9 &&
        sorted [3,1,2] == [1,2,3] && H.sorted [3,1,2] == [1,2,3]

actor Worker =
    on value () -> Int = H.increment 6
    on localValue () -> Int = increment 6
    on functionValue () -> Int =
        let f = ordinary
        f ()
    on aliasValue () -> Int =
        let f = H.increment
        f 6
pub fn spawn time actorCheck () -> Bool =
    let worker = spawn Worker
    (worker ?> value ()) == Ok 7 && (worker ?> localValue ()) == Ok 7 && (worker ?> aliasValue ()) == Ok 7 && (worker ?> functionValue ()) == Ok 7

pub fn io effects () -> Int =
    let arg = fn (label: Text) (n: Int) -> Int =
        Io.println label
        n
    H.subtract (arg "first" 10) (arg "second" 3)

pub const sink: fn io Int -> Int = fn (n: Int) -> Int =
    Io.println "called"
    n
pub fn io effectValue () -> Int = sink 7
"#;

#[test]
fn function_constants_keep_call_arity_values_captures_and_imports() {
    which::which("erl").expect("erl required");
    which::which("erlc").expect("erlc required");
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"constant-functions\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("app/src/Helpers.ridge"),
        FUNCTION_CONSTANT_DEFS,
    )
    .unwrap();
    let main = FUNCTION_CONSTANT_MAIN.replace("{definitions}", FUNCTION_CONSTANT_DEFS);
    std::fs::write(dir.path().join("app/src/Main.ridge"), main).unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let expression = r#"M=ridge_app_Main,case {M:check(),M:actorCheck()} of {true,true}->ok; Other->io:format("~p~n",[Other]),halt(5) end,7=M:effects(),7=M:effectValue(),io:put_chars("constant-values-ok\n"),halt()."#;
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
        .args(["-eval", expression])
        .output()
        .unwrap();
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
        "first\nsecond\ncalled\nconstant-values-ok\n"
    );
}

const SOURCE: &str = r#"
import std.list as List
import app.Helpers as Helpers

type Ranked = | Zed | Aye deriving (Eq, Ord)

pub fn direct () -> List Int = List.sort [3, 1, 2]
pub fn alias () -> List Int =
    let sort = List.sort
    sort [3, 1, 2]
pub fn tupled () -> List Int =
    let (sort, count) = (List.sort, 0)
    sort [3, 1, 2]
pub fn captured () -> List Int =
    let sort = List.sort
    let call = fn xs -> sort xs
    call [3, 1, 2]
pub fn piped () -> List Int =
    let sort = List.sort
    [3, 1, 2] |> sort
pub fn directPipe () -> List Int = [3, 1, 2] |> List.sort

fn applySort (sort: List Int -> List Int) -> List Int = sort [3, 1, 2]
pub fn passed () -> List Int = applySort List.sort

fn sorted (xs: List a) -> List a where Ord a =
    let sort = List.sort
    sort xs
pub fn forwarded () -> Bool =
    sorted [3, 1, 2] == [1, 2, 3] && sorted [Aye, Zed] == [Zed, Aye]

fn sorter (witness: a) -> (List a -> List a) where Ord a = List.sort
pub fn returned () -> List Int =
    let sort = sorter 0
    sort [3, 1, 2]

fn describe (value: a) -> Text where ToText a = $"value:${value}"
pub fn local () -> Text =
    let render = describe
    render 7
pub fn imported () -> Text =
    let render = Helpers.describe
    render 9
pub fn inner () -> Text =
    fn render (value: a) -> Text where ToText a = $"inner:${value}"
    let saved = render
    saved 11
pub fn shadowed () -> (List Int, List Int) =
    let sort = List.sort
    let inside =
        let sort = fn xs -> List.reverse xs
        sort [3, 1, 2]
    (inside, sort [3, 1, 2])
pub fn innerShadowed () -> (Int, Text) =
    fn render (value: a) -> Text where ToText a = $"outer:${value}"
    let inside =
        let render = fn x -> x + 1
        let saved = render
        saved 4 + render 5
    (inside, render 7)
"#;

#[test]
fn constrained_values_keep_evidence_through_storage_and_calls() {
    which::which("erl").expect("erl required");
    which::which("erlc").expect("erlc required");
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/src")).unwrap();
    std::fs::write(
        dir.path().join("ridge.toml"),
        "[workspace]\nname=\"constrained-values\"\nversion=\"0.1.0\"\nmembers=[\"app\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/ridge.toml"),
        "[project]\nname=\"app\"\nversion=\"0.1.0\"\nkind=\"library\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("app/src/Main.ridge"), SOURCE).unwrap();
    std::fs::write(
        dir.path().join("app/src/Helpers.ridge"),
        "pub fn describe (value: a) -> Text where ToText a = $\"imported:${value}\"\n",
    )
    .unwrap();
    let artifacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .unwrap();
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    let module = artifacts
        .beam_files
        .iter()
        .find(|p| p.file_stem().and_then(|s| s.to_str()) == Some("ridge_app_Main"))
        .unwrap();
    let expression = r#"M=ridge_app_Main, Actual={M:direct(),M:alias(),M:tupled(),M:captured(),M:piped(),M:directPipe(),M:passed(),M:forwarded(),M:returned(),M:local(),M:imported(),M:inner(),M:shadowed(),M:innerShadowed()}, Expected={[1,2,3],[1,2,3],[1,2,3],[1,2,3],[1,2,3],[1,2,3],[1,2,3],true,[1,2,3],<<"value:7">>,<<"imported:9">>,<<"inner:11">>,{[2,1,3],[1,2,3]},{11,<<"outer:7">>}}, case Actual of Expected -> io:put_chars("constrained-values-ok\n"); _ -> io:format("unexpected: ~p~n",[Actual]),halt(5) end,halt()."#;
    let output = std::process::Command::new("erl")
        .args(["-noshell", "-pa"])
        .arg(module.parent().unwrap())
        .args(["-eval", expression])
        .output()
        .unwrap();
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
        "constrained-values-ok\n"
    );
}
