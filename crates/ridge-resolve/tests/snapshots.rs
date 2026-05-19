//! Phase 3 snapshot-test scaffolding (T4 + T8).
//!
//! One test per canonical example program. T14 replaces the plain-assertion
//! bodies with `insta::assert_debug_snapshot!` on the `ResolvedModule`. For T4
//! we only assert parseability and graph construction.  T8 adds acceptance
//! tests that run `assign_node_ids` + `resolve_module_uses` over each example
//! and assert: no R010/R011 errors, no panic, `bindings.len()` == `node_ids.len()`.

// Integration tests are allowed to use expect/unwrap/panic freely.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_resolve::{
    assign_node_ids, build_module_graph, check_forbid_rules, collect_symbols,
    detect_cycles_authoritative, discover_workspace, resolve_imports, resolve_module_uses,
    resolve_workspace, Binding, ImportTarget, ModuleId, ResolveError, SymbolTable,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// ── Fixture helpers ───────────────────────────────────────────────────────────

/// Write `content` to `dir/relative_path`, creating parent directories.
fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// Build a synthetic workspace in a tempdir for the given example:
///
/// ```text
/// ridge.toml           (workspace)
/// apps/demo/ridge.toml (project, kind = "app")
/// apps/demo/src/<name>.ridge (copy of examples/<name>.ridge)
/// ```
///
/// Returns the tempdir; callers pass `td.path()` to `discover_workspace`.
fn load_example_into_workspace(example_name: &str) -> TempDir {
    // CARGO_MANIFEST_DIR for ridge-resolve is `crates/ridge-resolve`.
    // `../../examples/<name>.ridge` reaches the repo-root `examples/` directory.
    let example_src = format!(
        "{}/../../examples/{}.ridge",
        env!("CARGO_MANIFEST_DIR"),
        example_name
    );

    let src_content = fs::read_to_string(&example_src)
        .unwrap_or_else(|e| panic!("could not read example file {example_src}: {e}"));

    let td = TempDir::new().expect("tempdir");

    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"examples-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        td.path(),
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        td.path(),
        &format!("apps/demo/src/{example_name}.ridge"),
        &src_content,
    );

    td
}

// ── Canonical-example snapshot tests ─────────────────────────────────────────

#[test]
fn log_analyzer_parses_and_builds_graph() {
    let td = load_example_into_workspace("log_analyzer");
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "R-errors: {:?}",
        disc.resolve_errors
    );
    let ws = disc.graph.expect("graph present on happy path");
    let g = build_module_graph(&ws);
    assert_eq!(g.modules.len(), 1, "expected exactly 1 module");
    let m = &g.modules[0];
    assert!(m.read_error.is_none(), "read error: {:?}", m.read_error);
    assert!(
        m.parse_errors.is_empty(),
        "parse errors: {:?}",
        m.parse_errors
    );
    assert!(m.lex_errors.is_empty(), "lex errors: {:?}", m.lex_errors);
    // log_analyzer has 8 imports; at minimum there must be edges.
    assert!(!g.tentative_edges.is_empty(), "no tentative edges recorded");
}

#[test]
fn url_shortener_parses_and_builds_graph() {
    let td = load_example_into_workspace("url_shortener");
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "R-errors: {:?}",
        disc.resolve_errors
    );
    let ws = disc.graph.expect("graph present on happy path");
    let g = build_module_graph(&ws);
    assert_eq!(g.modules.len(), 1, "expected exactly 1 module");
    let m = &g.modules[0];
    assert!(m.read_error.is_none(), "read error: {:?}", m.read_error);
    assert!(
        m.parse_errors.is_empty(),
        "parse errors: {:?}",
        m.parse_errors
    );
    assert!(m.lex_errors.is_empty(), "lex errors: {:?}", m.lex_errors);
    assert!(!g.tentative_edges.is_empty(), "no tentative edges recorded");
}

#[test]
fn game_of_life_parses_and_builds_graph() {
    let td = load_example_into_workspace("game_of_life");
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "R-errors: {:?}",
        disc.resolve_errors
    );
    let ws = disc.graph.expect("graph present on happy path");
    let g = build_module_graph(&ws);
    assert_eq!(g.modules.len(), 1, "expected exactly 1 module");
    let m = &g.modules[0];
    assert!(m.read_error.is_none(), "read error: {:?}", m.read_error);
    assert!(
        m.parse_errors.is_empty(),
        "parse errors: {:?}",
        m.parse_errors
    );
    assert!(m.lex_errors.is_empty(), "lex errors: {:?}", m.lex_errors);
    assert!(!g.tentative_edges.is_empty(), "no tentative edges recorded");
}

