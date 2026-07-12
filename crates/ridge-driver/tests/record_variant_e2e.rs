//! End-to-end value checks for record-payload union variants through the full
//! pipeline: parse → typecheck against the variant's inline record schema →
//! lower to a tagged tuple wrapping a record map → Core Erlang → run on the BEAM
//! → assert runtime values.
//!
//! The runtime shape of `Login { userId = 7, at = 1000 }` is the tagged tuple
//! `{'Login', #{userId => 7, at => 1000}}`. Because the payload is a map keyed by
//! field name, construction and matching are order-insensitive.
//!
//! Each `pub fn` returns an `Int` so the harness can assert exact values from a
//! single BEAM boot. Gated on `beam-runtime` plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

const SOURCE: &str = r#"
import std.json as Json

type Event =
    | Login { userId: Int, at: Int }
    | Logout { userId: Int, reason: Int }
    | Tick

type Box a = | Wrap { val: a }

type Msg =
    | Ping { seq: Int, ttl: Int }
    | Pong Int
    | Idle
    deriving (Eq, ToText, Encode, Decode)

-- Construct in declared field order; match a record variant with a rest pattern.
pub fn construct_and_match () -> Int =
    let e = Login { userId = 7, at = 1000 }
    match e
        Login { userId, .. }  -> userId
        Logout { userId, .. } -> userId
        Tick                  -> 0

-- Construct with fields in the reverse of the declared order. The map payload is
-- keyed by name, so the value still matches and binds correctly.
pub fn field_order_swapped () -> Int =
    let e = Logout { reason = 5, userId = 42 }
    match e
        Login { userId, .. }  -> userId
        Logout { userId, .. } -> userId
        Tick                  -> 0

-- Bind and combine two fields of a record variant (40 + 2 = 42).
pub fn sum_two_fields () -> Int =
    let e = Login { userId = 40, at = 2 }
    match e
        Login { userId, at }  -> userId + at
        Logout { userId, .. } -> userId
        Tick                  -> 0

-- Nullary variant alongside record variants.
pub fn nullary_arm () -> Int =
    match Tick
        Login { userId, .. }  -> userId
        Logout { userId, .. } -> userId
        Tick                  -> 123

-- Generic record-payload variant: the field type `a` is Int at this use.
pub fn generic_unwrap () -> Int =
    let b = Wrap { val = 99 }
    match b
        Wrap { val } -> val

-- Derived Encode/Decode/Eq round-trip through JSON on a record-style variant.
-- Encodes `Ping { seq = 55, ttl = 3 }`, parses it back, and confirms the decoded
-- value equals the original (returns `seq` on success, a negative code on any
-- failure). Exercises the record-variant pattern (encode) and construction (decode).
fn roundtrip (m: Msg) -> Result Msg Text =
    match Json.decode (Json.encode (encode m))
        Ok j  -> decode j
        Err _ -> Err "parse"

pub fn derive_roundtrip () -> Int =
    let m = Ping { seq = 55, ttl = 3 }
    match roundtrip m
        Ok m2 -> if m2 == m then 55 else -1
        Err _ -> -2
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"record-variant-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

// ── Test ──────────────────────────────────────────────────────────────────────

#[test]
fn record_variant_types_compute_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping record_variant_types_compute_correct_values");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-record-variant-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-record-variant-e2e-cache-")
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
         lists:foreach(F,['construct_and_match','field_order_swapped',\
         'sum_two_fields','nullary_arm','generic_unwrap','derive_roundtrip']), halt()."
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
        ("construct_and_match", 7),
        ("field_order_swapped", 42),
        ("sum_two_fields", 42),
        ("nullary_arm", 123),
        ("generic_unwrap", 99),
        ("derive_roundtrip", 55),
    ] {
        let needle = format!("{name}={want}");
        assert!(
            stdout.contains(&needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
