//! Integration tests for the stdlib bridge map (T7 coverage proof).
//!
//! Each `stdlib_bridge_covers_*` test runs the full pipeline on one of the
//! four example programs and asserts that every `SymbolRef::Stdlib(module,
//! name)` referenced in the lowered IR has a corresponding `BridgeTarget`
//! in `stdlib_map::lookup`.  A failure reports the first offending
//! `(module, name)` pair.
//!
//! §3.4 coverage matrix: all ~50 symbols used by the four examples must
//! map to a `BridgeTarget` (or be handled as an identity shortcut).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_codegen_erl::stdlib_map::{self, BridgeTarget};
use ridge_ir::{
    IrActor, IrConst, IrExpr, IrFn, IrHandler, IrInit, IrItem, IrTimeout, LoweredWorkspace,
    SymbolRef,
};
use ridge_lower::lower_workspace;
use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::typecheck_workspace;
use std::fs;
use std::path::{Path, PathBuf};

// ── Pipeline helpers ──────────────────────────────────────────────────────────

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new(id: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ridge_codegen_erl_test_{id}"));
        if path.exists() {
            let _ = fs::remove_dir_all(&path);
        }
        fs::create_dir_all(&path).expect("create temp workspace dir");
        Self { path }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(&full, content).expect("write file");
}

fn make_workspace(id: &str, module_name: &str, source: &str) -> TempWorkspace {
    let tw = TempWorkspace::new(id);
    write_file(
        &tw.path,
        "ridge.toml",
        "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [\"apps/*\"]\n",
    );
    write_file(
        &tw.path,
        "apps/demo/ridge.toml",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nkind = \"library\"\n",
    );
    write_file(
        &tw.path,
        &format!("apps/demo/src/{module_name}.ridge"),
        source,
    );
    tw
}

fn load_example_workspace(example_name: &str) -> TempWorkspace {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let example_path = format!("{manifest_dir}/../../examples/{example_name}.ridge");
    let src = fs::read_to_string(&example_path)
        .unwrap_or_else(|e| panic!("could not read example {example_path}: {e}"));
    make_workspace(
        &format!("codegen_example_{example_name}"),
        example_name,
        &src,
    )
}

fn run_pipeline(workspace_path: &Path) -> LoweredWorkspace {
    let disc = discover_workspace(workspace_path);
    let ws_graph = disc.graph.expect("workspace graph must be present");
    let resolved = resolve_workspace(ws_graph);
    let typecheck_result = typecheck_workspace(&resolved);
    lower_workspace(&typecheck_result.typed, &resolved)
}

// ── Stdlib symbol walker ──────────────────────────────────────────────────────

/// Identity shortcut modules/names that are handled at the call site (no map
/// entry required — `lower_call_to_stdlib` erases them).
fn is_identity_shortcut(module: &str, name: &str) -> bool {
    module == "std.text" && name == "toText"
}

/// `std.map.empty` is a literal, not a function call — it appears as a
/// `SymbolRef::Stdlib` node but is emitted as `#{}` via a different code path.
/// Skip it in the coverage walker.
fn is_non_call_symbol(module: &str, name: &str) -> bool {
    module == "std.map" && name == "empty"
}

