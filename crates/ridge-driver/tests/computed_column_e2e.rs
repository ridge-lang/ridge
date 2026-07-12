//! End-to-end check for stored generated (computed) columns.
//!
//! A hand-written schema marks `total` a stored generated column — `total = qty * price`,
//! computed by the database on every write. This renders the column DDL for both dialects
//! (`GENERATED ALWAYS AS (…) STORED`, the same on Postgres 12+ and SQLite 3.31+), round-trips
//! the schema back to source (a phantom-erased schema rebuilds the column through
//! `generated (Computed <tree>)`), and, on SQLite, inserts a row without the computed column
//! and reads the value the database filled in — proving it is omitted on write and computed
//! on read.
//!
//! Gated on `beam-runtime` (real OTP + the baked SQLite NIF) plus a `which` guard.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

const SOURCE: &str = r#"
import std.data (connectSqlite, sqliteMemory, Sqlite, appendRow)
import std.migrate as Migrate
import std.repo as Repo
import std.schema (EntitySchema, schema, withColumn, mkColumn, generated, primaryKey, computed, eraseSchema, schemaToDdl, schemaToDdlFor, schemaToSource, Identity)
import std.sql (DbBigInt, SqliteDialect, sqlInt)
import std.map as Map

pub type Order = { id: Int, qty: Int, price: Int, total: Int } deriving (Row)

-- A hand-written schema with a stored generated column: `total` is `qty * price`, computed
-- and stored by the database, never supplied on insert. `deriving (Schema)` cannot mark a
-- field computed, so the schema is spelled out.
fn orderSchema () -> EntitySchema Order =
    schema "Order" "orders"
        |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
        |> withColumn (mkColumn "qty" "qty" DbBigInt false)
        |> withColumn (mkColumn "price" "price" DbBigInt false)
        |> withColumn (mkColumn "total" "total" DbBigInt true |> computed (fn (o: Order) -> o.qty * o.price))

-- The rendered CREATE TABLE for each dialect — the `GENERATED ALWAYS AS (…) STORED` clause
-- is spelled the same either way; only the column type differs.
pub fn ddlPg () -> Text = schemaToDdl (orderSchema ())
pub fn ddlLite () -> Text = schemaToDdlFor SqliteDialect (orderSchema ())

-- The snapshot source: a phantom-erased schema has lost the quote's entity type, so the
-- computed column round-trips through `generated (Computed <tree>)` rather than a rebuilt
-- `computed` quote.
pub fn srcOut () -> Text = schemaToSource (eraseSchema (orderSchema ()))

fn setup (conn: Sqlite) -> Result (List Text) Error =
    Migrate.run conn [ Migrate.migration "0001_orders" [ Migrate.createSchema (orderSchema ()) ] ]

-- Insert without `total` (or `id`): both are database-generated, so the row map carries only
-- the supplied columns.
fn insertOrder (conn: Sqlite) (q: Int) (p: Int) -> Result Unit Error =
    appendRow conn "orders" (Map.fromList [("qty", sqlInt q), ("price", sqlInt p)])

fn readOrders (conn: Sqlite) -> Result (List Order) Error =
    let orders: Repo Order Sqlite = Repo.repo conn "orders"
    Repo.all orders

-- Insert qty=3, price=5; the database computes total=15 and the read decodes it.
pub fn db computedTotal () -> Int =
    match connectSqlite (sqliteMemory ())
        Err _ -> 0 - 1
        Ok conn ->
            match setup conn
                Err _ -> 0 - 2
                Ok _  ->
                    match insertOrder conn 3 5
                        Err _ -> 0 - 3
                        Ok _  ->
                            match readOrders conn
                                Ok (o :: _) -> o.total
                                Ok []       -> 0 - 4
                                Err _       -> 0 - 5
"#;

fn write_workspace(root: &std::path::Path) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"computed-column-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n\n[capabilities]\nallow = [\"db\"]\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), SOURCE).expect("write source");
}

#[test]
fn computed_column_renders_and_computes() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping computed_column_renders_and_computes");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-computed-column-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-computed-column-e2e-cache-")
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
        "io:format(\"ddlPg=~s~n\",[{module}:ddlPg()]), \
         io:format(\"ddlLite=~s~n\",[{module}:ddlLite()]), \
         io:format(\"src=~s~n\",[{module}:srcOut()]), \
         io:format(\"total=~w~n\",[{module}:computedTotal()]), \
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

    let want = |needle: &str| {
        assert!(
            stdout.contains(needle),
            "expected `{needle}`\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    };

    // The Postgres and SQLite column DDL: the generated clause is identical, the type differs
    // (`bigint` vs `INTEGER`), so each fragment pins one dialect's render.
    want(r#""total" bigint GENERATED ALWAYS AS (("qty" * "price")) STORED"#);
    want(r#""total" INTEGER GENERATED ALWAYS AS (("qty" * "price")) STORED"#);

    // The snapshot round-trip rebuilds the column through `generated (Computed <tree>)`.
    want(r#" |> generated (Computed (QMul (QCol "qty") (QCol "price")))"#);

    // The database computed the omitted column: qty 3 * price 5 = 15, read back off the row.
    want("total=15");
}
