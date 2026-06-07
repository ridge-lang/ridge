//! Schema codegen — `deriving (Schema)` generates a structural descriptor.
//!
//! For a record entity, `deriving (Schema)` emits one descriptor value
//! (`<entity>Schema : Schema`) carrying the entity name, its SQL table name, and
//! a per-field list of `{ name, column, ty, optional }` entries. It is the
//! introspection source the data/web layers map to an `OpenAPI` spec or a
//! migration diff. The type-level tests here prove the descriptor checks; the
//! runtime test proves the generated value survives to the BEAM.
//!
//! The runtime test is guarded on `erl`/`erlc` being on PATH.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::redundant_clone
)]

mod common;
use common::make_workspace;
use ridge_driver::{check_workspace, CheckOptions};

/// A `User` entity that derives its schema descriptor. No imports needed —
/// `deriving (Schema)` synthesizes everything from compiler builtins. `Option`
/// is a prelude type, so the nullable field needs no import either.
const SCHEMA: &str = r"
pub type User = { id: Int, email: Text, nickname: Option Text } deriving (Schema)
";

/// The generated descriptor type-checks: `userSchema.name` / `.table` are
/// `Text`, and `.fields` is a `List FieldSchema` (so `List.length` accepts it).
#[test]
fn schema_descriptor_typechecks() {
    let source = format!(
        "import std.list as List\n\
         {SCHEMA}\n\
         pub fn modelName () -> Text = userSchema.name\n\
         pub fn tableName () -> Text = userSchema.table\n\
         pub fn fieldCount () -> Int = List.length userSchema.fields\n"
    );
    let tw = make_workspace("Models", &source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.is_empty(),
        "expected a clean check; got {:?}",
        result.diagnostics
    );
}

/// `Table` and `Schema` coexist on one entity: both the column mirror and the
/// descriptor are generated and check together.
#[test]
fn table_and_schema_coexist() {
    let source = "
pub type Post = { id: Int, title: Text } deriving (Table, Schema)
pub fn tname () -> Text = postTable.name
pub fn sname () -> Text = postSchema.name
pub fn idCol () -> Text = postCols.id.name
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.is_empty(),
        "expected a clean check; got {:?}",
        result.diagnostics
    );
}

/// Reading a field that the descriptor record does not have is a compile error —
/// the descriptor is a real typed record, not an untyped map.
#[test]
fn nonexistent_descriptor_field_is_compile_error() {
    let source = format!("{SCHEMA}\npub fn bad () -> Text = userSchema.nope\n");
    let tw = make_workspace("Models", &source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        !result.diagnostics.is_empty(),
        "expected a type error for a nonexistent descriptor field, got a clean check"
    );
}

/// The generated descriptor survives to the BEAM: it carries the entity name,
/// the SQL table name, and per-field entries with type tags and nullability.
///
/// Gated on `beam-runtime` and a `which` guard for `erl`/`erlc`.
#[cfg(feature = "beam-runtime")]
#[test]
fn schema_descriptor_roundtrip_survives_beam() {
    use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};
    use std::process::Command;

    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping schema_descriptor_roundtrip_survives_beam");
        return;
    }

    // Project each field's descriptor out so the runtime can assert on the
    // generated tags. `List.map` comes from the stdlib; the bare-param lambda's
    // `f` unifies with `FieldSchema`, so `f.ty` resolves to the descriptor field.
    let source = format!(
        "import std.list as List\n\
         {SCHEMA}\n\
         pub fn modelName () -> Text = userSchema.name\n\
         pub fn tableName () -> Text = userSchema.table\n\
         pub fn fieldNames () -> List Text = List.map (fn f -> f.name) userSchema.fields\n\
         pub fn fieldTypes () -> List Text = List.map (fn f -> f.ty) userSchema.fields\n\
         pub fn fieldOptional () -> List Bool = List.map (fn f -> f.optional) userSchema.fields\n"
    );
    let tw = make_workspace("Models", &source);
    let cache = tempfile::Builder::new()
        .prefix("ridge-schema-codegen-cache-")
        .tempdir()
        .expect("cache dir");

    let artefacts = compile_workspace(
        CompileOptions::new(tw.path.clone())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");
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
        "io:format(\"mname=~s~n\",[{module}:modelName()]), \
         io:format(\"tname=~s~n\",[{module}:tableName()]), \
         io:format(\"fnames=~p~n\",[{module}:fieldNames()]), \
         io:format(\"ftypes=~p~n\",[{module}:fieldTypes()]), \
         io:format(\"fopt=~p~n\",[{module}:fieldOptional()]), \
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
        stdout.contains("mname=User"),
        "descriptor should carry the entity name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("tname=users"),
        "descriptor should carry the SQL table name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("fnames=[<<\"id\">>,<<\"email\">>,<<\"nickname\">>]"),
        "descriptor should list field names in order\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("ftypes=[<<\"Int\">>,<<\"Text\">>,<<\"Option Text\">>]"),
        "descriptor should carry the rendered type tags\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("fopt=[false,false,true]"),
        "descriptor should flag the Option field as optional\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