#[test]
fn rate_limiter_parses_and_builds_graph() {
    let td = load_example_into_workspace("rate_limiter");
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "R-errors: {:?}",
        disc.resolve_errors
    );
    let ws = disc.graph.expect("graph present on happy path");
    let g = build_module_graph(&ws);
    assert_eq!(g.modules.len(), 1, "expected exactly 1 module");
    let m = &g.modules[0];
    assert!(m.read_error.is_none(), "read error: {:?}", m.read_error);
    assert!(
        m.parse_errors.is_empty(),
        "parse errors: {:?}",
        m.parse_errors
    );
    assert!(m.lex_errors.is_empty(), "lex errors: {:?}", m.lex_errors);
    assert!(!g.tentative_edges.is_empty(), "no tentative edges recorded");
}

// ── T7 acceptance tests: resolve_imports over each example ────────────────────

/// Helper: run the full T7 pipeline over an example.
///
/// Returns `(ImportResolutionResult)`.  Panics on setup failure.
fn resolve_imports_for_example(example_name: &str) -> ridge_resolve::ImportResolutionResult {
    let td = load_example_into_workspace(example_name);
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "{example_name}: R-errors during discovery: {:?}",
        disc.resolve_errors
    );
    let mut ws = disc.graph.expect("graph present");
    let g = build_module_graph(&ws);
    assert_eq!(g.modules.len(), 1, "{example_name}: expected 1 module");
    let pm = &g.modules[0];
    assert!(
        pm.parse_errors.is_empty(),
        "{example_name}: parse errors: {:?}",
        pm.parse_errors
    );
    let symbol_tables: Vec<SymbolTable> = g
        .modules
        .iter()
        .map(|pm| {
            let (t, _) = collect_symbols(pm.id, &pm.ast);
            t
        })
        .collect();
    // Keep td alive until after resolve_imports — it owns the temp files.
    let result = resolve_imports(&mut ws, &g, &symbol_tables);
    // Verify cycle detection also runs cleanly.
    let cycle_errors = detect_cycles_authoritative(&ws, &result.imports);
    assert!(
        cycle_errors.is_empty(),
        "{example_name}: unexpected cycle errors: {cycle_errors:?}",
    );
    std::mem::drop(td);
    result
}

#[test]
fn t7_acceptance_log_analyzer_imports_resolve() {
    let result = resolve_imports_for_example("log_analyzer");
    assert!(
        result.resolve_errors.is_empty(),
        "log_analyzer: R-errors: {:?}",
        result.resolve_errors
    );
    assert!(
        result.manifest_errors.is_empty(),
        "log_analyzer: M-errors: {:?}",
        result.manifest_errors
    );
    // All imports must resolve to non-Unresolved.
    let module_imports = result.imports.first().expect("module 0");
    for res in module_imports {
        assert_ne!(
            res.target,
            ImportTarget::Unresolved,
            "log_analyzer: unresolved import (alias={:?})",
            res.alias
        );
    }
    // log_analyzer has 8 user imports + 3 prelude IRs (R013 × 2 + R015 × 1) = 11.
    // R015 aliases IR always added (user has List+Map suppressed → 6 remaining bindings).
    assert_eq!(
        module_imports.len(),
        11,
        "log_analyzer: expected 11 imports (8 user + 3 prelude), got {}",
        module_imports.len()
    );
}

#[test]
fn t7_acceptance_url_shortener_imports_resolve() {
    let result = resolve_imports_for_example("url_shortener");
    assert!(
        result.resolve_errors.is_empty(),
        "url_shortener: R-errors: {:?}",
        result.resolve_errors
    );
    assert!(
        result.manifest_errors.is_empty(),
        "url_shortener: M-errors: {:?}",
        result.manifest_errors
    );
    let module_imports = result.imports.first().expect("module 0");
    for res in module_imports {
        assert_ne!(
            res.target,
            ImportTarget::Unresolved,
            "url_shortener: unresolved import"
        );
    }
    // url_shortener has 7 user imports (including std.net.http as Http per R015)
    // + 3 prelude IRs (R013 × 2 + R015 × 1) = 10.
    // R015 aliases IR has List+Map suppressed → 6 remaining bindings, still added.
    assert_eq!(
        module_imports.len(),
        10,
        "url_shortener: expected 10 imports (7 user + 3 prelude), got {}",
        module_imports.len()
    );
}

#[test]
fn t7_acceptance_game_of_life_imports_resolve() {
    let result = resolve_imports_for_example("game_of_life");
    assert!(
        result.resolve_errors.is_empty(),
        "game_of_life: R-errors: {:?}",
        result.resolve_errors
    );
    assert!(
        result.manifest_errors.is_empty(),
        "game_of_life: M-errors: {:?}",
        result.manifest_errors
    );
    let module_imports = result.imports.first().expect("module 0");
    for res in module_imports {
        assert_ne!(
            res.target,
            ImportTarget::Unresolved,
            "game_of_life: unresolved import"
        );
    }
    // game_of_life has 5 user imports + 3 prelude IRs (R013 × 2 + R015 × 1) = 8.
    // R015 aliases IR has List suppressed → 7 remaining bindings, still added.
    assert_eq!(
        module_imports.len(),
        8,
        "game_of_life: expected 8 imports (5 user + 3 prelude), got {}",
        module_imports.len()
    );
}

