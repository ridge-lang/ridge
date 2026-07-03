//! End-to-end check for the `std.schema` / `std.migrate` Ridge-source renderer —
//! running on the BEAM.
//!
//! `schemaToSource`/`columnToSource` render a schema descriptor back to the
//! `schema … |> withColumn (mkColumn … |> …)` builder source it is written as, and
//! `modelToSource`/`migrationToSource` lay a model snapshot and a migration out as the
//! `[ schema … ]` and `migration "name" [ … ]` a generated module holds. This is the
//! writer side of the snapshot auto-diff: a persisted snapshot is a plain Ridge module,
//! and `migrate add` freezes a table's shape into a migration as source.
//!
//! Two things are proven. First, the exact rendered shape (the golden asserts). Second,
//! and the real guarantee, that the rendered text is valid Ridge that rebuilds an equal
//! value: a second module splices the rendered model and migration back in, compiles
//! them, and re-renders — the render is a fixpoint, so the reconstructed value renders
//! to the identical source. A checked column rebuilds through `checkRaw` over the
//! reconstructed `QExpr` tree, and a `DefaultLit` default through `sqlValueSource` (the
//! `SqlValue` rebuilt as its own factory call); both round-trip through the fixpoint even
//! though the phantom-erased schema has lost the entity type the original quote closed over.
//!
//! Gated on `beam-runtime` (real OTP) plus a `which` guard for `erl`/`erlc`.

#![cfg(feature = "beam-runtime")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ridge_driver::{compile_workspace, CompileOptions, EmitArtefacts};

// A model of two entities and a migration touching every diff-produced step — an
// entity-driven create, an entity-driven add-column, an in-place alter, a drop-column,
// and a drop-table — rendered both directly and, for the model and migration, through
// the snapshot/migration writers.
const RENDER_SRC: &str = r#"
import std.schema (EntitySchema, DbBigInt, DbInt, DbText, DbVarchar, Identity, DefaultLit, Cascade, mkColumn, withColumn, schema, generated, primaryKey, unique, indexed, foreignKey, references, onDelete, check, eraseSchema, columnToSource, schemaToSource)
import std.migrate as Migrate
import std.sql (sqlInt)

type Widget = { qty: Int }

fn userSchema () -> EntitySchema Unit =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "email" "email" (DbVarchar 255) false |> unique)
      |> withColumn (mkColumn "bio" "bio" DbText true)
      |> withColumn (mkColumn "logins" "logins" DbInt false |> generated (DefaultLit (sqlInt 0)))

fn postSchema () -> EntitySchema Unit =
    schema "Post" "posts"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "author" "author" DbBigInt false |> foreignKey (references "users" "id" |> onDelete Cascade) |> indexed)

fn widgetSchema () -> EntitySchema Widget =
    schema "Widget" "widgets"
      |> withColumn (mkColumn "qty" "qty" DbInt false |> check (fn (w: Widget) -> w.qty >= 0))

fn model () -> List (EntitySchema Unit) = [ userSchema (), postSchema (), eraseSchema (widgetSchema ()) ]

fn sampleMigration () =
    Migrate.migration "0002_evolve"
      [ Migrate.createSchema (postSchema ())
      , Migrate.addEntityColumn "users" (mkColumn "nickname" "nickname" DbText true)
      , Migrate.alterColumn "users" (mkColumn "bio" "bio" DbText false) (mkColumn "bio" "bio" DbText true)
      , Migrate.dropColumn "users" "legacy"
      , Migrate.dropTable "old_widgets"
      ]

pub fn columnSrc () -> Text = columnToSource (mkColumn "email" "email" (DbVarchar 255) false |> unique)
pub fn checkColSrc () -> Text = columnToSource (mkColumn "qty" "qty" DbInt false |> check (fn (w: Widget) -> w.qty >= 0))
pub fn defaultColSrc () -> Text = columnToSource (mkColumn "logins" "logins" DbInt false |> generated (DefaultLit (sqlInt 0)))
pub fn schemaSrc () -> Text = schemaToSource (userSchema ())
pub fn modelSrc () -> Text = Migrate.modelToSource (model ())
pub fn migrationSrc () -> Text = Migrate.migrationToSource (sampleMigration ())
"#;

