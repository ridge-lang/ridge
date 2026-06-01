//! Equality tests for `typecheck_module_incremental`.
//!
//! Re-checking one edited module against a cached workspace must produce the
//! same observable result as a full rebuild. Because an incremental check
//! appends the edited module's `TyCons` to the arena, its raw `TyConId`s differ
//! from the full build — so the comparison is over RENDERED types (and
//! diagnostics), not raw ids.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;

use ridge_resolve::{
    build_module_graph, discover_workspace, resolve_module_incremental, resolve_workspace, ModuleId,
};
use ridge_typecheck::{render_type_with, typecheck_module_incremental, typecheck_workspace};

fn write_file(dir: &Path, rel: &str, content: &str) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(full, content).expect("write file");
}

/// A single-module library workspace whose `Main` module is the caller's source.
fn build_ws(main_src: &str) -> TempDir {
    let td = TempDir::new().expect("tempdir");
    write_file(
        td.path(),
        "ridge.toml",
        "[workspace]\nname = \"inc-ws\"\nversion = \"0.1.0\"\nmembers = [\"libs/*\"]\n",
    );
    write_file(
        td.path(),
        "libs/proj/ridge.toml",
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(td.path(), "libs/proj/src/Main.ridge", main_src);
    td
}

fn main_module_id(ws: &ridge_resolve::WorkspaceGraph) -> ModuleId {
    for m in &ws.modules {
        if m.fully_qualified_name.ends_with(".Main") {
            return m.id;
        }
    }
    panic!("Main module not found");
}

#[test]
fn incremental_typecheck_matches_full_after_body_edit() {
    let td = build_ws(
        "type Color = Red | Green | Blue\npub fn pick -> Color = Red\npub fn n -> Int = 42\n",
    );

    // Cache: a full resolve + typecheck of the original source.
    let ws1 = discover_workspace(td.path()).graph.expect("graph");
    let mid = main_module_id(&ws1);
    let mut resolved = resolve_workspace(ws1);
    let typed = typecheck_workspace(&resolved).typed;

    // Body edit: add a function, leaving the type and public surface intact.
    let v2 = "type Color = Red | Green | Blue\npub fn pick -> Color = Red\npub fn n -> Int = 42\npub fn m -> Int = 7\n";
    write_file(td.path(), "libs/proj/src/Main.ridge", v2);

    // Update the resolved cache (PR-B), then incrementally type-check (PR-C).
    let ws2 = discover_workspace(td.path()).graph.expect("graph");
    let v2_ast = {
        let g = build_module_graph(&ws2);
        Arc::clone(&g.modules[mid.0 as usize].ast)
    };
    let _ = resolve_module_incremental(&mut resolved, mid, &v2_ast, true);
    let inc = typecheck_module_incremental(mid, &resolved, &typed);

    // Oracle: a from-scratch full build of the edited workspace.
    let ws3 = discover_workspace(td.path()).graph.expect("graph");
    let resolved2 = resolve_workspace(ws3);
    let full2 = typecheck_workspace(&resolved2);
    let full_main = &full2.typed.modules[mid.0 as usize];

    let inc_rendered: Vec<Option<String>> = inc
        .result
        .typed
        .node_types
        .iter()
        .map(|o| o.as_ref().map(|ty| render_type_with(ty, &inc.tycons)))
        .collect();
    let full_rendered: Vec<Option<String>> = full_main
        .node_types
        .iter()
        .map(|o| {
            o.as_ref()
                .map(|ty| render_type_with(ty, &full2.typed.tycons))
        })
        .collect();
    assert_eq!(
        inc_rendered, full_rendered,
        "incremental node types must render identically to the full build"
    );

    let mut inc_errs: Vec<String> = inc
        .result
        .errors
        .iter()
        .map(|e| format!("{}: {e}", e.code()))
        .collect();
    inc_errs.sort();
    let mut full_errs: Vec<String> = full2
        .errors
        .iter()
        .filter(|(m, _)| *m == mid)
        .map(|(_, e)| format!("{}: {e}", e.code()))
        .collect();
    full_errs.sort();
    assert_eq!(
        inc_errs, full_errs,
        "incremental diagnostics must match the full build for the edited module"
    );
}

#[test]
fn incremental_typecheck_matches_full_after_type_decl_edit() {
    let td = build_ws("type Color = Red | Green\npub fn pick -> Color = Red\n");

    let ws1 = discover_workspace(td.path()).graph.expect("graph");
    let mid = main_module_id(&ws1);
    let mut resolved = resolve_workspace(ws1);
    let typed = typecheck_workspace(&resolved).typed;

    // Tier-2a edit: add a constructor to the user type (changing its TyCon) and
    // a function that uses the new constructor. The incremental arena must
    // append the edited type's fresh TyCon while staying semantically identical.
    let v2 = "type Color = Red | Green | Blue\npub fn pick -> Color = Red\npub fn other -> Color = Blue\n";
    write_file(td.path(), "libs/proj/src/Main.ridge", v2);

    let ws2 = discover_workspace(td.path()).graph.expect("graph");
    let v2_ast = {
        let g = build_module_graph(&ws2);
        Arc::clone(&g.modules[mid.0 as usize].ast)
    };
    let _ = resolve_module_incremental(&mut resolved, mid, &v2_ast, true);
    let inc = typecheck_module_incremental(mid, &resolved, &typed);

    let ws3 = discover_workspace(td.path()).graph.expect("graph");
    let resolved2 = resolve_workspace(ws3);
    let full2 = typecheck_workspace(&resolved2);
    let full_main = &full2.typed.modules[mid.0 as usize];

    let inc_rendered: Vec<Option<String>> = inc
        .result
        .typed
        .node_types
        .iter()
        .map(|o| o.as_ref().map(|ty| render_type_with(ty, &inc.tycons)))
        .collect();
    let full_rendered: Vec<Option<String>> = full_main
        .node_types
        .iter()
        .map(|o| {
            o.as_ref()
                .map(|ty| render_type_with(ty, &full2.typed.tycons))
        })
        .collect();
    assert_eq!(
        inc_rendered, full_rendered,
        "incremental node types after a type-decl edit must render identically to the full build"
    );
}