#[test]
fn t7_acceptance_rate_limiter_imports_resolve() {
    let result = resolve_imports_for_example("rate_limiter");
    assert!(
        result.resolve_errors.is_empty(),
        "rate_limiter: R-errors: {:?}",
        result.resolve_errors
    );
    assert!(
        result.manifest_errors.is_empty(),
        "rate_limiter: M-errors: {:?}",
        result.manifest_errors
    );
    let module_imports = result.imports.first().expect("module 0");
    for res in module_imports {
        assert_ne!(
            res.target,
            ImportTarget::Unresolved,
            "rate_limiter: unresolved import"
        );
    }
    // rate_limiter has 5 user imports + 3 prelude IRs (R013 × 2 + R015 × 1) = 8.
    // R015 aliases IR has List suppressed → 7 remaining bindings, still added.
    assert_eq!(
        module_imports.len(),
        8,
        "rate_limiter: expected 8 imports (5 user + 3 prelude), got {}",
        module_imports.len()
    );
}

// ── collect_symbols acceptance tests over each example ───────────────────────

/// Helper: parse the given example into a module graph and run `collect_symbols`
/// on its (only) module.  Panics if the graph/parse fails.
fn collect_symbols_for_example(example_name: &str) -> ridge_resolve::SymbolTable {
    let td = load_example_into_workspace(example_name);
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "{example_name}: R-errors: {:?}",
        disc.resolve_errors
    );
    let ws = disc.graph.expect("graph present");
    let g = build_module_graph(&ws);
    assert_eq!(g.modules.len(), 1, "{example_name}: expected 1 module");
    let pm = &g.modules[0];
    assert!(
        pm.parse_errors.is_empty(),
        "{example_name}: parse errors: {:?}",
        pm.parse_errors
    );
    let (table, errors) = collect_symbols(ModuleId(pm.id.0), &pm.ast);
    assert!(
        errors.is_empty(),
        "{example_name}: collect_symbols R005 errors: {errors:?}"
    );
    table
}

#[test]
fn t6_acceptance_log_analyzer() {
    let table = collect_symbols_for_example("log_analyzer");
    assert!(
        !table.entries.is_empty(),
        "log_analyzer: expected at least one SymbolEntry"
    );
}

#[test]
fn t6_acceptance_url_shortener() {
    let table = collect_symbols_for_example("url_shortener");
    assert!(
        !table.entries.is_empty(),
        "url_shortener: expected at least one SymbolEntry"
    );
}

#[test]
fn t6_acceptance_game_of_life() {
    let table = collect_symbols_for_example("game_of_life");
    assert!(
        !table.entries.is_empty(),
        "game_of_life: expected at least one SymbolEntry"
    );
}

#[test]
fn t6_acceptance_rate_limiter() {
    let table = collect_symbols_for_example("rate_limiter");
    assert!(
        !table.entries.is_empty(),
        "rate_limiter: expected at least one SymbolEntry"
    );
}

// ── T8 acceptance tests: resolve_module_uses over each example ─────────────────

/// Helper: run the full T8 pipeline (T7 imports + `assign_node_ids` + `resolve_module_uses`)
/// over an example.  Returns `(bindings, errors, node_id_count)`.
fn t8_resolve_for_example(example_name: &str) -> (Vec<Option<Binding>>, Vec<ResolveError>, usize) {
    let td = load_example_into_workspace(example_name);
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "{example_name}: R-errors during discovery: {:?}",
        disc.resolve_errors
    );
    let mut ws = disc.graph.expect("graph present");
    let g = build_module_graph(&ws);
    assert_eq!(g.modules.len(), 1, "{example_name}: expected 1 module");
    let pm = &g.modules[0];
    assert!(
        pm.parse_errors.is_empty(),
        "{example_name}: parse errors: {:?}",
        pm.parse_errors
    );

    let symbol_tables: Vec<SymbolTable> = g
        .modules
        .iter()
        .map(|m| {
            let (t, _) = collect_symbols(m.id, &m.ast);
            t
        })
        .collect();

    let import_result = resolve_imports(&mut ws, &g, &symbol_tables);
    assert!(
        import_result.resolve_errors.is_empty(),
        "{example_name}: import R-errors: {:?}",
        import_result.resolve_errors
    );

    let cycle_errors = detect_cycles_authoritative(&ws, &import_result.imports);
    assert!(
        cycle_errors.is_empty(),
        "{example_name}: cycle errors: {cycle_errors:?}"
    );

    // T8: assign NodeIds and resolve use-site bindings.
    let (nid_map, nid_errors) = assign_node_ids(&pm.ast);
    assert!(
        nid_errors.is_empty(),
        "{example_name}: NodeId collision errors: {nid_errors:?}"
    );

    let module_imports = import_result
        .imports
        .first()
        .map_or([].as_slice(), Vec::as_slice);

    let (bindings, errors) =
        resolve_module_uses(pm.id, &pm.ast, &nid_map, &symbol_tables, module_imports);

    // Invariant: bindings.len() == node_ids.len().
    assert_eq!(
        bindings.len(),
        nid_map.len(),
        "{example_name}: bindings.len() != node_id_map.len()"
    );

    let nid_count = nid_map.len();
    drop(td);
    (bindings, errors, nid_count)
}

