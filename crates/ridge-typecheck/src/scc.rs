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
//!    a. Build each `di`'s type from its signature and bind the name — as the
//!    declared scheme if the signature is complete, otherwise as a monoscheme
//!    (see note below).
//!    b. Infer each `di.body` against this env.
//!    c. Deep-resolve and batch-generalise all `di` types.
//!    d. Replace the step-(a) env bindings with the generalised schemes.
//! 4. After all decls, detect unsolved type variables via T023.
//!
//! # Recursion at a second type (T013) note
//!
//! Plain HM binds a recursive name monomorphically, so an occurrence at a
//! second type is caught by unification. That is the right answer while the
//! type is being inferred — inferring a polymorphic-recursive one is
//! undecidable — but not once the author has written the type down. A
//! declaration whose parameters and return type are all annotated is bound in
//! step (a) to that declared scheme, so each occurrence instantiates it, and
//! the body is still held to the signature because it is checked against the
//! rigids minted from the same declaration.
//!
//! A signature with a position left off keeps the monomorphic binding, and the
//! declaration is recorded in [`InferCtx::monomorphic_in_scc`] so the call that
//! fails is reported as T013 — naming the annotations that would let it
//! through — rather than as an argument mismatch that looks like the author's
//! fault.
//!
//! # T023 note
//!
//! After generalisation, any `Type::Var` that was never unified (genuinely
//! unconstrained) remains free.  If such a variable is not captured by a
//! top-level scheme's `vars` it constitutes an "unsolved type variable".
//! We detect this post-generalisation by scanning the final types and checking
//! for residual free vars.

use ridge_ast::{Body, Expr, FnDecl, Item, Param, Span};
use ridge_resolve::NodeKind;
use ridge_types::{
    BuiltinTyCons, CapRow, CapVid, CapabilitySet, Constraint, RigidId, RowVid, Scheme, Subst,
    TyVid, Type,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::caps_check::caps_from_ast_slice;
use crate::class_env::{ClassTable, InstanceEnv};
use crate::ctx::{InferCtx, RigidInfo};
use crate::error::TypeError;
use crate::infer::{infer_expr, infer_pattern};
use crate::instantiate::{collect_free_vars, generalise_with_env, monoscheme};
use crate::solve::solve_constraints;
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
        | Expr::Spawn { .. }
        | Expr::ChildSpec { .. } => {}
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                if let Some(ref val) = f.value {
                    collect_called_names(val, name_to_id, out);
                }
            }
        }
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
///
/// `retained_constraints` — constraints that the solver kept as polymorphic
/// (case b). Each is filtered to only the `TyVid`s that the scheme actually
/// generalises, then attached to `scheme.constraints`. This is the mechanism
/// by which `fn describe (x: a) -> Text where ToText a` keeps its constraint
/// in its generalised scheme.
///
/// Make the type variables a declaration wrote into constants for the duration
/// of its body, and return the substitution that applies them to its signature.
///
/// The variables were already allocated as ordinary unification variables so
/// that one name means one thing across the signature and its `where` clause.
/// Each becomes a rigid paired with the variable it will abstract back to, so
/// the constraint a body forwards and the dictionary parameter lowering
/// declares for it end up spelling the same name.
///
/// Substituting after the signature is built, rather than seeding rigids into
/// the name map, keeps `ast_type_to_ridge_type` unchanged — instance heads and
/// class-method signatures share it, and their variables are not promises of
/// this kind.
/// Collect the abstraction target of every signature variable in a type.
///
/// The counterpart of [`collect_free_vars`] for the one shape it refuses: a
/// rigid is a constant while its body is checked, so instantiation must not
/// freshen it and it is not a "free variable" in that sense. It is still a
/// variable the declaration generalises over, and [`mint_rigids`] pairs each
/// one with the unification variable it abstracts back to — `RigidId(vid.0)` —
/// so `TyVid(id.0)` recovers it.
fn collect_rigid_targets(ty: &Type, out: &mut FxHashSet<TyVid>) {
    match ty {
        Type::Rigid { id, .. } => {
            out.insert(TyVid(id.0));
        }
        Type::Con(_, args) | Type::Tuple(args) => {
            for a in args {
                collect_rigid_targets(a, out);
            }
        }
        Type::Fn { params, ret, .. } => {
            for p in params {
                collect_rigid_targets(p, out);
            }
            collect_rigid_targets(ret, out);
        }
        Type::Record { fields, .. } => {
            for (_, t) in fields {
                collect_rigid_targets(t, out);
            }
        }
        Type::Alias { body, .. } => collect_rigid_targets(body, out),
        // `Type` is `#[non_exhaustive]`; the remaining shapes hold no nested
        // types, so there is nothing to walk.
        _ => {}
    }
}

