//! End-to-end check that every scalar with a built-in codec survives
//! `encode` → `decode`, and that the ones with no JSON counterpart travel as
//! strings.
//!
//! `Decimal`, `Uuid`, `Bytes`, `Date`, `Time` and `Timestamp` have no lossless
//! JSON value to map onto. Before they had instances the derive recorded such
//! a field as "already JSON" and handed the runtime representation straight to
//! the encoder, which crashed inside the runtime with nothing reported at
//! compile time. The interesting part is therefore what the wire actually
//! carries, so the assertions below check the JSON text as well as the round
//! trip.
//!
//! Each `pub fn` returns an `Int` so the harness can assert exact values from a
//! single BEAM boot. Gated on `beam-runtime` plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const MAIN: &str = r#"
import std.json as Json
import std.text as Text
import std.decimal as Dec
import std.uuid as Uuid
import std.bytes as Bytes
import std.date as Date
import std.timeofday as Tod
import std.time as Time

type Row = { i: Int, f: Float, b: Bool, t: Text, d: Decimal, u: Uuid, y: Bytes, dt: Date, tm: Time, ts: Timestamp } deriving (Encode, Decode)

type Amount = { d: Decimal } deriving (Encode, Decode)

fn sample () -> Result Row Error =
    let u = Uuid.fromText "3f2504e0-4f89-11d3-9a0c-0305e82c3301" ?
    let dt = Date.fromYmd 2026 8 16 ?
    let tm = Tod.fromHms 1 2 3 ?
    Ok (Row { i = 7, f = 1.5, b = true, t = "hi", d = Dec.fromInt 5, u = u, y = Bytes.fromUtf8 "hi", dt = dt, tm = tm, ts = Time.epoch () })

fn attemptRoundtrip () -> Result Int Error =
    let r = sample () ?
    let r2 : Row = decode (encode r) ?
    -- `Eq Float` is intentionally absent from the prelude, so the two records
    -- are compared by what they encode to rather than field by field.
    Ok (if Json.encode (encode r2) == Json.encode (encode r) then 10 else -2)

-- 10 when a record of every built-in scalar survives encode -> decode.
pub fn roundtrip () -> Int =
    match attemptRoundtrip ()
        Ok n -> n
        Err _ -> -1

fn attemptWire () -> Result Int Error =
    let r = sample () ?
    let txt = Json.encode (encode r)
    let quoted = Text.contains "\"d\":\"5\"" txt
    let iso = Text.contains "\"dt\":\"2026-08-16\"" txt
    let hex = Text.contains "\"y\":\"6869\"" txt
    Ok (if quoted && iso && hex then 20 else -2)

-- 20 when Decimal, Date and Bytes land on the wire as strings in their
-- canonical spelling, rather than as a JSON number or a raw runtime value.
pub fn wire_form () -> Int =
    match attemptWire ()
        Ok n -> n
        Err _ -> -1

fn attemptBad () -> Result Int Error =
    let j = Json.decode "{\"d\":\"not-a-decimal\"}" ?
    let back : Result Amount Error = decode j
    match back
        Ok _ -> Ok (-2)
        Err _ -> Ok 30

-- 30 when a string that is not a Decimal is rejected by decode instead of
-- being stored in the field as-is.
pub fn bad_scalar_is_rejected () -> Int =
    match attemptBad ()
        Ok n -> n
        Err _ -> -1
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"scalar-codec-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), MAIN).expect("write Main");
}

#[test]
fn every_builtin_scalar_roundtrips_through_the_derived_codec() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping every_builtin_scalar_roundtrips");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-scalar-codec-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-scalar-codec-e2e-cache-")
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
        .unwrap_or_else(|| {
            panic!(
                "no beam files were emitted; the fixture did not compile: {:?}",
                artefacts.diagnostics
            )
        })
        .to_path_buf();
    let modules: Vec<String> = artefacts
        .beam_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .filter(|stem| {
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
        .map(ToOwned::to_owned)
        .collect();

    let expr = format!(
        "Mods=[{}], \
         Try=fun(M)->try io:format(\"~s=~p~n\",['roundtrip',M:roundtrip()]), \
             io:format(\"~s=~p~n\",['wire_form',M:wire_form()]), \
             io:format(\"~s=~p~n\",['bad_scalar_is_rejected',M:bad_scalar_is_rejected()]) \
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
    assert!(
        stdout.contains("roundtrip=10"),
        "every scalar must survive encode -> decode.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("wire_form=20"),
        "the scalars with no JSON counterpart must travel as canonical strings.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("bad_scalar_is_rejected=30"),
        "a string that is not a Decimal must fail to decode.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