/// Count bindings matching a predicate.
fn count_b<F: Fn(&Binding) -> bool>(bindings: &[Option<Binding>], f: F) -> usize {
    bindings.iter().flatten().filter(|b| f(b)).count()
}

/// R010 names previously expected to fire on the canonical examples and now
/// resolved.  Kept as an empty slice (with documentation of past deferrals)
/// so the per-example acceptance tests have a single, easy-to-reopen
/// allowlist if a regression surfaces.
///
/// Past entries — all closed:
/// - `Some`/`None`/`Ok`/`Err` — closed by R013 (implicit prelude, 2026-04-24).
/// - `line` — closed by R014 option A (layout-in-brackets, 2026-04-24).
/// - `Response` — closed by import-list upper-ident extension (`ImportList` accepts `UPPER_IDENT`,
///   2026-04-25); `examples/url_shortener.ridge` now does
///   `import std.net.http as Http (Request, Response, listen, respond)` and
///   `BUILTINS[std.net.http]` exports include `Request`/`Response`.
/// - `report`, `run` — closed by the `Expr::Send` walker arm
///   (`crates/ridge-resolve/src/walker.rs::visit_send_message`, 2026-04-25);
///   handler names are treated as labels and skipped, mirroring `Expr::Ask`.
///
/// Any name in this slice would be silently allowed by the walker acceptance
/// tests; an empty slice means every R010 in the canonical examples is a
/// regression.
const fn r010_t9_scope_names() -> &'static [&'static str] {
    &[]
}

#[test]
fn t8_acceptance_log_analyzer() {
    let (bindings, errors, nid_count) = t8_resolve_for_example("log_analyzer");

    // R011 (duplicate locals) must be zero — no ambiguous scope.
    let r011 = errors
        .iter()
        .filter(|e| matches!(e, ResolveError::DuplicateLocal { .. }))
        .count();
    assert_eq!(
        r011, 0,
        "log_analyzer: R011 count must be 0; errors: {errors:?}"
    );

    // R010 may fire only for known T9-scope names (constructors, handler names,
    // external types).  Any other R010 is a genuine T8 walker bug.
    let t9_names = r010_t9_scope_names();
    let unexpected_r010: Vec<_> = errors
        .iter()
        .filter(|e| match e {
            ResolveError::UnresolvedIdent { name, .. } => !t9_names.contains(&name.as_str()),
            _ => false,
        })
        .collect();
    assert!(
        unexpected_r010.is_empty(),
        "log_analyzer: unexpected R010 (non-T9 names): {unexpected_r010:?}"
    );

    // At least one binding must be stamped.
    let stamped = bindings.iter().filter(|b| b.is_some()).count();
    assert!(
        stamped > 0,
        "log_analyzer: expected at least one stamped binding, nid_count={nid_count}"
    );

    // Non-trivial binding mix.
    let local_count = count_b(&bindings, |b| matches!(b, Binding::Local(_)));
    let module_sym = count_b(&bindings, |b| matches!(b, Binding::ModuleSymbol { .. }));
    let stdlib_sym = count_b(&bindings, |b| matches!(b, Binding::StdlibSymbol { .. }));
    assert!(
        local_count + module_sym + stdlib_sym > 0,
        "log_analyzer: expected non-trivial binding mix"
    );
}

#[test]
fn t8_acceptance_url_shortener() {
    let (bindings, errors, _nid_count) = t8_resolve_for_example("url_shortener");

    let r011 = errors
        .iter()
        .filter(|e| matches!(e, ResolveError::DuplicateLocal { .. }))
        .count();
    assert_eq!(
        r011, 0,
        "url_shortener: R011 count must be 0; errors: {errors:?}"
    );

    // T11 DoD: state fields are used, not shadowed — R017 must be 0.
    let r017 = errors
        .iter()
        .filter(|e| matches!(e, ResolveError::StateFieldShadowedByLocal { .. }))
        .count();
    assert_eq!(
        r017, 0,
        "url_shortener: R017 count must be 0; errors: {errors:?}"
    );

    let t9_names = r010_t9_scope_names();
    let unexpected_r010: Vec<_> = errors
        .iter()
        .filter(|e| match e {
            ResolveError::UnresolvedIdent { name, .. } => !t9_names.contains(&name.as_str()),
            _ => false,
        })
        .collect();
    assert!(
        unexpected_r010.is_empty(),
        "url_shortener: unexpected R010 (non-T9 names): {unexpected_r010:?}"
    );

    let stamped = bindings.iter().filter(|b| b.is_some()).count();
    assert!(
        stamped > 0,
        "url_shortener: expected at least one stamped binding"
    );
}

