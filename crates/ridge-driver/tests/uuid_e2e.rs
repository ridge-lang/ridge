//! End-to-end check for the `Uuid` primitive and the `std.uuid` module on a real
//! BEAM.
//!
//! Uuid is carried as its canonical lowercase text (`{uuid, Bin}`), so this proves
//! the whole loop through codegen and the runtime:
//! - a canonical string parses and renders back unchanged,
//! - an upper-case string is normalised to lowercase, so equality is by value,
//! - the nil uuid renders as the all-zero form,
//! - `gen` mints a well-formed, distinct value from the `random` capability,
//! - ordering compares by the 128-bit value, and
//! - a malformed string is a recoverable `Err`, not a crash.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
-- `Uuid` is a prelude primitive and `std.uuid` is aliased as `Uuid` with no
-- import, the same as `Int`/`Float`/`Decimal`.

-- Parse or fall back to the nil uuid, so each probe is a total function.
fn uu (s: Text) -> Uuid =
    match Uuid.fromText s
        Ok u  -> u
        Err _ -> Uuid.nil ()

-- parse then render: a canonical string comes back unchanged.
pub fn roundTrip () -> Text = Uuid.toText (uu "550e8400-e29b-41d4-a716-446655440000")

-- an upper-case string is normalised to canonical lowercase.
pub fn normalizeCase () -> Text = Uuid.toText (uu "550E8400-E29B-41D4-A716-446655440000")

-- the nil uuid renders as the all-zero form.
pub fn nilText () -> Text = Uuid.toText (Uuid.nil ())

-- equality is by value, so the same uuid in either case is equal.
pub fn eqCase () -> Text =
    if Uuid.eq (uu "550E8400-E29B-41D4-A716-446655440000") (uu "550e8400-e29b-41d4-a716-446655440000")
    then "eq" else "ne"

-- ordering compares by the 128-bit value: an all-zero uuid is below an all-f one.
pub fn lessThan () -> Text =
    if Uuid.lt (uu "00000000-0000-0000-0000-000000000000") (uu "ffffffff-ffff-ffff-ffff-ffffffffffff")
    then "lt" else "notlt"

-- a malformed string is a recoverable Err, not a runtime failure.
pub fn badParse () -> Text =
    match Uuid.fromText "not-a-uuid"
        Ok _  -> "ok"
        Err _ -> "err"

-- a string of the right length but with a non-hex digit is rejected.
pub fn badHex () -> Text =
    match Uuid.fromText "550e8400-e29b-41d4-a716-44665544000z"
        Ok _  -> "ok"
        Err _ -> "err"

-- a generated uuid is well-formed: rendering it and parsing it back succeeds.
pub fn random genValid () -> Text =
    match Uuid.fromText (Uuid.toText (Uuid.gen ()))
        Ok _  -> "valid"
        Err _ -> "invalid"

-- two generated uuids differ (a v4 collision is astronomically unlikely).
pub fn random genDistinct () -> Text =
    if Uuid.eq (Uuid.gen ()) (Uuid.gen ()) then "same" else "distinct"
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"uuid-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn uuid_module_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping uuid_module_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-uuid-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-uuid-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    if !artefacts.diagnostics.is_empty() {
        eprintln!("COMPILE DIAGNOSTICS:");
        for d in &artefacts.diagnostics {
            eprintln!("  {d:?}");
        }
    }
    assert!(
        artefacts.diagnostics.is_empty(),
        "no compile errors expected; got {:?}",
        artefacts.diagnostics
    );

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
        "io:format(\"roundTrip=~s~n\",[{module}:roundTrip()]), \
         io:format(\"normalizeCase=~s~n\",[{module}:normalizeCase()]), \
         io:format(\"nilText=~s~n\",[{module}:nilText()]), \
         io:format(\"eqCase=~s~n\",[{module}:eqCase()]), \
         io:format(\"lessThan=~s~n\",[{module}:lessThan()]), \
         io:format(\"badParse=~s~n\",[{module}:badParse()]), \
         io:format(\"badHex=~s~n\",[{module}:badHex()]), \
         io:format(\"genValid=~s~n\",[{module}:genValid()]), \
         io:format(\"genDistinct=~s~n\",[{module}:genDistinct()]), \
         halt()."
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

    for (probe, why) in [
        (
            "roundTrip=550e8400-e29b-41d4-a716-446655440000",
            "a canonical string round-trips unchanged",
        ),
        (
            "normalizeCase=550e8400-e29b-41d4-a716-446655440000",
            "an upper-case string is normalised to lowercase",
        ),
        (
            "nilText=00000000-0000-0000-0000-000000000000",
            "the nil uuid renders as the all-zero form",
        ),
        ("eqCase=eq", "equality is by value across letter case"),
        ("lessThan=lt", "ordering compares by the 128-bit value"),
        ("badParse=err", "a malformed string is a recoverable Err"),
        ("badHex=err", "a non-hex digit is rejected"),
        ("genValid=valid", "a generated uuid is well-formed"),
        ("genDistinct=distinct", "two generated uuids differ"),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