/// Collect all `(module, name)` pairs from `SymbolRef::Stdlib` nodes in `expr`,
/// pushing them into `out`.  Walks the entire expression tree recursively.
fn collect_stdlib_symbols(expr: &IrExpr, out: &mut Vec<(String, String)>) {
    match expr {
        IrExpr::Symbol {
            sym: SymbolRef::Stdlib { module, name },
            ..
        } => {
            out.push((module.clone(), name.clone()));
        }
        IrExpr::Call { callee, args, .. } => {
            collect_stdlib_symbols(callee, out);
            for a in args {
                collect_stdlib_symbols(a, out);
            }
        }
        IrExpr::Lambda { body, .. } => {
            collect_stdlib_symbols(body, out);
        }
        IrExpr::LetIn { value, body, .. } | IrExpr::VarIn { value, body, .. } => {
            collect_stdlib_symbols(value, out);
            collect_stdlib_symbols(body, out);
        }
        IrExpr::Assign { value, .. } | IrExpr::Return { value, .. } => {
            collect_stdlib_symbols(value, out);
        }
        IrExpr::Block { stmts, .. } => {
            for s in stmts {
                collect_stdlib_symbols(s, out);
            }
        }
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            collect_stdlib_symbols(scrutinee, out);
            for arm in arms {
                if let Some(guard) = &arm.when {
                    collect_stdlib_symbols(guard, out);
                }
                collect_stdlib_symbols(&arm.body, out);
            }
        }
        IrExpr::Construct { fields, .. } => {
            for (_, val) in fields {
                collect_stdlib_symbols(val, out);
            }
        }
        IrExpr::Field { base, .. } => {
            collect_stdlib_symbols(base, out);
        }
        IrExpr::ListLit { elems, .. } | IrExpr::Tuple { elems, .. } => {
            for e in elems {
                collect_stdlib_symbols(e, out);
            }
        }
        IrExpr::Cons { head, tail, .. } => {
            collect_stdlib_symbols(head, out);
            collect_stdlib_symbols(tail, out);
        }
        IrExpr::Send { handle, args, .. } => {
            collect_stdlib_symbols(handle, out);
            for a in args {
                collect_stdlib_symbols(a, out);
            }
        }
        IrExpr::Ask {
            handle,
            args,
            timeout,
            ..
        } => {
            collect_stdlib_symbols(handle, out);
            for a in args {
                collect_stdlib_symbols(a, out);
            }
            if let Some(IrTimeout::Millis(ms)) = timeout {
                collect_stdlib_symbols(ms, out);
            }
        }
        IrExpr::Spawn { args, .. } => {
            for a in args {
                collect_stdlib_symbols(a, out);
            }
        }
        // #[non_exhaustive] catch — ignore unknown variants.
        _ => {}
    }
}

fn collect_from_fn(f: &IrFn, out: &mut Vec<(String, String)>) {
    collect_stdlib_symbols(&f.body, out);
}

fn collect_from_const(c: &IrConst, out: &mut Vec<(String, String)>) {
    collect_stdlib_symbols(&c.value, out);
}

fn collect_from_init(init: &IrInit, out: &mut Vec<(String, String)>) {
    collect_stdlib_symbols(&init.body, out);
}

fn collect_from_handler(h: &IrHandler, out: &mut Vec<(String, String)>) {
    collect_stdlib_symbols(&h.body, out);
}

fn collect_from_actor(a: &IrActor, out: &mut Vec<(String, String)>) {
    for sf in &a.state_fields {
        if let Some(default) = &sf.default {
            collect_stdlib_symbols(default, out);
        }
    }
    if let Some(init) = &a.init {
        collect_from_init(init, out);
    }
    for h in &a.dispatch {
        collect_from_handler(h, out);
    }
}

fn collect_from_workspace(ws: &LoweredWorkspace) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for module in ws.modules.iter().flatten() {
        for item in &module.items {
            match item {
                IrItem::Fn(f) => collect_from_fn(f, &mut out),
                IrItem::Const(c) => collect_from_const(c, &mut out),
                IrItem::Actor(a) => collect_from_actor(a, &mut out),
                _ => {}
            }
        }
    }
    out
}

/// Assert every stdlib symbol referenced in a lowered workspace has a bridge entry.
fn assert_bridge_covers(example_name: &str, ws: &LoweredWorkspace) {
    let symbols = collect_from_workspace(ws);
    for (module, name) in &symbols {
        // Skip identity shortcuts and non-call symbols — they don't need map entries.
        if is_identity_shortcut(module, name) || is_non_call_symbol(module, name) {
            continue;
        }
        assert!(
            stdlib_map::lookup(module, name).is_some(),
            "example '{example_name}': no bridge entry for ({module}, {name})"
        );
    }
}

// ── Coverage tests ────────────────────────────────────────────────────────────

#[test]
fn stdlib_bridge_covers_log_analyzer() {
    let tw = load_example_workspace("log_analyzer");
    let ws = run_pipeline(&tw.path);
    assert_bridge_covers("log_analyzer", &ws);
}

#[test]
fn stdlib_bridge_covers_url_shortener() {
    let tw = load_example_workspace("url_shortener");
    let ws = run_pipeline(&tw.path);
    assert_bridge_covers("url_shortener", &ws);
}

#[test]
fn stdlib_bridge_covers_game_of_life() {
    let tw = load_example_workspace("game_of_life");
    let ws = run_pipeline(&tw.path);
    assert_bridge_covers("game_of_life", &ws);
}

#[test]
fn stdlib_bridge_covers_rate_limiter() {
    let tw = load_example_workspace("rate_limiter");
    let ws = run_pipeline(&tw.path);
    assert_bridge_covers("rate_limiter", &ws);
}