fn mint_rigids(ctx: &mut InferCtx, decl: &FnDecl, tyvar_map: &FxHashMap<&str, TyVid>) -> Subst {
    let complete = signature_is_complete(decl);
    let mut subst = Subst::empty();
    for (name, vid) in tyvar_map {
        let id = RigidId(vid.0);
        ctx.rigids.insert(
            id,
            RigidInfo {
                decl: decl.name.text.clone(),
                span: decl.span,
                givens: Vec::new(),
                complete,
            },
        );
        subst = subst.compose(Subst::singleton(
            *vid,
            Type::Rigid {
                id,
                name: (*name).into(),
            },
        ));
    }
    subst
}

/// The scheme a complete signature binds its own name to while its body is
/// checked, so a recursive occurrence is instantiated rather than shared.
///
/// Inferring a polymorphic-recursive type is undecidable, which is why plain
/// Hindley-Milner binds a recursive name monomorphically. Checking a recursive
/// call against a type the author wrote down is not, and this is the scheme it
/// is checked against — the declared one, not the one the body turns out to
/// have. That distinction is the whole safety argument: the body is still held
/// to the signature, because the parameters and the return type it is checked
/// against are the rigids [`mint_rigids`] minted from the same declaration.
/// `fn g (x: a) -> a = g 1` instantiates to `Int -> Int` and is then a body of
/// type `Int` where the signature promised `a`, which is a mismatch at the
/// declaration rather than a surprise at the first honest caller.
///
/// Quantifies the signature's type variables only. A capability row is
/// concrete by the time it gets here, and a record row variable stays shared —
/// which is exactly what a monomorphic binding did, so nothing that used to
/// check stops.
fn self_binding_from_signature(
    declared_ty: &Type,
    tyvar_map: &FxHashMap<&str, TyVid>,
    constraints: Vec<Constraint>,
) -> Scheme {
    // Sorted so the fresh variables an occurrence instantiates are allocated in
    // the same order on every run; the map's own order is a hash order.
    let mut vars: Vec<TyVid> = tyvar_map.values().copied().collect();
    vars.sort_unstable_by_key(|v| v.0);
    Scheme {
        vars,
        cap_vars: Vec::new(),
        row_vars: Vec::new(),
        ty: declared_ty.clone(),
        constraints,
    }
}

/// Whether every parameter and the return type carry an annotation.
///
/// The line between a signature that is checked and one that is inferred. A
/// declaration on this side of it has written its whole type down, which is
/// what lets the checker hold the body to it and lets a recursive occurrence
/// instantiate it. Leaving a position off is asking for it to be worked out,
/// and inference has nothing to instantiate.
fn signature_is_complete(decl: &FnDecl) -> bool {
    decl.ret.is_some() && !decl.params.iter().any(|p| matches!(p, Param::Bare(_)))
}

/// What a declaration would have to annotate to be called at a second type.
///
/// Phrased as the edit rather than the rule, because the author is looking at a
/// call the compiler rejected and needs to know which end to change.
fn annotations_missing_from(decl: &FnDecl) -> String {
    let bare = decl
        .params
        .iter()
        .filter(|p| matches!(p, Param::Bare(_)))
        .count();
    let name = &decl.name.text;
    match (bare, decl.ret.is_none()) {
        (0, _) => format!("annotate the return type of `{name}`"),
        (_, false) => format!("annotate every parameter of `{name}`"),
        (_, true) => format!("annotate every parameter and the return type of `{name}`"),
    }
}

/// Turn the signature's constants back into the variables a scheme quantifies.
///
/// A rigid is a checking-time device: it exists so the body cannot choose a
/// type on the author's behalf, and once the body has been checked there is
/// nothing left for it to prevent. Every rigid was minted paired with the
/// variable it abstracts to, so this is a rename rather than a fresh
/// allocation — which is what keeps the constraint the solver forwarded and
/// the dictionary parameter lowering declares pointing at each other.
///
/// Applied to everything that outlives the pass, so no rigid is observable
/// from outside it. The wildcard match arms downstream — in lowering, in the
/// language server, in the snapshot renderer — hold because of this, not by
/// luck. Reached from outside through [`InferCtx::resolve_for_export`].
pub(crate) fn abstract_rigids(ty: &Type) -> Type {
    match ty {
        Type::Rigid { id, .. } => Type::Var(TyVid(id.0)),
        Type::Con(id, args) => Type::Con(*id, args.iter().map(abstract_rigids).collect()),
        Type::Fn { params, ret, caps } => Type::Fn {
            params: params.iter().map(abstract_rigids).collect(),
            ret: Box::new(abstract_rigids(ret)),
            caps: caps.clone(),
        },
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(abstract_rigids).collect()),
        Type::Record { fields, tail } => Type::Record {
            fields: fields
                .iter()
                .map(|(l, t)| (l.clone(), abstract_rigids(t)))
                .collect(),
            tail: tail.clone(),
        },
        Type::Alias { name, body } => Type::Alias {
            name: *name,
            body: Box::new(abstract_rigids(body)),
        },
        // A variable, an error, and any future variant hold no rigid.
        _ => ty.clone(),
    }
}

