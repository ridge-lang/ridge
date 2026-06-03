//! End-to-end runtime checks for parametric typeclass instances.
//!
//! Exercises parametric instances (`instance Encode (List a) where Encode a`)
//! through the full pipeline: parse -> collect (`ctx_constraints` +
//! `head_var_positions`) -> typecheck (constraint solving + dict-of-dicts plans)
//! -> lower (fn-valued `$inst_` + call-site dictionary application) -> Core
//! Erlang -> run on the BEAM -> assert the JSON wire string.
//!
//! The dictionary-passing mechanism: a parametric instance
//! `instance Encode (List a) where Encode a` lowers `$inst_Encode_List` to a
//! *function* of the element dictionary. At a `List Color` call site the
//! lowering applies it to `$inst_Encode_Color`, building the concrete dictionary
//! at runtime; the method projects `encode` from the element dict per element.
//!
//! Each call site's dictionary is selected from the *full resolved argument
//! type* — the element type, not just the head constructor — so an `Option Int`
//! and an `Option Text` in the same module each dispatch to the correct element
//! encoder, and a `Result Int Text` threads two element dictionaries to the
//! right positions. A call whose element type is never pinned (a bare
//! `toJson None`) is reported as an ambiguity error rather than encoding the
//! wrong value; see `tests/parametric_ambiguous_e2e.rs`.
//!
//! The source redeclares `class Encode a` so (a) the class is user-local —
//! satisfying the orphan rule for any hand-written instance — and (b) the
//! resolver's `ClassMethodIndex` resolves bare `encode x` calls (the same trick
//! used in `encode_deriving_e2e.rs`).
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source ────────────────────────────────────────────────────────────────────

/// The prelude `Encode (List a)` instance over a hand-written element instance
/// `Encode Color`, encoding a `List Color` to a JSON array.
///
/// The list dictionary is the prelude-synthesised `Encode (List a)`; the call
/// site applies it to the user-module `$inst_Encode_Color`. `List`/`Option`/
/// `Map`/`Result` instances are prelude-reserved, so the test relies on the
/// prelude one and only writes the element instance.
///
/// `Color` is a nullary union, so list literals `[Red, Green]` need no payload
/// constructor calls (avoiding the payload-ctor-as-function caveat).
///
/// `[Red, Green, Blue]` must encode to `["red","green","blue"]`; an empty list
/// pinned to `List Color` -> `[]`.
const SOURCE_LIST_COLOR: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

type Color = Red | Green | Blue

instance Encode Color =
    encode (c: Color) -> JsonValue =
        match c
            Red   -> JText "red"
            Green -> JText "green"
            Blue  -> JText "blue"

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_list () -> Text =
    toJson [Red, Green, Blue]

pub fn main_list_empty () -> Text =
    let xs : List Color = []
    toJson xs
"#;

/// Nested `List (List Color)` to exercise dict-of-dicts: the prelude
/// `Encode (List a)` dictionary is applied to itself applied to `Encode Color`.
///
/// `[[Red, Green], [Blue]]` must encode to `[["red","green"],["blue"]]`.
const SOURCE_LIST_LIST_COLOR: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

type Color = Red | Green | Blue

instance Encode Color =
    encode (c: Color) -> JsonValue =
        match c
            Red   -> JText "red"
            Green -> JText "green"
            Blue  -> JText "blue"

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_nested () -> Text =
    toJson [[Red, Green], [Blue]]
"#;

/// Parametric `Encode (List a)` over a PRIMITIVE element (`Int`), exercising the
/// synthesised prelude dictionaries. `$inst_Encode_List` is applied to the
/// synthesised `Encode Int` dictionary at the call site.
///
/// `[1, 2, 3]` must encode to `[1,2,3]`; an empty list pinned to `List Int` ->
/// `[]`.
const SOURCE_LIST_INT: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_ints () -> Text =
    toJson [1, 2, 3]

pub fn main_ints_empty () -> Text =
    let xs : List Int = []
    toJson xs
"#;

/// Parametric `Encode (List a)` over `Text` — proves the synthesised `Encode Text`
/// primitive dictionary. `["a", "b"]` → `["a","b"]`.
const SOURCE_LIST_TEXT: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_texts () -> Text =
    toJson ["a", "b"]
"#;

/// A generic user type `type Box a = { val: a }` deriving both `Encode` and
/// `Decode`. The derived instances become constrained
/// (`instance Encode (Box a) where Encode a`), with `$inst_Encode_Box` a
/// function of the element dictionary. Over `Box Int` the full round-trip must
/// hold: `decode (encode (Box { val = 7 })) == Ok (Box { val = 7 })`.
const SOURCE_GENERIC_BOX: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

