//! End-to-end check for the `Decimal` primitive and the `std.decimal` module on
//! a real BEAM.
//!
//! Decimal is carried as a scaled integer (`{decimal, Unscaled, Scale}`), so this
//! proves the whole loop through codegen and the runtime:
//! - text parses and renders back to the same string, with the scale preserved
//!   (`1.50` stays `1.50`, not `1.5`),
//! - exponent and signed/fractional forms parse to the right value,
//! - a value far beyond `Int`/`Float` range round-trips exactly (arbitrary
//!   precision), which is the reason the type exists,
//! - `compare` aligns scales, so `1.5` and `1.50` compare equal, and
//! - a malformed literal is a recoverable `Err`, not a crash.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
-- `Decimal` is a prelude primitive and `std.decimal` is aliased as `Decimal`
-- with no import, the same as `Int`/`Float`.

-- Parse or fall back to zero, so each probe is a total function returning text.
fn dec (s: Text) -> Decimal =
    match Decimal.fromText s
        Ok d  -> d
        Err _ -> Decimal.fromInt 0

-- parse then render: the canonical text matches the input.
pub fn roundTrip () -> Text = Decimal.toText (dec "19.99")

-- the scale survives the round-trip: a trailing zero is kept.
pub fn preserveScale () -> Text = Decimal.toText (dec "1.50")

-- exponent notation resolves to its plain decimal value.
pub fn exponent () -> Text = Decimal.toText (dec "1.5e3")

-- a negative value with a leading-zero fraction renders back exactly.
pub fn negFrac () -> Text = Decimal.toText (dec "-0.05")

-- an integer becomes a scale-0 decimal.
pub fn fromIntText () -> Text = Decimal.toText (Decimal.fromInt 42)

-- arbitrary precision: a value well past Int64 and past what a Float can hold
-- exactly round-trips digit for digit. This is why Decimal is not just a Float.
pub fn bigExact () -> Text = Decimal.toText (dec "123456789012345678901234567890.123456789")

-- compare aligns scales: 1.5 and 1.50 are the same number, so compare is 0.
pub fn cmpEqualScales () -> Text = Int.toText (Decimal.compare (dec "1.50") (dec "1.5"))

-- the value comparisons agree: 1.5 == 1.50 and 1.5 < 2.0.
pub fn eqScales () -> Text = if Decimal.eq (dec "1.5") (dec "1.50") then "eq" else "ne"
pub fn lessThan () -> Text = if Decimal.lt (dec "1.5") (dec "2.0") then "lt" else "notlt"

-- a malformed literal is a recoverable Err, not a runtime failure.
pub fn badParse () -> Text =
    match Decimal.fromText "not a number"
        Ok _  -> "ok"
        Err _ -> "err"

-- toFloat narrows for display; 0.5 is representable so the value is exact here.
pub fn toFloatText () -> Text = Float.toText (Decimal.toFloat (dec "0.5"))

-- exact addition: 0.1 + 0.2 is exactly 0.3, where binary floats give
-- 0.30000000000000004. This is the headline reason to reach for Decimal.
pub fn addExact () -> Text = Decimal.toText (Decimal.add (dec "0.1") (dec "0.2"))

-- exact subtraction.
pub fn subExact () -> Text = Decimal.toText (Decimal.sub (dec "0.30") (dec "0.10"))

-- multiplication is exact and its scale is the sum of the operand scales:
-- 1.10 (scale 2) * 1.10 (scale 2) = 1.2100 (scale 4).
pub fn mulExact () -> Text = Decimal.toText (Decimal.mul (dec "1.10") (dec "1.10"))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"decimal-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn decimal_module_runs_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping decimal_module_runs_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-decimal-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-decimal-e2e-cache-")
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
         io:format(\"preserveScale=~s~n\",[{module}:preserveScale()]), \
         io:format(\"exponent=~s~n\",[{module}:exponent()]), \
         io:format(\"negFrac=~s~n\",[{module}:negFrac()]), \
         io:format(\"fromIntText=~s~n\",[{module}:fromIntText()]), \
         io:format(\"bigExact=~s~n\",[{module}:bigExact()]), \
         io:format(\"cmpEqualScales=~s~n\",[{module}:cmpEqualScales()]), \
         io:format(\"eqScales=~s~n\",[{module}:eqScales()]), \
         io:format(\"lessThan=~s~n\",[{module}:lessThan()]), \
         io:format(\"badParse=~s~n\",[{module}:badParse()]), \
         io:format(\"toFloatText=~s~n\",[{module}:toFloatText()]), \
         io:format(\"addExact=~s~n\",[{module}:addExact()]), \
         io:format(\"subExact=~s~n\",[{module}:subExact()]), \
         io:format(\"mulExact=~s~n\",[{module}:mulExact()]), \
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
            "roundTrip=19.99",
            "text round-trips through the scaled-integer value",
        ),
        (
            "preserveScale=1.50",
            "the scale is preserved, so a trailing zero is kept",
        ),
        (
            "exponent=1500",
            "exponent notation parses to its plain value",
        ),
        (
            "negFrac=-0.05",
            "a signed leading-zero fraction renders exactly",
        ),
        ("fromIntText=42", "an integer becomes a scale-0 decimal"),
        (
            "bigExact=123456789012345678901234567890.123456789",
            "a value beyond Int/Float precision round-trips exactly",
        ),
        (
            "cmpEqualScales=0",
            "compare aligns scales, so 1.50 and 1.5 are equal",
        ),
        ("eqScales=eq", "value equality holds across scales"),
        ("lessThan=lt", "the ordering comparisons work"),
        ("badParse=err", "a malformed literal is a recoverable Err"),
        ("toFloatText=0.5", "toFloat narrows to a Float for display"),
        (
            "addExact=0.3",
            "0.1 + 0.2 is exactly 0.3, not the float 0.30000000000000004",
        ),
        ("subExact=0.20", "subtraction is exact and keeps scale"),
        (
            "mulExact=1.2100",
            "multiplication is exact and its scale is the sum of the operand scales",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