// The round-trip module: the rendered model and migration spliced back in (parenthesised
// so their internal layout cannot clash with the surrounding declaration), re-rendered so
// the fixpoint can be checked.
fn build_roundtrip_source(model_src: &str, migration_src: &str) -> String {
    format!(
        r#"
import std.schema (EntitySchema, DbBigInt, DbInt, DbText, DbVarchar, Identity, DefaultLit, Cascade, mkColumn, withColumn, schema, generated, primaryKey, unique, indexed, foreignKey, references, onDelete, checkRaw)
import std.migrate (migration, createSchema, addEntityColumn, alterColumn, dropColumn, dropTable, modelToSource, migrationToSource)
import std.sql (sqlInt)

fn rebuiltModel () -> List (EntitySchema Unit) = (
{model_src}
)

fn rebuiltMigration () = (
{migration_src}
)

pub fn modelReSrc () -> Text = modelToSource (rebuiltModel ())
pub fn migrationReSrc () -> Text = migrationToSource (rebuiltMigration ())
"#
    )
}

fn write_workspace(root: &Path, source: &str) {
    let app_src = root.join("app").join("src");
    std::fs::create_dir_all(&app_src).expect("create workspace dirs");
    std::fs::write(
        root.join("ridge.toml"),
        "[workspace]\nname = \"migrate-source-e2e\"\nversion = \"0.1.0\"\nmembers = [\"app\"]\n",
    )
    .expect("write workspace manifest");
    std::fs::write(
        root.join("app").join("ridge.toml"),
        "[project]\nname = \"app\"\nversion = \"0.1.0\"\nkind = \"app\"\nentry = \"src/Main.ridge\"\n",
    )
    .expect("write project manifest");
    std::fs::write(app_src.join("Main.ridge"), source).expect("write source");
}

fn compile(dir: &Path, cache: &Path) -> (PathBuf, String) {
    let artefacts = compile_workspace(
        CompileOptions::new(dir.to_path_buf())
            .with_emit(EmitArtefacts::Beam)
            .with_cache_root(cache.to_path_buf()),
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
    (beam_dir, module)
}

// Run one zero-arity `Text`-returning function and return its exact output — no name
// prefix and no trailing newline, so the whole stdout is the rendered value.
fn run_fun(beam_dir: &Path, module: &str, fun: &str) -> String {
    let expr = format!("io:format(\"~s\", [{module}:{fun}()]), halt().");
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.is_empty(),
        "`{fun}` produced no output; stderr:\n{stderr}"
    );
    stdout
}

