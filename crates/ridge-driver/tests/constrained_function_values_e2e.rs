//! Constrained function values retain their evidence and source-level arity.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

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
