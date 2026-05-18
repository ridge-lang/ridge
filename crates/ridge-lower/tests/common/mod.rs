//! Shared test helpers for `ridge-lower` integration tests.
//!
//! # Pipeline
//!
//! [`run_pipeline`] wires together the full Phase 1-5 pipeline:
//! `discover_workspace` → `resolve_workspace` → `typecheck_workspace` →
//! `lower_workspace`.
//!
//! # Snapshot renderer
//!
//! [`render_lowered_module`] produces a stable, deterministic YAML projection
//! of a [`LoweredModule`]:
//! - `IrNodeId` values are renumbered in first-appearance order as `n0, n1, ...`
//! - Type variables are rendered by canonical letter scheme (`a, b, c, ...`)
//! - Spans are rendered as `<start>..<end>` byte-offset form (deterministic)
//! - Synthesised local names are preserved verbatim

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_docs_in_private_items,
    clippy::too_many_lines,
    clippy::useless_format,
    clippy::cast_possible_truncation,
    clippy::trivially_copy_pass_by_ref,
    unreachable_patterns,
    dead_code
)]

use ridge_ir::{
    AssignTarget, CtorKind, IrActor, IrArm, IrConst, IrExpr, IrFn, IrHandler, IrInit, IrItem,
    IrLit, IrNodeId, IrParam, IrPat, IrTimeout, LoweredModule, SymbolRef,
};
use ridge_lower::lower_workspace;
use ridge_resolve::{discover_workspace, resolve_workspace};
use ridge_typecheck::typecheck_workspace;
use ridge_types::Type;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// ── Workspace scaffolding ─────────────────────────────────────────────────────

/// Write `content` to `dir/relative_path`, creating parent directories.
pub fn write_file(dir: &Path, relative_path: &str, content: &str) {
    let full = dir.join(relative_path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create dirs");
    }
    fs::write(&full, content).expect("write file");
}

/// A temporary directory that cleans itself up on drop.
pub struct TempWorkspace {
    /// The root path of the temp workspace.
    pub path: PathBuf,
}

