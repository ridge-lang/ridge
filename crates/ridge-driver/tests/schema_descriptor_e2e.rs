//! End-to-end check for the `std.schema` descriptor — running on the BEAM.
//!
//! `std.schema` is the typed, persistence-side companion to a domain record: it
//! names an entity's SQL table and the per-column mapping (type, nullability,
//! generation, constraints). A descriptor is built by hand through `mkColumn` and
//! the pipe-friendly refinement steps, collected with `withColumn`, and read back
//! through the accessors.
//!
//! The module is pure Ridge over reconciled descriptor types, so it carries no
//! runtime FFI — this oracle is what proves the whole layer lowers and runs: it
//! builds a two-column schema (an identity id, a unique email), then reads the
//! table name, the entity name, and the database-generated column set back. That
//! exercises constructing the reconciled `ColumnSchema`/`EntitySchema` records and
//! the `DbType`/`Generation` constructors from stdlib source, the setter rebuilds,
//! `withColumn`'s list append, and the accessor reads (including `colGenerated`'s
//! match on `Generation`).
//!
//! A second schema builds the same shape through quoted accessors — `column (fn (u:
//! User) -> u.field)` instead of a string name — and a checked column (`check (fn (u:
//! User) -> u.age >= 0)`). That proves the accessor capture lowers to a column
//! reference and the predicate capture lowers to a stored `QExpr`, both round-tripping
//! through the BEAM exactly as a `set`/`filter` quote does.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.sql (DbType, DbBigInt, DbText, DbInt)
import std.schema (Generation, Identity, ColumnSchema, mkColumn, column, withColumn, schema, generated, primaryKey, unique, check, schemaName, schemaTable, schemaColumns, colColumn, colCheck, generatedColumns)
import std.text as Text

-- A small two-column schema built from explicit strings: an identity primary-key
-- id and a unique email.
fn sampleSchema () =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "email" "email" DbText false |> unique)

-- The SQL table name read back off the descriptor.
pub fn schemaTableName () -> Text = schemaTable (sampleSchema ())

-- The entity name read back off the descriptor.
pub fn schemaEntityName () -> Text = schemaName (sampleSchema ())

-- The database-generated columns — only the identity `id`, since `email` is
-- caller-supplied. Joined so the set is visible in the assertion.
pub fn generatedCols () -> Text = Text.join "," (generatedColumns (sampleSchema ()))

-- The domain record the typed schema below describes. Persistence-ignorant: the
-- descriptor is its separate mapping companion.
type User = { id: Int, email: Text, age: Int }

-- The same shape built through quoted accessors rather than strings, plus a checked
-- column: each `column` names a field with `fn (u: User) -> u.field` (the capture
-- `set`/`onConflict` use), and `age` carries a CHECK predicate.
fn typedSchema () =
    schema "User" "users"
      |> withColumn (column (fn (u: User) -> u.id) DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (column (fn (u: User) -> u.email) DbText false |> unique)
      |> withColumn (column (fn (u: User) -> u.age) DbInt false |> check (fn (u: User) -> u.age >= 0))

-- The SQL column names the accessors captured, in declaration order — proves each
-- `column` quote lowered to a column reference and ran.
pub fn typedColumns () -> Text = Text.join "," (columnNames (schemaColumns (typedSchema ())))

-- The columns carrying a CHECK constraint — proves the quoted predicate was
-- captured, lowered to a `QExpr`, and stored on the column.
pub fn checkedColumns () -> Text = Text.join "," (checkedNames (schemaColumns (typedSchema ())))

fn columnNames (cols: List (ColumnSchema User)) -> List Text =
    match cols
        []        -> []
        c :: rest -> colColumn c :: columnNames rest

fn checkedNames (cols: List (ColumnSchema User)) -> List Text =
    match cols
        []        -> []
        c :: rest ->
            match colCheck c
                Some _ -> colColumn c :: checkedNames rest
                None   -> checkedNames rest
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"schema-descriptor-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn schema_descriptor_builds_and_reads_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping schema_descriptor_builds_and_reads_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-schema-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-schema-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path());

    let artefacts = compile_workspace(
        CompileOptions::new(dir.path().to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.path().to_path_buf()),
    )
    .expect("compile to BEAM");

    assert!(
        artefacts.diagnostics.is_empty(),
        "expected a clean compile, got diagnostics: {:?}",
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
         lists:foreach(F,['schemaTableName','schemaEntityName','generatedCols',\
         'typedColumns','checkedColumns']), halt()."
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

    let want = |needle: &str| {
        assert!(
            stdout.contains(needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    };

    want("schemaTableName=users");
    want("schemaEntityName=User");
    want("generatedCols=id");
    want("typedColumns=id,email,age");
    want("checkedColumns=age");
}
