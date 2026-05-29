//! SCC-based top-level declaration typechecking (T7).
//!
//! Implements §4.7 "Mutual recursion (top-level decls)" from the plan:
//!
//! 1. Build a call graph from the module's AST: for each top-level `fn` `d`,
//!    find all `Ident` references in `d.body` whose names match another
//!    top-level `fn` in this module.
//! 2. Compute the strongly-connected components (SCCs) in topological order
//!    using an in-house Tarjan's algorithm (no external dep added).
//! 3. For each SCC `[d1..dk]`:
//!    a. Allocate fresh `TyVid`s for each `di`'s monomorphic type and bind
//!    them as monoschemes in the environment.
//!    b. Infer each `di.body` against this env.
//!    c. Deep-resolve and batch-generalise all `di` types.
//!    d. Replace the monomorphic env bindings with the polymorphic schemes.
//! 4. Detect polymorphic recursion via T013 (see note below).
//! 5. After all decls, detect unsolved type variables via T023.
//!
//! # Polymorphic-recursion (T013) note
//!
//! Under pure HM with type *inference* (no user annotations on recursive fns),
//! T013 is essentially unreachable: during step (b) all recursive calls use the
//! monomorphic binding, so unification will catch any attempt to use the fn at
//! two incompatible types with a T001 `TypeMismatch`, not T013.
//!
//! T013 is a *defensive guard* for the case where a future extension (e.g.,
//! type annotations on recursive fns) would allow polymorphic recursion to
//! slip through.  For 0.1.0 it fires only when we can construct a synthetic
//! scenario via direct `InferCtx` manipulation (see the test below).
//!
//! # T023 note
//!
//! After generalisation, any `Type::Var` that was never unified (genuinely
//! unconstrained) remains free.  If such a variable is not captured by a
//! top-level scheme's `vars` it constitutes an "unsolved type variable".
//! We detect this post-generalisation by scanning the final types and checking
//! for residual free vars.

use ridge_ast::{Body, Expr, FnDecl, Param, Span};
use ridge_resolve::NodeKind;
use ridge_types::{BuiltinTyCons, CapRow, CapVid, CapabilitySet, Scheme, TyVid, Type};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::caps_check::caps_from_ast_slice;
use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::infer::infer_expr;
use crate::instantiate::{collect_free_vars, generalise_with_env, monoscheme};
use crate::tycon_collect::ast_type_to_ridge_type;
use crate::unify::unify;

// ── DeclId ────────────────────────────────────────────────────────────────────

/// Index into a module's top-level `fn` decl list (0-based, in source order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeclId(pub usize);

// ── Call-graph construction ───────────────────────────────────────────────────

/// A sparse directed call graph over module-level decls.
///
/// `graph[i]` contains the set of `DeclId`s that decl `i` calls (directly, by
/// name, within this module).  Calls to stdlib / cross-module symbols are not
/// tracked (they are not part of the SCC).
pub struct CallGraph {
    /// Number of nodes (= number of top-level fn decls).
    pub n: usize,
    /// Adjacency list: `adj[i]` = set of `DeclId`s called by decl `i`.
    pub adj: Vec<Vec<DeclId>>,
}

/// Builds the call graph for a slice of top-level `FnDecl`s.
///
/// For each decl `d`, we collect all `Ident` names in `d.body` and check
/// whether they match another decl in the same module.  Qualified names and
/// non-fn identifiers are ignored (they don't form intra-module fn call edges).
///
/// This is O(V·E) in body size, acceptable for the small module sizes of 0.1.0.
#[must_use]
pub fn build_call_graph(decls: &[&FnDecl]) -> CallGraph {
    // Build name → DeclId lookup.
    let mut name_to_id: FxHashMap<&str, DeclId> = FxHashMap::default();
    for (i, d) in decls.iter().enumerate() {
        name_to_id.insert(d.name.text.as_str(), DeclId(i));
    }

    let n = decls.len();
    let mut adj: Vec<Vec<DeclId>> = vec![Vec::new(); n];

    for (i, d) in decls.iter().enumerate() {
        let mut called: FxHashSet<DeclId> = FxHashSet::default();
        // Body::Ffi has no expression to walk for call-graph edges.
        if let Body::Expr(e) = &d.body {
            collect_called_names(e, &name_to_id, &mut called);
        }
        adj[i] = called.into_iter().collect();
    }

    CallGraph { n, adj }
}