#[test]
fn t8_acceptance_game_of_life() {
    let (bindings, errors, _nid_count) = t8_resolve_for_example("game_of_life");

    let r011 = errors
        .iter()
        .filter(|e| matches!(e, ResolveError::DuplicateLocal { .. }))
        .count();
    assert_eq!(
        r011, 0,
        "game_of_life: R011 count must be 0; errors: {errors:?}"
    );

    // `line` is in the T9-scope list (layout-suppressed lambda body: see note above).
    // `Ok` is an unqualified stdlib constructor — T9 concern.
    let t9_names = r010_t9_scope_names();
    let unexpected_r010: Vec<_> = errors
        .iter()
        .filter(|e| match e {
            ResolveError::UnresolvedIdent { name, .. } => !t9_names.contains(&name.as_str()),
            _ => false,
        })
        .collect();
    assert!(
        unexpected_r010.is_empty(),
        "game_of_life: unexpected R010 (non-T9 names): {unexpected_r010:?}"
    );

    let stamped = bindings.iter().filter(|b| b.is_some()).count();
    assert!(
        stamped > 0,
        "game_of_life: expected at least one stamped binding"
    );
}

#[test]
fn t8_acceptance_rate_limiter() {
    let (bindings, errors, _nid_count) = t8_resolve_for_example("rate_limiter");

    let r011 = errors
        .iter()
        .filter(|e| matches!(e, ResolveError::DuplicateLocal { .. }))
        .count();
    assert_eq!(
        r011, 0,
        "rate_limiter: R011 count must be 0; errors: {errors:?}"
    );

    // T11 DoD: state fields are used, not shadowed — R017 must be 0.
    let r017 = errors
        .iter()
        .filter(|e| matches!(e, ResolveError::StateFieldShadowedByLocal { .. }))
        .count();
    assert_eq!(
        r017, 0,
        "rate_limiter: R017 count must be 0; errors: {errors:?}"
    );

    // `report` and `run` are actor handler names used in send-message expressions.
    // `Ok` is an unqualified stdlib constructor — all T9 concerns.
    let t9_names = r010_t9_scope_names();
    let unexpected_r010: Vec<_> = errors
        .iter()
        .filter(|e| match e {
            ResolveError::UnresolvedIdent { name, .. } => !t9_names.contains(&name.as_str()),
            _ => false,
        })
        .collect();
    assert!(
        unexpected_r010.is_empty(),
        "rate_limiter: unexpected R010 (non-T9 names): {unexpected_r010:?}"
    );

    let stamped = bindings.iter().filter(|b| b.is_some()).count();
    assert!(
        stamped > 0,
        "rate_limiter: expected at least one stamped binding"
    );
}

// ── T9 acceptance tests: qualified-name resolution ────────────────────────────

/// Qualified-name head segments that are known-unresolvable in the canonical
/// examples and are therefore expected R012 sites.
///
/// After R015 (implicit prelude module aliases, resolved 2026-04-24) plus
/// the `log_analyzer.ridge` `Error.text` fix (replaced with `Err "…"` over
/// `Result Unit Text`), all qualified-name heads in the 4 canonical examples
/// resolve cleanly.  Empty list — any future R012 in the examples is a
/// regression.
const fn t9_known_r012_heads() -> &'static [&'static str] {
    &[]
}

/// T9 test 1: canonical stdlib qualified names that DO have import aliases
/// resolve to `StdlibSymbol` bindings; no R014 ("unknown stdlib symbol") fires.
///
/// `log_analyzer` imports `std.io as Io`, `std.list as List`, `std.map as Map`.
/// All three produce `StdlibSymbol` bindings for their dotted use-sites.
#[test]
fn t9_acceptance_stdlib_qualified_names_bind() {
    let (bindings, errors, _) = t8_resolve_for_example("log_analyzer");

    // R014 (UnknownStdlibSymbol) must never fire — every exported name that is
    // used in the examples is present in BUILTINS.
    let r014: Vec<_> = errors
        .iter()
        .filter(|e| matches!(e, ResolveError::UnknownStdlibSymbol { .. }))
        .collect();
    assert!(
        r014.is_empty(),
        "log_analyzer: R014 (UnknownStdlibSymbol) errors: {r014:?}"
    );

    // At least one StdlibSymbol binding must be present (Io.println, List.*, Map.*).
    let stdlib_count = count_b(&bindings, |b| matches!(b, Binding::StdlibSymbol { .. }));
    assert!(
        stdlib_count > 0,
        "log_analyzer: expected at least one StdlibSymbol binding; got none"
    );
}

