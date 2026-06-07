//! End-to-end round-trip for transparent newtype-deriving.
//!
//! An `opaque` single-field wrapper that derives a class forwards every method
//! to its inner type's instance, so the wrapper is indistinguishable from its
//! payload at runtime. This test proves the `SqlType` codec survives a full
//! `fromSql (toSql …)` round-trip through such wrappers on the real BEAM, for an
//! `Int`-backed and a `Text`-backed newtype.
//!
//! `Money` additionally derives the full set (`Encode`, `Decode`, `ToText`,
//! `Eq`, `Ord`) so that every delegated dictionary is lowered, codegen'd, and
//! loaded — exercising the whole module, not just the methods called below.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// `Money` wraps `Int`; `Email` wraps `Text`. Both derive `SqlType` and get the
/// codec for free by delegating to the inner type's instance. `fromMoney` /
/// `fromEmail` pin the polymorphic `fromSql` result to the wrapper type via a
/// return annotation. Field access (`m.cents`) is in-module, so opacity allows it.
const SOURCE: &str = r#"
import std.sql (toSql, fromSql, SqlValue)

opaque type Money = { cents: Int } deriving (SqlType, Encode, Decode, ToText, Eq, Ord)

opaque type Email = { raw: Text } deriving (SqlType)

fn fromMoney (v: SqlValue) -> Result Money Error = fromSql v

fn fromEmail (v: SqlValue) -> Result Email Error = fromSql v

pub fn roundTripMoney (n: Int) -> Int =
    match fromMoney (toSql (Money { cents = n }))
        Ok m  -> m.cents
        Err _ -> 0 - 1

pub fn roundTripEmail (s: Text) -> Text =
    match fromEmail (toSql (Email { raw = s }))
        Ok e  -> e.raw
        Err _ -> "error"
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"newtype-deriving-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn newtype_sqltype_roundtrip_survives_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping newtype_sqltype_roundtrip_survives_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-newtype-deriving-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-newtype-deriving-e2e-cache-")
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
        "io:format(\"money=~w~n\",[{module}:roundTripMoney(1000)]), \
         io:format(\"email=~s~n\",[{module}:roundTripEmail(<<\"a@b.io\">>)]), \
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

    assert!(
        stdout.contains("money=1000"),
        "expected `money=1000` — SqlType round-trip through the Int newtype failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("email=a@b.io"),
        "expected `email=a@b.io` — SqlType round-trip through the Text newtype failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