class Decode a =
    decode (j: JsonValue) -> Result a Error

type Box a = { val: a } deriving (Encode, Decode)

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

fn fromJson (s: Text) -> Result (Box Int) Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

pub fn main_encode () -> Text =
    toJson (Box { val = 7 })

pub fn main_roundtrip () -> Text =
    let b = Box { val = 7 }
    match fromJson (toJson b)
        Ok q ->
            match q.val
                7 -> "roundtrip_ok"
                _ -> "roundtrip_wrong_value"
        Err e -> e.code
"#;

/// Parametric `Option a` over a primitive element, applied DIRECTLY: `Some 5`
/// and `None` flow straight into `toJson` with no wrapper function pinning the
/// element type. The element type comes from the argument (`Option Int`), and
/// the decode round-trip recovers both. `Some 5` → `5`, `None` → `null`.
const SOURCE_OPTION_INT: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

class Decode a =
    decode (j: JsonValue) -> Result a Error

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

fn fromJson (s: Text) -> Result (Option Int) Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

pub fn main_some () -> Text =
    toJson (Some 5)

pub fn main_none () -> Text =
    let o : Option Int = None
    toJson o

pub fn main_roundtrip_some () -> Text =
    match fromJson (toJson (Some 5))
        Ok (Some 5) -> "ok_some"
        Ok _ -> "wrong"
        Err e -> e.code

pub fn main_roundtrip_none () -> Text =
    let o : Option Int = None
    match fromJson (toJson o)
        Ok None -> "ok_none"
        Ok _ -> "wrong"
        Err e -> e.code
"#;

/// Two distinct `Option` element types in the SAME module: an `Option Int` and
/// an `Option Text`. Both share the `(Encode, Option)` head, so each call site
/// must pick its element dictionary from the full resolved type. `Some 5` → `5`
/// and `Some "hi"` → `"hi"`.
const SOURCE_OPTION_TWO_TYPES: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_int () -> Text =
    toJson (Some 5)

pub fn main_text () -> Text =
    toJson (Some "hi")
"#;

/// A nested `List (Option Int)`: the outer `Encode (List a)` dictionary is
/// applied to the `Encode (Option a)` dictionary, itself applied to the
/// synthesised `Encode Int`. `[Some 1, None, Some 3]` → `[1,null,3]`.
const SOURCE_LIST_OPTION_INT: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_list_option () -> Text =
    let xs : List (Option Int) = [Some 1, None, Some 3]
    toJson xs
"#;

/// Parametric `Map Text a` over a primitive value. `{"a": 1}` → `{"a":1}` and the
/// decode round-trip recovers the map.
const SOURCE_MAP_TEXT_INT: &str = r#"
import std.map as Map

class Encode a =
    encode (x: a) -> JsonValue

class Decode a =
    decode (j: JsonValue) -> Result a Error

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

fn fromJson (s: Text) -> Result (Map Text Int) Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

pub fn main_map () -> Text =
    let m = Map.fromList [("a", 1)]
    toJson m

pub fn main_roundtrip () -> Text =
    let m = Map.fromList [("a", 1)]
    match fromJson (toJson m)
        Ok m2 ->
            match Map.get "a" m2
                Some 1 -> "ok_map"
                _ -> "wrong"
        Err e -> e.code
"#;

/// Parametric multi-parameter `Result a e`, applied DIRECTLY with no wrapper
/// function: `Ok 7` and `Err "bad"` flow straight into `toJson`. The two element
/// dictionaries (for the `Ok` and `Err` arms) are threaded to the right
/// positions purely from the resolved `Result Int Text` argument type.
const SOURCE_RESULT_INT_TEXT: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

class Decode a =
    decode (j: JsonValue) -> Result a Error

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

fn fromJson (s: Text) -> Result (Result Int Text) Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

pub fn main_ok () -> Text =
    let r : Result Int Text = Ok 7
    toJson r

pub fn main_err () -> Text =
    let r : Result Int Text = Err "bad"
    toJson r

pub fn main_roundtrip_ok () -> Text =
    let r : Result Int Text = Ok 7
    match fromJson (toJson r)
        Ok (Ok 7) -> "ok_ok"
        Ok _ -> "wrong"
        Err e -> e.code

pub fn main_roundtrip_err () -> Text =
    let r : Result Int Text = Err "bad"
    match fromJson (toJson r)
        Ok (Err "bad") -> "ok_err"
        Ok _ -> "wrong"
        Err e -> e.code
