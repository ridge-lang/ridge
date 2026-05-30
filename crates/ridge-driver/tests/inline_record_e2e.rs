//! End-to-end value checks for inline record types through the full pipeline.
//!
//! Covers the inline-specific chain: parse → typecheck anonymous TyCon →
//! lower by shape → Core Erlang → run on the BEAM → assert runtime values.
//!
//! Each `pub fn` in the Ridge source returns an `Int` so the harness can
//! assert exact values.  All cases are driven from a single BEAM boot.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Each `pub fn` returns an `Int` so the harness can assert exact values.
///
/// All functions use inline record type annotations (no `type` alias).
/// The codegen path exercised: shape interning + id-agreement + lower-by-ShapeKey.
const SOURCE: &str = r#"
-- Case 1: construct an inline-typed record and read a field.
-- Return type annotation drives anon TyCon creation; literal { v = 10, n = 0 }
-- is checked against the shape.  Field access .v must return 10.
pub fn field_read () -> Int =
    let r: { v: Int, n: Int } = { v = 10, n = 0 }
    r.v

-- Case 2: pattern-match an inline record.
-- Destructure { v, n } and return v + n = 10 + 3 = 13.
pub fn pattern_match () -> Int =
    let r: { v: Int, n: Int } = { v = 10, n = 3 }
    match r
        { v, n } -> v + n

-- Case 3: with-update on an inline-typed record.
-- Start with { v = 10, n = 0 }, update v to v + 5 = 15, read .v.
pub fn with_update () -> Int =
    let r: { v: Int, n: Int } = { v = 10, n = 0 }
    let r2 = r with { v = r.v + 5 }
    r2.v

-- Case 4: empty inline record — prove {} lowers and runs.
-- A helper that accepts {} and returns a constant; proves the empty map
-- round-trips through the inline-record path without a BEAM crash.
-- {} is not a valid argument atom without parens, so we wrap it.
fn empty_helper (e: {}) -> Int = 42

pub fn empty_record () -> Int =
    empty_helper ({})

-- Case 5: order-insensitivity at runtime.
-- f returns a record as { b: Text, a: Int } (b first in type, a first in literal).
-- g returns the same shape written in the reverse field order in both type and literal.
-- Both must produce the same BEAM representation; reading .a from each must give
-- the same values, proving shape keys agree across declaration order differences.
fn make_ba () -> { b: Text, a: Int } = { a = 7, b = "x" }
fn make_ab () -> { a: Int, b: Text } = { b = "y", a = 9 }

pub fn order_insensitive () -> Int =
    let r1 = make_ba ()
    let r2 = make_ab ()
    r1.a + r2.a

-- Case 6: nested inline record — access .inner.id.
-- Outer shape: { inner: { id: Int } }; read through two field accesses.
pub fn nested_access () -> Int =
    let r: { inner: { id: Int } } = { inner = { id = 55 } }
    r.inner.id
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

/// Build a single-member workspace whose entry module holds `SOURCE`.
fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"inline-record-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn inline_record_types_compute_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping inline_record_types_compute_correct_values");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-inline-rec-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-inline-rec-e2e-cache-")
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

    // Drive every case in one BEAM boot; each prints `name=value`.
    let expr = format!(
        "F=fun(N)->io:format(\"~s=~p~n\",[N,{module}:N()])end, \
         lists:foreach(F,['field_read','pattern_match','with_update',\
         'empty_record','order_insensitive','nested_access']), halt()."
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
        ("field_read", 10),        // .v of { v = 10, n = 0 }
        ("pattern_match", 13),     // 10 + 3
        ("with_update", 15),       // 10 + 5
        ("empty_record", 42),      // empty_helper {} → 42
        ("order_insensitive", 16), // 7 + 9 — both shapes share same BEAM key
        ("nested_access", 55),     // .inner.id
    ] {
        let needle = format!("{name}={want}");
        assert!(
            stdout.contains(&needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}