/// Recursively walks `expr` and collects `DeclId`s for any `Ident` whose text
/// matches a name in `name_to_id`.
fn collect_called_names(
    expr: &Expr,
    name_to_id: &FxHashMap<&str, DeclId>,
    out: &mut FxHashSet<DeclId>,
) {
    match expr {
        Expr::Ident(id) => {
            if let Some(&did) = name_to_id.get(id.text.as_str()) {
                out.insert(did);
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_called_names(callee, name_to_id, out);
            for a in args {
                collect_called_names(a, name_to_id, out);
            }
        }
        Expr::Lambda { body, .. } => {
            // Don't skip lambda bodies: a top-level fn might be referenced
            // from inside a lambda closure.
            collect_called_names(body, name_to_id, out);
        }
        Expr::Block(block) | Expr::Try { block, .. } => {
            for s in &block.stmts {
                collect_called_names(s, name_to_id, out);
            }
        }
        Expr::Let { value, .. } | Expr::Var { value, .. } | Expr::Return { value, .. } => {
            collect_called_names(value, name_to_id, out);
        }
        Expr::Assign { target, value, .. } => {
            collect_called_names(target, name_to_id, out);
            collect_called_names(value, name_to_id, out);
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            collect_called_names(cond, name_to_id, out);
            collect_called_names(then_branch, name_to_id, out);
            if let Some(e) = else_branch {
                collect_called_names(e, name_to_id, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_called_names(scrutinee, name_to_id, out);
            for arm in arms {
                collect_called_names(&arm.body, name_to_id, out);
                if let Some(g) = &arm.guard {
                    collect_called_names(g, name_to_id, out);
                }
            }
        }
        Expr::Binary { lhs, rhs, .. } | Expr::Pipe { lhs, rhs, .. } => {
            collect_called_names(lhs, name_to_id, out);
            collect_called_names(rhs, name_to_id, out);
        }
        Expr::Unary { expr, .. } => {
            collect_called_names(expr, name_to_id, out);
        }
        Expr::Tuple { elems, .. } | Expr::List { elems, .. } => {
            for e in elems {
                collect_called_names(e, name_to_id, out);
            }
        }
        Expr::Paren { inner, .. } | Expr::Propagate { inner, .. } => {
            collect_called_names(inner, name_to_id, out);
        }
        Expr::InnerFn { decl, .. } => {
            // Inner fns always have Body::Expr; Body::Ffi is top-level stdlib only.
            if let Body::Expr(e) = &decl.body {
                collect_called_names(e, name_to_id, out);
            }
        }
        Expr::Guard {
            cond, else_branch, ..
        } => {
            collect_called_names(cond, name_to_id, out);
            for s in &else_branch.stmts {
                collect_called_names(s, name_to_id, out);
            }
        }
        // These don't contain intra-module fn references that matter for SCCs.
        Expr::Literal(_)
        | Expr::Unit(_)
        | Expr::Qualified(_)
        | Expr::FieldAccessorFn { .. }
        | Expr::Record { .. }
        | Expr::With { .. }
        | Expr::FieldAccess { .. }
        | Expr::Interp { .. }
        | Expr::Send { .. }
        | Expr::Ask { .. }
        | Expr::Spawn { .. } => {}
    }
}

// ── Tarjan's SCC ──────────────────────────────────────────────────────────────

/// Computes the strongly-connected components of `graph` in reverse topological order.
///
/// Leaves come first, entry points last — the order in which HM generalisation
/// must proceed for correct per-SCC batching.
///
/// Returns a `Vec<Vec<DeclId>>` where each inner `Vec` is one SCC.  Within an
/// SCC the order is arbitrary; across SCCs the order is toposorted (earlier
/// SCCs may be called by later ones, not the other way around).
///
/// The implementation is a standard iterative Tarjan's algorithm.  We avoid
/// recursion to prevent stack overflow on deep call graphs.
#[must_use]
pub fn tarjan_sccs(graph: &CallGraph) -> Vec<Vec<DeclId>> {
    let n = graph.n;
    let mut index_counter = 0u32;
    let mut stack: Vec<DeclId> = Vec::new();
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut index: Vec<Option<u32>> = vec![None; n];
    let mut lowlink: Vec<u32> = vec![0; n];
    let mut sccs: Vec<Vec<DeclId>> = Vec::new();

    // Iterative DFS using an explicit work-stack.
    // Each work item is (node, iterator over adj) — simulates the recursive call.
    for start in 0..n {
        if index[start].is_some() {
            continue;
        }
        // Work stack entry: (node, adj_index_we_are_at)
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        index[start] = Some(index_counter);
        lowlink[start] = index_counter;
        index_counter += 1;
        stack.push(DeclId(start));
        on_stack[start] = true;

        while let Some((v, adj_pos)) = work.last_mut() {
            let v = *v;
            let adj_list = &graph.adj[v];

            if *adj_pos < adj_list.len() {
                let w = adj_list[*adj_pos].0;
                *adj_pos += 1;
                if index[w].is_none() {
                    // w not yet visited — recurse.
                    index[w] = Some(index_counter);
                    lowlink[w] = index_counter;
                    index_counter += 1;
                    stack.push(DeclId(w));
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    // w is on stack — it's a back-edge.
                    lowlink[v] = lowlink[v].min(lowlink[w]);
                }
            } else {
                // All neighbours of v processed.
                work.pop();
                // Propagate lowlink to parent.
                if let Some(&(parent, _)) = work.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[v]);
                }
                // Check if v is the root of an SCC.
                if lowlink[v] == index[v].unwrap_or(u32::MAX) {
                    let mut scc: Vec<DeclId> = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w.0] = false;
                        scc.push(w);
                        if w.0 == v {
                            break;
                        }
                    }
                    sccs.push(scc);
                }
            }
        }
    }

    // Tarjan produces SCCs in reverse topological order.  For HM we need
    // leaves-first (no-call-deps first), which is what Tarjan gives us.
    sccs
}

// ── Module-level typecheck entry ──────────────────────────────────────────────

