//! End-to-end check for the `HasSchema` binding — running on the BEAM.
//!
//! `HasSchema` binds an entity type to its `EntitySchema` descriptor, the way
//! EF's `IEntityTypeConfiguration<T>` binds a type to its mapping. The single
//! method `schemaOf` answers the schema from the type alone, so it takes a
//! phantom `Option e` witness that fixes `e` and selects the instance — the same
//! dispatch `Row.rowColumns` uses.
//!
//! This oracle declares two hand-written instances (`User` and `Product`) over
//! distinct tables, then reads each entity's schema back through a witness typed
//! at that entity. Both instances coexist and the witness routes to the right one
//! — the real proof that the binding dispatches by type rather than collapsing to
//! a single schema. It also reads the bound table name, entity name, and the
//! database-generated column set off the descriptor the instance returns, so the
//! whole select-and-read path runs on the BEAM.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.schema (DbBigInt, DbText, DbInt, Identity, mkColumn, withColumn, schema, generated, primaryKey, unique, schemaName, schemaTable, generatedColumns, EntitySchema, HasSchema, schemaOf)
import std.text as Text
import std.map (fromList)
import std.sql (toSql, SqlValue)

-- Two persistence-ignorant domain records, each with its own mapping below.
type User = { id: Int, email: Text, age: Int }
type Product = { id: Int, sku: Text }

-- A hand-written `HasSchema` instance binds `User` to its table and columns: an
-- identity primary-key id, a unique email, and a plain age. `deriving (Schema)`
-- will synthesise this shape; spelling it out here exercises the class directly.
instance HasSchema User =
    schemaOf (_w: Option User) -> EntitySchema User =
        schema "User" "users"
          |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
          |> withColumn (mkColumn "email" "email" DbText false |> unique)
          |> withColumn (mkColumn "age" "age" DbInt false)
    toInsertRow (shape: InsertShape User) -> Map Text SqlValue = fromList [("email", toSql shape.email), ("age", toSql shape.age)]

-- A second instance over a different table. Its presence proves the witness
-- discriminates: `schemaOf` must route each call to the matching schema.
instance HasSchema Product =
    schemaOf (_w: Option Product) -> EntitySchema Product =
        schema "Product" "products"
          |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
          |> withColumn (mkColumn "sku" "sku" DbText false |> unique)
    toInsertRow (shape: InsertShape Product) -> Map Text SqlValue = fromList [("sku", toSql shape.sku)]

-- A witness typed at each entity — the value a caller threads to select the
-- instance, the same role `entityWitness` plays for `Row.rowColumns`.
fn userWitness () -> Option User = None
fn productWitness () -> Option Product = None

-- The bound table names, read off whichever schema the witness selects.
pub fn userTable () -> Text = schemaTable (schemaOf (userWitness ()))
pub fn productTable () -> Text = schemaTable (schemaOf (productWitness ()))

-- The bound entity name and the database-generated column set for `User`.
pub fn userEntity () -> Text = schemaName (schemaOf (userWitness ()))
pub fn userGenerated () -> Text = Text.join "," (generatedColumns (schemaOf (userWitness ())))
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"schema-instance-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
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
fn has_schema_instance_dispatches_by_type_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping has_schema_instance_dispatches_by_type_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-schema-instance-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-schema-instance-e2e-cache-")
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
         lists:foreach(F,['userTable','productTable','userEntity','userGenerated']), halt()."
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

    want("userTable=users");
    want("productTable=products");
    want("userEntity=User");
    want("userGenerated=id");
}