/// T9 test 2: R012 errors in the canonical examples are only for known
/// unresolvable qualified-name heads (no import alias, or primitive-type name
/// used as a module).  No R012 fires for properly-aliased imports.
///
/// Specifically: `Io.println`, `List.map`, `Map.empty`, `Random.choice` etc.
/// must NOT appear in any R012 diagnostic.
#[test]
fn t9_acceptance_no_unexpected_r012() {
    // These heads have proper import aliases (explicit or R015 prelude) and
    // must never produce R012.
    let properly_aliased = &[
        "Io", "List", "Map", "Random", "Fs", "Env", "Cli", "Time", "Option",
        // R015: now pre-bound as prelude ModuleAlias entries.
        "Int", "Float", "Bool", "Text", "Set", "Json",
        // Http: url_shortener.ridge now has explicit `import std.net.http as Http`.
        "Http",
    ];
    let known_r012 = t9_known_r012_heads();

    for example in &[
        "log_analyzer",
        "url_shortener",
        "game_of_life",
        "rate_limiter",
    ] {
        let (_, errors, _) = t8_resolve_for_example(example);
        let unexpected_r012: Vec<_> = errors
            .iter()
            .filter(|e| {
                if let ResolveError::UnresolvedQualifiedName { segments, .. } = e {
                    let head = segments.first().map_or("", String::as_str);
                    // It is unexpected if the head is a properly-aliased import.
                    properly_aliased.contains(&head)
                } else {
                    false
                }
            })
            .collect();
        assert!(
            unexpected_r012.is_empty(),
            "{example}: R012 for properly-aliased qualified names: {unexpected_r012:?}"
        );

        // All actual R012 errors must only have known-unresolvable heads.
        let unexplained_r012: Vec<_> = errors
            .iter()
            .filter(|e| {
                if let ResolveError::UnresolvedQualifiedName { segments, .. } = e {
                    let head = segments.first().map_or("", String::as_str);
                    !known_r012.contains(&head)
                } else {
                    false
                }
            })
            .collect();
        assert!(
            unexplained_r012.is_empty(),
            "{example}: R012 for unexpected head segments: {unexplained_r012:?}"
        );
    }
}

// ── T12 acceptance: workspace forbid rules over a multi-project fixture ──────

/// Return the path to a committed workspace fixture tree.
///
/// `name` is a subdirectory of `tests/fixtures/workspace/` relative to
/// `CARGO_MANIFEST_DIR` (e.g. `"acme_happy"`, `"acme_forbid"`,
/// `"acme_mismatch_rule"`).
fn acme_workspace_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/workspace")
        .join(name)
}

/// Run the full discovery → module-graph → `resolve_imports` → forbid pipeline
/// over the workspace at `path`.  Returns the post-forbid resolve errors.
fn run_pipeline_and_collect_forbid_errors(path: &std::path::Path) -> Vec<(ModuleId, ResolveError)> {
    let disc = discover_workspace(path);
    assert!(
        disc.resolve_errors.is_empty(),
        "discovery R-errors: {:?}",
        disc.resolve_errors
    );
    assert!(
        disc.manifest_errors.is_empty(),
        "discovery M-errors: {:?}",
        disc.manifest_errors
    );

    let mut ws = disc.graph.expect("graph present on happy path");
    let g = build_module_graph(&ws);

    let symbol_tables: Vec<SymbolTable> = g
        .modules
        .iter()
        .map(|pm| {
            let (t, _) = collect_symbols(pm.id, &pm.ast);
            t
        })
        .collect();

    let result = resolve_imports(&mut ws, &g, &symbol_tables);
    assert!(
        result.resolve_errors.is_empty(),
        "import R-errors (R006/R007/R008/R009): {:?}",
        result.resolve_errors
    );
    let cycle_errors = detect_cycles_authoritative(&ws, &result.imports);
    assert!(cycle_errors.is_empty(), "cycle errors: {cycle_errors:?}");

    let mut forbid_errors = Vec::new();
    check_forbid_rules(&ws, &result.imports, &mut forbid_errors);
    forbid_errors
}

#[test]
fn t12_acceptance_acme_forbid_emits_one_r013() {
    // acme_forbid fixture: forbid = [{ from = "acme.domain.**", to = "acme.infra.**" }]
    // RegisterUser.ridge imports acme.infra.Postgres → violation.
    let path = acme_workspace_path("acme_forbid");
    let errors = run_pipeline_and_collect_forbid_errors(&path);

    assert_eq!(
        errors.len(),
        1,
        "expected exactly 1 R013 in acme_forbid fixture, got: {errors:?}"
    );
    let (_, first_err) = &errors[0];
    match first_err {
        ResolveError::ForbidViolation {
            rule_text,
            importer_fqn,
            target_fqn,
            import_span,
            ..
        } => {
            assert_eq!(importer_fqn, "acme.domain.RegisterUser");
            assert_eq!(target_fqn, "acme.infra.Postgres");
            assert!(
                rule_text.contains("acme.domain.**"),
                "rule_text should reference from pattern; got: {rule_text:?}"
            );
            // Span must be the ImportDecl span — non-empty, anchored at the
            // first byte of the `import` keyword in RegisterUser.ridge.
            assert!(!import_span.is_empty(), "import span must be non-empty");
            assert_eq!(import_span.start, 0);
        }
        other => panic!("expected ForbidViolation, got: {other:?}"),
    }
}