impl TempWorkspace {
    /// Create a new temp workspace under the OS temp dir.
    pub fn new(id: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ridge_lower_test_{id}"));
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

/// Build a synthetic workspace in a temp dir for the given source content.
///
/// Creates:
/// ```text
/// <tmp>/ridge.toml           (workspace)
/// <tmp>/apps/demo/ridge.toml (project, kind = "library")
/// <tmp>/apps/demo/src/<name>.ridge
/// ```
pub fn make_workspace(id: &str, module_name: &str, source: &str) -> TempWorkspace {
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

// ── Pipeline runner ───────────────────────────────────────────────────────────

/// Result of running the full pipeline on a fixture.
pub struct PipelineResult {
    /// The lowered workspace.
    pub lowered: ridge_ir::LoweredWorkspace,
}

/// Run the full pipeline: discover → resolve → typecheck → lower.
///
/// Returns `PipelineResult` with the `LoweredWorkspace`.
/// Panics if any step fails fatally (discovery or workspace-graph errors).
pub fn run_pipeline(workspace_path: &Path) -> PipelineResult {
    let disc = discover_workspace(workspace_path);
    let ws_graph = disc.graph.expect("workspace graph must be present");
    let resolved = resolve_workspace(ws_graph);
    let typecheck_result = typecheck_workspace(&resolved);
    let lowered = lower_workspace(&typecheck_result.typed, &resolved);
    PipelineResult { lowered }
}

/// Load an example file from `examples/<name>.ridge` (relative to workspace root).
pub fn load_example_workspace(example_name: &str) -> TempWorkspace {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let example_path = format!("{manifest_dir}/../../examples/{example_name}.ridge");
    let src = fs::read_to_string(&example_path)
        .unwrap_or_else(|e| panic!("could not read example {example_path}: {e}"));
    make_workspace(&format!("example_{example_name}"), example_name, &src)
}

// ── Snapshot renderer ─────────────────────────────────────────────────────────

/// Render a `LoweredModule` as a deterministic YAML string.
///
/// - `IrNodeId` values are renumbered in first-appearance order (`n0`, `n1`, …)
/// - Types are rendered symbolically (no raw `TyVid` integers)
/// - Spans are rendered as `start..end`
/// - Synthesised names (`__prop_ok_*`, `__with_base_*`, etc.) are preserved
pub fn render_lowered_module(m: &LoweredModule) -> String {
    let mut r = Renderer::new();
    r.render_module(m);
    r.buf
}

struct Renderer {
    buf: String,
    indent: usize,
    /// Maps raw `IrNodeId.0` to renumbered index for display.
    node_renumber: HashMap<u32, u32>,
    next_node_index: u32,
}

impl Renderer {
    fn new() -> Self {
        Self {
            buf: String::new(),
            indent: 0,
            node_renumber: HashMap::new(),
            next_node_index: 0,
        }
    }

    fn push(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.buf.push_str("  ");
        }
        self.buf.push_str(s);
        self.buf.push('\n');
    }

    fn push_kv(&mut self, key: &str, val: &str) {
        self.push(&format!("{key}: {val}"));
    }

    fn renumber_node(&mut self, id: IrNodeId) -> u32 {
        let raw = id.0;
        if let Some(&n) = self.node_renumber.get(&raw) {
            n
        } else {
            let n = self.next_node_index;
            self.next_node_index += 1;
            self.node_renumber.insert(raw, n);
            n
        }
    }

    fn render_node_id(&mut self, id: IrNodeId) -> String {
        format!("n{}", self.renumber_node(id))
    }

    fn render_module(&mut self, m: &LoweredModule) {
        self.push(&format!("module: m{}", m.id.0));
        self.push(&format!("items_count: {}", m.items.len()));
        if m.items.is_empty() {
            self.push("items: []");
        } else {
            self.push("items:");
            self.indent += 1;
            for item in &m.items {
                self.render_item(item);
            }
            self.indent -= 1;
        }
    }

    fn render_item(&mut self, item: &IrItem) {
        match item {
            IrItem::Fn(f) => self.render_fn(f),
            IrItem::Actor(a) => self.render_actor(a),
            IrItem::Const(c) => self.render_const(c),
            _ => self.push("- kind: unknown_item"),
        }
    }

    fn render_fn(&mut self, f: &IrFn) {
        self.push(&format!("- kind: fn"));
        self.indent += 1;
        self.push_kv("name", &f.name);
        self.push_kv("pub", &f.is_pub.to_string());
        self.push_kv("main", &f.is_main.to_string());
        self.push_kv("caps", &render_caps(&f.caps));
        self.push_kv("ret_ty", &render_type(&f.ret_ty));
        if f.params.is_empty() {
            self.push("params: []");
        } else {
            self.push("params:");
            self.indent += 1;
            for p in &f.params {
                self.render_param(p);
            }
            self.indent -= 1;
        }
        self.push("body:");
        self.indent += 1;
        self.render_expr(&f.body);
        self.indent -= 1;
        self.indent -= 1;
    }

    fn render_actor(&mut self, a: &IrActor) {
        self.push(&format!("- kind: actor"));
        self.indent += 1;
        self.push_kv("name", &a.name);
        self.push_kv("pub", &a.is_pub.to_string());
        // state fields
        if a.state_fields.is_empty() {
            self.push("state_fields: []");
        } else {
            self.push("state_fields:");
            self.indent += 1;
            for sf in &a.state_fields {
                self.push(&format!("- name: {}", sf.name));
                self.indent += 1;
                self.push_kv("ty", &render_type(&sf.ty));
                self.indent -= 1;
            }
            self.indent -= 1;
        }
        // init
        if let Some(init) = &a.init {
            self.render_init(init);
        } else {
            self.push("init: null");
        }
        // dispatch
        if a.dispatch.is_empty() {
            self.push("handlers: []");
        } else {
            self.push("handlers:");
            self.indent += 1;
            for h in &a.dispatch {
                self.render_handler(h);
            }
            self.indent -= 1;
        }
        self.indent -= 1;
    }

    fn render_init(&mut self, init: &IrInit) {
        self.push("init:");
        self.indent += 1;
        self.push_kv("caps", &render_caps(&init.caps));
        if init.params.is_empty() {
            self.push("params: []");
        } else {
            self.push("params:");
            self.indent += 1;
            for p in &init.params {
                self.render_param(p);
            }
            self.indent -= 1;
        }
        self.push("body:");
        self.indent += 1;
        self.render_expr(&init.body);
        self.indent -= 1;
        self.indent -= 1;
    }

    fn render_handler(&mut self, h: &IrHandler) {
        self.push(&format!("- message: {}", h.message_name));
        self.indent += 1;
        self.push_kv("caps", &render_caps(&h.caps));
        self.push_kv("ret_ty", &render_type(&h.ret_ty));
        if h.params.is_empty() {
            self.push("params: []");
        } else {
            self.push("params:");
            self.indent += 1;
            for p in &h.params {
                self.render_param(p);
            }
            self.indent -= 1;
        }
        self.push("body:");
        self.indent += 1;
        self.render_expr(&h.body);
        self.indent -= 1;
        self.indent -= 1;
    }

    fn render_const(&mut self, c: &IrConst) {
        self.push(&format!("- kind: const"));
        self.indent += 1;
        self.push_kv("name", &c.name);
        self.push_kv("ty", &render_type(&c.ty));
        self.push("value:");
        self.indent += 1;
        self.render_expr(&c.value);
        self.indent -= 1;
        self.indent -= 1;
    }

    fn render_param(&mut self, p: &IrParam) {
        self.push(&format!("- name: {}", p.name));
        self.indent += 1;
        self.push_kv("ty", &render_type(&p.ty));
        self.indent -= 1;
    }

    fn render_expr(&mut self, expr: &IrExpr) {
        match expr {
            IrExpr::Lit { id, value, .. } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Lit({nid}): {}", render_lit(value)));
            }
            IrExpr::Local { id, name, .. } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Local({nid}): {name}"));
            }
            IrExpr::Symbol { id, sym, .. } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Symbol({nid}): {}", render_sym(sym)));
            }
            IrExpr::Call {
                id, callee, args, ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Call({nid}):"));
                self.indent += 1;
                self.push("callee:");
                self.indent += 1;
                self.render_expr(callee);
                self.indent -= 1;
                if args.is_empty() {
                    self.push("args: []");
                } else {
                    self.push("args:");
                    self.indent += 1;
                    for a in args {
                        self.push("-");
                        self.indent += 1;
                        self.render_expr(a);
                        self.indent -= 1;
                    }
                    self.indent -= 1;
                }
                self.indent -= 1;
            }
            IrExpr::Lambda {
                id,
                params,
                body,
                caps,
                ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Lambda({nid}):"));
                self.indent += 1;
                self.push_kv("caps", &render_caps(caps));
                if params.is_empty() {
                    self.push("params: []");
                } else {
                    self.push("params:");
                    self.indent += 1;
                    for p in params {
                        self.render_param(p);
                    }
                    self.indent -= 1;
                }
                self.push("body:");
                self.indent += 1;
                self.render_expr(body);
                self.indent -= 1;
                self.indent -= 1;
            }
            IrExpr::Construct {
                id, ctor, fields, ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Construct({nid}):"));
                self.indent += 1;
                self.push_kv("ctor", &render_sym(ctor));
                if fields.is_empty() {
                    self.push("fields: []");
                } else {
                    self.push("fields:");
                    self.indent += 1;
                    for (name, val) in fields {
                        self.push(&format!("{name}:"));
                        self.indent += 1;
                        self.render_expr(val);
                        self.indent -= 1;
                    }
                    self.indent -= 1;
                }
                self.indent -= 1;
            }
            IrExpr::Field {
                id, base, field, ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Field({nid}).{field}:"));
                self.indent += 1;
                self.render_expr(base);
                self.indent -= 1;
            }
            IrExpr::ListLit { id, elems, .. } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("ListLit({nid}):"));
                self.indent += 1;
                for e in elems {
                    self.render_expr(e);
                }
                self.indent -= 1;
            }
            IrExpr::Tuple { id, elems, .. } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Tuple({nid}):"));
                self.indent += 1;
                for e in elems {
                    self.render_expr(e);
                }
                self.indent -= 1;
            }
            IrExpr::Cons { id, head, tail, .. } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Cons({nid}):"));
                self.indent += 1;
                self.push("head:");
                self.indent += 1;
                self.render_expr(head);
                self.indent -= 1;
                self.push("tail:");
                self.indent += 1;
                self.render_expr(tail);
                self.indent -= 1;
                self.indent -= 1;
            }
            IrExpr::Match {
                id,
                scrutinee,
                arms,
                ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Match({nid}):"));
                self.indent += 1;
                self.push("scrutinee:");
                self.indent += 1;
                self.render_expr(scrutinee);
                self.indent -= 1;
                self.push("arms:");
                self.indent += 1;
                for arm in arms {
                    self.render_arm(arm);
                }
                self.indent -= 1;
                self.indent -= 1;
            }
            IrExpr::Block { id, stmts, .. } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Block({nid}):"));
                self.indent += 1;
                for s in stmts {
                    self.render_expr(s);
                }
                self.indent -= 1;
            }
            IrExpr::LetIn {
                id,
                pat,
                value,
                body,
                ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("LetIn({nid}):"));
                self.indent += 1;
                self.push("pat:");
                self.indent += 1;
                self.render_pat(pat);
                self.indent -= 1;
                self.push("value:");
                self.indent += 1;
                self.render_expr(value);
                self.indent -= 1;
                self.push("body:");
                self.indent += 1;
                self.render_expr(body);
                self.indent -= 1;
                self.indent -= 1;
            }
            IrExpr::VarIn {
                id,
                name,
                ty,
                value,
                body,
                ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("VarIn({nid}):"));
                self.indent += 1;
                self.push_kv("name", name);
                self.push_kv("ty", &render_type(ty));
                self.push("value:");
                self.indent += 1;
                self.render_expr(value);
                self.indent -= 1;
                self.push("body:");
                self.indent += 1;
                self.render_expr(body);
                self.indent -= 1;
                self.indent -= 1;
            }
            IrExpr::Assign {
                id, target, value, ..
            } => {
                let nid = self.render_node_id(*id);
                let tgt = match target {
                    AssignTarget::Local { name, .. } => format!("Local({name})"),
                    AssignTarget::StateField { name, .. } => format!("StateField({name})"),
                    _ => "unknown_target".to_string(),
                };
                self.push(&format!("Assign({nid}) -> {tgt}:"));
                self.indent += 1;
                self.render_expr(value);
                self.indent -= 1;
            }
            IrExpr::Return { id, value, .. } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Return({nid}):"));
                self.indent += 1;
                self.render_expr(value);
                self.indent -= 1;
            }
            IrExpr::Send {
                id,
                handle,
                message,
                args,
                ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Send({nid}) ! {}:", render_sym(message)));
                self.indent += 1;
                self.push("handle:");
                self.indent += 1;
                self.render_expr(handle);
                self.indent -= 1;
                if !args.is_empty() {
                    self.push("args:");
                    self.indent += 1;
                    for a in args {
                        self.render_expr(a);
                    }
                    self.indent -= 1;
                }
                self.indent -= 1;
            }
            IrExpr::Ask {
                id,
                handle,
                message,
                args,
                timeout,
                ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Ask({nid}) ?> {}:", render_sym(message)));
                self.indent += 1;
                self.push("handle:");
                self.indent += 1;
                self.render_expr(handle);
                self.indent -= 1;
                if !args.is_empty() {
                    self.push("args:");
                    self.indent += 1;
                    for a in args {
                        self.render_expr(a);
                    }
                    self.indent -= 1;
                }
                // Render timeout only when Some — None (the default) is omitted
                // so that existing Phase 5 snapshots remain byte-identical.
                match timeout {
                    None => {}
                    Some(IrTimeout::Never) => {
                        self.push("timeout: never");
                    }
                    Some(IrTimeout::Millis(ms_expr)) => {
                        self.push("timeout_ms:");
                        self.indent += 1;
                        self.render_expr(ms_expr);
                        self.indent -= 1;
                    }
                    // #[non_exhaustive] guard — defensive catch for future variants.
                    _ => {
                        self.push("timeout: <unknown>");
                    }
                }
                self.indent -= 1;
            }
            IrExpr::Spawn {
                id, actor, args, ..
            } => {
                let nid = self.render_node_id(*id);
                self.push(&format!("Spawn({nid}) {}:", render_sym(actor)));
                self.indent += 1;
                if args.is_empty() {
                    self.push("args: []");
                } else {
                    self.push("args:");
                    self.indent += 1;
                    for a in args {
                        self.render_expr(a);
                    }
                    self.indent -= 1;
                }
                self.indent -= 1;
            }
            _ => {
                self.push("Expr: <unknown_variant>");
            }
        }
    }

    fn render_arm(&mut self, arm: &IrArm) {
        self.push("-");
        self.indent += 1;
        self.push("pat:");
        self.indent += 1;
        self.render_pat(&arm.pat);
        self.indent -= 1;
        if let Some(when) = &arm.when {
            self.push("when:");
            self.indent += 1;
            self.render_expr(when);
            self.indent -= 1;
        }
        self.push("body:");
        self.indent += 1;
        self.render_expr(&arm.body);
        self.indent -= 1;
        self.indent -= 1;
    }

    fn render_pat(&mut self, pat: &IrPat) {
        match pat {
            IrPat::Wild { .. } => self.push("Wild"),
            IrPat::Lit { value, .. } => self.push(&format!("Lit: {}", render_lit(value))),
            IrPat::Bind {
                name, inner: None, ..
            } => self.push(&format!("Bind: {name}")),
            IrPat::Bind {
                name,
                inner: Some(inner),
                ..
            } => {
                self.push(&format!("Bind({name}):"));
                self.indent += 1;
                self.render_pat(inner);
                self.indent -= 1;
            }
            IrPat::Ctor {
                sym, fields, args, ..
            } => {
                self.push(&format!("Ctor: {}", render_sym(sym)));
                if !fields.is_empty() {
                    self.indent += 1;
                    self.push("fields:");
                    self.indent += 1;
                    for (name, p) in fields {
                        self.push(&format!("{name}:"));
                        self.indent += 1;
                        self.render_pat(p);
                        self.indent -= 1;
                    }
                    self.indent -= 1;
                    self.indent -= 1;
                }
                if !args.is_empty() {
                    self.indent += 1;
                    self.push("args:");
                    self.indent += 1;
                    for p in args {
                        self.render_pat(p);
                    }
                    self.indent -= 1;
                    self.indent -= 1;
                }
            }
            IrPat::Tuple { elems, .. } => {
                self.push("Tuple:");
                self.indent += 1;
                for e in elems {
                    self.render_pat(e);
                }
                self.indent -= 1;
            }
            IrPat::Cons { head, tail, .. } => {
                self.push("Cons:");
                self.indent += 1;
                self.push("head:");
                self.indent += 1;
                self.render_pat(head);
                self.indent -= 1;
                self.push("tail:");
                self.indent += 1;
                self.render_pat(tail);
                self.indent -= 1;
                self.indent -= 1;
            }
            IrPat::Nil { .. } => self.push("Nil"),
            _ => self.push("Pat: <unknown_variant>"),
        }
    }
}

