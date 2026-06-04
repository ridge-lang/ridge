//! End-to-end value checks for typeclass dictionary passing through the full pipeline.
//!
//! Exercises the complete chain for class/instance/constrained-fn:
//! parse → collect (class/instance registry) → typecheck (constraint solving +
//! dict resolution) → lower (dict params + instance dict consts + call-site
//! threading) → Core Erlang → run on the BEAM → assert runtime values.
//!
//! Covers both a static call site (concrete type known at compile time, dict
//! literal threaded directly) and a polymorphic-forwarding call site (a
//! constrained function calling another constrained function, forwarding its
//! own dict parameter).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// Source exercising the full typeclass dictionary-passing path.
///
/// - `class Show a` declares a single-method class (desugared to `ToText` at
///   parse time). `Show` is the alias; the class registry stores `ToText`.
/// - `colorToText` is an ordinary fn used as the instance method body.
/// - `instance Show Color` provides the concrete dictionary.
/// - `fn describe` is constrained: takes a leading dict param at the IR level
///   and dispatches `toText` through the dictionary via string interpolation.
/// - `fn announce` calls `describe`, forwarding its own dict param.
/// - `fn main_static` calls `describe Red` — static path, peephole fires.
/// - `fn main_forward` calls `announce Green` — `DictPlan::Forward` path.
const SOURCE: &str = r#"
class Show a =
    toText (x: a) -> Text

type Color = Red | Green | Blue

fn colorToText (c: Color) -> Text =
    match c
        Red   -> "red"
        Green -> "green"
        Blue  -> "blue"

instance Show Color =
    toText (c: Color) -> Text = colorToText c

fn describe (x: a) -> Text where Show a =
    $"color:${x}"

fn announce (x: a) -> Text where Show a =
    describe x

pub fn main_static () -> Text =
    describe Red

pub fn main_forward () -> Text =
    announce Green
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"typeclass-dict-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn typeclass_dict_passing_computes_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping typeclass_dict_passing_computes_correct_values");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-typeclass-dict-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-typeclass-dict-e2e-cache-")
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

    // Drive both cases in one BEAM boot; each prints `name=value`.
    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['main_static','main_forward']), halt()."
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

    // Static call site: `describe Red` — dict is the instance literal, the
    // static peephole folds the `maps:get` lookup into a direct fn call.
    assert!(
        stdout.contains("main_static=color:red"),
        "expected `main_static=color:red`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Polymorphic-forwarding: `announce Green` calls `describe Green`,
    // forwarding its own incoming dict param (`DictPlan::Forward`).
    assert!(
        stdout.contains("main_forward=color:green"),
        "expected `main_forward=color:green`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

// ── Method invocation by bare name ────────────────────────────────────────────

/// Source exercising class method invocation by bare name — the front door
/// that was missing before this feature was added. `describe Red` calls the
/// class method directly without going through string interpolation.
///
/// - Static call site: `direct_call` calls `describe Red` — the resolver
///   stamps `Binding::ClassMethod`, the env scheme seeds the constraint, the
///   solver produces `DictPlan::Static`, and the lowering emits a field
///   projection `maps:get('describe', $inst_Describe_Color)`.
/// - Implicit constraint: `forward_call` calls `announce Red` where `announce`
///   calls `describe` WITHOUT an explicit `where` clause. The constraint is
///   acquired implicitly and retained on `announce`'s scheme.
const METHOD_CALL_SOURCE: &str = r#"
class Describe a =
    describe (x: a) -> Text

type Color = Red | Green | Blue

fn colorDesc (c: Color) -> Text =
    match c
        Red   -> "red"
        Green -> "green"
        Blue  -> "blue"

instance Describe Color =
    describe (x: Color) -> Text = colorDesc x

pub fn direct_call () -> Text =
    describe Red

fn announce (x: a) -> Text =
    describe x

pub fn forward_call () -> Text =
    announce Green
"#;

/// Two DISTINCT instances of one user class, each invoked by bare method name.
/// `describe Red` must select `Describe Color` and `describe Square` must select
/// `Describe Shape`. Before per-call-site pinning, the lowering saw two `Static`
/// plans for the same class, judged them ambiguous, and fell back to a unit
/// placeholder — the dictionary projected as the atom `ok` and `maps:get`
/// crashed at runtime.
const METHOD_CALL_MULTI_SOURCE: &str = r#"
class Describe a =
    describe (x: a) -> Text

type Color = Red | Green
type Shape = Circle | Square

instance Describe Color =
    describe (c: Color) -> Text =
        match c
            Red   -> "red"
            Green -> "green"

instance Describe Shape =
    describe (s: Shape) -> Text =
        match s
            Circle -> "circle"
            Square -> "square"

pub fn call_color () -> Text =
    describe Red

pub fn call_shape () -> Text =
    describe Square
"#;

fn write_method_call_workspace(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"method-call-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = []\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

#[test]
fn method_invocation_by_name_computes_correct_values() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping method_invocation_by_name_computes_correct_values"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-method-call-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-method-call-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_method_call_workspace(dir.path(), METHOD_CALL_SOURCE);

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

    // Compilation must succeed with no fatal diagnostics.
    let fatal_count = artefacts
        .diagnostics
        .iter()
        .filter(|d| format!("{d:?}").contains("Error"))
        .count();
    assert_eq!(
        fatal_count, 0,
        "expected no fatal diagnostics; got: {:?}",
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

    // Run both exported fns in a single BEAM boot.
    let expr = format!(
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['direct_call','forward_call']), halt()."
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

    // Static path: `describe Red` resolves the class method by bare name,
    // the dict is the instance literal, peephole folds to a direct call.
    assert!(
        stdout.contains("direct_call=red"),
        "expected `direct_call=red`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Implicit constraint: `announce Green` calls `describe` without an explicit
    // `where` clause; the constraint is acquired implicitly.
    assert!(
        stdout.contains("forward_call=green"),
        "expected `forward_call=green`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn method_invocation_distinct_instances_dispatch_by_argument() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!(
            "erl/erlc not on PATH — skipping method_invocation_distinct_instances_dispatch_by_argument"
        );
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-method-call-multi-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-method-call-multi-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_method_call_workspace(dir.path(), METHOD_CALL_MULTI_SOURCE);

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

    let fatal_count = artefacts
        .diagnostics
        .iter()
        .filter(|d| format!("{d:?}").contains("Error"))
        .count();
    assert_eq!(
        fatal_count, 0,
        "expected no fatal diagnostics; got: {:?}",
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
        "F=fun(N)->io:format(\"~s=~s~n\",[N,{module}:N()])end, \
         lists:foreach(F,['call_color','call_shape']), halt()."
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

    // Each bare `describe` call must pin its own instance from the argument
    // type: `describe Red` → `Describe Color`, `describe Square` → `Describe
    // Shape`. A regression to the sole-static-plan fallback would crash both
    // with `{badmap,ok}` (two same-class plans judged ambiguous).
    assert!(
        stdout.contains("call_color=red"),
        "expected `call_color=red`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("call_shape=square"),
        "expected `call_shape=square`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