/// A genuinely auto-promoted `pub fn toText` (§5.6.6) is the exception to the
/// "bind the name" rule: its scheme is still generalised and, for a
/// `Body::Expr`, still written to `ctx.schemes_accum` keyed by the body's
/// `NodeId` — but neither `ctx.env` nor `ctx.name_schemes_accum` gets an entry
/// under the bare name `toText`, so the low-precedence polymorphic
/// class-method scheme seeded before SCC processing stays in effect for every
/// use site. `class_table`/`instance_env` are threaded through only to make
/// that check.
#[allow(
    clippy::too_many_arguments,
    reason = "three pre-SCC env free-var snapshots (ty/cap/row) plus retained constraints and the class/instance registries needed for the auto-promotion check"
)]
fn write_back_schemes(
    ctx: &mut InferCtx,
    scc: &[DeclId],
    decls: &[&FnDecl],
    mut scc_fn_types: FxHashMap<DeclId, Type>,
    env_snap_ty: &FxHashSet<TyVid>,
    env_snap_cap: &FxHashSet<CapVid>,
    env_snap_row: &FxHashSet<RowVid>,
    retained_constraints: &[Constraint],
    class_table: &ClassTable,
    instance_env: &InstanceEnv,
) {
    let mut generalised: Vec<(&Expr, String, Scheme, bool)> = Vec::new();
    for &did in scc {
        let decl = decls[did.0];
        if let Some(fn_ty) = scc_fn_types.remove(&did) {
            let mut scheme =
                generalise_with_env(ctx, &fn_ty, env_snap_ty, env_snap_cap, env_snap_row);
            // Attach retained constraints whose TyVid ended up in this scheme's
            // generalised variable set. Constraints over vars not in `scheme.vars`
            // escaped to an outer scope and were already reported as T030 by the
            // solver; we skip them here rather than double-reporting.
            scheme.constraints = retained_constraints
                .iter()
                .filter(|c| c.tys.iter().any(|v| scheme.vars.contains(v)))
                .cloned()
                .collect();
            let skip_name_bind = crate::collect::is_auto_promoted_totext(
                decl,
                class_table,
                &ctx.user_tycon_names,
                instance_env,
            );
            // Body::Ffi has no expression span to key a scheme entry by.
            // We still bind the name in the env for forward references.
            if let Body::Expr(e) = &decl.body {
                generalised.push((e, decl.name.text.clone(), scheme, skip_name_bind));
            } else if !skip_name_bind {
                // Ffi: bind the scheme in the env but skip schemes_accum.
                ctx.name_schemes_accum
                    .insert(decl.name.text.clone(), scheme.clone());
                ctx.env.bind(decl.name.text.clone(), scheme);
            }
        }
    }
    for (body, name, scheme, skip_name_bind) in generalised {
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
        if !skip_name_bind {
            ctx.name_schemes_accum.insert(name.clone(), scheme.clone());
            ctx.env.bind(name, scheme);
        }
    }
}