"#;

/// Decode failure cases for a parametric instance: a non-array fed to a
/// `List Int` decoder must yield `decode.expected_array`.
const SOURCE_LIST_DECODE_FAIL: &str = r#"
class Decode a =
    decode (j: JsonValue) -> Result a Error

fn fromJson (s: Text) -> Result (List Int) Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

pub fn main_not_array () -> Text =
    match fromJson "123"
        Ok _ -> "ok"
        Err e -> e.code

pub fn main_wrong_elem () -> Text =
    match fromJson "[1, \"two\", 3]"
        Ok _ -> "ok"
        Err e -> e.code
"#;

// ── Workspace setup ───────────────────────────────────────────────────────────

fn write_workspace_source(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"gap-a-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
        .prefix("ridge-gap-a-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-gap-a-e2e-cache-")
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

/// Parametric `Encode (List a)` on `List Color`: the dict-of-dicts mechanism
/// applies `$inst_Encode_List` to `$inst_Encode_Color` at the call site.
#[test]
fn parametric_encode_list_color() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_LIST_COLOR) else {
        eprintln!("erl/erlc not on PATH — skipping parametric_encode_list_color");
        return;
    };

    let expr = format!(
        "io:format(\"main_list=~s~n\",[{module}:main_list()]), \
         io:format(\"main_list_empty=~s~n\",[{module}:main_list_empty()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains(r#"main_list=["red","green","blue"]"#),
        "expected `main_list=[\"red\",\"green\",\"blue\"]`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("main_list_empty=[]"),
        "expected `main_list_empty=[]`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// `List Int` — proves the synthesised prelude `Encode Int` dictionary makes a
/// list of primitives encode end to end on the BEAM.
#[test]
fn parametric_encode_list_int() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_LIST_INT) else {
        eprintln!("erl/erlc not on PATH — skipping parametric_encode_list_int");
        return;
    };

    let expr = format!(
        "io:format(\"main_ints=~s~n\",[{module}:main_ints()]), \
         io:format(\"main_ints_empty=~s~n\",[{module}:main_ints_empty()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("main_ints=[1,2,3]"),
        "expected `main_ints=[1,2,3]`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("main_ints_empty=[]"),
        "expected `main_ints_empty=[]`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// `List Text` — proves the synthesised prelude `Encode Text` dictionary.
#[test]
fn parametric_encode_list_text() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_LIST_TEXT) else {
        eprintln!("erl/erlc not on PATH — skipping parametric_encode_list_text");
        return;
    };

    let expr = format!("io:format(\"main_texts=~s~n\",[{module}:main_texts()]), halt().");
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains(r#"main_texts=["a","b"]"#),
        "expected `main_texts=[\"a\",\"b\"]`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Nested `List (List Color)`: exercises dict-of-dicts — the outer list dict is
/// applied to the inner list dict, which is itself applied to the color dict.
#[test]
fn parametric_encode_nested_list() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_LIST_LIST_COLOR)
    else {
        eprintln!("erl/erlc not on PATH — skipping parametric_encode_nested_list");
        return;
    };

    let expr = format!("io:format(\"main_nested=~s~n\",[{module}:main_nested()]), halt().");
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains(r#"main_nested=[["red","green"],["blue"]]"#),
        "expected `main_nested=[[\"red\",\"green\"],[\"blue\"]]`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Generic user type `Box a` deriving `(Encode, Decode)` — the deriving Var-lift.
/// `Box Int` must encode to `{"val":7}` and round-trip cleanly.
#[test]
fn parametric_generic_box_roundtrip() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_GENERIC_BOX) else {
        eprintln!("erl/erlc not on PATH — skipping parametric_generic_box_roundtrip");
        return;
    };

    let expr = format!(
        "io:format(\"main_encode=~s~n\",[{module}:main_encode()]), \
         io:format(\"main_roundtrip=~s~n\",[{module}:main_roundtrip()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains(r#"main_encode={"val":7}"#),
        "expected `main_encode={{\"val\":7}}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("main_roundtrip=roundtrip_ok"),
        "expected `main_roundtrip=roundtrip_ok`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// `Option Int` applied directly (no wrapper fn): `Some` encodes to the value,
/// `None` to `null`; both round-trip.
#[test]
fn parametric_option_int_roundtrip() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_OPTION_INT) else {
        eprintln!("erl/erlc not on PATH — skipping parametric_option_int_roundtrip");
        return;
    };

    let expr = format!(
        "io:format(\"main_some=~s~n\",[{module}:main_some()]), \
         io:format(\"main_none=~s~n\",[{module}:main_none()]), \
         io:format(\"rt_some=~s~n\",[{module}:main_roundtrip_some()]), \
         io:format(\"rt_none=~s~n\",[{module}:main_roundtrip_none()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("main_some=5"),
        "expected `main_some=5`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("main_none=null"),
        "expected `main_none=null`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("rt_some=ok_some") && stdout.contains("rt_none=ok_none"),
        "expected both Option round-trips to succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Two distinct `Option` element types in one module: `Option Int` and
/// `Option Text` must each dispatch to the correct element encoder even though
/// they share the `(Encode, Option)` head. `Some 5` → `5`, `Some "hi"` → `"hi"`.
#[test]
fn parametric_option_two_element_types() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_OPTION_TWO_TYPES)
    else {
        eprintln!("erl/erlc not on PATH — skipping parametric_option_two_element_types");
        return;
    };

    let expr = format!(
        "io:format(\"main_int=~s~n\",[{module}:main_int()]), \
         io:format(\"main_text=~s~n\",[{module}:main_text()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("main_int=5"),
        "expected `main_int=5`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(r#"main_text="hi""#),
        "expected `main_text=\"hi\"`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Nested `List (Option Int)` — the outer list dictionary is applied to the
/// option dictionary, applied to the int dictionary. `[Some 1, None, Some 3]` →
/// `[1,null,3]`.
#[test]
fn parametric_list_option_int() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_LIST_OPTION_INT)
    else {
        eprintln!("erl/erlc not on PATH — skipping parametric_list_option_int");
        return;
    };

    let expr =
        format!("io:format(\"main_list_option=~s~n\",[{module}:main_list_option()]), halt().");
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("main_list_option=[1,null,3]"),
        "expected `main_list_option=[1,null,3]`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// `Map Text Int` — encodes to a JSON object and round-trips.
