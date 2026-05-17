//! Snapshot tests for the four example Ridge programs (`DoD §13`) plus the
//! T2 `@ffi` attribute fixtures.
//!
//! Each test asserts `lex_errors.is_empty()` and `errors.is_empty()` then
//! locks the parsed `Module` in an `insta` snapshot.  Run
//! `cargo insta review` to accept new/changed snapshots.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::doc_markdown
)]

use ridge_parser::parse_source;

const LOG_ANALYZER: &str = include_str!("../../../examples/log_analyzer.rg");
const URL_SHORTENER: &str = include_str!("../../../examples/url_shortener.rg");
const GAME_OF_LIFE: &str = include_str!("../../../examples/game_of_life.rg");
const RATE_LIMITER: &str = include_str!("../../../examples/rate_limiter.rg");

fn assert_example_parses_clean(name: &str, src: &str) {
    let result = parse_source(src);
    assert!(
        result.lex_errors.is_empty(),
        "lex errors in {name}: {:#?}",
        result.lex_errors
    );
    assert!(
        result.errors.is_empty(),
        "parse errors in {name}: {:#?}",
        result.errors
    );
    insta::assert_debug_snapshot!(name, result.module);
}

#[test]
fn snapshot_log_analyzer() {
    assert_example_parses_clean("log_analyzer", LOG_ANALYZER);
}

#[test]
fn snapshot_url_shortener() {
    assert_example_parses_clean("url_shortener", URL_SHORTENER);
}

#[test]
fn snapshot_game_of_life() {
    assert_example_parses_clean("game_of_life", GAME_OF_LIFE);
}

#[test]
fn snapshot_rate_limiter() {
    assert_example_parses_clean("rate_limiter", RATE_LIMITER);
}

// ── T2: @ffi attribute snapshot fixtures ─────────────────────────────────────
//
// These two snapshots cover the two surface-syntax shapes from §5.1:
//
//   1. Minimal — no capabilities, no generic cap-variables:
//      `@ffi("erlang", "+", 2)\npub fn add (a: Int) (b: Int) -> Int`
//
//   2. With capabilities and cap-variables (as stdlib HOFs will look):
//      `@ffi("lists", "map", 2)\npub fn ffi map [c] (f: fn a -> b) (xs: [a]) -> [b]`
//
// Both must parse with zero lex errors, zero parse errors, and produce a
// `Body::Ffi { .. }` in the locked snapshot.

/// T2-ffi-1: minimal `@ffi` with two annotated params and a return type.
///
/// Corresponds to §5.1's first example:
/// ```ridge
/// @ffi("erlang", "+", 2)
/// pub fn add (a: Int) (b: Int) -> Int
/// ```
#[test]
fn snapshot_ffi_minimal() {
    let src = "@ffi(\"erlang\", \"+\", 2)\npub fn add (a: Int) (b: Int) -> Int\n";
    let result = parse_source(src);
    assert!(
        result.lex_errors.is_empty(),
        "unexpected lex errors: {:#?}",
        result.lex_errors
    );
    assert!(
        result.errors.is_empty(),
        "unexpected parse errors: {:#?}",
        result.errors
    );
    assert_eq!(result.module.items.len(), 1, "expected 1 item");
    // Assert the body is Body::Ffi, not Body::Expr.
    let ridge_ast::Item::Fn(fn_decl) = &result.module.items[0] else {
        panic!("expected Item::Fn");
    };
    assert!(
        matches!(&fn_decl.body, ridge_ast::Body::Ffi { module, name, arity }
            if module == "erlang" && name == "+" && *arity == 2),
        "expected Body::Ffi {{ module: \"erlang\", name: \"+\", arity: 2 }}, got {:?}",
        fn_decl.body
    );
    insta::assert_debug_snapshot!("ffi_minimal", result.module);
}

/// T2-ffi-2: `@ffi` with capability annotation and HOF-style parameters.
///
/// Corresponds to §5.1's second example:
/// ```ridge
/// @ffi("lists", "map", 2)
/// pub fn ffi map (f: fn Int -> Int) (xs: [Int]) -> [Int]
/// ```
///
/// Note: The plan's surface syntax uses cap-variables `[c]` which are generic
/// capability annotations (`ffi` is the declared capability here).  This
/// fixture uses `ffi` as the capability prefix per §4.1 CapList.
#[test]
fn snapshot_ffi_with_caps() {
    let src =
        "@ffi(\"lists\", \"map\", 2)\npub fn ffi map (f: fn Int -> Int) (xs: [Int]) -> [Int]\n";
    let result = parse_source(src);
    assert!(
        result.lex_errors.is_empty(),
        "unexpected lex errors: {:#?}",
        result.lex_errors
    );
    assert!(
        result.errors.is_empty(),
        "unexpected parse errors: {:#?}",
        result.errors
    );
    assert_eq!(result.module.items.len(), 1, "expected 1 item");
    let ridge_ast::Item::Fn(fn_decl) = &result.module.items[0] else {
        panic!("expected Item::Fn");
    };
    assert!(
        matches!(&fn_decl.body, ridge_ast::Body::Ffi { module, name, arity }
            if module == "lists" && name == "map" && *arity == 2),
        "expected Body::Ffi {{ module: \"lists\", name: \"map\", arity: 2 }}, got {:?}",
        fn_decl.body
    );
    assert!(
        fn_decl.caps.contains(&ridge_ast::Capability::Ffi),
        "expected Ffi capability in caps: {:?}",
        fn_decl.caps
    );
    insta::assert_debug_snapshot!("ffi_with_caps", result.module);
}
