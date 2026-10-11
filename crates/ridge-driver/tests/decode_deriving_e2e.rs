//! End-to-end runtime checks for `deriving (Decode)`.
//!
//! Exercises the full pipeline for `type T = … deriving (Decode)`:
//! parse → collect (deriving generators + prelude Decode instances) →
//! typecheck → lower (DerivedDecodeRecord/Union IR) → Core Erlang →
//! run on the BEAM → assert decoded values and error codes.
//!
//! Wire format (adjacently-tagged, DX-first — inverse of the Encode wire format):
//! - Record ← JSON object, field names as keys.
//! - Nullary union ctor ← bare JSON string (`"CtorName"`).
//! - Payload union ctor ← adjacently-tagged `{"tag":"Ctor","values":[…]}`.
//! - `Option T` ← `T | null`.
//! - `List T` ← JSON array.
//! - `Map Text T` ← JSON object with text keys.
//!
//! Error codes emitted on failure:
//! - `decode.expected_object` — JSON value was not an object when one was required.
//! - `decode.expected_array` — JSON value was not an array when one was required.
//! - `decode.expected_int`, `decode.expected_string`, etc. — wrong primitive kind.
//! - `decode.missing_field` — required record field absent from the JSON object.
//! - `decode.unknown_tag` — union tag string not recognised.
//! - `decode.bad_arity` — `values` array length does not match the constructor arity.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source constants ──────────────────────────────────────────────────────────

/// A record-of-primitives type with derived Decode.
///
/// The test redeclares `class Decode a = decode (j: JsonValue) -> Result a Error`
/// so the resolver's `ClassMethodIndex` resolves bare `decode j` calls (the same
/// trick used in `encode_deriving_e2e.rs` for the Encode class).
const SOURCE_RECORD_PRIMITIVES: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

class Decode a =
    decode (j: JsonValue) -> Result a Error

type Person = { name: Text, age: Int } deriving (Encode, Decode)

fn fromJson (s: Text) -> Result Person Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_ok () -> Text =
    match fromJson "{\"name\":\"Ann\",\"age\":30}"
        Ok p -> p.name
        Err e -> e.code

pub fn main_missing_field () -> Text =
    match fromJson "{\"name\":\"Ann\"}"
        Ok p -> "ok"
        Err e -> e.code

pub fn main_wrong_kind () -> Text =
    match fromJson "{\"name\":123,\"age\":30}"
        Ok p -> "ok"
        Err e -> e.code

pub fn main_not_object () -> Text =
    match fromJson "\"hello\""
        Ok p -> "ok"
        Err e -> e.code

pub fn main_roundtrip () -> Text =
    let p = Person { name = "Ann", age = 30 }
    match fromJson (toJson p)
        Ok q -> q.name
        Err e -> e.code
"#;

/// A nullary union type with derived Decode.
const SOURCE_NULLARY_UNION: &str = r#"
class Decode a =
    decode (j: JsonValue) -> Result a Error

type Role = Admin | Guest | Editor deriving (Decode)

fn fromJson (s: Text) -> Result Role Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

pub fn main_admin () -> Text =
    match fromJson "\"Admin\""
        Ok r -> "ok_admin"
        Err e -> e.code

pub fn main_guest () -> Text =
    match fromJson "\"Guest\""
        Ok r -> "ok_guest"
        Err e -> e.code

pub fn main_unknown () -> Text =
    match fromJson "\"Superuser\""
        Ok r -> "ok"
        Err e -> e.code
"#;

/// A record with `List` and `Option` fields, with derived Decode.
const SOURCE_RECORD_CONTAINERS: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

class Decode a =
    decode (j: JsonValue) -> Result a Error

type Profile = { tags: List Text, nick: Option Text } deriving (Encode, Decode)

fn fromJson (s: Text) -> Result Profile Error =
    match Json.decode s
        Ok j -> decode j
        Err e -> Err e

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_full_ok () -> Text =
    match fromJson "{\"tags\":[\"a\",\"b\"],\"nick\":\"Bob\"}"
        Ok p -> "ok"
        Err e -> e.code

pub fn main_empty_ok () -> Text =
    match fromJson "{\"tags\":[],\"nick\":null}"
        Ok p -> "ok"
        Err e -> e.code