#[test]
fn parametric_map_text_int_roundtrip() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_MAP_TEXT_INT)
    else {
        eprintln!("erl/erlc not on PATH — skipping parametric_map_text_int_roundtrip");
        return;
    };

    let expr = format!(
        "io:format(\"main_map=~s~n\",[{module}:main_map()]), \
         io:format(\"rt=~s~n\",[{module}:main_roundtrip()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains(r#"main_map={"a":1}"#),
        "expected `main_map={{\"a\":1}}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("rt=ok_map"),
        "expected `rt=ok_map`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// `Result Int Text` applied directly (no wrapper fn) — the multi-parameter
/// (two-dictionary) case. Both `Ok` and `Err` arms round-trip through the
/// adjacently-tagged wire.
#[test]
fn parametric_result_int_text_roundtrip() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_RESULT_INT_TEXT)
    else {
        eprintln!("erl/erlc not on PATH — skipping parametric_result_int_text_roundtrip");
        return;
    };

    let expr = format!(
        "io:format(\"main_ok=~s~n\",[{module}:main_ok()]), \
         io:format(\"main_err=~s~n\",[{module}:main_err()]), \
         io:format(\"rt_ok=~s~n\",[{module}:main_roundtrip_ok()]), \
         io:format(\"rt_err=~s~n\",[{module}:main_roundtrip_err()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains(r#"main_ok={"tag":"Ok","values":[7]}"#),
        "expected `main_ok={{\"tag\":\"Ok\",\"values\":[7]}}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(r#"main_err={"tag":"Err","values":["bad"]}"#),
        "expected `main_err={{\"tag\":\"Err\",\"values\":[\"bad\"]}}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("rt_ok=ok_ok") && stdout.contains("rt_err=ok_err"),
        "expected both Result round-trips to succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Decode failure cases for the parametric `List Int` decoder: a non-array input
/// yields `decode.expected_array`; a wrong element type yields `decode.expected_int`.
#[test]
fn parametric_list_decode_failures() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_LIST_DECODE_FAIL)
    else {
        eprintln!("erl/erlc not on PATH — skipping parametric_list_decode_failures");
        return;
    };

    let expr = format!(
        "io:format(\"not_array=~s~n\",[{module}:main_not_array()]), \
         io:format(\"wrong_elem=~s~n\",[{module}:main_wrong_elem()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("not_array=decode.expected_array"),
        "expected `not_array=decode.expected_array`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("wrong_elem=decode.expected_int"),
        "expected `wrong_elem=decode.expected_int`\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