#[test]
fn t12_acceptance_acme_happy_emits_no_r013() {
    // acme_mismatch_rule fixture: forbid rule from = "acme.api.**" does NOT
    // match the only importer `acme.domain.RegisterUser`, so no R013 fires.
    let path = acme_workspace_path("acme_mismatch_rule");
    let errors = run_pipeline_and_collect_forbid_errors(&path);
    assert!(
        errors.is_empty(),
        "workspace with non-matching forbid rule must not fire R013, got: {errors:?}"
    );
}

#[test]
fn t12_acceptance_acme_no_rules_emits_no_r013() {
    // acme_happy fixture: no [workspace.rules].forbid entries — never emits
    // R013 regardless of import shape.
    let path = acme_workspace_path("acme_happy");
    let errors = run_pipeline_and_collect_forbid_errors(&path);
    assert!(
        errors.is_empty(),
        "workspace without forbid rules must not fire R013, got: {errors:?}"
    );
}

// ── T14: full-pipeline insta snapshots over the 4 canonical examples ─────────
//
// Plan §10 T14 — commit deterministic insta snapshots for every example.  The
// snapshot captures a `ResolvedSnapshot` value (errors + per-binding-kind
// counts + import alias summary) so that any drift in resolver behaviour over
// the four canonical Ridge programs is caught by `cargo insta test`.
//
// The two workspace fixtures (`acme_happy/`, `acme_forbid/`) live in
// `tests/workspace.rs` per the plan's file-touch list.
//
// Every canonical example resolves cleanly (zero R-errors) thanks to two decisions:
// - Import-list upper-ident extension (2026-04-25): `ImportList` accepts `UPPER_IDENT`, so
//   `examples/url_shortener.ridge` can `import std.net.http as Http (Request,
//   Response, listen, respond)` and reference `Request` / `Response` directly.
// - **`Expr::Send` walker arm** (`src/walker.rs::visit_send_message`,
//   2026-04-25): the head of a `Send.message` is treated as a handler-name
//   label (validated against the actor's `on` list by Phase 4) and is not
//   resolved against the lexical scope, mirroring how `Expr::Ask` already
//   silently skips its `message: Ident` field.

/// Deterministic, snapshot-friendly view of the post-resolve-pipeline state
/// for one example or workspace fixture.
///
/// Field choices:
/// - `errors` — formatted as `"<R-code>: <Display>"`, sorted by `(R-code,
///   span.start, name)` for cross-platform determinism.
/// - `binding_kind_counts` — `BTreeMap` so iteration order is stable.
/// - `import_aliases` — sorted alias→target summary.
///
/// All fields are read by `insta` via the derived `Debug`; the dead-code
/// lint cannot see through Debug formatters, so suppress it here.
#[allow(dead_code)]
#[derive(Debug)]
struct ResolvedSnapshot {
    errors: Vec<String>,
    nid_count: usize,
    stamped_bindings: usize,
    binding_kind_counts: BTreeMap<&'static str, usize>,
    import_aliases: Vec<String>,
}

const fn binding_kind_name(b: &Binding) -> &'static str {
    match b {
        Binding::Local(_) => "Local",
        Binding::ModuleSymbol { .. } => "ModuleSymbol",
        Binding::ImportedSymbol { .. } => "ImportedSymbol",
        Binding::ModuleAlias { .. } => "ModuleAlias",
        Binding::StdlibSymbol { .. } => "StdlibSymbol",
        Binding::ActorName { .. } => "ActorName",
        Binding::Constructor { .. } => "Constructor",
        Binding::FieldAccessor { .. } => "FieldAccessor",
        Binding::Error => "Error",
        // #[non_exhaustive]: new variants added in future phases
        _ => "Unknown",
    }
}

fn format_error(e: &ResolveError) -> String {
    use ridge_lexer::Span;
    fn span_str(s: Span) -> String {
        format!("{}..{}", s.start, s.end)
    }
    let code = e.code();
    let body = match e {
        ResolveError::UnresolvedIdent { name, span, .. } => {
            format!("UnresolvedIdent name={name:?} span={}", span_str(*span))
        }
        ResolveError::UnresolvedQualifiedName { segments, span, .. } => {
            format!(
                "UnresolvedQualifiedName segments={segments:?} span={}",
                span_str(*span)
            )
        }
        ResolveError::ForbidViolation {
            rule_text,
            importer_fqn,
            target_fqn,
            import_span,
            ..
        } => format!(
            "ForbidViolation importer={importer_fqn:?} target={target_fqn:?} rule={rule_text:?} span={}",
            span_str(*import_span)
        ),
        other => format!("{other:?}"),
    };
    format!("{code}: {body}")
}

