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
/// so the resolver's ClassMethodIndex resolves bare `decode j` calls (the same
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

/// Nullary union: `"Admin"` → Ok Admin, `"Superuser"` → Err(unknown_tag).
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