// Lock the new dispatch for `std.net.http.respond`: it must resolve through
// `stdlib_map::lookup` as a regular 2-arg call, not as an identity shortcut.
// The previous (incorrect) treatment lifted the call as `args[0]` (the status
// code Int) and discarded the body, so callers got an Int back where a
// Response was expected, and any 2-arg `respond status body` form crashed at
// codegen with E001 "expects 1 arg, got 2".
#[test]
fn stdlib_bridge_respond_resolves_as_two_arg_call() {
    match stdlib_map::lookup("std.net.http", "respond") {
        Some(BridgeTarget::RidgeStdlibLocal {
            beam_module,
            fn_name,
            arity,
        }) => {
            assert_eq!(beam_module, "std.net.http");
            assert_eq!(fn_name, "respond");
            assert_eq!(*arity, 2);
        }
        other => panic!(
            "expected RidgeStdlibLocal for std.net.http.respond, got {other:?}\n\
             If `respond` is being treated as an identity shortcut, restore the bridge route."
        ),
    }
}

// Source-level smoke: `Map.empty ()` (idiomatic call form against a 0-arity
// stdlib bridge) must round-trip through codegen.  Before the fix the
// parser-supplied `[Unit]` argument made the bridge dispatch fail with
// `E001 stdlib local-call 'std.map.empty' expects 0 args, got 1`.  The
// PR #71 shim that handled the same shape for user-defined `fn f () = …`
// did not cover stdlib bridges.
#[test]
fn stdlib_zero_arity_paren_call_compiles() {
    use ridge_codegen_erl::{codegen_workspace, BuildProfile, CodegenOptions};
    let src = r#"
import std.map as Map

pub fn empty_map () -> Map Text Text =
    Map.empty ()
"#;
    let tw = make_workspace("zero_arity_paren", "empty_map", src);
    let ws = run_pipeline(&tw.path);
    let mut opts = CodegenOptions::default();
    opts.out_root = tw.path.join("target_codegen");
    opts.profile = BuildProfile::Debug;
    opts.invoke_erlc = false;
    opts.install_runtime = false;
    let result = codegen_workspace(&ws, opts);
    let arity_e001s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| {
            let s = format!("{e:?}");
            s.contains("std.map") && s.contains("empty") && s.contains("expects 0 args")
        })
        .collect();
    assert!(
        arity_e001s.is_empty(),
        "codegen on `Map.empty ()` should not surface a `expects 0 args, got 1` error; got: {arity_e001s:?}"
    );
}

// Source-level smoke: `respond status body` must round-trip through codegen
// without errors.  Before the fix the codegen treated `respond` as a 1-arg
// identity shortcut and crashed with E001 "expects 1 arg, got 2".
#[test]
fn stdlib_respond_two_arg_call_compiles() {
    use ridge_codegen_erl::{codegen_workspace, BuildProfile, CodegenOptions};
    let src = r#"
import std.net.http as Http (Response, respond)

pub fn ok () -> Response =
    respond 200 "hello"
"#;
    let tw = make_workspace("respond_two_arg", "ok", src);
    let ws = run_pipeline(&tw.path);
    let mut opts = CodegenOptions::default();
    opts.out_root = tw.path.join("target_codegen");
    opts.profile = BuildProfile::Debug;
    opts.invoke_erlc = false;
    opts.install_runtime = false;
    let result = codegen_workspace(&ws, opts);
    let shortcut_e001s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| format!("{e:?}").contains("identity shortcut"))
        .collect();
    assert!(
        shortcut_e001s.is_empty(),
        "codegen on `respond 200 \"hello\"` should not trip the identity-shortcut path; got: {shortcut_e001s:?}"
    );
}

#[test]
fn stdlib_bridge_no_perm_for_list_map() {
    // T11: std.list.map is now served by path B (RidgeStdlibLocal) because
    // list.ridge has `@ffi("lists", "map", 2)`.  The BEAM target is the same
    // (lists:map/2), but the variant changed from BeamStdlib to RidgeStdlibLocal.
    // No arg permutation is applied — the comment from T14 still holds:
    // Phase 5 delivers IR args in BEAM order (fn, list) for pipe calls.
    match stdlib_map::lookup("std.list", "map") {
        Some(BridgeTarget::RidgeStdlibLocal {
            beam_module,
            fn_name,
            arity,
        }) => {
            assert_eq!(beam_module, "lists");
            assert_eq!(fn_name, "map");
            assert_eq!(*arity, 2);
        }
        other => panic!("expected RidgeStdlibLocal for std.list.map (T11), got {other:?}"),
    }
}
