//! Phase 5 snapshot harness — 16 micro-fixture tests.
//!
//! Each fixture under `tests/fixtures/lower/*.ridge` is run through the full
//! pipeline (discover → resolve → typecheck → lower) and the resulting
//! `LoweredModule` is snapshot-asserted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::doc_markdown
)]

mod common;
use common::{make_workspace, render_lowered_module, run_pipeline};
use std::fs;
use std::path::Path;

/// Run the full pipeline for a fixture file and assert it snapshots correctly.
fn snapshot_fixture(fixture_name: &str) {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lower")
        .join(format!("{fixture_name}.ridge"));

    let source = fs::read_to_string(&fixture_path)
        .unwrap_or_else(|e| panic!("could not read fixture {}: {e}", fixture_path.display()));

    // Use a unique ID per fixture so parallel tests don't collide.
    let tw = make_workspace(fixture_name, fixture_name, &source);
    let result = run_pipeline(&tw.path);

    // The workspace has exactly one module.
    assert!(
        !result.lowered.modules.is_empty(),
        "no modules in lowered workspace for fixture {fixture_name}"
    );
    let module_opt = &result.lowered.modules[0];
    assert!(
        module_opt.is_some(),
        "module[0] is None for fixture {fixture_name}"
    );

    let rendered = render_lowered_module(module_opt.as_ref().unwrap());
    insta::assert_snapshot!(fixture_name, rendered);
}

// ── Pipe ──────────────────────────────────────────────────────────────────────

#[test]
fn snap_pipe_simple() {
    snapshot_fixture("pipe_simple");
}

#[test]
fn snap_pipe_chained() {
    snapshot_fixture("pipe_chained");
}

// ── Propagate ─────────────────────────────────────────────────────────────────

#[test]
fn snap_propagate_result() {
    snapshot_fixture("propagate_result");
}

#[test]
fn snap_propagate_option() {
    snapshot_fixture("propagate_option");
}

// ── Try block ─────────────────────────────────────────────────────────────────

#[test]
fn snap_try_block_basic() {
    snapshot_fixture("try_block_basic");
}

#[test]
fn snap_try_block_nested_propagate() {
    snapshot_fixture("try_block_nested_propagate");
}

// ── Guard ─────────────────────────────────────────────────────────────────────

#[test]
fn snap_guard_single() {
    snapshot_fixture("guard_single");
}

#[test]
fn snap_guard_multi() {
    snapshot_fixture("guard_multi");
}

// ── Interpolation ─────────────────────────────────────────────────────────────

#[test]
fn snap_interp_int() {
    snapshot_fixture("interp_int");
}

#[test]
fn snap_interp_mixed_types() {
    snapshot_fixture("interp_mixed_types");
}

// ── With update ───────────────────────────────────────────────────────────────

#[test]
fn snap_with_simple() {
    snapshot_fixture("with_simple");
}

#[test]
fn snap_with_chained() {
    snapshot_fixture("with_chained");
}

// ── Actor ─────────────────────────────────────────────────────────────────────

#[test]
fn snap_actor_dispatch_no_init() {
    snapshot_fixture("actor_dispatch_no_init");
}

#[test]
fn snap_actor_dispatch_with_init() {
    snapshot_fixture("actor_dispatch_with_init");
}

// ── Inner fn ─────────────────────────────────────────────────────────────────

#[test]
fn snap_inner_fn_basic() {
    snapshot_fixture("inner_fn_basic");
}

#[test]
fn snap_inner_fn_recursive() {
    snapshot_fixture("inner_fn_recursive");
}

// ── Ask timeout (Phase 6 T0, OQ-E001) ────────────────────────────────────────

/// T0-1: `?> handler()` with no timeout postfix → IR `timeout: None` (default).
#[test]
fn snap_ask_default_timeout() {
    snapshot_fixture("ask_default_timeout");
}

/// T0-2: `?> handler() timeout 1000` → IR `timeout: Some(Millis(Lit(Int(1000))))`.
#[test]
fn snap_ask_explicit_timeout() {
    snapshot_fixture("ask_explicit_timeout");
}

/// T0-3: `?> handler() timeout never` → IR `timeout: Some(Never)`.
#[test]
fn snap_ask_never_timeout() {
    snapshot_fixture("ask_never_timeout");
}

// ── Phase 5 followup B-1/B-2/B-3/B-5 micro-fixtures ─────────────────────────

/// B-1: Prelude constructors (Ok/Err/Some/None) lower to SymbolRef::Prelude.
#[test]
fn snap_prelude_ctor_ok() {
    snapshot_fixture("prelude_ctor_ok");
}

/// B-2: Lambda with tuple-pattern params synthesises a Match wrapper.
#[test]
fn snap_lambda_tuple_param() {
    snapshot_fixture("lambda_tuple_param");
}

/// B-3: Partial application wraps Call in a synthetic Lambda.
#[test]
fn snap_partial_app() {
    snapshot_fixture("partial_app");
}

/// B-5: Actor state-field reads lower to Field { Local("__state"), field }.
#[test]
fn snap_actor_state_field_read() {
    snapshot_fixture("actor_state_field_read");
}