/// Typechecks a list of top-level `FnDecl`s from a single module using SCC-based
/// HM generalisation.
///
/// After this call, `ctx.env` (at the outermost frame) contains a generalised
/// [`Scheme`] for every decl.  Any `T###` diagnostics are pushed to `ctx.errors`.
///
/// # Constraint solving
///
/// When `instance_env` is non-empty (i.e. the workspace has registered typeclass
/// instances), the constraint solver runs after each SCC's bodies are inferred
/// and before generalisation. Retained constraints are attached to the
/// generalised schemes (see [`crate::solve::solve_constraints`]).
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
/// typecheck_module_decls(&mut ctx, &b, &decls, &class_table, &instance_env);
/// let schemes = /* read from ctx.env */;
/// ctx.env.pop_frame();
/// ```
#[expect(
    clippy::too_many_lines,
    reason = "SCC algorithm + constraint solving + generalisation — all tightly coupled; splitting would require passing \
              large amounts of shared state between sub-functions without clarity gain"
)]
pub fn typecheck_module_decls(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    decls: &[&FnDecl],
    class_table: &crate::class_env::ClassTable,
    instance_env: &crate::class_env::InstanceEnv,
) {
    if decls.is_empty() {
        return;
    }

    // Snapshot the ToText instance set into the inference context so that
    // interpolation-hole type-checking can perform O(1) membership tests.
    ctx.set_to_text_instances(instance_env);

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
        let env_snap_row = ctx.env_free_rowvids();

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

            // Build a type-variable name → fresh TyVid map for this fn.
            // We pre-allocate one TyVid per unique type variable name so that
            // the same name (e.g. `a` in both `(x: a)` and `where ToText a`)
            // maps to the SAME TyVid throughout the signature and the `where`
            // clause. Without this, each `ast_type_to_ridge_type` call for an
            // unknown name allocates a NEW TyVid, making the constraint's TyVid
            // different from the param's TyVid and breaking constraint solving.
            let mut tyvar_map: FxHashMap<&str, TyVid> = FxHashMap::default();
            // Collect type variable names from the `where` clause (each
            // constraint may carry several for a multi-parameter class).
            for c in &decl.constraints {
                for tv in &c.ty_vars {
                    tyvar_map
                        .entry(tv.text.as_str())
                        .or_insert_with(|| ctx.fresh_tyvid());
                }
            }
            // Collect type variable names from annotated params.
            for p in &decl.params {
                if let Param::Annotated { ty, .. } | Param::PatternAnnotated { ty, .. } = p {
                    collect_tyvars_from_ast_type(ty, &mut tyvar_map, ctx);
                }
            }
            // Collect type variable names from the return type.
            if let Some(ret_ast) = &decl.ret {
                collect_tyvars_from_ast_type(ret_ast, &mut tyvar_map, ctx);
            }

            let user_tycon_names = ctx.user_tycon_names.clone();
            let param_types: Vec<Type> = decl
                .params
                .iter()
                .map(|p| match p {
                    Param::Bare(_) => Type::Var(ctx.fresh_tyvid()),
                    Param::Annotated { ty, .. } | Param::PatternAnnotated { ty, .. } => {
                        ast_type_to_ridge_type(b, ctx, ty, &user_tycon_names, &tyvar_map)
                    }
                })
                .collect();
            let ret_ty = match &decl.ret {
                Some(ret_ast_ty) => {
                    ast_type_to_ridge_type(b, ctx, ret_ast_ty, &user_tycon_names, &tyvar_map)
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
            let to_rigid = mint_rigids(ctx, decl, &tyvar_map);
            let declared_ty = Type::Fn {
                params: param_types.clone(),
                ret: Box::new(ret_ty),
                caps,
            };
            let fn_ty = to_rigid.apply_to_ty(&declared_ty);
            scc_fn_types.insert(did, fn_ty.clone());
            scc_spans.insert(did, decl.span);

            // Seed deferred constraints from the `where` clause.
            // For each `where ClassName TyVar`, look up the TyVid allocated
            // for `TyVar` in `tyvar_map` and push a deferred constraint.
            // This allows the constraint solver to track the requirement on
            // the fn's own type variable through body inference.
            let mut declared_constraints: Vec<Constraint> = Vec::new();
            for c in &decl.constraints {
                let Some(class_id) = class_table.id_by_name(&c.class.text) else {
                    continue; // Unknown class — a typecheck error will fire elsewhere.
                };
                let tys: smallvec::SmallVec<[TyVid; 1]> = c
                    .ty_vars
                    .iter()
                    .filter_map(|tv| tyvar_map.get(tv.text.as_str()).copied())
                    .collect();
                if tys.len() != c.ty_vars.len() {
                    continue; // a constraint variable did not resolve — skip
                }
                // Record the promise, so a requirement the body raises on this
                // variable is met by the caller's dictionary instead of being
                // hunted for among the instances of a type nobody has picked.
                for tv in &tys {
                    if let Some(info) = ctx.rigids.get_mut(&RigidId(tv.0)) {
                        info.givens.push(class_id);
                    }
                }
                // Still deferred, and on the same variables as before. A
                // declared constraint belongs on the published scheme whether
                // or not the body ever exercises it, and these variables are
                // untouched by unification, so this travels the existing
                // free-variable path and is retained exactly as it was.
                let constraint = Constraint::new(class_id, tys);
                declared_constraints.push(constraint.clone());
                ctx.deferred_constraints.push(constraint);
            }

            // A genuinely auto-promoted `pub fn toText` must not bind the
            // top-level name `toText` — that would shadow the polymorphic
            // `ToText` class-method scheme for every other type in the module
            // (§5.6.6). Its body is still inferred below against `fn_ty` via
            // `scc_fn_types`; only this self-binding is skipped.
            if !crate::collect::is_auto_promoted_totext(
                decl,
                class_table,
                &ctx.user_tycon_names,
                instance_env,
            ) {
                let scheme = if signature_is_complete(decl) {
                    self_binding_from_signature(&declared_ty, &tyvar_map, declared_constraints)
                } else {
                    ctx.monomorphic_in_scc.insert(
                        decl.name.text.clone(),
                        crate::ctx::MonomorphicInScc {
                            vars: tyvar_map.values().copied().collect(),
                            bare_params: decl
                                .params
                                .iter()
                                .map(|p| matches!(p, Param::Bare(_)))
                                .collect(),
                            fix_hint: annotations_missing_from(decl),
                        },
                    );
                    monoscheme(fn_ty)
                };
                ctx.env.bind(decl.name.text.clone(), scheme);
            }
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
                    match param {
                        ridge_ast::Param::Bare(id) => {
                            ctx.env.bind(id.text.clone(), monoscheme(ty.clone()));
                        }
                        ridge_ast::Param::Annotated { name, .. } => {
                            ctx.env.bind(name.text.clone(), monoscheme(ty.clone()));
                        }
                        // A destructuring param binds every binder of its pattern
                        // against the param type; the pattern must be irrefutable.
                        // Bind first so a pattern-driven annotation (e.g. a tuple
                        // over a type variable) is resolved before the check.
                        ridge_ast::Param::PatternAnnotated { pat, span, .. } => {
                            infer_pattern(ctx, b, pat, ty);
                            crate::exhaustiveness::check_param_irrefutable(ctx, b, pat, ty, *span);
                        }
                    }
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
                    let expected_ty = ctx.deep_resolve(ret_ty_box);
                    let found_ty = ctx.deep_resolve(&body_ty);
                    // Issue #377 Trap B: an anonymous record literal where a
                    // named record type is expected gets the constructor hint.
                    let hint =
                        crate::unify::record_ctor_hint(&ctx.tycon_decls, &expected_ty, &found_ty);
                    let (expected, found) = crate::render::render_type_pair_with(
                        &expected_ty,
                        &found_ty,
                        &ctx.tycon_decls,
                    );
                    ctx.errors.push(TypeError::TypeMismatch {
                        expected,
                        found,
                        span,
                        hint,
                    });
                }

                ctx.current_fn_ret = saved_ret;
            }
            ctx.env.pop_frame();
        }

        // Past this point the SCC's names are rebound to generalised schemes,
        // and calling one at a second type is ordinary polymorphism.
        ctx.monomorphic_in_scc.clear();

        // ── Constraint solving — between step b and generalisation ───────────
        // Drain deferred constraints accumulated during body inference. For
        // modules with no constrained functions (the common pre-typeclass
        // case) this is a no-op: deferred_constraints is empty and the solver
        // returns immediately with empty retained + empty dict_resolution.
        //
        // The SCC span (first decl's span) is used as a fallback location in
        // diagnostics when a constraint carries no more precise span.
        let scc_span = scc
            .first()
            .and_then(|did| scc_spans.get(did))
            .copied()
            .unwrap_or_else(|| Span::point(0));
        let (retained, scc_dict_resolution) = crate::solve::solve_constraints(
            ctx,
            instance_env,
            class_table,
            &env_snap_ty,
            scc_span,
            Some(b),
        );

        // The type variables this SCC will generalise over — exactly the free
        // variables of its function signatures after the bodies have been
        // inferred. A parametric instance's element dictionary may forward one
        // of these (e.g. `Encode a` inside a `where Encode a` body); any element
        // variable that is neither here nor in an enclosing scope was never
        // pinned by the caller, so its dictionary is unsatisfiable (T030).
        //
        // A signature variable counts here too. It is a variable this SCC
        // generalises; it just does not travel as one, and `collect_free_vars`
        // skips it deliberately so instantiation never hands out a fresh copy.
        // For the question this set answers — can anyone supply the element
        // dictionary? — a rigid answers yes, under the abstraction target it
        // was minted from, which is the name the caller's dictionary parameter
        // is declared with.
        let mut generalizable: FxHashSet<TyVid> = FxHashSet::default();
        for fn_ty in scc_fn_types.values() {
            let resolved = ctx.deep_resolve(fn_ty);
            let (vars, _) = collect_free_vars(&resolved);
            generalizable.extend(vars);
            collect_rigid_targets(&resolved, &mut generalizable);
        }
        crate::solve::report_ambiguous_element_dicts(
            ctx,
            class_table,
            &scc_dict_resolution,
            &generalizable,
            &env_snap_ty,
            scc_span,
        );

        // Merge this SCC's resolution into the per-module accumulator so the
        // lowering pass can read the full map from ctx.dict_resolution_accum.
        ctx.dict_resolution_accum.extend(scc_dict_resolution);

        // The body has been checked, so the constants have done their job:
        // turn them back into the variables the scheme quantifies. The target
        // is the one reserved when each was minted, which is what lets the
        // dictionary a constraint forwards and the parameter lowering declares
        // for it spell the same name.
        let scc_fn_types: FxHashMap<DeclId, Type> = scc_fn_types
            .into_iter()
            .map(|(did, ty)| (did, abstract_rigids(&ty)))
            .collect();

        // ── Steps c+d: generalise and write back schemes ──────────────────────
        // OQ-PHASE45-003: top-level decl schemes only (no let-bound locals).
        // OQ-PHASE45-005: span-keyed via body span (same as T5 inferred_caps).
        write_back_schemes(
            ctx,
            scc,
            decls,
            scc_fn_types,
            &env_snap_ty,
            &env_snap_cap,
            &env_snap_row,
            &retained,
            class_table,
            instance_env,
        );
    }

    // The signature variables have done their job. Note that clearing this is
    // not what stops a rigid escaping: the union-find still binds every
    // variable that met one during checking, so anything deep-resolved from
    // here on can still produce one. Types leave through
    // `InferCtx::resolve_for_export`, which is where that is undone.
    ctx.rigids.clear();

    // 3. Detect T023 — unsolved type variables.
    //    Walk every binding in the current (outermost) frame, deep-resolve the
    //    scheme body, and check for residual free TyVids.
    detect_unsolved_type_vars(ctx);
}

