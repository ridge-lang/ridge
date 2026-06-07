//! Column codegen — `deriving (Table)` generates a typed column mirror.
//!
//! For a record entity, `deriving (Table)` emits a column-mirror type, a
//! column-mirror value, and a table-metadata value. The type-level tests here
//! prove the mirror checks (and that a nonexistent column is a compile error);
//! the runtime test proves the generated values survive to the BEAM.
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

/// A `User` entity that derives its column mirror. No imports needed —
/// `deriving (Table)` synthesizes everything from compiler builtins.
const SCHEMA: &str = r"
pub type User = { id: Int, email: Text, age: Int } deriving (Table)
";

/// The generated mirror type-checks: `userCols.<field>` is a `Column User <T>`
/// whose `.name` is `Text`, and the table-metadata value exists.
#[test]
fn column_mirror_typechecks() {
    let source = format!(
        "{SCHEMA}\n\
         pub fn idColumnName () -> Text = userCols.id.name\n\
         pub fn ageColumnName () -> Text = userCols.age.name\n\
         pub fn theTableName () -> Text = userTable.name\n"
    );
    let tw = make_workspace("Models", &source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.is_empty(),
        "expected a clean check; got {:?}",
        result.diagnostics
    );
}

/// Selecting a column that does not exist is a compile error — the payoff of
/// typed columns.
#[test]
fn nonexistent_column_is_compile_error() {
    let source = format!("{SCHEMA}\npub fn bad () -> Text = userCols.nope.name\n");
    let tw = make_workspace("Models", &source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        !result.diagnostics.is_empty(),
        "expected a type error for a nonexistent column, got a clean check"
    );
}

/// The generated values survive to the BEAM: each column carries its SQL name
/// and table, and the table metadata lists the columns in order.
///
/// Gated on `beam-runtime` and a `which` guard for `erl`/`erlc`.
#[cfg(feature = "beam-runtime")]
#[test]
fn column_mirror_roundtrip_survives_beam() {
    use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};
    use std::process::Command;

    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping column_mirror_roundtrip_survives_beam");
        return;
    }

    let source = format!(
        "{SCHEMA}\n\
         pub fn ageColName () -> Text = userCols.age.name\n\
         pub fn ageColTable () -> Text = userCols.age.table\n\
         pub fn theTableName () -> Text = userTable.name\n\
         pub fn tableColumns () -> List Text = userTable.columns\n"
    );
    let tw = make_workspace("Models", &source);
    let cache = tempfile::Builder::new()
        .prefix("ridge-column-codegen-cache-")
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
        "io:format(\"age_name=~s~n\",[{module}:ageColName()]), \
         io:format(\"age_table=~s~n\",[{module}:ageColTable()]), \
         io:format(\"tname=~s~n\",[{module}:theTableName()]), \
         io:format(\"cols=~p~n\",[{module}:tableColumns()]), \
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
        stdout.contains("age_name=age"),
        "column should carry its SQL name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("age_table=users"),
        "column should carry its table name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("tname=users"),
        "table metadata should carry the table name\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("cols=[<<\"id\">>,<<\"email\">>,<<\"age\">>]"),
        "table metadata should list columns in order\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
