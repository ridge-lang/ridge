//! Insert companions — `deriving (Schema)` synthesizes an `<Entity>Insert` record.
//!
//! When an entity's schema marks database-generated columns (by the convention, a
//! non-null integer `id` — the serial primary key), `deriving (Schema)` also
//! synthesizes a companion record that is the entity minus those columns. The
//! companion is the value a typed insert accepts, so a serial/identity column is
//! simply absent from it and cannot be written by hand. An entity with no
//! generated column gets no companion — its insert shape is the entity itself.
//!
//! These tests cover the type-level foundation: the companion is synthesized,
//! user-constructable, drops exactly the generated columns, and is not created
//! for an entity that has none. The `insert`/`insertMany` verbs are wired to the
//! `InsertShape` projection separately.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::redundant_clone
)]

mod common;
use common::make_workspace;
use ridge_driver::{check_workspace, CheckOptions};

/// `UserInsert` is synthesized for an entity with a serial `id` and is
/// constructable with exactly the caller-supplied fields — no `id`.
#[test]
fn companion_synthesized_and_constructable() {
    let source = "
pub type User = { id: Int, email: Text, nickname: Option Text } deriving (Schema)

pub fn newUser () -> UserInsert = UserInsert { email = \"a@b.com\", nickname = Some \"ab\" }
pub fn pickEmail (u: UserInsert) -> Text = u.email
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.is_empty(),
        "expected a clean check; got {:?}",
        result.diagnostics
    );
}

/// The companion drops exactly the generated columns: writing the serial `id` by
/// hand is a type error because the companion has no `id` field.
#[test]
fn companion_rejects_generated_column() {
    let source = "
pub type User = { id: Int, email: Text } deriving (Schema)

pub fn bad () -> UserInsert = UserInsert { id = 1, email = \"a@b.com\" }
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        !result.diagnostics.is_empty(),
        "expected a type error: the companion has no `id` field to set"
    );
}

/// Passing the full entity where a typed insert expects its `<Entity>Insert`
/// companion is the targeted `T047`, not a bare `T001`: the diagnostic names the
/// companion and points at the generated column to drop.
#[test]
fn full_entity_to_insert_is_t047() {
    let source = "
import std.data (memAdapter, MemAdapter)
import std.repo as Repo

pub type User = { id: Int, email: Text } deriving (Row, Schema)

pub fn seed (r: Repo User MemAdapter) -> Result Unit Error =
    Repo.insert (User { id = 1, email = \"a@b.com\" }) r
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.iter().any(|d| d.code == "T047"),
        "expected T047 (full entity where the insert shape is expected); got {:?}",
        result.diagnostics
    );
}

/// An entity with no generated column gets no companion — its insert shape is the
/// entity itself — so referencing `<Entity>Insert` is an unknown type.
#[test]
fn no_companion_without_generated_columns() {
    let source = "
pub type Tag = { name: Text, slug: Text } deriving (Schema)

pub fn bad () -> TagInsert = TagInsert { name = \"x\", slug = \"y\" }
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        !result.diagnostics.is_empty(),
        "expected an unknown-type error: no companion is synthesized without a generated column"
    );
}

/// A nullable or non-integer `id` is supplied by the caller, not generated, so the
/// entity has no generated column and no companion: `id` stays a field the caller
/// sets and `<Entity>Insert` does not exist.
#[test]
fn nullable_id_is_not_generated() {
    let source = "
pub type Doc = { id: Option Int, body: Text } deriving (Schema)

pub fn bad () -> DocInsert = DocInsert { body = \"x\" }
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        !result.diagnostics.is_empty(),
        "expected an unknown-type error: a nullable `id` is caller-supplied, so no companion exists"
    );
}
