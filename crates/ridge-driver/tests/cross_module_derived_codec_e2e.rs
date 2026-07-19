//! End-to-end check that a derived `Encode`/`Decode` instance round-trips when a
//! field's type is a user type declared in ANOTHER module.
//!
//! `Main` imports `Point` from a sibling `Inner` module and derives codecs for
//! records that nest it directly, inside a `List`, and inside an `Option`. The
//! generated `Encode__Shape__encode` must call `Inner`'s `Encode__Point__encode`
//! cross-module (a module-qualified call to an exported function), not a bare
//! local call that erlc rejects as undefined.
//!
//! Each `pub fn` returns an `Int` so the harness can assert exact values from a
//! single BEAM boot. Gated on `beam-runtime` plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const INNER: &str = "pub type Point = { x: Int, y: Int } deriving (Eq, Encode, Decode)\n";

const MAIN: &str = r#"
import app.Inner (Point)

type Shape = { tag: Int, at: Point } deriving (Eq, Encode, Decode)
type Bag = { items: List Point } deriving (Eq, Encode, Decode)
type Maybe = { p: Option Point } deriving (Eq, Encode, Decode)

-- Nested imported field, round-tripped through the derived codec.
pub fn roundtrip_nested () -> Int =
    let s = Shape { tag = 7, at = Point { x = 3, y = 4 } }
    match decode (encode s)
        Ok s2 -> if s2 == s then 7 else -1
        Err _ -> -2

-- Imported field inside a List.
pub fn roundtrip_list () -> Int =
    let b = Bag { items = [Point { x = 1, y = 2 }, Point { x = 3, y = 4 }] }
    match decode (encode b)
        Ok b2 -> if b2 == b then 2 else -1
        Err _ -> -2

-- Imported field inside an Option.
pub fn roundtrip_option () -> Int =
    let m = Maybe { p = Some (Point { x = 5, y = 6 }) }
    match decode (encode m)
        Ok m2 -> if m2 == m then 11 else -1
        Err _ -> -2
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"xmod-codec-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Inner.ridge"), INNER).expect("write Inner");
    std::fs::write(app_src.join("Main.ridge"), MAIN).expect("write Main");
}

#[test]
fn nested_imported_field_codec_roundtrips() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping nested_imported_field_codec_roundtrips");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-xmod-codec-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-xmod-codec-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    let beam_dir = artefacts
        .beam_files
        .iter()
        .find_map(|p| p.parent())
        .expect("at least one beam file")
        .to_path_buf();
    // The entry module (defines `main`/the pub fns) is the one importing Inner.
    // Both are `ridge_module_N`; pick the one exporting `roundtrip_nested`.
    let modules: Vec<String> = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .filter(|stem| stem.starts_with("ridge_module_"))
        .map(ToOwned::to_owned)
        .collect();

    // Try each candidate module; the right one answers the calls.
    let expr = format!(
        "Mods=[{}], \
         Try=fun(M)->try io:format(\"~s=~p~n\",['roundtrip_nested',M:roundtrip_nested()]), \
             io:format(\"~s=~p~n\",['roundtrip_list',M:roundtrip_list()]), \
             io:format(\"~s=~p~n\",['roundtrip_option',M:roundtrip_option()]) \
             catch _:_ -> ok end end, \
         lists:foreach(Try, Mods), halt().",
        modules
            .iter()
            .map(|m| format!("'{m}'"))
            .collect::<Vec<_>>()
            .join(",")
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

    for (name, want) in [
        ("roundtrip_nested", 7),
        ("roundtrip_list", 2),
        ("roundtrip_option", 11),
    ] {
        let needle = format!("{name}={want}");
        assert!(
            stdout.contains(&needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
