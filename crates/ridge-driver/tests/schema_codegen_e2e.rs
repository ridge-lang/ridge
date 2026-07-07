//! Schema codegen — `deriving (Schema)` synthesizes a `HasSchema` instance.
//!
//! For a record entity, `deriving (Schema)` derives a `HasSchema` instance (the
//! way `deriving (Row)` derives a `Row` instance) whose `schemaOf` returns the
//! entity's `EntitySchema`, built from the record fields by the data-layer
//! convention: snake-cased columns, base-type `DbType`s, `Option` fields
//! nullable, and a field named `id` taken as the identity primary key. The
//! instance is reached by type through a phantom `Option e` witness, so reading
//! the schema needs no descriptor value — `schemaOf (witness ())` answers it.
//!
//! The type-level tests prove the derived instance checks and dispatches; the
//! runtime test proves the synthesized schema survives to the BEAM. A
//! hand-written `HasSchema` instance (see `schema_instance_e2e`) states the
//! deltas the convention cannot infer; deriving it and writing it by hand for the
//! same entity is a coherence error.
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

/// A `User` entity that derives its schema. The witness fixes `e = User` so
/// `schemaOf` selects the derived instance with no entity value in hand.
const SCHEMA: &str = r"
import std.sql (DbType)
import std.schema (schemaOf, schemaName, schemaTable, schemaColumns, generatedColumns, colColumn, colType, colNullable, EntitySchema)
import std.list as List

pub type User = { id: Int, email: Text, nickname: Option Text } deriving (Schema)

fn userWitness () -> Option User = None
fn schemaOfUser () -> EntitySchema User = schemaOf (userWitness ())
";

/// The derived instance checks and dispatches: `schemaName` / `schemaTable` are
/// `Text` and `schemaColumns` is a `List` the stdlib `List.length` accepts.
#[test]
fn derived_schema_typechecks() {
    let source = format!(
        "{SCHEMA}\n\
         pub fn modelName () -> Text = schemaName (schemaOfUser ())\n\
         pub fn tableName () -> Text = schemaTable (schemaOfUser ())\n\
         pub fn columnCount () -> Int = List.length (schemaColumns (schemaOfUser ()))\n"
    );
    let tw = make_workspace("Models", &source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.is_empty(),
        "expected a clean check; got {:?}",
        result.diagnostics
    );
}

/// `Table` and `Schema` coexist on one entity: the column mirror and the derived
/// `HasSchema` instance are both generated and check together.
#[test]
fn table_and_schema_coexist() {
    let source = "
import std.schema (schemaOf, schemaName, EntitySchema)

pub type Post = { id: Int, title: Text } deriving (Table, Schema)

fn postWitness () -> Option Post = None

pub fn tname () -> Text = postTable.name
pub fn idCol () -> Text = postCols.id.name
pub fn sname () -> Text = schemaName (schemaOf (postWitness ()))
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.is_empty(),
        "expected a clean check; got {:?}",
        result.diagnostics
    );
}

/// Deriving `Schema` and hand-writing a `HasSchema` instance for the same entity
/// is a coherence error — the derived instance and the explicit one overlap, so
/// the user must pick one.
#[test]
fn derived_and_hand_written_schema_conflict() {
    let source = "
import std.schema (schema, schemaOf, EntitySchema, HasSchema)

pub type User = { id: Int, email: Text } deriving (Schema)

instance HasSchema User =
    schemaOf (_w: Option User) -> EntitySchema User = schema \"User\" \"users\"
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        !result.diagnostics.is_empty(),
        "expected a coherence error for a derived + hand-written HasSchema, got a clean check"
    );
}

/// The synthesized schema survives to the BEAM: it carries the entity name, the
/// SQL table name, snake-cased column names, base `DbType`s (with `Option`
/// collapsed to the inner type and flagged nullable), and the identity `id` in
/// the database-generated set.
///
/// Gated on `beam-runtime` and a `which` guard for `erl`/`erlc`.
#[cfg(feature = "beam-runtime")]
#[test]
fn derived_schema_roundtrip_survives_beam() {
    use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};
    use std::process::Command;

    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping derived_schema_roundtrip_survives_beam");
        return;
    }

    let source = format!(
        "{SCHEMA}\n\
         pub fn modelName () -> Text = schemaName (schemaOfUser ())\n\
         pub fn tableName () -> Text = schemaTable (schemaOfUser ())\n\
         pub fn columnNames () -> List Text = List.map (fn c -> colColumn c) (schemaColumns (schemaOfUser ()))\n\
         pub fn columnTypes () -> List DbType = List.map (fn c -> colType c) (schemaColumns (schemaOfUser ()))\n\
         pub fn columnNullable () -> List Bool = List.map (fn c -> colNullable c) (schemaColumns (schemaOfUser ()))\n\
         pub fn genColumns () -> List Text = generatedColumns (schemaOfUser ())\n"
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
         io:format(\"cnames=~p~n\",[{module}:columnNames()]), \
         io:format(\"ctypes=~p~n\",[{module}:columnTypes()]), \
         io:format(\"cnull=~p~n\",[{module}:columnNullable()]), \
         io:format(\"gcols=~p~n\",[{module}:genColumns()]), \
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
        "schema should carry the entity name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("tname=users"),
        "schema should carry the SQL table name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("cnames=[<<\"id\">>,<<\"email\">>,<<\"nickname\">>]"),
        "schema should list snake-cased columns in order\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("ctypes=['DbBigInt','DbText','DbText']"),
        "Int → DbBigInt, Text → DbText, Option Text → DbText by convention\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("cnull=[false,false,true]"),
        "the Option field should be the nullable column\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("gcols=[<<\"id\">>]"),
        "the identity id should be the lone database-generated column\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
