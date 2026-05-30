//! Module graph construction: parse every module, collect tentative import edges,
//! and detect import cycles via iterative Tarjan SCC.
//!
//! `build_module_graph` reads each source file, calls the parser, and records
//! `TentativeEdge` entries for every `import` declaration. `detect_cycles`
//! (iterative Tarjan) emits `R003 CyclicImport` and `R004 SelfImport`.
//! Import-target `ModuleId` resolution to authoritative bindings happens during
//! import resolution.

use std::sync::Arc;

use ridge_ast::{Item, Module};
use ridge_lexer::{LexError, Span};
use ridge_parser::ParseError;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{ModuleId, ModuleMetadata, WorkspaceGraph};

use crate::error::ResolveError;

// ── Public types ──────────────────────────────────────────────────────────────

/// A per-module parse result keyed by [`ModuleId`].
///
/// Indexed by `ModuleId.0` for O(1) access.
#[derive(Debug)]
pub struct ParsedModule {
    /// The `ModuleMetadata` this entry corresponds to.
    pub id: ModuleId,
    /// The raw source text, retained for snapshot / diagnostic rendering.
    pub source: String,
    /// The parser's AST.
    ///
    /// Always present (the parser always produces a well-formed `Module`, possibly
    /// empty, even on errors). On I/O error the AST is a stub empty `Module`.
    pub ast: Arc<Module>,
    /// Parser errors (P###).
    pub parse_errors: Vec<ParseError>,
    /// Lexer errors (L###) seen during the initial lex pass.
    pub lex_errors: Vec<LexError>,
    /// I/O error that prevented reading the source file, if any.
    ///
    /// When `Some`, `source` is empty and `ast` is a stub empty `Module`.
    pub read_error: Option<String>,
}

/// A tentative import edge.
///
/// Target `ModuleId` resolution happens during import resolution.
#[derive(Debug, Clone)]
pub struct TentativeEdge {
    /// The module that contains this import declaration.
    pub from: ModuleId,
    /// Dot-separated module path, e.g. `"std.list"` or `"acme.domain.Models.User"`.
    pub path_dotted: String,
    /// The `as Alias` rename, if present.
    pub alias: Option<String>,
    /// Explicit item list from `import … (a, b)`; `None` for whole-module imports.
    pub items: Option<Vec<String>>,
    /// Full span of the `ImportDecl`.
    pub span: Span,
}