#[test]
fn source_renderer_round_trips_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping source_renderer_round_trips_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-migrate-source-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-migrate-source-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), RENDER_SRC);
    let (beam_dir, module) = compile(dir.path(), cache.path());

    let column_src = run_fun(&beam_dir, &module, "columnSrc");
    let check_col_src = run_fun(&beam_dir, &module, "checkColSrc");
    let default_col_src = run_fun(&beam_dir, &module, "defaultColSrc");
    let schema_src = run_fun(&beam_dir, &module, "schemaSrc");
    let model_src = run_fun(&beam_dir, &module, "modelSrc");
    let migration_src = run_fun(&beam_dir, &module, "migrationSrc");

    // A column renders as `mkColumn name column type nullable` plus the steps it carries,
    // each omitted at its default; a parametric type renders parenthesised.
    assert_eq!(
        column_src,
        r#"mkColumn "email" "email" (DbVarchar 255) false |> unique"#
    );

    // A CHECK renders through `checkRaw` over the reconstructed `QExpr` tree (the erased
    // schema cannot rebuild the original quote); it round-trips through the fixpoint below.
    assert!(
        check_col_src.contains(r#"|> checkRaw (QGe (QCol "qty") (QLitInt 0))"#),
        "check_col_src: {check_col_src}"
    );

    // A `DefaultLit` default renders through `sqlValueSource` — the SqlValue rebuilt as its
    // own factory call, so `sqlInt 0` round-trips as `(DefaultLit (sqlInt 0))`.
    assert_eq!(
        default_col_src,
        r#"mkColumn "logins" "logins" DbInt false |> generated (DefaultLit (sqlInt 0))"#
    );

    // A schema renders single-line: the `schema "Name" "table"` head, then a
    // `|> withColumn (…)` per column in declaration order, an identity primary key and a
    // nullable column among them.
    assert!(
        schema_src.contains(
            r#"schema "User" "users" |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)"#
        ),
        "schema_src: {schema_src}"
    );
    assert!(
        schema_src.contains(
            r#"|> withColumn (mkColumn "email" "email" (DbVarchar 255) false |> unique)"#
        ),
        "schema_src email: {schema_src}"
    );
    assert!(
        schema_src.contains(r#"|> withColumn (mkColumn "bio" "bio" DbText true)"#),
        "schema_src bio: {schema_src}"
    );

    // A model renders one entity per line, leading comma, with a foreign key's
    // `references … |> onDelete` and an index step preserved.
    assert!(
        model_src.starts_with("[ schema \"User\" \"users\""),
        "model_src open: {model_src}"
    );
    assert!(
        model_src.contains("\n, schema \"Post\" \"posts\""),
        "model_src leading-comma: {model_src}"
    );
    assert!(
        model_src
            .contains(r#"|> indexed |> foreignKey (references "users" "id" |> onDelete Cascade))"#),
        "model_src fk: {model_src}"
    );
    assert!(model_src.ends_with("\n]"), "model_src close: {model_src}");

    // A migration renders `migration "name"` then one step per line, leading comma: an
    // entity-driven create delegating to the schema renderer, an add-column, an in-place
    // alter carrying both descriptors, a drop-column, and a drop-table.
    assert!(
        migration_src
            .starts_with("migration \"0002_evolve\"\n  [ createSchema (schema \"Post\" \"posts\""),
        "migration_src open: {migration_src}"
    );
    assert!(
        migration_src.contains(
            r#"  , addEntityColumn "users" (mkColumn "nickname" "nickname" DbText true)"#
        ),
        "migration_src add: {migration_src}"
    );
    assert!(
        migration_src.contains(
            r#"  , alterColumn "users" (mkColumn "bio" "bio" DbText false) (mkColumn "bio" "bio" DbText true)"#
        ),
        "migration_src alter: {migration_src}"
    );
    assert!(
        migration_src.contains(r#"  , dropColumn "users" "legacy""#),
        "migration_src drop-col: {migration_src}"
    );
    assert!(
        migration_src.contains(r#"  , dropTable "old_widgets""#),
        "migration_src drop-table: {migration_src}"
    );
    assert!(
        migration_src.ends_with("\n  ]"),
        "migration_src close: {migration_src}"
    );

    // The round-trip: compile the rendered model and migration back into a module and
    // re-render them. A render fixpoint (re-render == original render) proves the text is
    // valid Ridge that rebuilds an equal value — the property a snapshot depends on.
    let rt_src = build_roundtrip_source(&model_src, &migration_src);
    let rt_dir = tempfile::Builder::new()
        .prefix("ridge-migrate-source-e2e-rt-")
        .tempdir()
        .expect("temp dir");
    let rt_cache = tempfile::Builder::new()
        .prefix("ridge-migrate-source-e2e-rt-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(rt_dir.path(), &rt_src);
    let (rt_beam, rt_module) = compile(rt_dir.path(), rt_cache.path());

    let model_resrc = run_fun(&rt_beam, &rt_module, "modelReSrc");
    let migration_resrc = run_fun(&rt_beam, &rt_module, "migrationReSrc");

    assert_eq!(
        model_resrc, model_src,
        "model source is not a render fixpoint"
    );
    assert_eq!(
        migration_resrc, migration_src,
        "migration source is not a render fixpoint"
    );
}

// A model evolving from v1 (users + posts) to v2 (users with a swapped column + orders,
// posts dropped), rendered into whole snapshot and migration modules through
// `snapshotModule`/`migrationModule`.
const RENDER_MODULE_SRC: &str = r#"
import std.schema (EntitySchema, DbBigInt, DbText, Identity, mkColumn, withColumn, schema, generated, primaryKey)
import std.migrate as Migrate

fn userV1 () -> EntitySchema Unit =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "bio" "bio" DbText true)

fn postV1 () -> EntitySchema Unit =
    schema "Post" "posts"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)

fn modelV1 () -> List (EntitySchema Unit) = [ userV1 (), postV1 () ]

fn userV2 () -> EntitySchema Unit =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "nickname" "nickname" DbText true)

fn orderV2 () -> EntitySchema Unit =
    schema "Order" "orders"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "total" "total" DbBigInt false)

fn modelV2 () -> List (EntitySchema Unit) = [ userV2 (), orderV2 () ]

pub fn snapMod () -> Text = Migrate.snapshotModule (modelV1 ())
pub fn migMod () -> Text = Migrate.migrationModule (Migrate.migration "0002_evolve" (Migrate.diffSchemas (modelV1 ()) (modelV2 ())))
pub fn snapV2Mod () -> Text = Migrate.snapshotModule (modelV2 ())
"#;

// Run one zero-arity function and print its value with `~p`, so a non-`Text` result (a
// model list, a migration record) can be inspected for the names it should carry.
fn run_fun_term(beam_dir: &Path, module: &str, fun: &str) -> String {
    let expr = format!("io:format(\"~p\", [{module}:{fun}()]), halt().");
    let output = Command::new("erl")
        .arg("-noshell")
        .arg("-pa")
        .arg(beam_dir)
        .arg("-eval")
        .arg(&expr)
        .output()
        .expect("run erl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.is_empty(),
        "`{fun}` produced no output; stderr:\n{stderr}"
    );
    stdout
}

// Compile a generated module on its own and run its entry point — proving the emitted
// file is valid Ridge (imports resolve, layout parses, types check) and that the value it
// exposes carries the expected name.
fn check_generated_module(label: &str, source: &str, entry: &str, needle: &str) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("ridge-migrate-module-e2e-{label}-"))
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix(&format!("ridge-migrate-module-e2e-{label}-cache-"))
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), source);
    let (beam, module) = compile(dir.path(), cache.path());
    let out = run_fun_term(&beam, &module, entry);
    assert!(
        out.contains(needle),
        "generated `{label}` module `{entry}()` is missing `{needle}`\noutput: {out}\nsource:\n{source}"
    );
}

