//! End-to-end check that string interpolation renders a `Decimal` and a `Uuid`.
//!
//! A `${hole}` whose type has a `ToText` instance is wrapped in that instance's
//! `toText` at lowering time. The closed built-in set already covered
//! `Int`/`Float`/`Bool`/`Text`/`Timestamp`; `Decimal` and `Uuid` were the parity
//! gap — both have a canonical stdlib `toText`, yet a hole of either type was
//! rejected (`Decimal`) or unrenderable. This proves the two now render:
//! - a `Decimal` hole prints every digit, keeping the scale a `Float` would drift
//!   (`0.10` stays `0.10`, not `0.1`), through `std.decimal.toText`,
//! - a `Uuid` hole prints the canonical hyphenated form through `std.uuid.toText`,
//! - the two mix freely with plain `Int` holes in one string, and
//! - a monomorphic hole of a `deriving (ToText)` type (and of a type with an
//!   explicit `instance ToText`) renders through the type's dictionary — the
//!   path that used to crash, since those instances emit a private method rather
//!   than a bare module `toText`. A derived record's `Decimal` field rides the
//!   same per-field map into `std.decimal.toText`.
//!
//! `Bytes` is deliberately absent: it has no single canonical text form (`toHex`
//! and `toUtf8` disagree), so it has no `ToText` instance and a `${bytes}` hole is
//! still a type error — the correct outcome, covered by the type-check suite.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r##"
-- Parse or fall back, so the helpers are total. `Decimal` and `Uuid` resolve as
-- qualified module names without an explicit import (their companion modules are
-- always in scope, like `Int` or `Text`).
fn dec (s: Text) -> Decimal =
    match Decimal.fromText s
        Ok d  -> d
        Err _ -> Decimal.fromInt 0

fn uid (s: Text) -> Uuid =
    match Uuid.fromText s
        Ok u  -> u
        Err _ -> Uuid.nil ()

-- A bare Decimal hole renders through std.decimal.toText, keeping the scale.
pub fn priceLine () -> Text =
    let p = dec "19.99"
    $"price: ${p}"

-- The scale a naive float render would drop: 0.10 stays 0.10, not 0.1.
pub fn exactLine () -> Text =
    let p = dec "0.10"
    $"sum: ${p}"

-- A Uuid hole renders the canonical hyphenated form through std.uuid.toText.
pub fn tokenLine () -> Text =
    let u = uid "11111111-1111-1111-1111-111111111111"
    $"token: ${u}"

-- Int, Decimal and Uuid holes side by side in one interpolated string.
pub fn mixed () -> Text =
    let p = dec "3.50"
    let u = uid "22222222-2222-2222-2222-222222222222"
    $"n=${42} p=${p} u=${u}"

-- A record whose derived ToText must dispatch its Decimal field through the same
-- std.decimal.toText, so the rendered string carries the exact amount. This hole
-- is monomorphic, so it dispatches on the type's `$inst_ToText_Money` dictionary
-- — the path that used to crash because a derived instance's method function is
-- private, not a bare module `toText`.
pub type Money = { amount: Decimal, tag: Text } deriving (ToText)

pub fn moneyLine () -> Text =
    let m = Money { amount = dec "5.00", tag = "usd" }
    $"m=${m}"

-- A hand-written explicit instance, also interpolated as a monomorphic hole. Its
-- method function is private the same way a derived one is, so it rides the same
-- dictionary dispatch.
pub type Tag = { label: Text }

instance ToText Tag =
    toText (t: Tag) -> Text = Text.concat "#" t.label

pub fn tagLine () -> Text =
    let t = Tag { label = "vip" }
    $"tag=${t}"
"##;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"interp-rich-scalars-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn interpolation_renders_decimal_and_uuid_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping interpolation_renders_decimal_and_uuid_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-interp-rich-scalars-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-interp-rich-scalars-e2e-cache-")
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
        "io:format(\"priceLine=~s~n\",[{module}:priceLine()]), \
         io:format(\"exactLine=~s~n\",[{module}:exactLine()]), \
         io:format(\"tokenLine=~s~n\",[{module}:tokenLine()]), \
         io:format(\"mixed=~s~n\",[{module}:mixed()]), \
         io:format(\"moneyLine=~s~n\",[{module}:moneyLine()]), \
         io:format(\"tagLine=~s~n\",[{module}:tagLine()]), \
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
            "priceLine=price: 19.99",
            "a Decimal hole renders through std.decimal.toText",
        ),
        (
            "exactLine=sum: 0.10",
            "the Decimal render keeps the scale a float would drop",
        ),
        (
            "tokenLine=token: 11111111-1111-1111-1111-111111111111",
            "a Uuid hole renders the canonical form through std.uuid.toText",
        ),
        (
            "mixed=n=42 p=3.50 u=22222222-2222-2222-2222-222222222222",
            "Int, Decimal and Uuid holes mix in one interpolated string",
        ),
        (
            "moneyLine=m=Money { amount = 5.00, tag = usd }",
            "a monomorphic hole of a derived-ToText record renders through its dictionary, Decimal field and all",
        ),
        (
            "tagLine=tag=#vip",
            "a monomorphic hole of a type with an explicit ToText instance renders through its dictionary",
        ),
    ] {
        assert!(
            stdout.contains(probe),
            "missing `{probe}` ({why})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
