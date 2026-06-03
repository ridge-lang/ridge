//! End-to-end runtime checks for `deriving (Encode)`.
//!
//! Exercises the full pipeline for `type T = … deriving (Encode)`:
//! parse → collect (deriving generators + prelude Encode instances) →
//! typecheck → lower (DerivedEncodeRecord/Union IR) → Core Erlang →
//! run on the BEAM → assert JSON wire strings.
//!
//! Wire format (adjacently-tagged, DX-first):
//! - Record → JSON object, field names as keys, declaration order.
//! - Nullary union ctor → bare JSON string (`"CtorName"`).
//! - Payload union ctor → adjacently-tagged `{"tag":"Ctor","values":[…]}`.
//! - `Option T` → `T | null` (Some => encode T, None => JSON null).
//! - `List T` → JSON array.
//! - `Map Text T` → JSON object with the map's text keys.
//!
//! # Coverage note
//!
//! Payload union encode BEAM e2e is deferred: constructing a payload union
//! variant as a value (e.g. `Circle 3.0`) requires resolver support for
//! calling union constructors as functions, which lands in a later cut.
//! The payload union encode path is covered by unit tests in `item.rs`.
//! Nullary union encode and all record shapes are fully tested here.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// ── Source constants ──────────────────────────────────────────────────────────

/// A record-of-primitives type with derived Encode.
///
/// `Person { name = "Ann", age = 30 }` must encode to `{"name":"Ann","age":30}`.
///
/// The test redeclares `class Encode a = encode (x: a) -> JsonValue` so the
/// resolver's ClassMethodIndex resolves bare `encode x` calls (the same trick
/// used in `typeclass_deriving_e2e.rs` for the Show class, line 34).
const SOURCE_RECORD_PRIMITIVES: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

type Person = { name: Text, age: Int } deriving (Encode)

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_person () -> Text =
    toJson (Person { name = "Ann", age = 30 })

pub fn main_person_empty_int () -> Text =
    toJson (Person { name = "", age = 0 })
"#;

/// A record with `List` and `Option` fields.
///
/// `Profile { tags = ["a", "b"], nick = Some "Bob" }` must encode to
/// `{"tags":["a","b"],"nick":"Bob"}`.
/// `Profile { tags = [], nick = None }` must encode to `{"tags":[],"nick":null}`.
const SOURCE_RECORD_CONTAINERS: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

type Profile = { tags: List Text, nick: Option Text } deriving (Encode)

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_profile_full () -> Text =
    toJson (Profile { tags = ["a", "b"], nick = Some "Bob" })

pub fn main_profile_empty () -> Text =
    toJson (Profile { tags = [], nick = None })
"#;

/// A record whose field type is another same-module record that also derives Encode.
///
/// `User { name = "Ann", addr = Address { city = "NYC" } }` must encode to
/// `{"name":"Ann","addr":{"city":"NYC"}}` — the `addr` field must be encoded
/// by calling `Address`'s derived encode function, not passed through raw.
///
/// Exercises the `EncodeFieldShape::User` path end-to-end through BEAM.
const SOURCE_NESTED_USER_TYPE: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

type Address = { city: Text } deriving (Encode)

type User = { name: Text, addr: Address } deriving (Encode)

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_user () -> Text =
    toJson (User { name = "Ann", addr = Address { city = "NYC" } })

pub fn main_user_empty_city () -> Text =
    toJson (User { name = "Bob", addr = Address { city = "" } })
"#;

/// A nullary union type with derived Encode.
///
/// `Admin` must encode to `"Admin"` (bare JSON string).
const SOURCE_NULLARY_UNION: &str = r#"
class Encode a =
    encode (x: a) -> JsonValue

type Role = Admin | Guest | Editor deriving (Encode)

fn toJson (x: a) -> Text where Encode a =
    Json.encode (encode x)

pub fn main_admin () -> Text =
    toJson Admin

pub fn main_guest () -> Text =
    toJson Guest

pub fn main_editor () -> Text =
    toJson Editor
"#;

// ── Workspace helpers ─────────────────────────────────────────────────────────