// ── Type rendering ─────────────────────────────────────────────────────────────

fn render_type(ty: &Type) -> String {
    match ty {
        Type::Con(id, args) => {
            if args.is_empty() {
                format!("Con({})", id.0)
            } else {
                let arg_strs: Vec<_> = args.iter().map(render_type).collect();
                format!("Con({}) [{}]", id.0, arg_strs.join(", "))
            }
        }
        Type::Var(vid) => {
            // Render type variables as letter scheme: 0→a, 1→b, …, 25→z, 26→a1
            let n = vid.0 as usize;
            let letter = (b'a' + (n % 26) as u8) as char;
            let suffix = if n < 26 {
                String::new()
            } else {
                (n / 26).to_string()
            };
            format!("{letter}{suffix}")
        }
        Type::Error => "Error".to_string(),
        _ => "Type".to_string(),
    }
}

// ── Lit rendering ──────────────────────────────────────────────────────────────

fn render_lit(lit: &IrLit) -> String {
    match lit {
        IrLit::Int(n) => format!("Int({n})"),
        IrLit::Float(f) => format!("Float({f})"),
        IrLit::Bool(b) => format!("Bool({b})"),
        IrLit::Text(s) => format!("Text({s:?})"),
        IrLit::Unit => "Unit".to_string(),
        IrLit::EmptyList => "EmptyList".to_string(),
        _ => "Lit".to_string(),
    }
}

