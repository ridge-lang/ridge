//! End-to-end value checks for the last-mile stdlib combinators
//! (`Float.min`/`max`/`pow`, `Result.orElse`, `List.partition`/`unique`),
//! exercised through the real modules on the BEAM.
//!
//! Each `pub fn` returns an `Int` so the harness can assert exact values from a
//! single boot. Gated on `beam-runtime` plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.float as Float
import std.result as Result
import std.list as List

-- Float.min / max, rounded so the value asserts as an Int.
pub fn float_min () -> Int = Float.round (Float.min 3.0 2.0)
pub fn float_max () -> Int = Float.round (Float.max 3.0 2.0)
pub fn float_pow () -> Int = Float.round (Float.pow 2.0 10.0)

-- Result.orElse recovers an Err with the fallback, and leaves an Ok untouched.
pub fn orelse_recovers () -> Int = Result.withDefault 0 (Result.orElse (Ok 7) (Err "boom"))
pub fn orelse_keeps_ok () -> Int = Result.withDefault 0 (Result.orElse (Ok 7) (Ok 5))

-- List.partition splits in original order: yes = [3, 4], no = [1, 2] -> 22.
pub fn partition_counts () -> Int =
    let (yes, no) = List.partition (fn n -> n > 2) [1, 2, 3, 4]
    List.length yes * 10 + List.length no

-- List.unique keeps the first occurrence of each element: [1,2,3] -> length 3.
pub fn unique_len () -> Int = List.length (List.unique [1, 1, 2, 3, 3, 3])
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"stdlib-last-mile-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn last_mile_combinators_compute_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping last_mile_combinators_compute_correct_values");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-stdlib-last-mile-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-stdlib-last-mile-e2e-cache-")
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
    let module = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .find(|stem| stem.starts_with("ridge_module_"))
        .expect("a user module")
        .to_owned();

    let expr = format!(
        "F=fun(N)->io:format(\"~s=~p~n\",[N,{module}:N()])end, \
         lists:foreach(F,['float_min','float_max','float_pow',\
         'orelse_recovers','orelse_keeps_ok','partition_counts','unique_len']), halt()."
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
        ("float_min", 2),
        ("float_max", 3),
        ("float_pow", 1024),
        ("orelse_recovers", 7),
        ("orelse_keeps_ok", 5),
        ("partition_counts", 22),
        ("unique_len", 3),
    ] {
        let needle = format!("{name}={want}");
        assert!(
            stdout.contains(&needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