pub fn main_roundtrip_full () -> Text =
    let p = Profile { tags = ["a", "b"], nick = Some "Bob" }
    match fromJson (toJson p)
        Ok q -> "ok"
        Err e -> e.code

pub fn main_roundtrip_empty () -> Text =
    let p = Profile { tags = [], nick = None }
    match fromJson (toJson p)
        Ok q -> "ok"
        Err e -> e.code
"#;

/// Regression: two return-type-directed `Decode` instances in one module.
///
/// `decode : JsonValue -> Result a Error` picks its instance from the result
/// type, not an argument. With a single such instance the compiler resolved the
/// dictionary; with two or more, a monomorphic `decode` call fell through to the
/// generic dict-passing path, which had no incoming dict to forward and read a
/// garbage local — `maps:get(decode, ok)` crashed with `{badmap,ok}` at runtime.
/// Both `main_event` and `main_level` decode a concrete type at a monomorphic
/// site, so this module carries two instances and exercises the trigger.
///
/// No `class Decode` redeclaration here (unlike the sources above): the calls
/// dispatch through the prelude `Decode` class, which is the exact shape the bug
/// was reported against.
const SOURCE_TWO_INSTANCES: &str = r#"
type Event = { kind: Text, at: Int } deriving (Encode, Decode)
type Level = Info | Warn deriving (Encode, Decode)

fn roundtripEvent (e: Event) -> Result Event Error = decode (encode e)
fn roundtripLevel (l: Level) -> Result Level Error = decode (encode l)

pub fn main_event () -> Text =
    match roundtripEvent (Event { kind = "boot", at = 1 })
        Ok ev -> ev.kind
        Err er -> er.code

pub fn main_level () -> Text =
    match roundtripLevel Warn
        Ok Info -> "info"
        Ok Warn -> "warn"
        Err er -> er.code
"#;

// ── Workspace helpers ─────────────────────────────────────────────────────────