// ── Symbol rendering ───────────────────────────────────────────────────────────

fn render_sym(sym: &SymbolRef) -> String {
    match sym {
        SymbolRef::Local { name, module } => format!("Local({name} @ m{})", module.0),
        SymbolRef::Stdlib { module, name } => format!("Stdlib({module}.{name})"),
        SymbolRef::External { module, name } => format!("External({name} @ m{})", module.0),
        SymbolRef::Handler {
            actor_module,
            actor,
            handler,
        } => format!("Handler(m{}.{actor}.{handler})", actor_module.0),
        SymbolRef::ActorType { module, name } => format!("ActorType({name} @ m{})", module.0),
        SymbolRef::Constructor {
            ctor_kind,
            name,
            owner_type,
            ..
        } => {
            let kind = match ctor_kind {
                CtorKind::Record => "Record",
                CtorKind::UnionVariant => "Variant",
            };
            format!("Ctor({kind}:{name} owner={})", owner_type.0)
        }
        SymbolRef::Prelude { name } => format!("Prelude({name})"),
        _ => "Symbol".to_string(),
    }
}

// ── Capability rendering ───────────────────────────────────────────────────────

fn render_caps(caps: &ridge_ir::CapabilitySet) -> String {
    // CapabilitySet is a bitmask; render the debug form but sanitised.
    let dbg = format!("{caps:?}");
    dbg
}
