//! Regression: interpolating a concretely-typed `Ordering` value renders its
//! name instead of splicing the raw atom.
//!
//! `Ordering` is a builtin with no stdlib module, so its `toText` cannot go
//! through the `std.<x>.toText` convention the other primitives use. Without a
//! dedicated path the raw `Less`/`Equal`/`Greater` atom reached
//! `iolist_to_binary` and crashed at runtime. It now renders through a private
//! `std.list` helper backed by the runtime `ordering_to_text`.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.
#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
-- `showOrd` receives its argument with a concrete `Ordering` type, so the hole
-- dispatches ToText by type. `compare` (over a derived `Ord`) is the only way
-- to obtain an Ordering value; passing it to a typed parameter pins it.
pub type Tag = { n: Int } deriving (Eq, Ord)

pub fn showOrd (o: Ordering) -> Text = $"ord=${o}"

pub fn showBool (b: Bool) -> Text = $"bool=${b}"

pub fn probe () -> Text =
    Text.concat
        (showOrd (compare (Tag { n = 1 }) (Tag { n = 2 })))
        (Text.concat "," (showBool (eq (Tag { n = 3 }) (Tag { n = 3 }))))
"#;

// Regression: a *bare, unannotated* prelude class-method result spliced
// into a string. `compare a b`/`eq a b` are not pinned by any surrounding
// annotation here, so before the fix their result reached the interpolation
// hole as an unresolved type variable (the identifier lowered to `Type::Error`
// and the call's result to a fresh var), the type-directed `ToText` dispatch
// never fired, and the raw `Less`/`false` atom was spliced into
// `iolist_to_binary` and crashed. Seeding `compare -> Ordering`, `eq -> Bool`,
// `toText -> Text` in the type env pins the result so the render path fires.
const SOURCE_BARE: &str = r#"
type Tag = { n: Int } deriving (Eq, Ord)

pub fn probe () -> Text =
    $"ord=${compare (Tag { n = 1 }) (Tag { n = 2 })},eq=${eq (Tag { n = 3 }) (Tag { n = 3 })}"
"#;

fn write_workspace(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"interp-prelude-totext-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

/// Compile `source` to BEAM and run its `probe/0`, returning `(stdout, stderr)`.
/// Returns `None` when `erl`/`erlc` are unavailable so the caller can skip.
fn compile_and_run_probe(source: &str) -> Option<(String, String)> {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        return None;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-interp-prelude-totext-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-interp-prelude-totext-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), source);

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    assert!(
        artefacts.diagnostics.is_empty(),
        "interpolating a prelude class-method result must compile cleanly; got {:?}",
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

    let expr = format!("io:format(\"probe=~s~n\",[{module}:probe()]), halt().");
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(&beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");

    Some((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

#[test]
fn interpolating_an_ordering_value_renders_its_name() {
    let Some((stdout, stderr)) = compile_and_run_probe(SOURCE) else {
        eprintln!("erl/erlc not on PATH — skipping");
        return;
    };

    // compare {n=1} {n=2} = Less; eq {n=3} {n=3} = true.
    assert!(
        stdout.contains("probe=ord=Less,bool=true"),
        "expected `probe=ord=Less,bool=true`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn interpolating_a_bare_prelude_method_result_renders() {
    let Some((stdout, stderr)) = compile_and_run_probe(SOURCE_BARE) else {
        eprintln!("erl/erlc not on PATH — skipping");
        return;
    };

    // compare {n=1} {n=2} = Less; eq {n=3} {n=3} = true. No annotation pins the
    // result — this only renders once compare/eq are seeded with concrete return
    // types (before the fix it crashed splicing the raw atom into iolist).
    assert!(
        stdout.contains("probe=ord=Less,eq=true"),
        "expected `probe=ord=Less,eq=true`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