/// Run the full T7..T13 pipeline over a single-module example and produce a
/// deterministic [`ResolvedSnapshot`].
fn snapshot_example(example_name: &str) -> ResolvedSnapshot {
    let td = load_example_into_workspace(example_name);
    let disc = discover_workspace(td.path());
    assert!(
        disc.resolve_errors.is_empty(),
        "{example_name}: discovery R-errors: {:?}",
        disc.resolve_errors
    );
    assert!(
        disc.manifest_errors.is_empty(),
        "{example_name}: discovery M-errors: {:?}",
        disc.manifest_errors
    );
    let ws = disc.graph.expect("graph present on happy path");

    // Verify the single module parsed cleanly before running the full pipeline.
    let g = build_module_graph(&ws);
    assert_eq!(g.modules.len(), 1, "{example_name}: expected 1 module");
    let pm = &g.modules[0];
    assert!(
        pm.parse_errors.is_empty(),
        "{example_name}: parse errors: {:?}",
        pm.parse_errors
    );
    assert!(
        pm.lex_errors.is_empty(),
        "{example_name}: lex errors: {:?}",
        pm.lex_errors
    );

    // Full resolution via the public entry point.
    let resolved = resolve_workspace(ws);
    let rm = resolved.modules.first().expect("one resolved module");

    let mut formatted: Vec<String> = resolved
        .errors
        .iter()
        .map(|(_, e)| format_error(e))
        .collect();
    formatted.sort();

    let mut binding_kind_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for b in rm.bindings.iter().flatten() {
        *binding_kind_counts.entry(binding_kind_name(b)).or_insert(0) += 1;
    }

    let mut import_aliases: Vec<String> = rm
        .imports
        .iter()
        .map(|ir| {
            let alias = ir.alias.clone().unwrap_or_else(|| "<bare>".to_string());
            let target = match &ir.target {
                ImportTarget::WorkspaceModule(m) => format!("WorkspaceModule({})", m.0),
                ImportTarget::BuiltinStdlib(m) => format!("BuiltinStdlib({})", m.0),
                ImportTarget::External { .. } => "External".to_string(),
                ImportTarget::Unresolved => "Unresolved".to_string(),
                _ => "Unknown".to_string(),
            };
            format!("{alias} -> {target}")
        })
        .collect();
    import_aliases.sort();

    let stamped_bindings = rm.bindings.iter().filter(|b| b.is_some()).count();
    let nid_count = rm.bindings.len(); // bindings.len() == nid_map.len() (walker invariant)
    drop(td);
    ResolvedSnapshot {
        errors: formatted,
        nid_count,
        stamped_bindings,
        binding_kind_counts,
        import_aliases,
    }
}

#[test]
fn t14_snapshot_log_analyzer() {
    let snap = snapshot_example("log_analyzer");
    // DoD §14.4: log_analyzer resolves cleanly (no R-errors).
    assert!(
        snap.errors.is_empty(),
        "log_analyzer must resolve cleanly; errors: {:#?}",
        snap.errors
    );
    insta::assert_debug_snapshot!("t14_log_analyzer", snap);
}

#[test]
fn t14_snapshot_url_shortener() {
    let snap = snapshot_example("url_shortener");
    // DoD §14.4: url_shortener resolves cleanly.
    // `ImportList` accepts `UPPER_IDENT` so `Request` / `Response` are
    // imported directly from `std.net.http`.
    assert!(
        snap.errors.is_empty(),
        "url_shortener must resolve cleanly; errors: {:#?}",
        snap.errors
    );
    insta::assert_debug_snapshot!("t14_url_shortener", snap);
}

#[test]
fn t14_snapshot_game_of_life() {
    let snap = snapshot_example("game_of_life");
    // DoD §14.4: game_of_life resolves cleanly.
    assert!(
        snap.errors.is_empty(),
        "game_of_life must resolve cleanly; errors: {:#?}",
        snap.errors
    );
    insta::assert_debug_snapshot!("t14_game_of_life", snap);
}

#[test]
fn t14_snapshot_rate_limiter() {
    let snap = snapshot_example("rate_limiter");
    // DoD §14.4: rate_limiter resolves cleanly.  Closed by the `Expr::Send`
    // walker arm (`src/walker.rs::visit_send_message`) — handler-name labels
    // in `actor ! handler args` are no longer treated as use-site idents.
    assert!(
        snap.errors.is_empty(),
        "rate_limiter must resolve cleanly; errors: {:#?}",
        snap.errors
    );
    insta::assert_debug_snapshot!("t14_rate_limiter", snap);
}