// ── Group B §3.1: Actor-name → ModuleId wiring ───────────────────────────────

// B-actor-1: Spawn expression resolves actor module via actor_module_cache.
//
// A single-module program that spawns an actor declared in the same module.
// After §3.1 wiring, the Spawn's ActorType symbol must carry the real
// ModuleId (m0 for a single-module workspace), not a placeholder.
//
// Because the test workspace has exactly one module, m0 is both the
// declaring module and the current module — so BindingMap + bare-name cache
// + current-module fallback all agree on ModuleId(0).
#[test]
fn actor_spawn_carries_correct_module_id() {
    let source = r"
actor Counter =
    state count: Int = 0
    on increment = ()

fn main = spawn Counter
";
    let tw = make_workspace("b_actor_module_spawn", "main", source);
    let result = run_pipeline(&tw.path);

    let m = result.lowered.modules[0].as_ref().unwrap();
    let rendered = render_lowered_module(m);

    // The rendered snapshot must contain `ActorType(Counter @ m0)` — the
    // real module index, not a stub.
    assert!(
        rendered.contains("ActorType(Counter @ m0)"),
        "spawn must carry ActorType(Counter @ m0); rendered:\n{rendered}"
    );
}

// B-actor-2: actor_module_cache correctly resolves actors in the current module
// (same-module dominant case). After the cache is built it is reused across
// multiple lookups without re-scanning the workspace.
//
// This test also exercises the cross-module fallback path: when the Spawn
// ident has a BindingMap entry for `ActorName { module: ModuleId(0) }`, the
// BindingMap result is returned immediately (step 1 of the 3-step precedence).
// The rendered ActorType must show m0, not m1 or another index.
#[test]
fn actor_module_cache_cross_actor_resolution() {
    // Two actors in the same module; both must resolve to m0.
    let source = r"
actor A =
    state x: Int = 0
    on get -> Int = 0

actor B =
    state y: Int = 0
    on run = ()

fn main =
    let a = spawn A
    spawn B
";
    let tw = make_workspace("b_actor_cache_multi", "main", source);
    let result = run_pipeline(&tw.path);

    let m = result.lowered.modules[0].as_ref().unwrap();
    let rendered = render_lowered_module(m);

    assert!(
        rendered.contains("ActorType(A @ m0)"),
        "actor A must resolve to m0; rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("ActorType(B @ m0)"),
        "actor B must resolve to m0; rendered:\n{rendered}"
    );
}

// ── Group B §3.2: Constructor TyConId via BindingMap ─────────────────────────

// B-ctor-1: Union-variant constructor pattern resolves to the correct TyConId.
//
// `type LogLevel = Info | Warn | Error` declares the `LogLevel` union.
// After §3.2 wiring, the Ctor patterns in a match arm must carry the real
// TyConId for `LogLevel`, not the placeholder TyConId(0).
//
// The exact TyConId depends on the built-in tycon count and declaration order,
// so we assert that `owner` is NOT 0 (which would be the Int built-in, a clear
// miss) and that the constructor name is present.
#[test]
fn constructor_pattern_resolves_real_tycon_id() {
    let source = r"
type Level = Info | Warn | Error

fn classify (l: Level) -> Int =
    match l
        Info  -> 0
        Warn  -> 1
        Error -> 2
";
    let tw = make_workspace("b_ctor_tycon_pattern", "main", source);
    let result = run_pipeline(&tw.path);

    let m = result.lowered.modules[0].as_ref().unwrap();
    let rendered = render_lowered_module(m);

    // The Ctor patterns must not use the placeholder `owner=0` (which is Int).
    // They should carry a non-zero TyConId corresponding to `Level`.
    assert!(
        !rendered.contains("Ctor(Variant:Info owner=0)"),
        "Info pattern must not use placeholder TyConId(0); rendered:\n{rendered}"
    );
    assert!(
        !rendered.contains("Ctor(Variant:Warn owner=0)"),
        "Warn pattern must not use placeholder TyConId(0); rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("Ctor(Variant:Info owner="),
        "Info pattern must appear in rendered output; rendered:\n{rendered}"
    );
}

// B-ctor-2: Record constructor (Expr::Record) resolves to the correct TyConId.
//
// `type Cell = { x: Int, y: Int }` declares a record type.
// After §3.2 wiring, the Construct IR node must carry Cell's real TyConId,
// not the placeholder TyConId(0) / Int.
#[test]
fn record_constructor_resolves_real_tycon_id() {
    let source = r"
type Cell = { x: Int, y: Int }

fn make_cell = Cell { x = 1, y = 2 }
";
    let tw = make_workspace("b_ctor_record_tycon", "main", source);
    let result = run_pipeline(&tw.path);

    let m = result.lowered.modules[0].as_ref().unwrap();
    let rendered = render_lowered_module(m);

    // Record constructor must not use placeholder TyConId(0) (which is Int).
    assert!(
        !rendered.contains("Ctor(Record:Cell owner=0)"),
        "Cell record ctor must not use placeholder TyConId(0); rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("Ctor(Record:Cell owner="),
        "Cell record ctor must appear in rendered output; rendered:\n{rendered}"
    );
}