/// Type-check the bodies of every source `instance` declaration's methods.
///
/// Instance method bodies are otherwise never inferred (only `Item::Fn` decls
/// go through [`typecheck_module_decls`]), so `node_types` carry nothing for the
/// expressions inside them and the lowering can only forward dictionaries by
/// positional order. That collapses two same-class constraints (`Row e, Row f`)
/// onto one entity whenever the body makes two separate single-dictionary calls
/// (a binary outer join's `rowColumns` on the left and the right entity).
///
/// This pass mirrors the per-fn inference of [`typecheck_module_decls`] steps
/// (a)/(b): one shared [`TyVid`] per instance head/`where` variable (so the head
/// variables the body references are the same variables the `where` constraints
/// name), the method params and return resolved against that map, the body
/// inferred and unified with the declared return. The inferred types are written
/// to `node_types`, which flow to the lowering unchanged.
///
/// Returns, per parametric instance (keyed by the `InstanceDecl` span), the
/// `where`-clause constraints paired with each constraint's head-variable real
/// `TyVid`, in source order — the lowering builds `current_fn_constraints` from
/// this so a class-method call's variable selects the matching dictionary. The
/// returned `TyVid`s are deep-resolved, matching the resolution applied to
/// `node_types`, so a body variable read from `node_types` compares equal.
///
/// Derived and prelude instances are not source `InstanceDecl`s and are left
/// untouched. With `record_errors` false the bodies are inferred for their
/// node-type side effects only; diagnostics they raise are discarded (the staged
/// rollout populates `node_types` before it changes which programs are rejected).
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "one linear pass mirroring the per-fn SCC inference; splitting the method loop would obscure the shared tyvar-map setup"
)]
pub fn infer_instance_methods(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    ast: &ridge_ast::Module,
    class_table: &ClassTable,
    instance_env: &InstanceEnv,
    record_errors: bool,
) -> FxHashMap<Span, Vec<TyVid>> {
    let mut recorded: FxHashMap<Span, Vec<TyVid>> = FxHashMap::default();
    let err_snapshot = ctx.errors.len();
    // Instance bodies own their deferred constraints; restore the module list after.
    let saved_deferred = std::mem::take(&mut ctx.deferred_constraints);

    for item in &ast.items {
        let Item::InstanceDecl(decl) = item else {
            continue;
        };

        // One shared TyVid per head/`where` type variable, allocated before any
        // body so a head variable reads the same way in the receiver type, the
        // method bodies, and the recorded `where` constraints.
        let mut tyvar_map: FxHashMap<&str, TyVid> = FxHashMap::default();
        for atom in &decl.head {
            collect_tyvars_from_ast_type(atom, &mut tyvar_map, ctx);
        }
        for c in &decl.constraints {
            for tv in &c.ty_vars {
                tyvar_map
                    .entry(tv.text.as_str())
                    .or_insert_with(|| ctx.fresh_tyvid());
            }
        }

        // The real head-variable TyVid of each `where` constraint's first
        // variable, one entry per constraint in source order so the index aligns
        // with `decl.constraints` in the lowering. The sentinel `TyVid(i)` is the
        // fallback (matching the lowering's positional default) for a variable
        // not found in the head — which a well-formed parametric instance never
        // hits, but keeps the index alignment exact.
        #[allow(clippy::cast_possible_truncation)]
        let head_vars: Vec<TyVid> = decl
            .constraints
            .iter()
            .enumerate()
            .map(|(i, c)| {
                c.ty_vars
                    .first()
                    .and_then(|tv| tyvar_map.get(tv.text.as_str()).copied())
                    .unwrap_or(TyVid(i as u32))
            })
            .collect();

        // The free variables in scope before this instance's own variables, so
        // the solver treats a class-method call on a head variable as a forward
        // rather than an unsatisfiable concrete requirement.
        let env_snap_ty = ctx.env_free_tyvids();

        for method in &decl.methods {
            // Method-local variables (only in this method's signature) extend the
            // shared head/`where` map.
            let mut method_map = tyvar_map.clone();
            for p in &method.params {
                if let Param::Annotated { ty, .. } | Param::PatternAnnotated { ty, .. } = p {
                    collect_tyvars_from_ast_type(ty, &mut method_map, ctx);
                }
            }
            collect_tyvars_from_ast_type(&method.ret, &mut method_map, ctx);

            let user_tycon_names = ctx.user_tycon_names.clone();
            let param_types: Vec<Type> = method
                .params
                .iter()
                .map(|p| match p {
                    Param::Bare(_) => Type::Var(ctx.fresh_tyvid()),
                    Param::Annotated { ty, .. } | Param::PatternAnnotated { ty, .. } => {
                        ast_type_to_ridge_type(b, ctx, ty, &user_tycon_names, &method_map)
                    }
                })
                .collect();
            let ret_ty =
                ast_type_to_ridge_type(b, ctx, &method.ret, &user_tycon_names, &method_map);

            ctx.env.push_frame();
            let saved_ret = ctx.current_fn_ret.take();
            ctx.current_fn_ret = Some(ret_ty.clone());

            for (param, ty) in method.params.iter().zip(param_types.iter()) {
                match param {
                    Param::Bare(id) => {
                        ctx.env.bind(id.text.clone(), monoscheme(ty.clone()));
                    }
                    Param::Annotated { name, .. } => {
                        ctx.env.bind(name.text.clone(), monoscheme(ty.clone()));
                    }
                    Param::PatternAnnotated { pat, span, .. } => {
                        infer_pattern(ctx, b, pat, ty);
                        crate::exhaustiveness::check_param_irrefutable(ctx, b, pat, ty, *span);
                    }
                }
            }

            let body_ty = infer_expr(ctx, b, &method.body);
            if unify(ctx, &body_ty, &ret_ty).is_err() {
                let expected_ty = ctx.deep_resolve(&ret_ty);
                let found_ty = ctx.deep_resolve(&body_ty);
                let hint =
                    crate::unify::record_ctor_hint(&ctx.tycon_decls, &expected_ty, &found_ty);
                let (expected, found) =
                    crate::render::render_type_pair_with(&expected_ty, &found_ty, &ctx.tycon_decls);
                ctx.errors.push(TypeError::TypeMismatch {
                    expected,
                    found,
                    span: method.span,
                    hint,
                });
            }

            ctx.current_fn_ret = saved_ret;
            ctx.env.pop_frame();
        }

        // Drain the bodies' deferred constraints. The dictionary plan is unused
        // (the lowering forwards via `current_fn_constraints`); this resolves the
        // body unifications so the recorded variables and `node_types` agree.
        let _ = solve_constraints(
            ctx,
            instance_env,
            class_table,
            &env_snap_ty,
            decl.span,
            Some(b),
        );

        if !head_vars.is_empty() {
            recorded.insert(decl.span, head_vars);
        }
    }

    // Deep-resolve the recorded variables against the now-complete union-find,
    // matching the resolution `node_types` receives at module end.
    for vars in recorded.values_mut() {
        for tyvid in vars.iter_mut() {
            if let Type::Var(rep) = ctx.deep_resolve(&Type::Var(*tyvid)) {
                *tyvid = rep;
            }
        }
    }

    ctx.deferred_constraints = saved_deferred;

    if !record_errors {
        // The bodies are inferred for their node-type side effects only. Some
        // stdlib instances trip the source-vs-builtin `SqlValue` split that exists
        // only during the standard library's own build (two distinct `SqlValue`
        // tycons), which the seeded class-method schemes reconcile for ordinary
        // functions but not yet for instance bodies. Those diagnostics are not
        // real — user builds carry a single `SqlValue` — so discard everything
        // raised here until that reconciliation is extended (then flip to true to
        // gain compile-time checking of instance bodies). A failed body-vs-return
        // unify binds nothing, so it leaves the sub-expression node_types intact.
        ctx.errors.truncate(err_snapshot);
    }

    recorded
}