#[test]
fn generated_modules_compile_on_beam() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping generated_modules_compile_on_beam");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-migrate-module-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-migrate-module-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), RENDER_MODULE_SRC);
    let (beam_dir, module) = compile(dir.path(), cache.path());

    let snap_mod = run_fun(&beam_dir, &module, "snapMod");
    let mig_mod = run_fun(&beam_dir, &module, "migMod");
    let snap_v2_mod = run_fun(&beam_dir, &module, "snapV2Mod");

    // A snapshot module carries its doc line, the schema import, and a parenthesised
    // `model ()`; a migration module adds the migrate import and a parenthesised `up ()`.
    assert!(
        snap_mod.starts_with("-- Generated model snapshot."),
        "snap_mod doc: {snap_mod}"
    );
    assert!(
        snap_mod.contains("import std.schema (EntitySchema,"),
        "snap_mod schema import: {snap_mod}"
    );
    assert!(
        snap_mod.contains(
            "pub fn model () -> List (EntitySchema Unit) = (\n[ schema \"User\" \"users\""
        ),
        "snap_mod model fn: {snap_mod}"
    );
    assert!(
        mig_mod.contains("import std.migrate (Migration, migration, createSchema,"),
        "mig_mod migrate import: {mig_mod}"
    );
    assert!(
        mig_mod.contains("pub fn up () -> Migration = (\nmigration \"0002_evolve\""),
        "mig_mod up fn: {mig_mod}"
    );
    // The diff of v1 -> v2: create orders, add users.nickname, drop users.bio, drop posts.
    assert!(
        mig_mod.contains(r#"createSchema (schema "Order" "orders""#),
        "mig_mod create: {mig_mod}"
    );
    assert!(
        mig_mod.contains(r#"addEntityColumn "users" (mkColumn "nickname""#),
        "mig_mod add: {mig_mod}"
    );
    assert!(
        mig_mod.contains(r#"dropColumn "users" "bio""#),
        "mig_mod drop-col: {mig_mod}"
    );
    assert!(
        mig_mod.contains(r#"dropTable "posts""#),
        "mig_mod drop-table: {mig_mod}"
    );

    // Each generated module compiles on its own and its entry point runs.
    check_generated_module("snap", &snap_mod, "model", "users");
    check_generated_module("mig", &mig_mod, "up", "0002_evolve");
    check_generated_module("snapv2", &snap_v2_mod, "model", "orders");
}

// A model declared child-before-parent on purpose: `orders` carries a foreign key to
// `users` yet is listed first, and `tags` is independent. The diff must reorder the creates
// so `users` precedes `orders` (a `CREATE TABLE` renders the reference inline, so the target
// must already exist), while the independent `tags` keeps its declaration position.
const RENDER_FK_ORDER_SRC: &str = r#"
import std.schema (EntitySchema, DbBigInt, Identity, Cascade, mkColumn, withColumn, schema, generated, primaryKey, foreignKey, references, onDelete)
import std.migrate as Migrate

fn usersT () -> EntitySchema Unit =
    schema "User" "users"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)

fn ordersT () -> EntitySchema Unit =
    schema "Order" "orders"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)
      |> withColumn (mkColumn "user_id" "user_id" DbBigInt false |> foreignKey (references "users" "id" |> onDelete Cascade))

fn tagsT () -> EntitySchema Unit =
    schema "Tag" "tags"
      |> withColumn (mkColumn "id" "id" DbBigInt false |> generated Identity |> primaryKey)

fn emptyModel () -> List (EntitySchema Unit) = []
fn nextModel () -> List (EntitySchema Unit) = [ ordersT (), usersT (), tagsT () ]

pub fn fkDiffSrc () -> Text = Migrate.migrationToSource (Migrate.migration "0003_fk" (Migrate.diffSchemas (emptyModel ()) (nextModel ())))
"#;

#[test]
fn fk_creates_are_topologically_ordered() {
    if which::which("erlc").is_err() || which::which("erl").is_err() {
        eprintln!("erl/erlc not on PATH — skipping fk_creates_are_topologically_ordered");
        return;
    }

    let dir = tempfile::Builder::new()
        .prefix("ridge-migrate-fk-order-e2e-")
        .tempdir()
        .expect("temp dir");
    let cache = tempfile::Builder::new()
        .prefix("ridge-migrate-fk-order-e2e-cache-")
        .tempdir()
        .expect("cache dir");
    write_workspace(dir.path(), RENDER_FK_ORDER_SRC);
    let (beam_dir, module) = compile(dir.path(), cache.path());

    let fk_src = run_fun(&beam_dir, &module, "fkDiffSrc");

    let users_at = fk_src
        .find(r#"createSchema (schema "User" "users""#)
        .unwrap_or_else(|| panic!("no users create in:\n{fk_src}"));
    let orders_at = fk_src
        .find(r#"createSchema (schema "Order" "orders""#)
        .unwrap_or_else(|| panic!("no orders create in:\n{fk_src}"));
    let tags_at = fk_src
        .find(r#"createSchema (schema "Tag" "tags""#)
        .unwrap_or_else(|| panic!("no tags create in:\n{fk_src}"));

    // The referenced table is created before the table referencing it, even though it was
    // declared second; the independent table keeps its trailing declaration position.
    assert!(
        users_at < orders_at,
        "users must be created before orders:\n{fk_src}"
    );
    assert!(
        orders_at < tags_at,
        "independent tags must keep its declaration position:\n{fk_src}"
    );
}