/// The populated module graph.
///
/// Edges are tentative — import resolution resolves them to concrete `ModuleId` targets.
#[derive(Debug)]
pub struct ModuleGraph {
    /// One entry per module in `WorkspaceGraph.modules`, same index basis
    /// (`ModuleId.0` == Vec index).
    pub modules: Vec<ParsedModule>,
    /// All import edges across all modules, in the order they were encountered.
    pub tentative_edges: Vec<TentativeEdge>,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Parse every module of the workspace and collect tentative import edges.
///
/// The input [`WorkspaceGraph`] is NOT mutated; the returned [`ModuleGraph`] is
/// a parallel structure keyed by the same `ModuleId` indices.
///
/// Non-fatal: an I/O or parse error on one module does not stop processing of
/// the others.
#[must_use]
pub fn build_module_graph(ws: &WorkspaceGraph) -> ModuleGraph {
    let mut modules: Vec<ParsedModule> = Vec::with_capacity(ws.modules.len());
    let mut tentative_edges: Vec<TentativeEdge> = Vec::new();

    for meta in &ws.modules {
        let (parsed, mut edges) = parse_and_collect_imports(meta.id, &meta.file_path);
        tentative_edges.append(&mut edges);
        modules.push(parsed);
    }

    ModuleGraph {
        modules,
        tentative_edges,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Parse a single module source file and collect its import edges.
///
/// Accepts a `file_path` for I/O. On I/O error, returns a `ParsedModule` with
/// `read_error` set and an empty AST stub produced by parsing `""`.
pub(crate) fn parse_and_collect_imports(
    module_id: ModuleId,
    file_path: &std::path::Path,
) -> (ParsedModule, Vec<TentativeEdge>) {
    // Read the source file.
    let (source, read_error) = match std::fs::read_to_string(file_path) {
        Ok(src) => (src, None),
        Err(e) => (String::new(), Some(e.to_string())),
    };

    // Parse (always succeeds structurally; may carry errors).
    let result = ridge_parser::parse_source(&source);

    let parsed = ParsedModule {
        id: module_id,
        source,
        ast: Arc::new(result.module),
        parse_errors: result.errors,
        lex_errors: result.lex_errors,
        read_error,
    };

    // Walk items and collect import edges.
    let edges = collect_import_edges(module_id, &parsed.ast);

    (parsed, edges)
}

/// Walk the top-level items of a `Module` and return one `TentativeEdge` per
/// `import` declaration encountered.
fn collect_import_edges(module_id: ModuleId, module: &Module) -> Vec<TentativeEdge> {
    let mut edges = Vec::new();

    for item in &module.items {
        if let Item::Import(decl) = item {
            let path_dotted = decl
                .path
                .segments
                .iter()
                .map(|seg| seg.text.clone())
                .collect::<Vec<_>>()
                .join(".");

            let alias = decl.alias.as_ref().map(|ident| ident.text.clone());

            let items = decl
                .items
                .as_ref()
                .map(|v| v.iter().map(|ident| ident.text.clone()).collect::<Vec<_>>());

            edges.push(TentativeEdge {
                from: module_id,
                path_dotted,
                alias,
                items,
                span: decl.span,
            });
        }
    }

    edges
}

// ── Cycle detection ───────────────────────────────────────────────────────────

/// Build a workspace-only adjacency list from tentative edges.
///
/// Edges whose `path_dotted` does not match any module FQN in the workspace
/// (stdlib paths, external paths, unresolved paths) are silently skipped —
/// they cannot participate in workspace-internal cycles.
///
/// Returns a `Vec<Vec<ModuleId>>` indexed by `ModuleId.0`.
fn build_workspace_adjacency(ws: &WorkspaceGraph, g: &ModuleGraph) -> Vec<Vec<ModuleId>> {
    // Build a fast index: FQN → ModuleId.
    let fqn_index: FxHashMap<&str, ModuleId> = ws
        .modules
        .iter()
        .map(|m| (m.fully_qualified_name.as_str(), m.id))
        .collect();

    let mut adj: Vec<Vec<ModuleId>> = (0..ws.modules.len()).map(|_| Vec::new()).collect();
    for edge in &g.tentative_edges {
        if let Some(&target_id) = fqn_index.get(edge.path_dotted.as_str()) {
            adj[edge.from.0 as usize].push(target_id);
        }
        // else: stdlib / unresolved / external — ignored at this layer
    }
    adj
}

/// Look up a `ModuleId` for a dotted path string in the workspace.
///
/// Returns `None` when the path is not a workspace module FQN.
fn fqn_to_module_id(ws: &WorkspaceGraph, path_dotted: &str) -> Option<ModuleId> {
    // Linear scan is acceptable; ws.modules is sorted so a binary-search upgrade
    // is straightforward if the workspace grows very large.
    ws.modules
        .iter()
        .find(|m| m.fully_qualified_name == path_dotted)
        .map(|m| m.id)
}

/// Iterative Tarjan strongly-connected-components algorithm.
///
/// Returns SCCs in reverse topological order (leaves first).  Each SCC is a
/// `Vec<usize>` of node indices into `adj`.
///
/// The algorithm is fully iterative (no recursion) to avoid stack-overflow on
/// deep graphs on Windows with its 1 MiB default thread stack (plan §4.2 risk R1).
///
/// Output is deterministic: SCC order and node order within each SCC depend
/// only on adjacency order, not on hash-map iteration.
#[must_use]
pub fn tarjan_scc(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = adj.len();
    let mut index: Vec<Option<u32>> = vec![None; n];
    let mut lowlink: Vec<u32> = vec![0; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index: u32 = 0;
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    // work stack: (node, next_neighbour_idx)
    let mut work: Vec<(usize, usize)> = Vec::new();

    for start in 0..n {
        if index[start].is_some() {
            continue;
        }

        work.push((start, 0));

        while let Some(frame) = work.last_mut() {
            let v = frame.0;
            let i = frame.1;

            if i == 0 {
                // First visit: assign discovery index and lowlink.
                index[v] = Some(next_index);
                lowlink[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }

            // Fetch the number of neighbours (borrow ends before we push).
            let neighbour_count = adj[v].len();

            if i < neighbour_count {
                let w = adj[v][i];
                // Advance the frame's position.
                frame.1 += 1;

                if index[w].is_none() {
                    // Tree edge: recurse into w.
                    work.push((w, 0));
                } else if on_stack[w] {
                    // Back edge: update lowlink.
                    // invariant: index[w] is Some because w was visited
                    if let Some(iw) = index[w] {
                        if iw < lowlink[v] {
                            lowlink[v] = iw;
                        }
                    }
                }
            } else {
                // All neighbours processed.
                // invariant: index[v] is Some (assigned on first visit)
                let lv = lowlink[v];
                let iv = index[v].unwrap_or(lv);

                if lv == iv {
                    // v is a root: pop the SCC off the node stack.
                    let mut scc = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        scc.push(w);
                        if w == v {
                            break;
                        }
                    }
                    sccs.push(scc);
                }

                // Pop this frame.
                work.pop();

                // Propagate lowlink to parent.
                if let Some(parent_frame) = work.last() {
                    let parent = parent_frame.0;
                    if lowlink[v] < lowlink[parent] {
                        lowlink[parent] = lowlink[v];
                    }
                }
            }
        }
    }

    sccs
}

/// Detect cyclic imports in the workspace module graph.
///
/// Emits:
/// - `R003 CyclicImport` — one per SCC of size > 1 (i.e., a real cycle).
/// - `R004 SelfImport`   — one per self-import edge (module importing its own FQN).
///
/// Both are **non-fatal**: this function returns errors that the caller may
/// accumulate and continue processing (plan §4.2).
#[must_use]
pub fn detect_cycles(ws: &WorkspaceGraph, g: &ModuleGraph) -> Vec<ResolveError> {
    let adj_mid = build_workspace_adjacency(ws, g);
    let adj_usize: Vec<Vec<usize>> = adj_mid
        .iter()
        .map(|v| v.iter().map(|m| m.0 as usize).collect())
        .collect();

    let mut errors = Vec::new();

    // 1. Self-imports: edge a → a where path_dotted == from-module's own FQN.
    //    We detect via tentative_edges so we get the precise import span.
    for edge in &g.tentative_edges {
        if let Some(meta) = ws.modules.get(edge.from.0 as usize) {
            if edge.path_dotted == meta.fully_qualified_name {
                errors.push(ResolveError::SelfImport { span: edge.span });
            }
        }
    }

    // 2. SCCs of size > 1 → R003.
    for scc in tarjan_scc(&adj_usize) {
        if scc.len() < 2 {
            // Size-1 SCCs are either isolated nodes or self-loops already
            // covered by R004 above.
            continue;
        }

        // Sort the SCC nodes by ModuleId for stable diagnostic output.
        // invariant: workspace cannot have > u32::MAX modules, so this truncation
        // is safe in practice. We use try_from to satisfy the lint.
        let mut cycle: Vec<ModuleId> = scc
            .iter()
            .map(|&i| ModuleId(u32::try_from(i).unwrap_or(u32::MAX)))
            .collect();
        cycle.sort_by_key(|m| m.0);

        let lowest = cycle[0];
        let cycle_set: FxHashSet<ModuleId> = cycle.iter().copied().collect();

        // Find the span of the first edge from the lowest ModuleId whose target
        // is also in the cycle. Falls back to the module's file-span if no
        // matching edge is found (defensive — should not occur on a valid cycle).
        let first_edge_span = g
            .tentative_edges
            .iter()
            .find(|e| {
                e.from == lowest
                    && fqn_to_module_id(ws, &e.path_dotted).is_some_and(|t| cycle_set.contains(&t))
            })
            .map(|e| e.span)
            .or_else(|| {
                // Defensive fallback: use the first edge from any cycle member
                // whose target is also in the cycle.
                g.tentative_edges
                    .iter()
                    .find(|e| {
                        cycle_set.contains(&e.from)
                            && fqn_to_module_id(ws, &e.path_dotted)
                                .is_some_and(|t| cycle_set.contains(&t))
                    })
                    .map(|e| e.span)
            })
            .unwrap_or_else(|| {
                // Last-resort fallback: use the file-span of the lowest module.
                // This path only triggers if graph invariants are violated.
                ws.modules
                    .get(lowest.0 as usize)
                    .map_or(Span::point(0), |m: &ModuleMetadata| m.span_within_file)
            });

        errors.push(ResolveError::CyclicImport {
            cycle,
            first_edge: first_edge_span,
        });
    }

    errors
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    use crate::{DiscoveryResult, WorkspaceGraph};
    use ridge_ast::Span;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Write `content` to `dir/relative_path`, creating parent directories.
    fn write_file(dir: &std::path::Path, relative_path: &str, content: &str) {
        let full = dir.join(relative_path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        fs::write(full, content).expect("write file");
    }

    fn workspace_toml(members: &[&str]) -> String {
        let members_list = members
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [{members_list}]\n"
        )
    }

    fn project_toml(name: &str) -> String {
        format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\nkind = \"library\"\n")
    }

    /// Build a minimal 1-module workspace in a tempdir with the given source.
    fn build_single_module_ws(src: &str) -> (TempDir, WorkspaceGraph) {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(td.path(), "libs/proj/src/Main.ridge", src);
        let disc = crate::discover_workspace(td.path());
        let ws = disc.graph.expect("workspace graph");
        (td, ws)
    }

    // ── Test 1: empty module → 0 edges, 0 errors ─────────────────────────────

    #[test]
    fn t1_empty_module_no_edges_no_errors() {
        let (_td, ws) = build_single_module_ws("");
        let g = build_module_graph(&ws);
        assert_eq!(g.modules.len(), 1);
        let m = &g.modules[0];
        assert!(m.read_error.is_none(), "read_error: {:?}", m.read_error);
        assert!(
            m.parse_errors.is_empty(),
            "parse_errors: {:?}",
            m.parse_errors
        );
        assert!(m.lex_errors.is_empty(), "lex_errors: {:?}", m.lex_errors);
        assert!(g.tentative_edges.is_empty(), "expected no edges");
    }

    // ── Test 2: single import → 1 edge, correct path_dotted ──────────────────

    #[test]
    fn t2_single_import_records_one_edge() {
        let (_td, ws) = build_single_module_ws("import std.list\n");
        let g = build_module_graph(&ws);
        let m = &g.modules[0];
        assert!(
            m.parse_errors.is_empty(),
            "parse_errors: {:?}",
            m.parse_errors
        );
        assert!(m.lex_errors.is_empty(), "lex_errors: {:?}", m.lex_errors);
        assert_eq!(g.tentative_edges.len(), 1);
        let e = &g.tentative_edges[0];
        assert_eq!(e.path_dotted, "std.list");
        assert!(e.alias.is_none(), "expected no alias");
        assert!(e.items.is_none(), "expected no items");
    }

    // ── Test 3: import with `as` alias ────────────────────────────────────────

    #[test]
    fn t3_import_with_alias() {
        let (_td, ws) = build_single_module_ws("import std.list as List\n");
        let g = build_module_graph(&ws);
        let m = &g.modules[0];
        assert!(
            m.parse_errors.is_empty(),
            "parse_errors: {:?}",
            m.parse_errors
        );
        assert_eq!(g.tentative_edges.len(), 1);
        let e = &g.tentative_edges[0];
        assert_eq!(e.path_dotted, "std.list");
        assert_eq!(e.alias, Some("List".to_owned()));
        assert!(e.items.is_none());
    }

    // ── Test 4: import with item list ─────────────────────────────────────────

    #[test]
    fn t4_import_with_items() {
        let (_td, ws) = build_single_module_ws("import std.map (get, insert)\n");
        let g = build_module_graph(&ws);
        let m = &g.modules[0];
        assert!(
            m.parse_errors.is_empty(),
            "parse_errors: {:?}",
            m.parse_errors
        );
        assert_eq!(g.tentative_edges.len(), 1);
        let e = &g.tentative_edges[0];
        assert_eq!(e.path_dotted, "std.map");
        assert!(e.alias.is_none());
        assert_eq!(e.items, Some(vec!["get".to_owned(), "insert".to_owned()]));
    }

    // ── Test 5: multiple imports → edges in source order ─────────────────────

    #[test]
    fn t5_multiple_imports_in_source_order() {
        let src = "import std.io as Io\nimport std.list as List\nimport std.map as Map\n";
        let (_td, ws) = build_single_module_ws(src);
        let g = build_module_graph(&ws);
        let m = &g.modules[0];
        assert!(
            m.parse_errors.is_empty(),
            "parse_errors: {:?}",
            m.parse_errors
        );
        assert_eq!(g.tentative_edges.len(), 3);
        assert_eq!(g.tentative_edges[0].path_dotted, "std.io");
        assert_eq!(g.tentative_edges[1].path_dotted, "std.list");
        assert_eq!(g.tentative_edges[2].path_dotted, "std.map");
    }

    // ── Test 6: parse error does not prevent edge collection ─────────────────
    //
    // Source has an import before the invalid token. The import above the error
    // should still be recorded (non-fatal parse policy).

    #[test]
    fn t6_parse_error_still_records_valid_imports() {
        // A valid import followed by a bare `!!!` token that the parser cannot
        // recognise. The parser accumulates an error but still returns the
        // ParsedModule with the valid import recorded.
        let src = "import std.list as List\n!!!\n";
        let (_td, ws) = build_single_module_ws(src);
        let g = build_module_graph(&ws);
        let m = &g.modules[0];
        // The read_error must be absent — the file was readable.
        assert!(m.read_error.is_none(), "read_error: {:?}", m.read_error);
        // parse_errors or lex_errors should be non-empty (the `!!!` is invalid).
        let has_any_error = !m.parse_errors.is_empty() || !m.lex_errors.is_empty();
        assert!(
            has_any_error,
            "expected at least one parse/lex error from `!!!`"
        );
        // The valid import before the error must still be recorded.
        assert!(
            g.tentative_edges
                .iter()
                .any(|e| e.path_dotted == "std.list"),
            "expected std.list edge even in presence of later parse error; edges: {:?}",
            g.tentative_edges
                .iter()
                .map(|e| &e.path_dotted)
                .collect::<Vec<_>>()
        );
    }

    // ── Test 7: 2-module workspace → modules.len() == 2, edges from both ──────

    #[test]
    fn t7_two_module_workspace_both_modules_and_edges() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(
            td.path(),
            "libs/proj/src/Alpha.ridge",
            "import std.io as Io\n",
        );
        write_file(
            td.path(),
            "libs/proj/src/Beta.ridge",
            "import std.list as List\nimport std.map as Map\n",
        );

        let disc: DiscoveryResult = crate::discover_workspace(td.path());
        let ws = disc.graph.expect("workspace graph");
        let g = build_module_graph(&ws);

        assert_eq!(g.modules.len(), 2, "expected 2 modules");
        // Combined edges: 1 from Alpha + 2 from Beta = 3.
        assert_eq!(
            g.tentative_edges.len(),
            3,
            "expected 3 tentative edges total"
        );

        // Verify both modules parsed cleanly.
        for m in &g.modules {
            assert!(
                m.read_error.is_none(),
                "read_error on {:?}: {:?}",
                m.id,
                m.read_error
            );
            assert!(
                m.parse_errors.is_empty(),
                "parse_errors on {:?}: {:?}",
                m.id,
                m.parse_errors
            );
            assert!(
                m.lex_errors.is_empty(),
                "lex_errors on {:?}: {:?}",
                m.id,
                m.lex_errors
            );
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Tarjan SCC algorithm tests (algorithm-level, synthetic adjacency)
    // ═══════════════════════════════════════════════════════════════════════════

    // ── Tarjan-A1: empty graph → empty SCC list ───────────────────────────────

    #[test]
    fn tarjan_empty_graph_yields_no_sccs() {
        let sccs = tarjan_scc(&[]);
        assert!(sccs.is_empty(), "expected no SCCs for empty graph");
    }

    // ── Tarjan-A2: single node, no edges → 1 SCC of size 1 ──────────────────

    #[test]
    fn tarjan_single_node_no_edges_one_scc() {
        let adj: Vec<Vec<usize>> = vec![vec![]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0], vec![0]);
    }

    // ── Tarjan-A3: two nodes, no edges → 2 SCCs of size 1 each ──────────────

    #[test]
    fn tarjan_two_nodes_no_edges_two_sccs() {
        let adj: Vec<Vec<usize>> = vec![vec![], vec![]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 2);
        // Each SCC is a singleton.
        for scc in &sccs {
            assert_eq!(scc.len(), 1);
        }
    }

    // ── Tarjan-A4: linear chain 0→1→2→3 → 4 SCCs of size 1 ──────────────────

    #[test]
    fn tarjan_linear_chain_four_singleton_sccs() {
        // 0 → 1 → 2 → 3
        let adj: Vec<Vec<usize>> = vec![vec![1], vec![2], vec![3], vec![]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(
            sccs.len(),
            4,
            "expected 4 singleton SCCs for a linear chain"
        );
        for scc in &sccs {
            assert_eq!(scc.len(), 1, "each SCC in a chain must be a singleton");
        }
    }

    // ── Tarjan-A5: self-loop on node 0 → SCC of size 1 (self-loop ≠ 2-node SCC)

    #[test]
    fn tarjan_self_loop_gives_one_singleton_scc() {
        // Node 0 has a self-loop.  Tarjan does not expand this to a 2-node SCC.
        let adj: Vec<Vec<usize>> = vec![vec![0]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0].len(), 1, "self-loop stays in a 1-node SCC");
        assert_eq!(sccs[0][0], 0);
    }

    // ── Tarjan-A6: two-cycle 0→1→0 → 1 SCC of size 2 ────────────────────────

    #[test]
    fn tarjan_two_cycle_gives_one_scc_of_size_two() {
        // 0 → 1 → 0
        let adj: Vec<Vec<usize>> = vec![vec![1], vec![0]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 1, "one SCC expected for a 2-cycle");
        assert_eq!(sccs[0].len(), 2, "SCC must contain both nodes");
        let mut nodes = sccs[0].clone();
        nodes.sort_unstable();
        assert_eq!(nodes, vec![0, 1]);
    }

    // ── Tarjan-A7: three-cycle 0→1→2→0 → 1 SCC of size 3 ────────────────────

    #[test]
    fn tarjan_three_cycle_gives_one_scc_of_size_three() {
        // 0 → 1 → 2 → 0
        let adj: Vec<Vec<usize>> = vec![vec![1], vec![2], vec![0]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 1, "one SCC expected for a 3-cycle");
        assert_eq!(sccs[0].len(), 3);
        let mut nodes = sccs[0].clone();
        nodes.sort_unstable();
        assert_eq!(nodes, vec![0, 1, 2]);
    }

    // ── Tarjan-A8: five-cycle 0→1→2→3→4→0 → 1 SCC of size 5 ────────────────

    #[test]
    fn tarjan_five_cycle_gives_one_scc_of_size_five() {
        // 0 → 1 → 2 → 3 → 4 → 0
        let adj: Vec<Vec<usize>> = vec![vec![1], vec![2], vec![3], vec![4], vec![0]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 1, "one SCC expected for a 5-cycle");
        assert_eq!(sccs[0].len(), 5, "all 5 nodes must be in the cycle SCC");
        let mut nodes = sccs[0].clone();
        nodes.sort_unstable();
        assert_eq!(nodes, vec![0, 1, 2, 3, 4]);
    }

    // ── Tarjan-A9: diamond 0→1, 0→2, 1→3, 2→3 → 4 SCCs of size 1 ───────────

    #[test]
    fn tarjan_diamond_no_cycle_four_singleton_sccs() {
        // 0 → 1 → 3
        // 0 → 2 → 3
        let adj: Vec<Vec<usize>> = vec![vec![1, 2], vec![3], vec![3], vec![]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 4, "diamond has no cycles — 4 singleton SCCs");
        for scc in &sccs {
            assert_eq!(scc.len(), 1);
        }
    }

    // ── Tarjan-A10: two disjoint cycles 0→1→0, 2→3→2 → 2 SCCs of size 2 ─────

    #[test]
    fn tarjan_two_disjoint_cycles_two_sccs_of_size_two() {
        // 0 → 1 → 0   and   2 → 3 → 2
        let adj: Vec<Vec<usize>> = vec![vec![1], vec![0], vec![3], vec![2]];
        let sccs = tarjan_scc(&adj);
        assert_eq!(sccs.len(), 2, "two disjoint cycles → two SCCs");
        for scc in &sccs {
            assert_eq!(scc.len(), 2, "each cycle SCC must have 2 nodes");
        }
    }

    // ── Tarjan-A11: SCC stability — same adjacency → same output ─────────────

    #[test]
    fn tarjan_deterministic_across_reruns() {
        // 0 → 1 → 2 → 0   plus   3 → 4 → 3
        let adj: Vec<Vec<usize>> = vec![vec![1], vec![2], vec![0], vec![4], vec![3]];
        let run1 = tarjan_scc(&adj);
        let run2 = tarjan_scc(&adj);
        assert_eq!(
            run1, run2,
            "tarjan_scc must produce identical output on the same input"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // detect_cycles integration tests (synthetic WorkspaceGraph + ModuleGraph)
    // ═══════════════════════════════════════════════════════════════════════════

    // Helper: build a synthetic WorkspaceGraph with `n` modules named
    // "proj.ModN" (0-indexed), and a ModuleGraph with no parsed content.
    #[allow(clippy::cast_possible_truncation)]
    fn synthetic_workspace(n: usize) -> (WorkspaceGraph, ModuleGraph) {
        let modules: Vec<crate::ModuleMetadata> = (0..n)
            .map(|i| crate::ModuleMetadata {
                id: ModuleId(i as u32),
                project: crate::ProjectId(0),
                fully_qualified_name: format!("proj.Mod{i}"),
                file_path: std::path::PathBuf::from(format!("/fake/Mod{i}.ridge")),
                span_within_file: Span::point(0),
            })
            .collect();

        let ws = WorkspaceGraph {
            root: std::path::PathBuf::from("/fake"),
            manifest: crate::manifest::WorkspaceManifest {
                name: "test-ws".into(),
                version: "0.1.0".into(),
                members_globs: vec![],
                forbid_rules: vec![],
                dependencies: vec![],
                capabilities_deny: vec![],
                source_path: std::path::PathBuf::from("/fake/ridge.toml"),
            },
            projects: vec![],
            modules,
            deps: vec![vec![]; n],
            is_stdlib: false,
        };

        let mg = ModuleGraph {
            modules: Vec::new(), // not needed for detect_cycles
            tentative_edges: Vec::new(),
        };

        (ws, mg)
    }

    /// Add a `TentativeEdge` to `mg` from module `from_idx` to FQN `to_fqn`.
    #[allow(clippy::cast_possible_truncation)]
    fn add_edge(mg: &mut ModuleGraph, from_idx: usize, to_fqn: &str, span: Span) {
        mg.tentative_edges.push(TentativeEdge {
            from: ModuleId(from_idx as u32),
            path_dotted: to_fqn.to_owned(),
            alias: None,
            items: None,
            span,
        });
    }

    // ── detect_cycles: no imports → no errors ────────────────────────────────

    #[test]
    fn detect_cycles_no_imports_no_errors() {
        let (ws, mg) = synthetic_workspace(3);
        let errors = detect_cycles(&ws, &mg);
        assert!(
            errors.is_empty(),
            "no imports → no R003/R004; got: {errors:?}"
        );
    }

    // ── detect_cycles: self-import → exactly one R004, no R003 ──────────────

    #[test]
    fn detect_cycles_self_import_emits_r004_only() {
        let (ws, mut mg) = synthetic_workspace(2);
        let self_span = Span::new(5, 20);
        // proj.Mod0 imports proj.Mod0 (itself)
        add_edge(&mut mg, 0, "proj.Mod0", self_span);

        let errors = detect_cycles(&ws, &mg);

        let r004_count = errors
            .iter()
            .filter(|e| matches!(e, ResolveError::SelfImport { .. }))
            .count();
        let r003_count = errors
            .iter()
            .filter(|e| matches!(e, ResolveError::CyclicImport { .. }))
            .count();

        assert_eq!(r004_count, 1, "expected exactly one R004");
        assert_eq!(r003_count, 0, "self-import must NOT emit R003");

        // The span must match the import statement.
        if let Some(ResolveError::SelfImport { span }) = errors
            .iter()
            .find(|e| matches!(e, ResolveError::SelfImport { .. }))
        {
            assert_eq!(*span, self_span, "R004 span must match the import span");
        }
    }

    // ── detect_cycles: two-module cycle → one R003, cycle.len() == 2 ─────────

    #[test]
    fn detect_cycles_two_module_cycle_emits_r003() {
        // proj.Mod0 imports proj.Mod1, proj.Mod1 imports proj.Mod0
        let (ws, mut mg) = synthetic_workspace(2);
        let span_0_to_1 = Span::new(1, 10);
        let span_1_to_0 = Span::new(11, 20);
        add_edge(&mut mg, 0, "proj.Mod1", span_0_to_1);
        add_edge(&mut mg, 1, "proj.Mod0", span_1_to_0);

        let errors = detect_cycles(&ws, &mg);

        let r003: Vec<_> = errors
            .iter()
            .filter_map(|e| {
                if let ResolveError::CyclicImport { cycle, first_edge } = e {
                    Some((cycle, first_edge))
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(r003.len(), 1, "expected exactly one R003 for a 2-cycle");
        let (cycle, first_edge) = r003[0];
        assert_eq!(cycle.len(), 2, "cycle must contain both modules");

        // The lowest ModuleId in the cycle is Mod0 (id=0); first_edge must be
        // the span of Mod0's import statement.
        assert_eq!(
            *first_edge, span_0_to_1,
            "first_edge must point to the lower module's import"
        );

        // No R004 emitted (no self-loops).
        assert!(
            errors
                .iter()
                .all(|e| !matches!(e, ResolveError::SelfImport { .. })),
            "no R004 expected for a 2-cycle without self-imports"
        );
    }

    // ── detect_cycles: five-module cycle → one R003, cycle.len() == 5 ────────

    #[test]
    fn detect_cycles_five_module_cycle_emits_r003_with_five() {
        // proj.Mod0 → proj.Mod1 → proj.Mod2 → proj.Mod3 → proj.Mod4 → proj.Mod0
        // plus proj.Mod5 (unrelated, no imports)
        let (ws, mut mg) = synthetic_workspace(6);
        add_edge(&mut mg, 0, "proj.Mod1", Span::new(1, 5));
        add_edge(&mut mg, 1, "proj.Mod2", Span::new(6, 10));
        add_edge(&mut mg, 2, "proj.Mod3", Span::new(11, 15));
        add_edge(&mut mg, 3, "proj.Mod4", Span::new(16, 20));
        add_edge(&mut mg, 4, "proj.Mod0", Span::new(21, 25));

        let errors = detect_cycles(&ws, &mg);

        let r003: Vec<_> = errors
            .iter()
            .filter_map(|e| {
                if let ResolveError::CyclicImport { cycle, .. } = e {
                    Some(cycle)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(r003.len(), 1, "expected exactly one R003 for a 5-cycle");
        assert_eq!(r003[0].len(), 5, "cycle must contain exactly 5 modules");

        // Mod5 must NOT appear in the cycle.
        assert!(
            !r003[0].iter().any(|m| m.0 == 5),
            "unrelated Mod5 must not appear in the cycle"
        );
    }

    // ── detect_cycles: stdlib import not in workspace → no cycle ─────────────

    #[test]
    fn detect_cycles_stdlib_import_does_not_contribute_to_cycle() {
        // proj.Mod0 imports "std.list" (not a workspace module) — must be skipped.
        let (ws, mut mg) = synthetic_workspace(2);
        add_edge(&mut mg, 0, "std.list", Span::new(1, 10));
        // proj.Mod1 imports proj.Mod0 (workspace edge, but no back-edge → no cycle).
        add_edge(&mut mg, 1, "proj.Mod0", Span::new(11, 20));

        let errors = detect_cycles(&ws, &mg);

        assert!(
            errors.is_empty(),
            "stdlib import must not trigger a false cycle; got: {errors:?}"
        );
    }

    // ── detect_cycles: mixed — 3-module cycle + unrelated self-import ─────────

    #[test]
    fn detect_cycles_mixed_r003_and_r004() {
        // Modules: proj.Mod0, proj.Mod1, proj.Mod2 (cycle), proj.Mod3 (self-import)
        let (ws, mut mg) = synthetic_workspace(4);
        // 3-module cycle
        add_edge(&mut mg, 0, "proj.Mod1", Span::new(1, 5));
        add_edge(&mut mg, 1, "proj.Mod2", Span::new(6, 10));
        add_edge(&mut mg, 2, "proj.Mod0", Span::new(11, 15));
        // Self-import on Mod3
        let self_span = Span::new(20, 30);
        add_edge(&mut mg, 3, "proj.Mod3", self_span);

        let errors = detect_cycles(&ws, &mg);

        let r003_count = errors
            .iter()
            .filter(|e| matches!(e, ResolveError::CyclicImport { .. }))
            .count();
        let r004_count = errors
            .iter()
            .filter(|e| matches!(e, ResolveError::SelfImport { .. }))
            .count();

        assert_eq!(r003_count, 1, "expected exactly 1 R003 for the 3-cycle");
        assert_eq!(r004_count, 1, "expected exactly 1 R004 for the self-import");

        // The R003 cycle must not include Mod3.
        if let Some(ResolveError::CyclicImport { cycle, .. }) = errors
            .iter()
            .find(|e| matches!(e, ResolveError::CyclicImport { .. }))
        {
            assert_eq!(cycle.len(), 3);
            assert!(
                cycle.iter().all(|m| m.0 != 3),
                "Mod3 (self-import only) must not appear in the R003 cycle"
            );
        }
    }
}