/// Collect all lower-case type variable names from an AST type annotation.
///
/// Used by the SCC step (a) to pre-allocate `TyVid`s for all type variables
/// in the fn signature BEFORE calling `ast_type_to_ridge_type`, so that the
/// same variable name (e.g. `a` appearing in both a param and the `where`
/// clause) maps to the SAME fresh `TyVid` throughout.
///
/// Lower-case single-letter identifiers are treated as type variables; all
/// others (upper-case or multi-character) are treated as type constructors.
fn collect_tyvars_from_ast_type<'a>(
    ty: &'a ridge_ast::Type,
    map: &mut rustc_hash::FxHashMap<&'a str, TyVid>,
    ctx: &mut crate::ctx::InferCtx,
) {
    match ty {
        ridge_ast::Type::Named { name, .. } => {
            // Heuristic: a fully-lowercase ident is a type variable (e.g. `a`,
            // `b`, `key`, `val`). Upper-case or mixed-case names are type
            // constructors and are not collected here.
            let n = name.text.as_str();
            let is_tyvar = n.chars().next().is_some_and(char::is_lowercase);
            if is_tyvar {
                map.entry(n).or_insert_with(|| ctx.fresh_tyvid());
            }
        }
        // A type variable that the parser/resolver already tagged as `Var` (rather
        // than the lowercase-`Named` heuristic above). `ast_type_to_ridge_type`
        // resolves both forms through this same map, so both must seed it —
        // otherwise the two halves of a signature that share a variable only in
        // `Var` form (e.g. `entityWitness (_r: Repo e a) -> Option e`, whose body
        // ignores the parameter so nothing else links them) each allocate a fresh
        // variable and the return type decouples from the parameter.
        ridge_ast::Type::Var { name, .. } => {
            map.entry(name.text.as_str())
                .or_insert_with(|| ctx.fresh_tyvid());
        }
        ridge_ast::Type::App { args, .. } => {
            for a in args {
                collect_tyvars_from_ast_type(a, map, ctx);
            }
        }
        ridge_ast::Type::Fn { fn_ty, .. } => {
            for p in &fn_ty.params {
                collect_tyvars_from_ast_type(p, map, ctx);
            }
            collect_tyvars_from_ast_type(&fn_ty.ret, map, ctx);
        }
        ridge_ast::Type::Tuple { elems, .. } => {
            for e in elems {
                collect_tyvars_from_ast_type(e, map, ctx);
            }
        }
        ridge_ast::Type::List { elem, .. } => {
            collect_tyvars_from_ast_type(elem, map, ctx);
        }
        ridge_ast::Type::Record { fields, .. } => {
            for f in fields {
                collect_tyvars_from_ast_type(&f.ty, map, ctx);
            }
        }
        // Primitive and other forms carry no type variables.
        _ => {}
    }
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

    /// Returns empty class and instance registries for tests that do not
    /// exercise typeclass constraint solving.
    fn empty_registries() -> (crate::class_env::ClassTable, crate::class_env::InstanceEnv) {
        (
            crate::class_env::ClassTable::new(),
            crate::class_env::InstanceEnv::new(),
        )
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
            constraints: vec![],
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

        let (ct, ie) = empty_registries();
        typecheck_module_decls(&mut ctx, &b, &decls, &ct, &ie);

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

        let (ct, ie) = empty_registries();
        typecheck_module_decls(&mut ctx, &b, &decls, &ct, &ie);

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

        let (ct, ie) = empty_registries();
        typecheck_module_decls(&mut ctx, &b, &decls, &ct, &ie);

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
            row_vars: vec![],
            ty: Type::Var(unbound),
            constraints: vec![],
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
            constraints: vec![],
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
        let (ct, ie) = empty_registries();
        typecheck_module_decls(&mut ctx, &b, &decls, &ct, &ie);
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
            constraints: vec![],
            body: ridge_ast::Body::Expr(Expr::Ident(Ident {
                text: "x".to_string(),
                span: body_ident_span,
            })),
            span: decl_span,
            doc: None,
        };
        let decls: Vec<&FnDecl> = vec![&decl];
        let (ct, ie) = empty_registries();
        typecheck_module_decls(&mut ctx, &b, &decls, &ct, &ie);
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
        let (ct, ie) = empty_registries();
        typecheck_module_decls(&mut ctx, &b, &decls, &ct, &ie);
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