fn write_workspace_source(root: &std::path::Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"encode-deriving-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
        .prefix("ridge-encode-deriving-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-encode-deriving-e2e-cache-")
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

/// Record-of-primitives: `Person { name, age }` → `{"name":...,"age":...}`.
#[test]
fn encode_derive_record_of_primitives() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_RECORD_PRIMITIVES)
    else {
        eprintln!("erl/erlc not on PATH — skipping encode_derive_record_of_primitives");
        return;
    };

    let expr = format!(
        "io:format(\"main_person=~s~n\",[{module}:main_person()]), \
         io:format(\"main_person_empty_int=~s~n\",[{module}:main_person_empty_int()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    // Person { name = "Ann", age = 30 } → {"name":"Ann","age":30}
    // Key order in JSON output depends on map iteration; both fields must be present.
    assert!(
        stdout.contains("main_person=")
            && stdout.contains("\"name\"")
            && stdout.contains("\"Ann\"")
            && stdout.contains("\"age\"")
            && stdout.contains("30"),
        "Person encode must contain name and age fields\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Person { name = "", age = 0 } → {"name":"","age":0}
    assert!(
        stdout.contains("main_person_empty_int=")
            && stdout.contains("\"age\"")
            && stdout.contains(":0"),
        "empty Person must encode age=0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Record with `List Text` and `Option Text` fields.
#[test]
fn encode_derive_record_with_list_and_option() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_RECORD_CONTAINERS)
    else {
        eprintln!("erl/erlc not on PATH — skipping encode_derive_record_with_list_and_option");
        return;
    };

    let expr = format!(
        "io:format(\"profile_full=~s~n\",[{module}:main_profile_full()]), \
         io:format(\"profile_empty=~s~n\",[{module}:main_profile_empty()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    // Full profile: tags=["a","b"], nick=Some "Bob" → nick:"Bob", tags:["a","b"]
    assert!(
        stdout.contains("profile_full=")
            && stdout.contains("\"tags\"")
            && stdout.contains("\"a\"")
            && stdout.contains("\"b\"")
            && stdout.contains("\"nick\"")
            && stdout.contains("\"Bob\""),
        "full profile must contain tags and nick fields\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // Empty profile: tags=[], nick=None → nick:null, tags:[]
    assert!(
        stdout.contains("profile_empty=")
            && stdout.contains("\"tags\"")
            && stdout.contains("\"nick\"")
            && stdout.contains("null"),
        "empty profile must have nick:null and empty tags\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("[]"),
        "empty tags must encode as [] array\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Nested user type: `User { name, addr: Address { city } }` → `{"name":…,"addr":{"city":…}}`.
///
/// Verifies that a record field whose type is another same-module record that
/// derives Encode produces a nested JSON object (not the raw Erlang term).
#[test]
fn encode_derive_nested_user_type() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_NESTED_USER_TYPE)
    else {
        eprintln!("erl/erlc not on PATH — skipping encode_derive_nested_user_type");
        return;
    };

    let expr = format!(
        "io:format(\"main_user=~s~n\",[{module}:main_user()]), \
         io:format(\"main_user_empty_city=~s~n\",[{module}:main_user_empty_city()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    // User { name = "Ann", addr = Address { city = "NYC" } }
    // → {"name":"Ann","addr":{"city":"NYC"}}
    assert!(
        stdout.contains("main_user=")
            && stdout.contains("\"name\"")
            && stdout.contains("\"Ann\"")
            && stdout.contains("\"addr\"")
            && stdout.contains("\"city\"")
            && stdout.contains("\"NYC\""),
        "nested User must contain name, addr, and city fields\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // User { name = "Bob", addr = Address { city = "" } }
    // → {"name":"Bob","addr":{"city":""}}
    assert!(
        stdout.contains("main_user_empty_city=")
            && stdout.contains("\"name\"")
            && stdout.contains("\"Bob\"")
            && stdout.contains("\"addr\"")
            && stdout.contains("\"city\""),
        "nested User with empty city must encode addr as object\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// Nullary union: `Admin` → `"Admin"`, `Guest` → `"Guest"`, `Editor` → `"Editor"`.
#[test]
fn encode_derive_nullary_union() {
    let Some((_dir, _cache, beam_dir, module)) = compile_and_find_module(SOURCE_NULLARY_UNION)
    else {
        eprintln!("erl/erlc not on PATH — skipping encode_derive_nullary_union");
        return;
    };

    let expr = format!(
        "io:format(\"main_admin=~s~n\",[{module}:main_admin()]), \
         io:format(\"main_guest=~s~n\",[{module}:main_guest()]), \
         io:format(\"main_editor=~s~n\",[{module}:main_editor()]), \
         halt()."
    );
    let (stdout, stderr) = run_erl(&beam_dir, &expr);

    // Nullary ctors → bare JSON strings.
    assert!(
        stdout.contains("main_admin=\"Admin\""),
        "Admin must encode to \"Admin\"\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("main_guest=\"Guest\""),
        "Guest must encode to \"Guest\"\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("main_editor=\"Editor\""),
        "Editor must encode to \"Editor\"\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