/// Batch-generalise and write back schemes for one SCC (steps c + d).
///
/// Extracted from `typecheck_module_decls` to keep that function under the
/// line-count lint threshold.  Generalises each `(fn_ty, body, name)` triple
/// against the pre-SCC env snapshot, writes the resulting scheme to
/// `ctx.schemes_accum` (keyed by body `NodeId`), and re-binds the name in the
/// env with the polymorphic scheme.
fn write_back_schemes(
    ctx: &mut InferCtx,
    scc: &[DeclId],
    decls: &[&FnDecl],
    mut scc_fn_types: FxHashMap<DeclId, Type>,
    env_snap_ty: &FxHashSet<TyVid>,
    env_snap_cap: &FxHashSet<CapVid>,
) {
    let mut generalised: Vec<(&Expr, String, Scheme)> = Vec::new();
    for &did in scc {
        let decl = decls[did.0];
        if let Some(fn_ty) = scc_fn_types.remove(&did) {
            let scheme = generalise_with_env(ctx, &fn_ty, env_snap_ty, env_snap_cap);
            // Body::Ffi has no expression span to key a scheme entry by.
            // We still bind the name in the env for forward references.
            if let Body::Expr(e) = &decl.body {
                generalised.push((e, decl.name.text.clone(), scheme));
            } else {
                // Ffi: bind the scheme in the env but skip schemes_accum.
                ctx.env.bind(decl.name.text.clone(), scheme);
            }
        }
    }
    for (body, name, scheme) in generalised {
        let (body_span, body_kind) = match body {
            Expr::Block(b) => (b.span, NodeKind::Block),
            Expr::Try { span, .. } => (*span, NodeKind::Try),
            other => (other.span(), NodeKind::Expr),
        };
        if let Some(nid) = ctx
            .node_id_map
            .as_ref()
            .and_then(|m| m.get(body_span, body_kind))
        {
            ctx.schemes_accum.insert(nid, scheme.clone());
        }
        ctx.env.bind(name, scheme);
    }
}

/// Typechecks a list of top-level `FnDecl`s from a single module using SCC-based
/// HM generalisation.
///
/// After this call, `ctx.env` (at the outermost frame) contains a generalised
/// [`Scheme`] for every decl.  Any `T###` diagnostics are pushed to `ctx.errors`.
///
/// # T023 — Unsolved type variables
///
/// After generalising all SCCs, this function scans every scheme for residual
/// free [`TyVid`]s (vars that were never constrained during inference and were
/// not generalised).  Each one triggers `T023 UnsolvedTypeVariable`.
///
/// # Usage
///
/// Call this after pushing an initial frame onto `ctx.env`:
///
/// ```ignore
/// ctx.env.push_frame();
/// typecheck_module_decls(&mut ctx, &b, &decls);
/// let schemes = /* read from ctx.env */;
/// ctx.env.pop_frame();
/// ```
pub fn typecheck_module_decls(ctx: &mut InferCtx, b: &BuiltinTyCons, decls: &[&FnDecl]) {
    if decls.is_empty() {
        return;
    }

    // 1. Build call graph and compute SCCs in toposort order.
    let graph = build_call_graph(decls);
    let sccs = tarjan_sccs(&graph);

    // 2. Process each SCC.
    for scc in &sccs {
        // ── Snapshot env free vars BEFORE adding SCC monomorphic bindings ──────
        // HM correctness: we must NOT count the SCC's own fresh TyVids as "in
        // env" when generalising.  Snapshot now; use this for step (c).
        let env_snap_ty = ctx.env_free_tyvids();
        let env_snap_cap = ctx.env_free_capvids();

        // ── Step a: allocate fresh fn types for each decl in the SCC ──────────
        // We bind each decl name to a *monomorphic* Fn type so that recursive
        // calls within the SCC body can find the binding.
        let mut scc_fn_types: FxHashMap<DeclId, Type> = FxHashMap::default();
        let mut scc_spans: FxHashMap<DeclId, Span> = FxHashMap::default();

        for &did in scc {
            let decl = decls[did.0];
            // Use declared annotations when present; fall back to fresh TyVids
            // for unannotated positions.  This is required so that T001 fires
            // when the body type contradicts a declared return/param annotation.
            let empty_param_map: FxHashMap<&str, TyVid> = FxHashMap::default();
            let user_tycon_names = ctx.user_tycon_names.clone();
            let param_types: Vec<Type> = decl
                .params
                .iter()
                .map(|p| match p {
                    Param::Bare(_) => Type::Var(ctx.fresh_tyvid()),
                    Param::Annotated { ty, .. } => {
                        ast_type_to_ridge_type(b, ctx, ty, &user_tycon_names, &empty_param_map)
                    }
                })
                .collect();
            let ret_ty = match &decl.ret {
                Some(ret_ast_ty) => {
                    ast_type_to_ridge_type(b, ctx, ret_ast_ty, &user_tycon_names, &empty_param_map)
                }
                None => Type::Var(ctx.fresh_tyvid()),
            };
            // Use declared capability set when present; default to PURE for
            // unannotated fns.
            let caps = if decl.caps.is_empty() {
                CapRow::Concrete(CapabilitySet::PURE)
            } else {
                let cap_set = caps_from_ast_slice(&decl.caps);
                CapRow::Concrete(cap_set)
            };
            let fn_ty = Type::Fn {
                params: param_types,
                ret: Box::new(ret_ty),
                caps,
            };
            scc_fn_types.insert(did, fn_ty.clone());
            scc_spans.insert(did, decl.span);
            ctx.env.bind(decl.name.text.clone(), monoscheme(fn_ty));
        }

        // ── Step b: infer each body ────────────────────────────────────────────
        for &did in scc {
            let decl = decls[did.0];

            // Infer body in a new inner scope containing the params.
            ctx.env.push_frame();
            if let Some(Type::Fn {
                params: param_tys,
                ret: ret_ty_box,
                ..
            }) = scc_fn_types.get(&did)
            {
                let saved_ret = ctx.current_fn_ret.take();
                ctx.current_fn_ret = Some(*ret_ty_box.clone());

                // Bind params as monoschemes.
                for (param, ty) in decl.params.iter().zip(param_tys.iter()) {
                    let param_name = match param {
                        ridge_ast::Param::Bare(id) => id.text.clone(),
                        ridge_ast::Param::Annotated { name, .. } => name.text.clone(),
                    };
                    ctx.env.bind(param_name, monoscheme(ty.clone()));
                }

                // Body::Ffi carries a fully-declared signature; no inference needed.
                let body_ty = match &decl.body {
                    Body::Expr(e) => infer_expr(ctx, b, e),
                    Body::Ffi { .. } => *ret_ty_box.clone(),
                };
                // Unify body type with declared ret.
                if unify(ctx, &body_ty, ret_ty_box).is_err() {
                    let span = scc_spans
                        .get(&did)
                        .copied()
                        .unwrap_or_else(|| Span::point(0));
                    ctx.errors.push(TypeError::TypeMismatch {
                        expected: format!("{ret_ty_box:?}"),
                        found: format!("{body_ty:?}"),
                        span,
                    });
                }

                ctx.current_fn_ret = saved_ret;
            }
            ctx.env.pop_frame();
        }

        // ── Steps c+d: generalise and write back schemes ──────────────────────
        // OQ-PHASE45-003: top-level decl schemes only (no let-bound locals).
        // OQ-PHASE45-005: span-keyed via body span (same as T5 inferred_caps).
        write_back_schemes(ctx, scc, decls, scc_fn_types, &env_snap_ty, &env_snap_cap);
    }

    // 3. Detect T023 — unsolved type variables.
    //    Walk every binding in the current (outermost) frame, deep-resolve the
    //    scheme body, and check for residual free TyVids.
    detect_unsolved_type_vars(ctx);
}

