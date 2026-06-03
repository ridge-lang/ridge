//! End-to-end BEAM test for the clean public API: bare prelude-method calls
//! without any inline `class` redeclaration.
//!
//! Before the `seed_prelude` fix, bare calls to `encode`/`decode`/`toText`/
//! `eq`/`compare` failed with R010 (unresolved) unless the user redeclared
//! the class inline.  This test asserts the canonical pattern the public docs
//! show — no class declaration, just `deriving` and bare method calls — works
//! end-to-end on the BEAM.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source constants ──────────────────────────────────────────────────────────

/// The canonical user-facing pattern: `deriving (Encode, Decode)` and bare
/// `encode`/`decode` calls, with NO inline `class` redeclaration.
///
/// This is the exact pattern the public docs show.  It must compile and run
/// correctly on the BEAM.
const SOURCE_CLEAN_API: &str = r#"
type Person = { name: Text, age: Int } deriving (Encode, Decode)

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

fn fromJson (s: Text) -> Result Person Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

pub fn main_to_json () -> Text =
    toJson (Person { name = "Ann", age = 30 })

pub fn main_roundtrip () -> Text =
    match fromJson (toJson (Person { name = "Ann", age = 30 }))
        Ok p -> p.name
        Err e -> e.code
"#;

// ── Workspace helpers ─────────────────────────────────────────────────────────

fn write_workspace_source(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"prelude-method-clean-api-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

fn compile_and_find_module(
    source: &str,
) -> Option<(
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    String,
)> {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        return None;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-prelude-clean-api-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-prelude-clean-api-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace_source(dir.path(), source);

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

    Some((dir, cache, beam_dir, module))
}

fn run_erl(beam_dir: &std::path::Path, expr: &str) -> (String, String) {
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(expr)
        .output()
        .expect("run erl");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// `toJson (Person {...})` produces the expected JSON fields.
///
/// Uses NO inline `class Encode` declaration — the fix seeds the prelude
/// class index automatically, so bare `encode p` resolves to
/// `Binding::ClassMethod { class_name: "Encode", method: "encode" }`.
#[test]
fn clean_api_encode_no_class_decl() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_CLEAN_API) else {
        eprintln!("erl/erlc not on PATH — skipping clean_api_encode_no_class_decl");
        return;
    };

    let expr = format!("io:format(\"tojson=~s~n\",[{module}:main_to_json()]), halt().");
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("tojson=")
            && stdout.contains("\"name\"")
            && stdout.contains("\"Ann\"")
            && stdout.contains("\"age\"")
            && stdout.contains("30"),
        "clean-API encode must produce name+age JSON fields\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Round-trip: `fromJson (toJson p)` returns `Ok` of the original.
///
/// Uses NO inline `class Decode` or `class Encode` declaration.
#[test]
fn clean_api_roundtrip_no_class_decl() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_CLEAN_API) else {
        eprintln!("erl/erlc not on PATH — skipping clean_api_roundtrip_no_class_decl");
        return;
    };

    let expr = format!("io:format(\"rt=~s~n\",[{module}:main_roundtrip()]), halt().");
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("rt=Ann"),
        "clean-API round-trip must recover 'Ann' as name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
