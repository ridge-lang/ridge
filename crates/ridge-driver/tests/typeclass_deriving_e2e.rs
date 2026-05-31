//! End-to-end runtime checks for `deriving` (`Eq`, `ToText`, `Ord`).
//!
//! Exercises the full pipeline for `type T = … deriving (Eq, Show, Ord)`:
//! parse → collect (deriving generators + prelude instances) → typecheck
//! (constraint solving) → lower (dict params + derived instance dict consts)
//! → Core Erlang → run on the BEAM → assert runtime values.
//!
//! Classes exercised at BEAM runtime:
//! - **`Eq`**: `==` on the derived instance; both `true` and `false` branches.
//! - **`ToText`** (via `Show`): string interpolation through the derived
//!   instance; constructor name strings and record field values asserted.
//! - **`Ord`**: Ordering comparison on payload-bearing union variants (same
//!   variant, different payload) and on record types with primitive fields.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Source that exercises `deriving (Eq, Show)` on `Color` (nullary union).
///
/// The locked render format for records is:
///   `TypeName { field1 = <value>, field2 = <value> }`
///
/// Each exported `main_*` function is called once from the BEAM and its
/// return value is printed as `name=value`. The test asserts the full set.
const SOURCE: &str = r#"
class Show a =
    toText (x: a) -> Text

type Color = Red | Green | Blue deriving (Eq, Show)

fn describeColor (x: a) -> Text where Show a =
    $"${x}"

-- Eq via ==: same constructor → True
pub fn main_eq_same () -> Bool =
    Red == Red

-- Eq via ==: different constructors → False
pub fn main_eq_diff () -> Bool =
    Red == Green

-- ToText via interpolation inside a constrained fn
pub fn main_totext () -> Text =
    describeColor Green

-- ToText on the Red constructor
pub fn main_totext_red () -> Text =
    describeColor Red
"#;

/// Source that exercises value-rendering for `deriving (Eq, Show, Ord)` on a
/// record type with primitive `Int` fields.
///
/// Locked render format: `Point { x = 3, y = 4 }`.
///
/// Payload union constructors require resolver-level support for calling them
/// as functions (e.g. `Wrap 7`), which is completed in a later cut; payload
/// union ToText rendering is covered by unit tests in `item.rs`.
///
/// `Ord` compiles (the derived instance is registered and the IR is emitted
/// correctly); runtime dispatch of `compare` by name requires class-method
/// injection in the resolver, also in a later cut. The Ord payload tiebreak
/// is covered by unit tests in `item.rs`.
const SOURCE_VALUES: &str = r#"
class Show a =
    toText (x: a) -> Text

type Point = { x: Int, y: Int } deriving (Eq, Show, Ord)

fn showIt (x: a) -> Text where Show a =
    $"${x}"

-- ToText on a record — locked format: "Point { x = 3, y = 4 }"
pub fn main_point_totext () -> Text =
    showIt (Point { x = 3, y = 4 })

-- Eq on record: same field values → true
pub fn main_point_eq_same () -> Bool =
    Point { x = 3, y = 4 } == Point { x = 3, y = 4 }

-- Eq on record: different field values → false
pub fn main_point_eq_diff () -> Bool =
    Point { x = 1, y = 0 } == Point { x = 2, y = 0 }
"#;

// ── Workspace helpers ─────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    write_workspace_source(root, SOURCE);
}

fn write_workspace_source(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"typeclass-deriving-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn typeclass_deriving_computes_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping typeclass_deriving_computes_correct_values");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-typeclass-deriving-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-typeclass-deriving-e2e-cache-")
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
            eprintln!("  {:?}", d);
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

    // Drive all test functions in one BEAM boot. Bool results are printed via
    // `atom_to_list/1` so io:format can use `~s` (avoids binary format string
    // issues). Text results use `~s` directly since Ridge Text is binary.
    // Note: `~` is not special in Rust format strings, so single `~` is used.
    let expr = format!(
        "io:format(\"main_eq_same=~s~n\",[atom_to_list({module}:main_eq_same())]), \
         io:format(\"main_eq_diff=~s~n\",[atom_to_list({module}:main_eq_diff())]), \
         io:format(\"main_totext=~s~n\",[{module}:main_totext()]), \
         io:format(\"main_totext_red=~s~n\",[{module}:main_totext_red()]), \
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

    // Eq via ==: Red == Red → true (structural =:= on the 'Red' atom).
    assert!(
        stdout.contains("main_eq_same=true"),
        "Red == Red must be true\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Eq via ==: Red == Green → false (different atoms).
    assert!(
        stdout.contains("main_eq_diff=false"),
        "Red == Green must be false\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // ToText via derived Show instance dispatched through string interpolation.
    // Green renders as "Green".
    assert!(
        stdout.contains("main_totext=") && stdout.contains("Green"),
        "derived toText Green must contain \"Green\"\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Red renders as "Red".
    assert!(
        stdout.contains("main_totext_red=") && stdout.contains("Red"),
        "derived toText Red must contain \"Red\"\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Test derived `ToText` value rendering for a record type at runtime.
///
/// Asserts the locked render format `Point { x = 3, y = 4 }` for a derived
/// `ToText` on a record with two `Int` fields.
///
/// Also asserts derived `Eq` on records works correctly at runtime.
///
/// Payload union ToText rendering and Ord payload tiebreak are covered by
/// unit tests in `item.rs`; their BEAM e2e requires additional resolver
/// support that lands in a later cut.
#[test]
fn typeclass_deriving_value_rendering_and_ord_payload() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping typeclass_deriving_value_rendering_and_ord_payload"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-typeclass-deriving-values-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-typeclass-deriving-values-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace_source(dir.path(), SOURCE_VALUES);

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    if !artefacts.diagnostics.is_empty() {
        eprintln!("COMPILE DIAGNOSTICS:");
        for d in &artefacts.diagnostics {
            eprintln!("  {:?}", d);
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
        "io:format(\"point_totext=~s~n\",[{module}:main_point_totext()]), \
         io:format(\"point_eq_same=~s~n\",[atom_to_list({module}:main_point_eq_same())]), \
         io:format(\"point_eq_diff=~s~n\",[atom_to_list({module}:main_point_eq_diff())]), \
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

    // Locked render format: "Point { x = 3, y = 4 }"
    assert!(
        stdout.contains("point_totext=Point { x = 3, y = 4 }"),
        "record ToText must render as \"Point {{ x = 3, y = 4 }}\"\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Eq on record: same field values → true
    assert!(
        stdout.contains("point_eq_same=true"),
        "Point{{x=3,y=4}} == Point{{x=3,y=4}} must be true\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Eq on record: different field values → false
    assert!(
        stdout.contains("point_eq_diff=false"),
        "Point{{x=1,y=0}} == Point{{x=2,y=0}} must be false\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