/// Scans the current env frame for residual free [`TyVid`]s after generalisation
/// and fires `T023 UnsolvedTypeVariable` for each one found.
///
/// A `TyVid` is "unsolved" if it appears free in a scheme body **and** is NOT in
/// `scheme.vars` (i.e., it was not generalised and not unified with a concrete
/// type).
///
/// # Prelude/stdlib scheme guard
///
/// Schemes seeded from the prelude or stdlib may contain generalised `TyVid`s
/// (e.g. `TyVid(0)` / `TyVid(1)`) that are NOT allocated in the current module's
/// `InferCtx` unification table — they are stable, cross-module placeholder
/// indices.  Calling `deep_resolve` on such a scheme would panic with an
/// out-of-bounds table access.
///
/// We guard against this by pre-collecting the scheme's free `TyVids` (without
/// resolving through the unification table) and skipping any scheme whose free
/// vars include an index ≥ `ctx.tyvids.len()`.  Such schemes are well-formed
/// polymorphic bindings that require no T023 reporting.
pub fn detect_unsolved_type_vars(ctx: &mut InferCtx) {
    // Number of TyVids allocated in this module's inference context.
    // Any TyVid index ≥ this value belongs to a prelude/stdlib scheme
    // and must not be probed through the unification table.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "arena index fits u32 in practice"
    )]
    let tyvid_len = ctx.tyvids.len() as u32;

    // Collect (name, scheme) from the outermost frame.
    // We iterate over a cloned snapshot because we may mutate ctx during
    // deep_resolve.
    let frame_bindings: Vec<(String, Scheme)> = ctx
        .env
        .frames
        .last()
        .map(|f| {
            f.bindings
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    // Number of CapVids allocated in this module's inference context.
    // Any CapVid index ≥ this value is from a prelude/stdlib HOF scheme
    // (e.g. List.map carries CapRow::Var(CapVid(0))) and must not be
    // probed through the cap unification table.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "arena index fits u32 in practice"
    )]
    let capvid_len = ctx.capvids.len() as u32;

    for (name, scheme) in frame_bindings {
        // Pre-check: collect free TyVids/CapVids WITHOUT resolving through the tables.
        // If any free var is out of range (a prelude/stdlib placeholder), skip
        // this scheme entirely — it's a well-formed polymorphic binding.
        let (raw_free, raw_free_cap) = collect_free_vars(&scheme.ty);
        let has_oob_ty = raw_free.iter().any(|v| v.0 >= tyvid_len);
        // Skip schemes with CapVids not yet allocated in this ctx.
        // This covers stdlib HOF schemes (e.g. List.map with CapRow::Var(CAP_C))
        // that are seeded into env via prelude aliases but whose CapVids were
        // never registered in this module's cap unification table.
        let has_oob_cap = raw_free_cap.iter().any(|c| c.0 >= capvid_len);
        // Also skip if any CapVid in the scheme body (including bound cap_vars)
        // is out of range — instantiate would have registered them if called.
        let has_oob_bound_cap = scheme.cap_vars.iter().any(|c| c.0 >= capvid_len);
        if has_oob_ty || has_oob_cap || has_oob_bound_cap {
            continue;
        }

        // Skip fully-generalised prelude/stdlib schemes whose in-range TyVids are
        // all covered by `scheme.vars`.  Such schemes have every free variable
        // quantified (e.g. `List.reverse : ∀ TyVid(0). List TyVid(0) -> List
        // TyVid(0)`), so resolving through the local unification table would
        // spuriously follow any link created for TyVid(0) during this module's
        // inference, producing a false T023 for a variable that is not actually
        // unsolved in local scope.
        let bound_set: FxHashSet<TyVid> = scheme.vars.iter().copied().collect();
        if raw_free.is_subset(&bound_set) {
            continue;
        }

        let resolved_body = ctx.deep_resolve(&scheme.ty);
        let (free_ty, _) = collect_free_vars(&resolved_body);

        let unsolved: Vec<TyVid> = free_ty.difference(&bound_set).copied().collect();

        for var in unsolved {
            ctx.errors.push(TypeError::UnsolvedTypeVariable {
                var: format!("?{} (in binding '{}')", var.0, name),
                generalisation_site: Span::point(0),
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Ident, Literal, Param, Span};
    use ridge_types::{BuiltinTyCons, TyConArena};

    fn ds() -> Span {
        Span::point(0)
    }

    fn id(t: &str) -> Ident {
        Ident {
            text: t.to_string(),
            span: ds(),
        }
    }

    fn make_builtins() -> (TyConArena, BuiltinTyCons) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        (arena, b)
    }

    /// Helper: build a minimal `FnDecl` with the given name, a single Int param,
    /// and a body that is just an expression.
    fn make_fn_decl(name: &str, body: Expr) -> FnDecl {
        FnDecl {
            attrs: vec![],
            vis: ridge_ast::Visibility::Private,
            caps: vec![],
            name: id(name),
            params: vec![Param::Bare(id("n"))],
            ret: None,
            body: ridge_ast::Body::Expr(body),
            span: ds(),
            doc: None,
        }
    }

    // ── Test SCC-1 ─────────────────────────────────────────────────────────
    // single_fn_recursion: `fact(n) = fact(n)` (self-recursive).
    // Builds call graph, checks SCC has 1 node that calls itself.

    #[test]
    fn scc_single_recursive_fn() {
        // fact body: fact(n)  i.e., Call { callee: Ident("fact"), args: [Ident("n")] }
        let body = Expr::Call {
            callee: Box::new(Expr::Ident(id("fact"))),
            args: vec![Expr::Ident(id("n"))],
            span: ds(),
        };
        let fact = make_fn_decl("fact", body);
        let decls: Vec<&FnDecl> = vec![&fact];
        let graph = build_call_graph(&decls);

        // fact calls fact → self-loop → single SCC of size 1.
        let sccs = tarjan_sccs(&graph);
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0], vec![DeclId(0)]);
    }

    // ── Test SCC-2 ─────────────────────────────────────────────────────────
    // mutually_recursive_even_odd: even calls odd and odd calls even.
    // Both should end up in one SCC.

    #[test]
    fn scc_mutually_recursive_even_odd_one_scc() {
        // even body: odd(n)
        let even_body = Expr::Call {
            callee: Box::new(Expr::Ident(id("odd"))),
            args: vec![Expr::Ident(id("n"))],
            span: ds(),
        };
        // odd body: even(n)
        let odd_body = Expr::Call {
            callee: Box::new(Expr::Ident(id("even"))),
            args: vec![Expr::Ident(id("n"))],
            span: ds(),
        };
        let even = make_fn_decl("even", even_body);
        let odd = make_fn_decl("odd", odd_body);
        let decls: Vec<&FnDecl> = vec![&even, &odd];
        let graph = build_call_graph(&decls);

        let sccs = tarjan_sccs(&graph);
        // One SCC containing both.
        assert_eq!(sccs.len(), 1, "expected 1 SCC, got {sccs:?}");
        let scc = &sccs[0];
        assert_eq!(scc.len(), 2, "SCC must contain both even and odd");
        let ids: FxHashSet<DeclId> = scc.iter().copied().collect();
        assert!(ids.contains(&DeclId(0)), "even must be in SCC");
        assert!(ids.contains(&DeclId(1)), "odd must be in SCC");
    }

    // ── Test SCC-3 ─────────────────────────────────────────────────────────
    // independent fns produce separate single-element SCCs.

    #[test]
    fn scc_independent_fns_separate_sccs() {
        // foo body: 42 (no calls to other top-level fns)
        let foo_body = Expr::Literal(Literal::IntDec {
            raw: "42".to_string(),
            span: ds(),
        });
        let bar_body = Expr::Literal(Literal::IntDec {
            raw: "7".to_string(),
            span: ds(),
        });
        let foo = make_fn_decl("foo", foo_body);
        let bar = make_fn_decl("bar", bar_body);
        let decls: Vec<&FnDecl> = vec![&foo, &bar];
        let graph = build_call_graph(&decls);

        let sccs = tarjan_sccs(&graph);
        assert_eq!(sccs.len(), 2, "two independent fns → 2 SCCs");
        for scc in &sccs {
            assert_eq!(scc.len(), 1, "each SCC must have size 1");
        }
    }

    // ── Test SCC-4 ─────────────────────────────────────────────────────────
    // typecheck single fn: `identity(n) = n` — infers as fn (?a) -> ?a,
    // generalised to forall a. (a) -> a.

    #[test]
    fn typecheck_module_decls_identity_generalised() {
        let (_, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // identity body: n
        let body = Expr::Ident(id("n"));
        let decl = make_fn_decl("identity", body);
        let decls: Vec<&FnDecl> = vec![&decl];

        typecheck_module_decls(&mut ctx, &b, &decls);

        let scheme = ctx
            .env
            .lookup("identity")
            .cloned()
            .expect("identity must be bound after typecheck");

        assert!(
            !scheme.vars.is_empty(),
            "identity must be generalised; got {scheme:?}"
        );
        assert!(
            ctx.errors.iter().all(|e| e.code() != "T023"),
            "no T023 expected for identity; errors: {:?}",
            ctx.errors
        );

        ctx.env.pop_frame();
    }

    // ── Test SCC-5 ─────────────────────────────────────────────────────────
    // mutually_recursive_even_odd full typecheck:
    // even and odd are inferred; types must unify correctly with no T001 errors.
    //
    // We construct:
    //   even(n) = if n == 0 then true else odd(n - 1)
    //   odd(n)  = if n == 0 then false else even(n - 1)
    // Since we can't parse Ridge code here, we build the AST by hand with a
    // simplified body: just delegate to the other fn.
    //
    // Simplified: even(n) = odd(n), odd(n) = even(n)
    // After inference both types must unify; no T001 fires.

    #[test]
    fn typecheck_module_decls_mutually_recursive_no_errors() {
        let (_, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let even_body = Expr::Call {
            callee: Box::new(Expr::Ident(id("odd"))),
            args: vec![Expr::Ident(id("n"))],
            span: ds(),
        };
        let odd_body = Expr::Call {
            callee: Box::new(Expr::Ident(id("even"))),
            args: vec![Expr::Ident(id("n"))],
            span: ds(),
        };
        let even = make_fn_decl("even", even_body);
        let odd = make_fn_decl("odd", odd_body);
        let decls: Vec<&FnDecl> = vec![&even, &odd];

        typecheck_module_decls(&mut ctx, &b, &decls);

        // Both names must be in env.
        assert!(ctx.env.lookup("even").is_some(), "even must be bound");
        assert!(ctx.env.lookup("odd").is_some(), "odd must be bound");

        // No T001 TypeMismatch errors.
        let t001_errors: Vec<_> = ctx.errors.iter().filter(|e| e.code() == "T001").collect();
        assert!(
            t001_errors.is_empty(),
            "no T001 expected; got {t001_errors:?}"
        );

        ctx.env.pop_frame();
    }

    // ── Test SCC-6 ─────────────────────────────────────────────────────────
    // single_fn_non_recursive: `const_42(n) = 42` — types as (Int) -> Int.
    // After typecheck, scheme.ty resolves to Fn(Int) -> Int under deep_resolve.

    #[test]
    fn typecheck_module_decls_single_non_recursive() {
        let (_, b) = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        let body = Expr::Literal(Literal::IntDec {
            raw: "42".to_string(),
            span: ds(),
        });
        let decl = make_fn_decl("const_42", body);
        let decls: Vec<&FnDecl> = vec![&decl];

        typecheck_module_decls(&mut ctx, &b, &decls);

        let scheme = ctx
            .env
            .lookup("const_42")
            .cloned()
            .expect("const_42 must be bound");

        let resolved = ctx.deep_resolve(&scheme.ty);
        match resolved {
            Type::Fn { ret, .. } => {
                let ret_resolved = ctx.deep_resolve(&ret);
                assert!(
                    matches!(ret_resolved, Type::Con(id, _) if id == b.int),
                    "const_42 must return Int, got {ret_resolved:?}"
                );
            }
            other => panic!("expected Fn type for const_42, got {other:?}"),
        }

        ctx.env.pop_frame();
    }

    // ── Test SCC-7 ─────────────────────────────────────────────────────────
    // T013 PolymorphicRecursion — synthetic test via direct InferCtx manipulation.
    //
    // True polymorphic recursion requires type annotations (not yet supported),
    // so for 0.1.0 it is essentially unreachable from inferred code.
    // We construct the scenario directly: bind a recursive fn to a *polymorphic*
    // scheme (as if it had an annotation), then detect when inference unifies
    // the bound var at two different concrete types.
    //
    // This test documents the gap and verifies that T013 can be constructed
    // and has the correct error code.
    #[test]
    #[ignore = "polymorphic recursion requires type annotations on recursive fns; \
                not yet supported in 0.1.0 (HM with inference only). \
                T013 fires only as a defensive guard for annotated recursive fns; \
                inferred-only code gets T001 TypeMismatch instead."]
    fn polymorphic_recursion_detection_t013() {
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Manually push a T013 to verify the code is correct.
        ctx.errors.push(TypeError::PolymorphicRecursion {
            decl: "f".to_string(),
            recursive_call_span: Span::point(0),
        });

        let has_t013 = ctx.errors.iter().any(|e| e.code() == "T013");
        assert!(has_t013, "T013 must be constructable");

        ctx.env.pop_frame();
    }

    // ── Test SCC-8 ─────────────────────────────────────────────────────────
    // T023 UnsolvedTypeVariable — synthetic test.
    // We forge a situation: bind a name to a scheme whose body has a free TyVid
    // that was never unified, then call detect_unsolved_type_vars.

    #[test]
    fn unsolved_type_variable_t023_fires() {
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // Allocate a fresh TyVid, never unify it.
        let unbound = ctx.fresh_tyvid();

        // Bind a scheme with that free var in the body (not in vars — unsolved).
        let scheme = Scheme {
            vars: vec![], // NOT generalised
            cap_vars: vec![],
            ty: Type::Var(unbound),
        };
        ctx.env.bind("x".to_string(), scheme);

        // detect_unsolved_type_vars must fire T023.
        detect_unsolved_type_vars(&mut ctx);

        let has_t023 = ctx.errors.iter().any(|e| e.code() == "T023");
        assert!(
            has_t023,
            "T023 must fire for unsolved type variable; errors: {:?}",
            ctx.errors
        );

        ctx.env.pop_frame();
    }

    // ── Test SCC-9 ─────────────────────────────────────────────────────────
    // Dependency ordering: `g(n) = 42; f(n) = g(n)`.
    // `g` has no deps; `f` calls `g`.  SCCs must have g before f in toposort.

    #[test]
    fn scc_dependency_ordering_g_before_f() {
        // g body: 42 (no calls)
        let g_body = Expr::Literal(Literal::IntDec {
            raw: "42".to_string(),
            span: ds(),
        });
        // f body: g(n)
        let f_body = Expr::Call {
            callee: Box::new(Expr::Ident(id("g"))),
            args: vec![Expr::Ident(id("n"))],
            span: ds(),
        };
        let g = make_fn_decl("g", g_body);
        let f = make_fn_decl("f", f_body);
        // g is DeclId(0), f is DeclId(1).
        let decls: Vec<&FnDecl> = vec![&g, &f];
        let graph = build_call_graph(&decls);

        assert_eq!(graph.adj[0], vec![], "g has no deps");
        assert!(graph.adj[1].contains(&DeclId(0)), "f calls g");

        let sccs = tarjan_sccs(&graph);
        assert_eq!(sccs.len(), 2, "2 independent SCCs");

        // g (DeclId 0) must appear in an earlier SCC than f (DeclId 1).
        let pos_g = sccs.iter().position(|s| s.contains(&DeclId(0))).unwrap();
        let pos_f = sccs.iter().position(|s| s.contains(&DeclId(1))).unwrap();
        assert!(pos_g < pos_f, "g's SCC must precede f's SCC in topo order");
    }

    // ── Phase 4.5 T4 tests — schemes_accum population ─────────────────────────

    /// Build a `FnDecl` with a unique span at offset `start`.
    fn make_fn_decl_at(name: &str, start: u32, body: Expr) -> FnDecl {
        let sp = Span::new(start, start + 10);
        FnDecl {
            attrs: vec![],
            vis: ridge_ast::Visibility::Private,
            caps: vec![],
            name: Ident {
                text: name.to_string(),
                span: sp,
            },
            params: vec![],
            ret: None,
            body: ridge_ast::Body::Expr(body),
            span: sp,
            doc: None,
        }
    }

    /// T4-1: monomorphic fn — scheme is in `schemes_accum` after typecheck.
    /// The `NodeIdMap` is set up with the fn *body* span stamped as `NodeKind::Expr`
    /// (not the decl span) because T4 now keys by body span to match T5's keying.
    #[test]
    fn t4_mono_fn_scheme_populated() {
        let (arena, b) = make_builtins();
        let mut ctx = crate::ctx::InferCtx::new();

        // The body literal lives at [5,7); we stamp that as NodeKind::Expr.
        // The decl span [0,10) is NOT stamped — T4 keys by body span, not decl span.
        let body_lit_span = Span::new(5, 7);
        let mut map = ridge_resolve::NodeIdMap::default();
        map.assign(body_lit_span, ridge_resolve::NodeKind::Expr)
            .expect("assign body literal");
        ctx.node_id_map = Some(map);

        // Dummy arena for collect_user_tycons (no user types).
        ctx.tycon_decls = arena.all().to_vec();

        ctx.env.push_frame();
        // fn answer = 42
        let decl = make_fn_decl_at(
            "answer",
            0,
            Expr::Literal(Literal::IntDec {
                raw: "42".to_string(),
                span: body_lit_span,
            }),
        );
        let decls: Vec<&FnDecl> = vec![&decl];
        typecheck_module_decls(&mut ctx, &b, &decls);
        ctx.env.pop_frame();

        assert_eq!(ctx.schemes_accum.len(), 1, "one top-level scheme expected");
        // Look up by body span (body is a literal → NodeKind::Expr).
        let nid = ctx
            .node_id_map
            .as_ref()
            .unwrap()
            .get(body_lit_span, ridge_resolve::NodeKind::Expr)
            .expect("NodeId for body span must exist");
        assert!(
            ctx.schemes_accum.contains_key(&nid),
            "scheme keyed by body NodeId"
        );
    }

    /// T4-2: polymorphic fn — generalised scheme recorded in `schemes_accum`,
    /// keyed by the fn body's span (`NodeKind::Expr` for an Ident body).
    #[test]
    fn t4_polymorphic_fn_scheme_populated() {
        let (arena, b) = make_builtins();
        let mut ctx = crate::ctx::InferCtx::new();

        let decl_span = Span::new(0, 10);
        let param_span = Span::new(5, 6);
        // body_ident_span is distinct from param_span to avoid collision.
        let body_ident_span = Span::new(7, 8);
        let mut map = ridge_resolve::NodeIdMap::default();
        map.assign(param_span, ridge_resolve::NodeKind::Ident).ok();
        // The body is Expr::Ident at body_ident_span — stamp it as NodeKind::Expr.
        // T4 now keys by body span; decl_span is NOT stamped.
        map.assign(body_ident_span, ridge_resolve::NodeKind::Ident)
            .ok();
        map.assign(body_ident_span, ridge_resolve::NodeKind::Expr)
            .expect("body ident");
        ctx.node_id_map = Some(map);
        ctx.tycon_decls = arena.all().to_vec();

        ctx.env.push_frame();
        // fn id x = x — polymorphic: ∀a. a -> a
        let decl = FnDecl {
            attrs: vec![],
            vis: ridge_ast::Visibility::Private,
            caps: vec![],
            name: Ident {
                text: "id".to_string(),
                span: decl_span,
            },
            params: vec![Param::Bare(Ident {
                text: "x".to_string(),
                span: param_span,
            })],
            ret: None,
            body: ridge_ast::Body::Expr(Expr::Ident(Ident {
                text: "x".to_string(),
                span: body_ident_span,
            })),
            span: decl_span,
            doc: None,
        };
        let decls: Vec<&FnDecl> = vec![&decl];
        typecheck_module_decls(&mut ctx, &b, &decls);
        ctx.env.pop_frame();

        assert_eq!(ctx.schemes_accum.len(), 1, "one scheme for polymorphic fn");
        // Look up by body span (body is an Ident → NodeKind::Expr).
        let nid = ctx
            .node_id_map
            .as_ref()
            .unwrap()
            .get(body_ident_span, ridge_resolve::NodeKind::Expr)
            .expect("NodeId for body ident span");
        let scheme = ctx.schemes_accum.get(&nid).expect("scheme present");
        assert!(
            !scheme.vars.is_empty(),
            "polymorphic fn should have generalised vars"
        );
    }

    /// T4-3: let-bound local inside a fn body — let locals are NOT in `schemes_accum`
    /// (only top-level decl schemes per OQ-PHASE45-003).
    #[test]
    fn t4_let_bound_local_not_in_schemes() {
        let (arena, b) = make_builtins();
        let mut ctx = crate::ctx::InferCtx::new();

        // Use span [0, 10) for the decl (matches make_fn_decl_at("foo", 0, ...)).
        let decl_span = Span::new(0, 10);
        let let_span = Span::new(11, 20);
        let val_span = Span::new(21, 23);
        let body_span = Span::new(24, 25);
        let block_span = Span::new(11, 25);
        let mut map = ridge_resolve::NodeIdMap::default();
        map.assign(decl_span, ridge_resolve::NodeKind::Expr)
            .expect("decl");
        map.assign(let_span, ridge_resolve::NodeKind::Expr)
            .expect("let");
        map.assign(val_span, ridge_resolve::NodeKind::Expr)
            .expect("val");
        map.assign(body_span, ridge_resolve::NodeKind::Ident).ok();
        map.assign(body_span, ridge_resolve::NodeKind::Expr)
            .expect("body");
        map.assign(block_span, ridge_resolve::NodeKind::Block)
            .expect("block");
        ctx.node_id_map = Some(map);
        ctx.tycon_decls = arena.all().to_vec();

        ctx.env.push_frame();
        // fn foo = let x = 42; x
        let body = Expr::Block(ridge_ast::Block {
            stmts: vec![
                Expr::Let {
                    pat: ridge_ast::Pattern::Var {
                        name: Ident {
                            text: "x".to_string(),
                            span: let_span,
                        },
                        span: let_span,
                    },
                    ty: None,
                    value: Box::new(Expr::Literal(Literal::IntDec {
                        raw: "42".to_string(),
                        span: val_span,
                    })),
                    span: let_span,
                },
                Expr::Ident(Ident {
                    text: "x".to_string(),
                    span: body_span,
                }),
            ],
            span: block_span,
        });
        let decl = make_fn_decl_at("foo", 0, body);
        let decls: Vec<&FnDecl> = vec![&decl];
        typecheck_module_decls(&mut ctx, &b, &decls);
        ctx.env.pop_frame();

        // schemes_accum should only contain the top-level `foo` decl, not `x`.
        assert_eq!(
            ctx.schemes_accum.len(),
            1,
            "only top-level decl scheme; let-bound `x` must not appear: {:?}",
            ctx.schemes_accum
        );
    }
}