fn write_workspace_source(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"decode-deriving-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[capabilities]\nallow = []\n",
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
        .prefix("ridge-decode-deriving-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-decode-deriving-e2e-cache-")
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
        .find(|stem| {
            stem.starts_with("ridge_")
                && !matches!(
                    *stem,
                    "ridge_rt"
                        | "ridge_main_runner"
                        | "ridge_test_runner"
                        | "ridge_pg"
                        | "ridge_sup"
                        | "ridge_sqlite"
                        | "ridge_bench_runner"
                )
        })
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
    assert!(
        output.status.success(),
        "erl failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Record-of-primitives: ok decode, missing-field error, wrong-kind error, not-object error.
#[test]
fn decode_derive_record_of_primitives() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_RECORD_PRIMITIVES)
    else {
        eprintln!("erl/erlc not on PATH — skipping decode_derive_record_of_primitives");
        return;
    };

    let expr = format!(
        "io:format(\"ok=~s~n\",[{module}:main_ok()]), \
         io:format(\"missing=~s~n\",[{module}:main_missing_field()]), \
         io:format(\"wrong=~s~n\",[{module}:main_wrong_kind()]), \
         io:format(\"noobj=~s~n\",[{module}:main_not_object()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("ok=Ann"),
        "ok decode must return name 'Ann'\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("missing=decode.missing_field"),
        "missing age field must yield decode.missing_field\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("wrong=decode.expected_string"),
        "wrong kind for name field must yield decode.expected_string\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("noobj=decode.expected_object"),
        "non-object input must yield decode.expected_object\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Round-trip: `encode x |> Json.encode |> Json.decode |> decode == Ok x`.
#[test]
fn decode_derive_record_roundtrip() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_RECORD_PRIMITIVES)
    else {
        eprintln!("erl/erlc not on PATH — skipping decode_derive_record_roundtrip");
        return;
    };

    let expr = format!(
        "io:format(\"rt=~s~n\",[{module}:main_roundtrip()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("rt=Ann"),
        "round-trip must recover 'Ann' as name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Nullary union: `"Admin"` → Ok Admin, `"Superuser"` → `Err(unknown_tag)`.
#[test]
fn decode_derive_nullary_union() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_NULLARY_UNION)
    else {
        eprintln!("erl/erlc not on PATH — skipping decode_derive_nullary_union");
        return;
    };

    let expr = format!(
        "io:format(\"admin=~s~n\",[{module}:main_admin()]), \
         io:format(\"guest=~s~n\",[{module}:main_guest()]), \
         io:format(\"unknown=~s~n\",[{module}:main_unknown()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("admin=ok_admin"),
        "Admin must decode successfully\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("guest=ok_guest"),
        "Guest must decode successfully\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("unknown=decode.unknown_tag"),
        "Unknown tag must yield decode.unknown_tag\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Regression: two return-type-directed `Decode` instances coexist in one
/// module and both dispatch to the right dictionary at monomorphic call sites.
#[test]
fn decode_two_return_directed_instances_dispatch() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_TWO_INSTANCES)
    else {
        eprintln!("erl/erlc not on PATH — skipping decode_two_return_directed_instances_dispatch");
        return;
    };

    let expr = format!(
        "io:format(\"event=~s~n\",[{module}:main_event()]), \
         io:format(\"level=~s~n\",[{module}:main_level()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("event=boot"),
        "Event must decode to its own instance (not crash on {{badmap,ok}})\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("level=warn"),
        "Level must decode to its own instance in the same module\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Record with `List` and `Option` fields: ok decode and round-trip.
#[test]
fn decode_derive_record_with_list_and_option() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_RECORD_CONTAINERS)
    else {
        eprintln!("erl/erlc not on PATH — skipping decode_derive_record_with_list_and_option");
        return;
    };

    let expr = format!(
        "io:format(\"full=~s~n\",[{module}:main_full_ok()]), \
         io:format(\"empty=~s~n\",[{module}:main_empty_ok()]), \
         io:format(\"rt_full=~s~n\",[{module}:main_roundtrip_full()]), \
         io:format(\"rt_empty=~s~n\",[{module}:main_roundtrip_empty()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    assert!(
        stdout.contains("full=ok"),
        "Full profile must decode ok\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("empty=ok"),
        "Empty profile must decode ok\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("rt_full=ok"),
        "Full profile round-trip must succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("rt_empty=ok"),
        "Empty profile round-trip must succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Parametric return-directed decoders must select their full payload type at
/// each call, independently of other Decode instances used in the module.
#[test]
fn decode_combined_parametric_payloads_dispatch() {
    let source = r#"
type Entry = { value: Int } deriving (Decode, Eq)
type Event = Number Int | Empty deriving (Decode, Eq)
type Box a = { value: a } deriving (Decode, Eq)

pub fn sameFunction () -> Bool =
    let ints: Result (Map Text Int) Error = decode (JObject (Map.fromList [("k", JInt 7)]))
    let texts: Result (Map Text Text) Error = decode (JObjectFields [("k", JText "seven")])
    let ok: Result (Result Int Text) Error = decode (JObjectFields [("tag", JText "Ok"), ("values", JList [JInt 7])])
    let err: Result (Result Text Int) Error = decode (JObject (Map.fromList [("tag", JText "Err"), ("values", JList [JInt 8])]))
    let entry: Result Entry Error = decode (JObjectFields [("value", JInt 9)])
    let event: Result Event Error = decode (JObjectFields [("tag", JText "Number"), ("values", JList [JInt 10])])
    let boxed: Result (Box Int) Error = decode (JObjectFields [("value", JInt 11)])
    let list: Result (List Int) Error = decode (JList [JInt 12])
    let option: Result (Option Text) Error = decode (JText "thirteen")
    ints == Ok (Map.fromList [("k", 7)]) && texts == Ok (Map.fromList [("k", "seven")]) && ok == Ok (Ok 7) && err == Ok (Err 8) && entry == Ok (Entry { value = 9 }) && event == Ok (Number 10) && boxed == Ok (Box { value = 11 }) && list == Ok [12] && option == Ok (Some "thirteen")

fn mapOnly () -> Result (Map Text Int) Error =
    decode (JObject (Map.fromList [("k", JInt 7)]))

fn resultOnly () -> Result (Result Int Text) Error =
    decode (JObject (Map.fromList [("tag", JText "Ok"), ("values", JList [JInt 8])]))

pub fn separateFunctions () -> Bool =
    mapOnly () == Ok (Map.fromList [("k", 7)]) && resultOnly () == Ok (Ok 8)

pub fn nested () -> Bool =
    let json = JObjectFields [("k", JObjectFields [("tag", JText "Ok"), ("values", JList [JList [JInt 3, JInt 4]])])]
    let back: Result (Map Text (Result (List Int) Text)) Error = decode json
    back == Ok (Map.fromList [("k", Ok [3, 4])])

fn decodeAs (j: JsonValue) -> Result a Error where Decode a = decode j

fn decodeWithConcrete (j: JsonValue) (witness: a) -> (Result a Error, Result Int Error) where Decode a =
    let generic: Result a Error = decode j
    let concrete: Result Int Error = decode (JInt 19)
    (generic, concrete)

fn decodeList (j: JsonValue) (witness: a) -> Result (List a) Error where Decode a = decode j

pub fn forwarding () -> Bool =
    let ints: Result (Map Text Int) Error = decodeAs (JObjectFields [("k", JInt 7)])
    let texts: Result (Map Text Text) Error = decodeAs (JObjectFields [("k", JText "seven")])
    ints == Ok (Map.fromList [("k", 7)]) && texts == Ok (Map.fromList [("k", "seven")])

pub fn concreteInGeneric () -> Bool =
    decodeWithConcrete (JText "text") "witness" == (Ok "text", Ok 19)

pub fn genericContainer () -> Bool =
    decodeList (JList [JText "text"]) "witness" == Ok ["text"]

pub fn piped () -> Bool =
    let map: Result (Map Text Int) Error = JObjectFields [("k", JInt 7)] |> decode
    let result: Result (Result Int Text) Error = JObjectFields [("tag", JText "Ok"), ("values", JList [JInt 8])] |> decode
    map == Ok (Map.fromList [("k", 7)]) && result == Ok (Ok 8)

pub fn methodsAsValues () -> Bool =
    let mapRead: JsonValue -> Result (Map Text Int) Error = decode
    let resultRead: JsonValue -> Result (Result Int Text) Error = decode
    mapRead (JObjectFields [("k", JInt 7)]) == Ok (Map.fromList [("k", 7)]) && resultRead (JObjectFields [("tag", JText "Ok"), ("values", JList [JInt 8])]) == Ok (Ok 8)

fn valueWithConcrete (j: JsonValue) (witness: a) -> (Result a Error, Result Int Error) where Decode a =
    let genericRead: JsonValue -> Result a Error = decode
    let concreteRead: JsonValue -> Result Int Error = decode
    (genericRead j, concreteRead (JInt 19))

pub fn valuesInGeneric () -> Bool =
    valueWithConcrete (JText "text") "witness" == (Ok "text", Ok 19)

pub fn invalidPayload () -> Text =
    let back: Result (Map Text Int) Error = decode (JObjectFields [("k", JText "wrong")])
    match back
        Ok _ -> "unexpected success"
        Err e -> e.code
"#;
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(source) else {
        eprintln!("erl/erlc not on PATH — skipping combined Decode regression");
        return;
    };
    let expr = format!(
        "io:format(\"same=~p~nseparate=~p~nnested=~p~nforward=~p~nconcrete=~p~ncontainer=~p~npipe=~p~nvalues=~p~ngenericValues=~p~ninvalid=~s~n\", \
         [{module}:sameFunction(), {module}:separateFunctions(), {module}:nested(), \
          {module}:forwarding(), {module}:concreteInGeneric(), {module}:genericContainer(), \
          {module}:piped(), {module}:methodsAsValues(), {module}:valuesInGeneric(), \
          {module}:invalidPayload()]), halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);
    assert_eq!(
        stdout.replace("\r\n", "\n"),
        concat!(
            "same=true\nseparate=true\nnested=true\nforward=true\n",
            "concrete=true\ncontainer=true\npipe=true\nvalues=true\ngenericValues=true\n",
            "invalid=decode.expected_int\n"
        ),
        "stderr: {stderr}"
    );
}
