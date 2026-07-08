//! `sum`/`avg` fold a numeric column; a non-numeric one is a compile error.
//!
//! The scalar aggregates `sumOf`/`avgOf` and the grouped `g.sum`/`g.avg` answer a
//! number, so their folded column must be numeric (`Int`, `Float`, or `Decimal`).
//! A `Text`/`Bool`/`Uuid`/`Bytes`/`Timestamp` column has a `SqlType` instance — it
//! orders, compares, and folds through `min`/`max` — but summing or averaging it is
//! meaningless, so it is rejected at the call site (`T040`) instead of reaching a
//! backend that would raise a runtime type error. `min`/`max` stay unrestricted.
//!
//! Pure type-check tests (`check_workspace`), no runtime needed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::redundant_clone
)]

mod common;
use common::make_workspace;
use ridge_driver::{check_workspace, CheckOptions};

/// A scalar `sumOf` over an `Int` column type-checks cleanly — the baseline the
/// negative cases are measured against.
#[test]
fn scalar_sum_over_numeric_is_clean() {
    let source = "
import std.data (memAdapter, MemAdapter)
import std.repo as Repo

pub type Sale = { id: Int, qty: Int } deriving (Row, Schema)

pub fn total (r: Repo Sale MemAdapter) -> Result (Option Int) Error =
    r |> Repo.query |> Repo.sumOf (fn (s: Sale) -> s.qty)
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.is_empty(),
        "summing a numeric column is valid; got {:?}",
        result.diagnostics
    );
}

/// A scalar `sumOf` over a `Text` column is the targeted `T040`, not a runtime
/// failure: summing text is meaningless.
#[test]
fn scalar_sum_over_text_is_t040() {
    let source = "
import std.data (memAdapter, MemAdapter)
import std.repo as Repo

pub type Sale = { id: Int, label: Text } deriving (Row, Schema)

pub fn total (r: Repo Sale MemAdapter) -> Result (Option Text) Error =
    r |> Repo.query |> Repo.sumOf (fn (s: Sale) -> s.label)
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.iter().any(|d| d.code == "T040"),
        "expected T040 (sum over a non-numeric column); got {:?}",
        result.diagnostics
    );
}

/// `maxOf` over a `Uuid` column is fine — `min`/`max` fold any comparable column,
/// so the numeric bound applies only to `sum`/`avg`.
#[test]
fn scalar_max_over_uuid_is_clean() {
    let source = "
import std.data (memAdapter, MemAdapter)
import std.repo as Repo

pub type Doc = { id: Int, token: Uuid } deriving (Row, Schema)

pub fn newest (r: Repo Doc MemAdapter) -> Result (Option Uuid) Error =
    r |> Repo.query |> Repo.maxOf (fn (d: Doc) -> d.token)
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.is_empty(),
        "min/max over a comparable column is valid; got {:?}",
        result.diagnostics
    );
}

/// A grouped `g.sum` over a `Text` column is rejected the same way as the scalar
/// form — the numeric bound applies on both aggregate surfaces.
#[test]
fn grouped_sum_over_text_is_t040() {
    let source = "
import std.data (memAdapter, MemAdapter)
import std.repo as Repo

pub type Sale = { id: Int, dept: Text, label: Text } deriving (Row, Schema)
pub type DeptSum = { dept: Text, total: Text } deriving (Row)

pub fn perDept (r: Repo Sale MemAdapter) -> Result (List DeptSum) Error =
    r
    |> Repo.query
    |> Repo.groupBy (fn (s: Sale) -> s.dept)
    |> Repo.summarize (fn g -> DeptSum { dept = g.key, total = g.sum (fn (s: Sale) -> s.label) })
";
    let tw = make_workspace("Models", source);
    let result = check_workspace(CheckOptions::new(tw.path.clone())).expect("check ran");
    assert!(
        result.diagnostics.iter().any(|d| d.code == "T040"),
        "expected T040 (grouped sum over a non-numeric column); got {:?}",
        result.diagnostics
    );
}
