//! Constraint solving for typeclass dispatch (0.2.13).
//!
//! # Overview
//!
//! This pass runs once per SCC, **after** body inference and **before**
//! generalisation. It drains [`crate::ctx::InferCtx::deferred_constraints`]
//! and classifies each constraint into one of three cases:
//!
//! - **(a) Concrete** — the constraint's type variable resolved to a concrete
//!   `Type::Con`; look up the `(ClassId, TyConId)` pair in the
//!   [`InstanceEnv`]. Missing instance → T029 [`TypeError::NoInstance`].
//!   Present instance → record a [`DictPlan::Static`] entry and recursively
//!   require the instance's `ctx_constraints` and all superclass instances.
//!
//! - **(b) Retained** — the constraint's type variable is still free and will
//!   be generalised by this SCC (it does not appear in the pre-SCC env
//!   snapshot). Push the constraint to the `retained` list; the caller
//!   attaches it to the resulting `Scheme.constraints`.
//!
//! - **(c) Ambiguous** — the constraint's type variable is still free but
//!   escapes the generalisation scope (it appeared in the env snapshot). This
//!   means the caller provided no context to pin the type, so we cannot
//!   resolve or generalise the constraint → T030
//!   [`TypeError::AmbiguousConstraint`].
//!
//! # Parallel with capability variables
//!
//! This mechanism mirrors the [`CapVid`] defer-and-resolve pattern:
//!
//! 1. **Introduce / defer**: at a call site, `instantiate` pushes each
//!    constraint's remapped `TyVid` into `ctx.deferred_constraints` — just as
//!    `infer_expr` pushes `CapRow::Var(fresh_capvid)` into the `capvids`
//!    union-find.
//!
//! 2. **Flow through inference**: unification stays constraint-unaware (as it
//!    is capvid-unaware). Constraints follow their `TyVid` through
//!    `deep_resolve`, exactly as capvids resolve through their union-find
//!    roots.
//!
//! 3. **Solve / generalise**: this pass replaces the per-SCC capvid
//!    collect-and-generalise step. The insertion point in `scc.rs` is the
//!    same: after step-b (infer bodies) and before `write_back_schemes`.
//!
//! # Termination
//!
//! - **No backtracking**: coherence guarantees at most one instance per
//!   `(ClassId, TyConId)` pair, so instance lookup is a single map probe.
//! - **Single-param classes**: each constraint has exactly one `TyVid`.
//! - **Finite superclass recursion**: the class graph is acyclic (T035 guards
//!   this before any instance solving runs). Each superclass step moves
//!   strictly up the DAG. A `visited` set prevents redundant work.
//! - **Finite `ctx_constraints` recursion**: each parametric instance
//!   constraint (`Show a` inside `Show (List a)`) strips one type constructor
//!   layer, so the depth is bounded by the structural depth of the type.

use ridge_ast::Span;
use ridge_types::{BuiltinTyCons, ClassId, Constraint, TyConId, TyVid, Type};
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

use crate::class_env::{ClassTable, InstanceEnv, InstanceInfo};
use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::tycon_collect::ast_type_to_ridge_type;
use crate::unify::unify;

// ── Dictionary resolution record ─────────────────────────────────────────────

/// How a class constraint at a specific call site is satisfied.
///
/// This record is produced by [`solve_constraints`] and stored on
/// [`DictResolution`] for the lowering pass to consume. The lowering pass
/// uses it to emit the correct dictionary argument at every constrained call
/// site.
///
/// The lowering pass reads this data; the typecheck pass only writes it.
#[derive(Debug, Clone)]
pub enum DictPlan {
    /// The constraint was satisfied by a concrete, statically-known instance.
    ///
    /// The lowering pass should pass the literal instance dictionary as the
    /// dictionary argument to the call.
    ///
    /// `tycon` is the concrete [`TyConId`] that satisfied the constraint —
    /// stored here so the lowering pass can look up the type's source name
    /// from [`TypedWorkspace::tycons`] to form the dict constant name
    /// `$inst_{ClassName}_{TypeName}`.
    Static {
        /// The class this dictionary satisfies. The lowering pass reads it to
        /// form the dict constant name `$inst_{ClassName}_…` from the *plan's
        /// own* class, so a heterogeneous context sub-dictionary (e.g. the
        /// `Adapter a` dict inside a `Projectable` instance) is named against
        /// its own class rather than the enclosing instance's class.
        class: ClassId,
        /// Instance metadata (method names, origin, etc.).
        info: Box<InstanceInfo>,
        /// The first concrete head constructor that was resolved. For a
        /// single-parameter class this is the whole head; the dict constant is
        /// `$inst_{ClassName}_{name(tycon)}`.
        tycon: TyConId,
        /// Additional head constructors for a multi-parameter class
        /// (`Convert Celsius Fahrenheit` → `tycon = Celsius`,
        /// `extra_head = [Fahrenheit]`). Empty for a single-parameter class, so
        /// the single-parameter dict name is unchanged; for a multi-parameter
        /// class the lowering pass appends these to form
        /// `$inst_{ClassName}_{name(tycon)}_{name(extra…)}`.
        extra_head: SmallVec<[TyConId; 1]>,
        /// Sub-dictionary plans for a parametric instance's context
        /// constraints, in `ctx_constraints` order.
        ///
        /// Empty for every non-parametric instance — the lowering pass emits a
        /// bare `$inst_{Class}_{Type}` symbol reference in that case, exactly as
        /// before. For a parametric instance such as
        /// `instance Encode (List a) where Encode a`, this holds the resolved
        /// element dictionary plan (e.g. the `Encode Int` plan when the head is
        /// `List Int`). The lowering pass applies the `$inst_` function to these
        /// sub-dicts, producing the dict-of-dicts at runtime.
        args: Vec<DictPlan>,
    },
    /// The constraint is still polymorphic: the caller receives a dictionary
    /// parameter and should forward it to the callee.
    ///
    /// Equivalent to Haskell's implicit parameter threading.
    Forward(Constraint),
}

/// Per-call-site dictionary resolution plan, keyed by the constraint itself.
///
/// This map is produced by [`solve_constraints`] and attached to the typed
/// module for the lowering pass (which is implemented in a later release).
/// The key is `(ClassId, TyVid_after_fresh_remap)` — uniquely identifies one
/// deferred constraint at one instantiation site.
///
/// The lowering pass iterates this map and emits dict arguments at call sites.
pub type DictResolution = FxHashMap<(ClassId, TyVid), DictPlan>;

// ── Main entry point ──────────────────────────────────────────────────────────

/// Solve the deferred class constraints accumulated during SCC body inference.
///
/// # Arguments
///
/// - `ctx` — the active inference context. `ctx.deferred_constraints` is
///   **drained** by this call; `ctx.errors` receives any T029/T030 diagnostics.
/// - `instance_env` — the workspace-level instance registry (read-only here).
/// - `class_table` — the workspace-level class registry (read-only here).
/// - `env_snap_ty` — the set of [`TyVid`]s that were free in the environment
///   **before** this SCC's monomorphic bindings were added. A constraint whose
///   type variable appears here is ambiguous (case c).
/// - `scc_span` — best-available span for the SCC (used as a fallback when a
///   constraint carries no span of its own).
///
/// # Returns
///
/// `(retained, dict_resolution)` where:
/// - `retained` — constraints that should be attached to the generalised
///   scheme (case b: the variable will be quantified).
/// - `dict_resolution` — the per-constraint dictionary plan for the lowering
///   pass.
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashSet is the canonical hasher for this crate; matches the pattern in instantiate.rs"
)]
pub fn solve_constraints(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    env_snap_ty: &FxHashSet<TyVid>,
    scc_span: Span,
    builtins: Option<&BuiltinTyCons>,
) -> (Vec<Constraint>, DictResolution) {
    // Drain the deferred list. We process it as a work queue so that recursive
    // superclass / ctx_constraint requirements can be appended and processed in
    // the same pass.
    let initial: Vec<Constraint> = std::mem::take(&mut ctx.deferred_constraints);
    let mut work: Vec<Constraint> = initial;

    let mut retained: Vec<Constraint> = Vec::new();
    let mut dict_resolution: DictResolution = FxHashMap::default();

    // Visited set: prevents re-processing the same (ClassId, TyConId) pair
    // when multiple constraints or superclass chains converge on the same
    // instance. This makes the solver idempotent on repeated requirements.
    let mut visited: FxHashSet<(ClassId, TyConId)> = FxHashSet::default();

    while let Some(c) = work.pop() {
        if c.tys.len() == 1 {
            // Single-parameter constraint — the established case (a)/(b)/(c) path.
            let resolved = ctx.deep_resolve(&Type::Var(c.sole_ty()));
            dispatch_constraint(
                ctx,
                instance_env,
                class_table,
                env_snap_ty,
                scc_span,
                &c,
                &resolved,
                &mut work,
                &mut visited,
                &mut retained,
                &mut dict_resolution,
            );
        } else {
            // Multi-parameter constraint (`Convert a b`, …).
            dispatch_multi_constraint(
                ctx,
                instance_env,
                class_table,
                env_snap_ty,
                scc_span,
                &c,
                &mut retained,
                &mut dict_resolution,
                builtins,
            );
        }
    }

    (retained, dict_resolution)
}

// ── Internal dispatch ─────────────────────────────────────────────────────────

/// Dispatch one constraint to case (a), (b), or (c) and update the work
/// queue / retained list / `dict_resolution` accordingly.
#[allow(clippy::too_many_arguments)]
fn dispatch_constraint(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    env_snap_ty: &FxHashSet<TyVid>,
    scc_span: Span,
    c: &Constraint,
    resolved: &Type,
    work: &mut Vec<Constraint>,
    visited: &mut FxHashSet<(ClassId, TyConId)>,
    retained: &mut Vec<Constraint>,
    dict_resolution: &mut DictResolution,
) {
    match resolved {
        // ── Case (a): concrete type — look up instance ────────────────────────
        Type::Con(tyconid, _) => {
            let tyconid = *tyconid;
            // Clone the full resolved type so discharge_concrete can read the
            // type arguments when substituting ctx_constraints for parametric
            // instances (e.g. `Encode (List Int)` → arg 0 is `Int`).
            let resolved_con = resolved.clone();
            discharge_concrete(
                ctx,
                instance_env,
                class_table,
                scc_span,
                c,
                tyconid,
                &resolved_con,
                work,
                visited,
                dict_resolution,
            );
        }

        // ── Case (b) / (c): type variable ────────────────────────────────────
        Type::Var(v) => {
            let v = *v;
            if env_snap_ty.contains(&v) {
                // Case (c): the variable escapes this SCC's generalisation
                // scope — it belongs to an outer binding and was never
                // pinned to a concrete type. The constraint is ambiguous.
                let class_name = class_table
                    .get(c.class)
                    .map_or("?", |info| info.name.as_str());
                ctx.errors.push(TypeError::AmbiguousConstraint {
                    class: class_name.to_string(),
                    ty_var: format!("?{}", v.0),
                    span: scc_span,
                });
            } else {
                // Case (b): the variable will be generalised by this SCC.
                // Retain the constraint so the caller can attach it to the
                // resulting scheme. Use the resolved canonical TyVid (`v`)
                // rather than the original `c.ty` so that `write_back_schemes`
                // can match the constraint against `scheme.vars` (which also
                // holds resolved vars). Without this, constraints introduced
                // through instantiation carry an aliased TyVid that diverges
                // from the generalised vars even though they represent the
                // same unification root.
                let resolved_c = Constraint::single(c.class, v);
                if !retained.iter().any(|r| r == &resolved_c) {
                    retained.push(resolved_c.clone());
                }
                // Record a Forward plan keyed by the resolved TyVid.
                dict_resolution
                    .entry((c.class, v))
                    .or_insert(DictPlan::Forward(resolved_c));
            }
        }

        // ── Case (a'): function type — key on the synthetic Fn/arity ─────────
        // A bare function satisfies a class with a function-type instance head
        // (`instance Handler (fn a -> R)`). Dispatch keys on `Fn/params.len()`
        // (arity only; the capability row is not part of the key). This
        // reuses the concrete discharge path wholesale: the function's
        // params/ret are projected as positional "type arguments" by
        // `resolve_ctx_dict_args`, exactly like `List a` / `Result a e`.
        Type::Fn { params, .. } => {
            if let Some(tyconid) = ridge_types::fn_tycon_id(params.len()) {
                let resolved_con = resolved.clone();
                discharge_concrete(
                    ctx,
                    instance_env,
                    class_table,
                    scc_span,
                    c,
                    tyconid,
                    &resolved_con,
                    work,
                    visited,
                    dict_resolution,
                );
            } else {
                // Arity exceeds the reserved Fn/N block — no instance can exist.
                let class_name = class_table
                    .get(c.class)
                    .map_or("?", |info| info.name.as_str());
                ctx.errors.push(TypeError::NoInstance {
                    class: class_name.to_string(),
                    ty: format!("a function of arity {}", params.len()),
                    span: scc_span,
                    fix_hint: format!(
                        "functions of arity {} cannot be class instances (max {})",
                        params.len(),
                        ridge_types::FN_ARITY_COUNT - 1
                    ),
                });
            }
        }

        // ── Other resolved types (Error, Alias, Tuple …) ─────────────────────
        // Error: already in an error path — skip silently to avoid cascading.
        // Alias: should have been resolved by deep_resolve; treat as unknown.
        // Tuple: not a valid class head in single-param dispatch.
        _ => {
            // Emit a no-instance error for non-Con / non-Var shapes. These
            // arise from ill-typed programs that already have other errors.
            let class_name = class_table
                .get(c.class)
                .map_or("?", |info| info.name.as_str());
            ctx.errors.push(TypeError::NoInstance {
                class: class_name.to_string(),
                ty: format!("{resolved:?}"),
                span: scc_span,
                fix_hint: format!("add `instance {class_name} T` where `T` is the concrete type"),
            });
        }
    }
}

/// Dispatch a multi-parameter class constraint (`Convert a b`, …).
///
/// Resolves every constrained variable, then:
/// - **all concrete** → look the instance up by the head tuple via
///   [`InstanceEnv::get_multi`]; a missing instance is T029.
/// - **all still variables** → retain the constraint for generalisation
///   (case b) when the variables are local, or report T030 ambiguity when one
///   escapes the SCC (case c).
/// - **mixed concrete/variable** → T030 ambiguity. Resolving such a constraint
///   without an annotation needs functional dependencies, which this release
///   does not implement; the user annotates the open position instead.
#[allow(clippy::too_many_arguments)]
#[expect(
    clippy::too_many_lines,
    reason = "one linear dispatch over the three resolution cases (all-concrete with the composite-receiver fallback, all-variable retention, mixed-ambiguity); splitting it would scatter the shared head/dict-resolution state"
)]
fn dispatch_multi_constraint(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    env_snap_ty: &FxHashSet<TyVid>,
    scc_span: Span,
    c: &Constraint,
    retained: &mut Vec<Constraint>,
    dict_resolution: &mut DictResolution,
    builtins: Option<&BuiltinTyCons>,
) {
    let n = c.tys.len();

    // Functional-dependency improvement runs before classification. For a class
    // that declares a fundep it pins each determined position to the matching
    // instance's head type — resolving an open position, and *verifying* an
    // already-fixed one. A determined type the fundep forbids is a hard error
    // (reported inside), and we stop. For a class with no fundep this is a no-op.
    if improve_via_fundeps(ctx, instance_env, class_table, scc_span, c, builtins) {
        return;
    }

    let resolved: Vec<Type> = c
        .tys
        .iter()
        .map(|&v| ctx.deep_resolve(&Type::Var(v)))
        .collect();

    let mut head_tycons: SmallVec<[TyConId; 1]> = SmallVec::new();
    let mut head_vars: SmallVec<[TyVid; 1]> = SmallVec::new();
    for r in &resolved {
        match r {
            Type::Con(id, _) => head_tycons.push(*id),
            // A function-type head position (a `Refinable`/`Run`-style instance
            // over `e -> Bool` / `e -> f -> Bool`) keys on the reserved arity
            // tycon `Fn/N`, exactly as the single-parameter dispatch path does.
            // The arity distinguishes a 1-row predicate from a 2-row one, which is
            // how the fundep tells a `Query` filter from a `Join` filter.
            Type::Fn { params, .. } => {
                if let Some(id) = ridge_types::fn_tycon_id(params.len()) {
                    head_tycons.push(id);
                }
            }
            Type::Var(v) => head_vars.push(*v),
            _ => {}
        }
    }

    let class_name = class_table
        .get(c.class)
        .map_or("?", |info| info.name.as_str());

    // ── All concrete: resolve the instance by the head tuple. ──
    if head_tycons.len() == n {
        // A nested-join composite receiver keys its terminal instance (and dict) by
        // the receiver alone: the functional dependency collapses the predicate, so
        // there is one instance per receiver, not one per predicate arity. Match the
        // full head first (binary receivers, keyed `[receiver, Fn/N]`); on a miss for
        // a composite-join receiver, fall back to the determining position alone.
        let recv_is_composite =
            !head_tycons.is_empty() && ctx.is_composite_join_tycon(head_tycons[0]);
        let matched: Option<(&InstanceInfo, &[TyConId])> = instance_env
            .get_multi(c.class, &head_tycons)
            .map(|i| (i, &head_tycons[..]))
            .or_else(|| {
                if head_tycons.len() > 1 && recv_is_composite {
                    instance_env
                        .get_multi(c.class, &head_tycons[..1])
                        .map(|i| (i, &head_tycons[..1]))
                } else {
                    None
                }
            });
        let Some((info, matched_head)) = matched else {
            let head_disp = head_tycons
                .iter()
                .map(|t| format!("{t:?}"))
                .collect::<Vec<_>>()
                .join(" ");
            ctx.errors.push(TypeError::NoInstance {
                class: class_name.to_string(),
                ty: head_disp,
                span: scc_span,
                fix_hint: format!("add `instance {class_name} …` for this combination of types"),
            });
            return;
        };
        let info = info.clone();
        let extra_head: SmallVec<[TyConId; 1]> = matched_head.iter().skip(1).copied().collect();
        let tycon = matched_head[0];
        let key = (c.class, c.tys[0]);
        // Idempotent: a second constraint converging on the same instance must
        // not re-resolve the context sub-dictionaries (which would re-report any
        // missing-instance diagnostic).
        if dict_resolution.contains_key(&key) {
            return;
        }
        // Resolve the instance's context constraints (`instance C (T a) (U b)
        // where D a`) against the resolved head atoms, threading their resolved
        // sub-dictionaries into the plan. Empty for a context-free instance.
        let args =
            resolve_ctx_dict_args_multi(ctx, instance_env, class_table, scc_span, &info, &resolved);
        dict_resolution.insert(
            key,
            DictPlan::Static {
                class: c.class,
                info: Box::new(info),
                tycon,
                extra_head,
                args,
            },
        );
        return;
    }

    // ── All variables: retain for generalisation, or report ambiguity. ──
    if head_vars.len() == n {
        if let Some(&escaping) = head_vars.iter().find(|v| env_snap_ty.contains(v)) {
            ctx.errors.push(TypeError::AmbiguousConstraint {
                class: class_name.to_string(),
                ty_var: format!("?{}", escaping.0),
                span: scc_span,
            });
        } else {
            let resolved_c = Constraint::new(c.class, head_vars.clone());
            if !retained.iter().any(|r| r == &resolved_c) {
                retained.push(resolved_c.clone());
            }
            dict_resolution
                .entry((c.class, head_vars[0]))
                .or_insert(DictPlan::Forward(resolved_c));
        }
        return;
    }

    // ── Mixed: an open position no fundep could determine. ──
    let var = head_vars
        .first()
        .map_or_else(|| format!("?{}", c.tys[0].0), |v| format!("?{}", v.0));
    ctx.errors.push(TypeError::AmbiguousConstraint {
        class: class_name.to_string(),
        ty_var: var,
        span: scc_span,
    });
}

/// Functional-dependency improvement for a multi-parameter constraint. For every
/// fundep whose determining positions all resolved to a concrete head
/// constructor, find the single matching instance and unify each determined
/// position against that instance's written head type — but only when that head
/// type is itself fully concrete. This both *pins* an open determined position
/// (so a result-determined method resolves with no annotation) and *verifies* an
/// already-fixed one: a determined type that disagrees with the instance's head
/// is exactly the type the fundep forbids, and the disagreement is reported.
///
/// Returns `true` when a conflict was reported and the caller should stop. A
/// no-op without `builtins` (the unit-test path) or for a class with no fundep.
fn improve_via_fundeps(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    scc_span: Span,
    c: &Constraint,
    builtins: Option<&BuiltinTyCons>,
) -> bool {
    let Some(b) = builtins else {
        return false;
    };
    let fundeps = class_table.fundeps_of(c.class);
    if fundeps.is_empty() {
        return false;
    }

    let resolved: Vec<Type> = c
        .tys
        .iter()
        .map(|&v| ctx.deep_resolve(&Type::Var(v)))
        .collect();

    let empty_names: FxHashMap<String, TyConId> = FxHashMap::default();

    for fd in fundeps {
        // The determining positions must all be concrete head constructors.
        let mut fixed: SmallVec<[(usize, TyConId); 2]> = SmallVec::new();
        let mut from_ok = true;
        for &p in &fd.from {
            if let Some(Type::Con(id, _)) = resolved.get(p) {
                fixed.push((p, *id));
            } else {
                from_ok = false;
                break;
            }
        }
        if !from_ok {
            continue;
        }

        // A nested-join composite receiver (`Joined`/`LeftJoined`/…) determines its
        // terminal predicate directly: the positional leaf list of the receiver,
        // returning a free result the lambda body fixes (`Bool` for `filter`/`every`,
        // the projected shape for `select`, the column type for an aggregate). The
        // arity is the leaf count, so a wrong-arity lambda fails to unify here — the
        // compile-time arity check the binary instances get from `Fn/N`-keyed
        // dispatch, here for an unbounded number of leaves. No `head_asts` entry is
        // needed (the seeded instances carry none); the receiver alone keys the
        // instance and its dictionary.
        if fd.from.len() == 1 && fd.to.len() == 1 {
            if let Some(recv) = resolved.get(fd.from[0]) {
                if ctx.is_composite_join_receiver(recv) {
                    if let Some(leaves) = ctx.join_entities(recv) {
                        let ret = ctx.fresh_tyvid();
                        let leaf_fn = Type::Fn {
                            params: leaves,
                            ret: Box::new(Type::Var(ret)),
                            caps: ridge_types::CapRow::Concrete(ridge_types::CapabilitySet::PURE),
                        };
                        if let Err(e) = unify(ctx, &Type::Var(c.tys[fd.to[0]]), &leaf_fn) {
                            ctx.errors
                                .push(crate::records::attach_span_pub(e, scc_span));
                            return true;
                        }
                        continue;
                    }
                }
            }
        }

        // Coherence guarantees at most one instance per determining tuple.
        let head_asts: Vec<ridge_ast::Type> = {
            let matches = instance_env.instances_matching(c.class, &fixed);
            if matches.len() != 1 {
                continue;
            }
            matches[0].1.clone()
        };

        // The instance head may be parametric — `instance Refinable (Query e a)
        // (fn e -> Bool)` determines `p = e -> Bool`, a type that mentions the
        // instance's own variable `e`. Give every head variable a fresh inference
        // variable, then bind them by unifying each determining position's written
        // head against the resolved constraint type: `Query e a` against a concrete
        // `Query User Mem` fixes `e = User`. The determined position then converts
        // to a concrete type through the same shared map.
        let head_var_names = collect_head_vars(&head_asts);
        let param_map: FxHashMap<&str, TyVid> = head_var_names
            .iter()
            .map(|n| (n.as_str(), ctx.fresh_tyvid()))
            .collect();

        for &p in &fd.from {
            if let (Some(from_ast), Some(actual)) = (head_asts.get(p), resolved.get(p)) {
                let from_ty = ast_type_to_ridge_type(b, ctx, from_ast, &empty_names, &param_map);
                let _ = unify(ctx, &from_ty, actual);
            }
        }

        for &p in &fd.to {
            let Some(to_ast) = head_asts.get(p) else {
                continue;
            };
            let to_ty = ast_type_to_ridge_type(b, ctx, to_ast, &empty_names, &param_map);
            // Re-resolve so the variables the determining positions just bound are
            // substituted in (`e -> Bool` becomes `User -> Bool`).
            let to_ty = ctx.deep_resolve(&to_ty);
            // Act only on a fully concrete determined head. A position no
            // determining variable reached stays open — left for an annotation
            // rather than risking an unsound pin.
            if type_contains_var(&to_ty) {
                continue;
            }
            // Pin an open determined position, or verify an already-fixed one. A
            // disagreement is the determined type the fundep forbids — report it.
            if let Err(e) = unify(ctx, &Type::Var(c.tys[p]), &to_ty) {
                ctx.errors
                    .push(crate::records::attach_span_pub(e, scc_span));
                return true;
            }
        }
    }

    false
}

/// Whether a resolved type still mentions any inference variable. Used by
/// fundep improvement to refuse pinning a determined position against a head
/// type that is not fully concrete.
fn type_contains_var(t: &Type) -> bool {
    match t {
        Type::Con(_, args) => args.iter().any(type_contains_var),
        Type::Fn { params, ret, .. } => {
            params.iter().any(type_contains_var) || type_contains_var(ret)
        }
        Type::Tuple(elems) => elems.iter().any(type_contains_var),
        Type::Alias { body, .. } => type_contains_var(body),
        // A bare variable, a record row, an error, or any other shape is treated
        // as not-fully-concrete, so we never pin a determined position against it.
        _ => true,
    }
}

/// Collect every type-variable name written in an instance head's types, in
/// first-seen order with no duplicates. Fundep improvement gives each one a
/// shared fresh inference variable so a parametric determined position
/// (`e -> Bool`) can be fixed from the determining position (`Query e a`).
fn collect_head_vars(head_asts: &[ridge_ast::Type]) -> Vec<String> {
    fn walk(t: &ridge_ast::Type, out: &mut Vec<String>) {
        match t {
            ridge_ast::Type::Var { name, .. } => {
                if !out.iter().any(|n| n == &name.text) {
                    out.push(name.text.clone());
                }
            }
            ridge_ast::Type::App { args, .. } => args.iter().for_each(|a| walk(a, out)),
            ridge_ast::Type::Tuple { elems, .. } => elems.iter().for_each(|e| walk(e, out)),
            ridge_ast::Type::List { elem, .. } => walk(elem, out),
            ridge_ast::Type::Fn { fn_ty, .. } => {
                fn_ty.params.iter().for_each(|p| walk(p, out));
                walk(&fn_ty.ret, out);
            }
            ridge_ast::Type::Paren { inner, .. } => walk(inner, out),
            ridge_ast::Type::Record { fields, .. } => {
                fields.iter().for_each(|f| walk(&f.ty, out));
            }
            ridge_ast::Type::Named { .. } | ridge_ast::Type::Primitive { .. } => {}
        }
    }
    let mut out = Vec::new();
    for t in head_asts {
        walk(t, &mut out);
    }
    out
}

/// Attempt to discharge a concrete `(ClassId, TyConId)` constraint.
///
/// On success: record a [`DictPlan::Static`] entry and enqueue the instance's
/// superclass and `ctx_constraints` requirements. On failure: push T029.
///
/// For parametric instances the `resolved_con` carries the full `Type::Con`
/// (including type arguments such as `[Int]` in `List Int`). When the instance
/// has a non-empty `head_var_positions`, each `ctx_constraint` is substituted
/// with the concrete arg type at the recorded position before being enqueued.
#[allow(clippy::too_many_arguments)]
fn discharge_concrete(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    scc_span: Span,
    c: &Constraint,
    tyconid: TyConId,
    resolved_con: &Type,
    work: &mut Vec<Constraint>,
    visited: &mut FxHashSet<(ClassId, TyConId)>,
    dict_resolution: &mut DictResolution,
) {
    let key = (c.class, tyconid);

    // Idempotence: if we have already processed this (class, type) pair,
    // skip. This prevents infinite recursion when multiple constraints
    // converge on the same instance (e.g. two call sites both require
    // `ToText Color`).
    if !visited.insert(key) {
        return;
    }

    let class_name = class_table
        .get(c.class)
        .map_or("?", |info| info.name.as_str());

    match instance_env.get(key) {
        None => {
            // T029 — no instance for this (class, type) pair.
            let fix_hint = build_fix_hint(class_name, tyconid);
            ctx.errors.push(TypeError::NoInstance {
                class: class_name.to_string(),
                ty: format!("{tyconid:?}"),
                span: scc_span,
                fix_hint,
            });
        }

        Some(inst_info) => {
            let inst_info = inst_info.clone();

            // For a parametric instance, resolve each context constraint's
            // sub-dictionary plan against the concrete type arguments. For a
            // non-parametric instance `head_var_positions` is empty, so this is
            // an empty vec and the lowering pass emits a bare `$inst_` symbol —
            // identical behaviour to before this feature.
            let args = resolve_ctx_dict_args(
                ctx,
                instance_env,
                class_table,
                scc_span,
                &inst_info,
                resolved_con,
            );

            // Record the static resolution plan for the lowering pass.
            // Include the concrete TyConId so the lowering pass can look up
            // the type name without re-resolving the instance.
            dict_resolution
                .entry((c.class, c.sole_ty()))
                .or_insert_with(|| DictPlan::Static {
                    class: c.class,
                    info: Box::new(inst_info.clone()),
                    tycon: tyconid,
                    extra_head: SmallVec::new(),
                    args,
                });

            // Enqueue superclass requirements for the same concrete type.
            // Termination: the class DAG is acyclic (T035 checked earlier).
            if let Some(class_info) = class_table.get(c.class) {
                for &superclass_id in &class_info.superclasses {
                    let super_key = (superclass_id, tyconid);
                    if !visited.contains(&super_key) {
                        // Use the same TyVid from the original constraint —
                        // the solver will deep_resolve it again.
                        work.push(Constraint::single(superclass_id, c.sole_ty()));
                    }
                }
            }

            // The instance's context constraints are NOT enqueued onto the
            // worklist. For a parametric instance the sub-dictionary plans were
            // already computed by `resolve_ctx_dict_args` (which recurses through
            // `resolve_dict_plan`) and attached to the Static plan's `args`. Any
            // missing-element-instance diagnostic (T029) is raised there.
            //
            // Enqueuing them here would record extra `dict_resolution` entries
            // keyed by fresh inference variables under the same `ClassId` as the
            // top-level constraint. The lowering pass selects a dictionary by
            // `ClassId` alone (`resolve_dict_arg`), so a spurious sub-constraint
            // entry could be picked instead of the real one — passing the element
            // dictionary where the container dictionary was required. Keeping
            // sub-dicts solely inside the parent plan's `args` avoids that.
        }
    }
}

/// Resolve the ordered sub-dictionary plans for a parametric instance's
/// context constraints.
///
/// For a parametric instance `instance Encode (List a) where Encode a`, the
/// head's concrete type arguments are matched against `head_var_positions` to
/// recover the element type (`Int` in `List Int`), and the matching context
/// constraint (`Encode a`) is resolved against that concrete element type to
/// produce its `DictPlan`. The result is one plan per `ctx_constraint`, in
/// order; the lowering pass applies `$inst_Encode_List` to these sub-dicts.
///
/// Returns an empty vec for non-parametric instances (`head_var_positions`
/// empty), so the caller records a plain `DictPlan::Static { args: [] }`.
fn resolve_ctx_dict_args(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    scc_span: Span,
    inst_info: &InstanceInfo,
    resolved_con: &Type,
) -> Vec<DictPlan> {
    if inst_info.head_var_positions.is_empty() {
        return Vec::new();
    }

    let con_args: Vec<Type> = match resolved_con {
        Type::Con(_, args) => args.clone(),
        // Project a function type into positional "type arguments": the
        // parameter types followed by the return type (`[p₀ … pₙ, ret]`). This
        // lets a parametric function instance (`instance Handler (fn a -> R)
        // where Handler a`) pull an element dictionary from a head position via
        // `head_var_positions` — the identical mechanism `List a` / `Result a e`
        // use for their type arguments.
        Type::Fn { params, ret, .. } => {
            let mut projected = params.clone();
            projected.push((**ret).clone());
            projected
        }
        _ => Vec::new(),
    };

    let mut args: Vec<DictPlan> = Vec::with_capacity(inst_info.ctx_constraints.len());
    for (ctx_c, &pos) in inst_info
        .ctx_constraints
        .iter()
        .zip(inst_info.head_var_positions.iter())
    {
        // The concrete element type at this head argument position.
        let arg_ty = con_args.get(pos).cloned().unwrap_or(Type::Error);
        let resolved_arg = ctx.deep_resolve(&arg_ty);
        let plan = resolve_dict_plan(
            ctx,
            instance_env,
            class_table,
            scc_span,
            ctx_c.class,
            &resolved_arg,
        );
        args.push(plan);
    }
    args
}

/// Resolve the ordered sub-dictionary plans for a **multi-parameter** instance's
/// context constraints (`instance Projectable (Query e a) (fn e -> s) where
/// Adapter a`).
///
/// The single-parameter [`resolve_ctx_dict_args`] indexes one resolved head
/// type's arguments. Here the head spans several atoms, so the resolved atoms
/// are flattened into one positional list — each `Con` contributes its type
/// arguments, each `Fn` its parameters then its return — exactly the order
/// `collect::flatten_head_arg_names` used when it recorded `head_var_positions`.
/// Each context constraint is then resolved against the concrete type at its
/// recorded position.
///
/// Returns an empty vec for a context-free instance (`head_var_positions`
/// empty), so the caller records a plain `DictPlan::Static { args: [] }`.
fn resolve_ctx_dict_args_multi(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    scc_span: Span,
    inst_info: &InstanceInfo,
    resolved_atoms: &[Type],
) -> Vec<DictPlan> {
    if inst_info.head_var_positions.is_empty() {
        return Vec::new();
    }

    let mut flat: Vec<Type> = Vec::new();
    for atom in resolved_atoms {
        match atom {
            Type::Con(_, args) => flat.extend(args.iter().cloned()),
            Type::Fn { params, ret, .. } => {
                flat.extend(params.iter().cloned());
                flat.push((**ret).clone());
            }
            _ => {}
        }
    }

    let mut args: Vec<DictPlan> = Vec::with_capacity(inst_info.ctx_constraints.len());
    for (ctx_c, &pos) in inst_info
        .ctx_constraints
        .iter()
        .zip(inst_info.head_var_positions.iter())
    {
        // The sentinel resolves to the determined predicate's return type — the
        // last flattened element — so a constraint over a composite terminal's
        // variable-arity result (`SqlType n`, `Row s`) lands on `n`/`s` whatever
        // the join depth. A fixed index reads its own flattened position.
        let arg_ty = if pos == crate::class_env::PREDICATE_RETURN_POS {
            flat.last().cloned().unwrap_or(Type::Error)
        } else {
            flat.get(pos).cloned().unwrap_or(Type::Error)
        };
        let resolved_arg = ctx.deep_resolve(&arg_ty);
        let plan = resolve_dict_plan(
            ctx,
            instance_env,
            class_table,
            scc_span,
            ctx_c.class,
            &resolved_arg,
        );
        args.push(plan);
    }
    args
}

/// Resolve a `(class, concrete-or-var type)` pair into a [`DictPlan`] without
/// touching the solver work queue.
///
/// This is the recursive core that builds the dict-of-dicts for parametric
/// instances. It mirrors [`dispatch_constraint`]'s case (a) / case (b)
/// classification but produces a plan directly rather than enqueuing:
///
/// - `Type::Con` with a registered instance → `DictPlan::Static`, recursing on
///   the instance's own context constraints (so `List (List Int)` nests).
/// - `Type::Var` → `DictPlan::Forward` (the enclosing scope threads a dict
///   param; e.g. resolving `Encode a` inside a `where Encode a` body).
/// - Missing instance / other shapes → a `Forward` placeholder; the missing
///   instance is reported by the normal worklist path, so no duplicate T029.
fn resolve_dict_plan(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    scc_span: Span,
    class: ClassId,
    resolved: &Type,
) -> DictPlan {
    // Placeholder for the "no resolution" cases. The top-level worklist still
    // enqueues the corresponding constraint and emits any T029 diagnostic, so
    // these placeholders never double-report.
    let forward_placeholder = || DictPlan::Forward(Constraint::single(class, TyVid(0)));

    match resolved {
        Type::Con(tyconid, _) => {
            let tyconid = *tyconid;
            // Clone the instance metadata out first so the immutable borrow on
            // `instance_env` is released before the recursive call below borrows
            // `ctx` mutably.
            let Some(inst_info) = instance_env.get((class, tyconid)).cloned() else {
                // No instance for this element type → T029. The element type is a
                // concrete `Type::Con`, so this is a genuine missing-instance
                // error (e.g. `Encode (List SomeType)` where `SomeType` has no
                // `Encode` instance). Report it here because the sub-constraint is
                // no longer enqueued onto the worklist.
                let class_name = class_table
                    .get(class)
                    .map_or("?", |info| info.name.as_str());
                let fix_hint = build_fix_hint(class_name, tyconid);
                ctx.errors.push(TypeError::NoInstance {
                    class: class_name.to_string(),
                    ty: format!("{tyconid:?}"),
                    span: scc_span,
                    fix_hint,
                });
                return forward_placeholder();
            };
            let args = resolve_ctx_dict_args(
                ctx,
                instance_env,
                class_table,
                scc_span,
                &inst_info,
                resolved,
            );
            DictPlan::Static {
                class,
                info: Box::new(inst_info),
                tycon: tyconid,
                extra_head: SmallVec::new(),
                args,
            }
        }
        Type::Var(v) => DictPlan::Forward(Constraint::single(class, *v)),
        // Other shapes are ill-typed for a class head; a diagnostic fires on
        // the normal path. Use a Forward placeholder so the plan is total.
        _ => forward_placeholder(),
    }
}

/// Report `T030` for any parametric-instance element dictionary that resolved
/// to a free type variable the caller never pinned.
///
/// A parametric instance such as `Encode (Option a)` needs the element's
/// dictionary at runtime. When a call site fixes the container head but leaves
/// the element type open — e.g. `toJson None`, where the `Option`'s element is
/// never constrained — the solver records a `DictPlan::Static` whose element
/// sub-plan is a `Forward` over an unbound variable. That variable cannot be
/// satisfied: it is neither concrete nor a parameter the enclosing function can
/// thread a dictionary for. Emitting a clear ambiguity error here is correct;
/// silently forwarding a non-existent dictionary would crash at runtime or
/// encode the wrong value.
///
/// `generalizable` holds the type variables that *will* become this SCC's
/// scheme variables (a legitimate `where C a` body forwards one of these).
/// `outer` holds variables that belong to an enclosing scope and are already
/// dictionary-threaded there. An element variable in neither set is ambiguous.
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashSet is the canonical hasher for this crate; matches solve_constraints"
)]
pub fn report_ambiguous_element_dicts(
    ctx: &mut InferCtx,
    class_table: &ClassTable,
    dict_resolution: &DictResolution,
    generalizable: &FxHashSet<TyVid>,
    outer: &FxHashSet<TyVid>,
    scc_span: Span,
) {
    // Collect first so the immutable borrow on `dict_resolution` is released
    // before pushing diagnostics through the mutable `ctx`.
    let mut ambiguous: Vec<(ClassId, Span)> = Vec::new();
    for plan in dict_resolution.values() {
        collect_ambiguous_element_vars(plan, generalizable, outer, scc_span, &mut ambiguous);
    }
    for (class, span) in ambiguous {
        let class_name = class_table
            .get(class)
            .map_or("?", |info| info.name.as_str());
        ctx.errors.push(TypeError::AmbiguousConstraint {
            class: class_name.to_string(),
            ty_var: "the element type".to_string(),
            span,
        });
    }
}

/// Walk a [`DictPlan`] tree and record every parametric sub-dictionary that
/// forwards an unsatisfiable element variable.
///
/// Only the `args` of a `Static` plan — the element/value/arm dictionaries of a
/// parametric instance — are inspected. A top-level `Forward` (the whole
/// constraint is polymorphic, classified separately by `dispatch_constraint`)
/// is not an element position and is left alone.
fn collect_ambiguous_element_vars(
    plan: &DictPlan,
    generalizable: &FxHashSet<TyVid>,
    outer: &FxHashSet<TyVid>,
    scc_span: Span,
    out: &mut Vec<(ClassId, Span)>,
) {
    if let DictPlan::Static { args, .. } = plan {
        for sub in args {
            match sub {
                DictPlan::Forward(c) => {
                    // A forwarded element dict is ambiguous when none of its
                    // variables is generalisable or threaded from an outer scope.
                    let satisfiable = c
                        .tys
                        .iter()
                        .any(|v| generalizable.contains(v) || outer.contains(v));
                    if !satisfiable {
                        out.push((c.class, scc_span));
                    }
                }
                DictPlan::Static { .. } => {
                    collect_ambiguous_element_vars(sub, generalizable, outer, scc_span, out);
                }
            }
        }
    }
}

/// Build a T029 fix hint for a given `(class, tyconid)` pair.
///
/// For the special case of `Eq Float` the hint includes the floating-point
/// footgun warning per the spec. All other cases get the generic suggestion.
fn build_fix_hint(class_name: &str, tyconid: TyConId) -> String {
    // Float is TyConId(1) in the builtin arena (builtins.rs allocation order:
    // Int=0, Float=1, Bool=2, Text=3, Unit=4, Timestamp=5, …).
    // We use the numeric id because `class_name` is the class, not the type,
    // and we have only the raw TyConId for the type at this call site.
    let is_eq_float = class_name == "Eq" && tyconid == ridge_types::TyConId(1);

    if is_eq_float {
        "floating-point equality is a footgun (`0.1 + 0.2 ≠ 0.3`); \
         `Eq Float` is intentionally omitted from the prelude. \
         Use explicit comparison or `Crypto.constantTimeEq` for secrets."
            .to_string()
    } else {
        format!(
            "add `instance {class_name} T` or add `deriving ({class_name})` to the type declaration"
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_types::{
        BuiltinTyCons, Constraint, Scheme, TyConArena, TyConId, TyVid, Type, ENCODE_CLASS,
        EQ_CLASS, ORD_CLASS, TOTEXT_CLASS,
    };
    use rustc_hash::FxHashSet;

    use crate::class_env::{
        register_prelude_classes, ClassTable, InstanceEnv, InstanceInfo, InstanceOrigin,
    };
    use crate::ctx::{InferCtx, TyValue, TyVidKey};
    use crate::instantiate::instantiate;

    fn dummy_span() -> Span {
        Span::point(0)
    }

    fn make_instance_info() -> InstanceInfo {
        InstanceInfo {
            def_module: None,
            methods: vec![],
            ctx_constraints: vec![],
            head_var_positions: vec![],
            origin: InstanceOrigin::Explicit,
            span: dummy_span(),
        }
    }

    fn make_class_table() -> ClassTable {
        let mut ct = ClassTable::new();
        register_prelude_classes(&mut ct);
        ct
    }

    fn make_instance_env_with(class: ridge_types::ClassId, tycon: TyConId) -> InstanceEnv {
        let mut env = InstanceEnv::new();
        env.insert((class, tycon), make_instance_info(), "ToText", "Color")
            .expect("single insert must succeed");
        env
    }

    // ── Case (a): concrete type with existing instance → no error, Static plan ─

    #[test]
    fn case_a_concrete_instance_present_no_error() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        // Use the Int TyConId as a stand-in for a concrete type with a ToText instance.
        let int_tycon = b.int;
        let a = ctx.fresh_tyvid();

        // Unify the TyVid with Int so deep_resolve returns Type::Con(int_tycon, []).
        ctx.tyvids
            .union_value(TyVidKey(a.0), TyValue(Some(Type::Con(int_tycon, vec![]))));

        // Push a deferred constraint: ToText a (which resolves to ToText Int).
        ctx.deferred_constraints
            .push(Constraint::single(TOTEXT_CLASS, a));

        let ct = make_class_table();
        let env = make_instance_env_with(TOTEXT_CLASS, int_tycon);
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, dict_res) =
            solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        // The constraint was satisfied: no errors, not retained.
        assert!(
            ctx.errors.is_empty(),
            "case (a) must produce no errors; got {:?}",
            ctx.errors
        );
        assert!(retained.is_empty(), "case (a): no retained constraints");
        // A Static dict plan must be recorded.
        let plan = dict_res.get(&(TOTEXT_CLASS, a));
        assert!(
            matches!(plan, Some(DictPlan::Static { .. })),
            "case (a): expected Static plan, got {plan:?}"
        );
    }

    // ── Case (a): concrete type with NO instance → T029 ──────────────────────

    #[test]
    fn case_a_concrete_no_instance_emits_t029() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        let float_tycon = b.float;
        let a = ctx.fresh_tyvid();

        ctx.tyvids
            .union_value(TyVidKey(a.0), TyValue(Some(Type::Con(float_tycon, vec![]))));

        // ToText a where a = Float, but no ToText Float instance exists.
        ctx.deferred_constraints
            .push(Constraint::single(TOTEXT_CLASS, a));

        let ct = make_class_table();
        let env = InstanceEnv::new(); // empty — no instances
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.iter().any(|e| e.code() == "T029"),
            "case (a) missing instance must emit T029; errors: {:?}",
            ctx.errors
        );
        assert!(retained.is_empty());
    }

    // ── Case (b): free var not in env snapshot → retained ────────────────────

    #[test]
    fn case_b_free_var_not_in_env_retained() {
        let mut ctx = InferCtx::new();
        let a = ctx.fresh_tyvid();

        // Push a deferred constraint: ToText a. `a` is fresh and not unified.
        ctx.deferred_constraints
            .push(Constraint::single(TOTEXT_CLASS, a));

        let ct = make_class_table();
        let env = InstanceEnv::new();
        // env_snap does NOT contain `a` → case (b).
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, dict_res) =
            solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.is_empty(),
            "case (b) must produce no errors; got {:?}",
            ctx.errors
        );
        assert_eq!(retained.len(), 1, "case (b): must retain the constraint");
        assert_eq!(retained[0], Constraint::single(TOTEXT_CLASS, a));
        // A Forward plan must be recorded.
        let plan = dict_res.get(&(TOTEXT_CLASS, a));
        assert!(
            matches!(plan, Some(DictPlan::Forward(_))),
            "case (b): expected Forward plan, got {plan:?}"
        );
    }

    // ── Case (c): free var IN env snapshot → T030 ────────────────────────────

    #[test]
    fn case_c_escaping_var_emits_t030() {
        let mut ctx = InferCtx::new();
        let a = ctx.fresh_tyvid();

        ctx.deferred_constraints
            .push(Constraint::single(TOTEXT_CLASS, a));

        let ct = make_class_table();
        let env = InstanceEnv::new();
        // env_snap CONTAINS `a` → case (c): ambiguous.
        let mut env_snap: FxHashSet<TyVid> = FxHashSet::default();
        env_snap.insert(a);

        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.iter().any(|e| e.code() == "T030"),
            "case (c) escaping var must emit T030; errors: {:?}",
            ctx.errors
        );
        assert!(retained.is_empty(), "case (c): nothing retained");
    }

    // ── Empty deferred list → no-op ──────────────────────────────────────────

    #[test]
    fn empty_deferred_is_noop() {
        let mut ctx = InferCtx::new();
        // No constraints pushed.
        let ct = make_class_table();
        let env = InstanceEnv::new();
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, dict_res) =
            solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(ctx.errors.is_empty());
        assert!(retained.is_empty());
        assert!(dict_res.is_empty());
    }

    // ── Superclass propagation: Ord requires Eq ──────────────────────────────

    #[test]
    fn superclass_requirement_propagated() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        let int_tycon = b.int;
        let a = ctx.fresh_tyvid();

        ctx.tyvids
            .union_value(TyVidKey(a.0), TyValue(Some(Type::Con(int_tycon, vec![]))));

        // Require Ord a → the solver should also check Eq a (Ord's superclass).
        ctx.deferred_constraints
            .push(Constraint::single(ORD_CLASS, a));

        let ct = make_class_table();
        let mut env = InstanceEnv::new();
        // Register BOTH Ord Int and Eq Int so the superclass check passes.
        env.insert((ORD_CLASS, int_tycon), make_instance_info(), "Ord", "Int")
            .expect("Ord Int insert");
        env.insert((EQ_CLASS, int_tycon), make_instance_info(), "Eq", "Int")
            .expect("Eq Int insert");

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.is_empty(),
            "superclass present — no error; got {:?}",
            ctx.errors
        );
        assert!(retained.is_empty());
    }

    // ── Superclass propagation: Ord without Eq → T029 ────────────────────────

    #[test]
    fn superclass_missing_emits_t029() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        let int_tycon = b.int;
        let a = ctx.fresh_tyvid();

        ctx.tyvids
            .union_value(TyVidKey(a.0), TyValue(Some(Type::Con(int_tycon, vec![]))));

        ctx.deferred_constraints
            .push(Constraint::single(ORD_CLASS, a));

        let ct = make_class_table();
        let mut env = InstanceEnv::new();
        // Only Ord Int, no Eq Int → T029 for Eq.
        env.insert((ORD_CLASS, int_tycon), make_instance_info(), "Ord", "Int")
            .expect("Ord Int insert");

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let (_, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.iter().any(|e| e.code() == "T029"),
            "missing Eq superclass must emit T029; errors: {:?}",
            ctx.errors
        );
    }

    // ── instantiate remap: constraint follows its TyVid ──────────────────────

    #[test]
    fn instantiate_remaps_constraint_to_fresh_tyvid() {
        let mut ctx = InferCtx::new();

        // Pre-allocate one TyVid so that the fresh var allocated during
        // instantiate gets a different raw index than any scheme-bound var.
        // Without this, both the bound var and the fresh var could be TyVid(0).
        let _pre = ctx.fresh_tyvid(); // TyVid(0) consumed

        let a = TyVid(99); // a raw index far from any allocated var
                           // forall a. a, with constraint ToText a
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Var(a),
            constraints: vec![Constraint::single(TOTEXT_CLASS, a)],
        };

        assert!(ctx.deferred_constraints.is_empty());
        let _inst_ty = instantiate(&mut ctx, &scheme);

        // One constraint must have been pushed.
        assert_eq!(
            ctx.deferred_constraints.len(),
            1,
            "one constraint must be deferred after instantiate"
        );
        // The constraint's TyVid must be the fresh var that was allocated
        // during instantiate — NOT the bound var TyVid(99).
        let dc = &ctx.deferred_constraints[0];
        assert_eq!(dc.class, TOTEXT_CLASS);
        assert_ne!(
            dc.sole_ty(),
            a,
            "constraint TyVid must be the fresh var, not the bound var TyVid(99)"
        );
        // The fresh var must match the TyVid allocated inside the ctx (TyVid(1)
        // because TyVid(0) was pre-consumed above).
        assert_eq!(
            dc.sole_ty().0,
            1,
            "fresh var for the first scheme var must be TyVid(1)"
        );
    }

    // ── No deferred constraints for an unconstrained scheme ──────────────────

    #[test]
    fn instantiate_unconstrained_scheme_pushes_nothing() {
        let mut ctx = InferCtx::new();
        let a = TyVid(0);
        let scheme = Scheme {
            vars: vec![a],
            cap_vars: vec![],
            row_vars: vec![],
            ty: Type::Var(a),
            constraints: vec![], // unconstrained
        };

        let _ = instantiate(&mut ctx, &scheme);
        assert!(
            ctx.deferred_constraints.is_empty(),
            "unconstrained scheme must not push any deferred constraints"
        );
    }

    // ── Polymorphic propagation: retained constraints attach to scheme ────────

    #[test]
    fn retained_constraints_returned_for_attachment() {
        let mut ctx = InferCtx::new();
        let a = ctx.fresh_tyvid();
        let b = ctx.fresh_tyvid();

        // Two constraints: ToText a (will be retained) and ToText b (also retained).
        ctx.deferred_constraints
            .push(Constraint::single(TOTEXT_CLASS, a));
        ctx.deferred_constraints
            .push(Constraint::single(TOTEXT_CLASS, b));

        let ct = make_class_table();
        let env = InstanceEnv::new();
        // Neither `a` nor `b` in env_snap → case (b) for both.
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(ctx.errors.is_empty());
        assert_eq!(retained.len(), 2, "both constraints must be retained");
        assert!(
            retained.iter().any(|c| c.class == TOTEXT_CLASS),
            "retained must include ToText constraint"
        );
    }

    // ── Parametric instance solver substitution ──────────────────────────────
    //
    // When discharging a concrete constraint whose instance is parametric, the
    // solver must substitute the concrete type arg(s) into the ctx_constraints
    // before resolving their sub-dictionaries.

    /// Build a parametric `InstanceInfo` for a 1-arg head like `List a`.
    ///
    /// `ctx_class` is the class required on the element (e.g. `ENCODE_CLASS`).
    /// `arg_pos` is the arg position of the constrained var (0 for `List a`).
    fn make_parametric_instance(ctx_class: ClassId, arg_pos: usize) -> InstanceInfo {
        InstanceInfo {
            def_module: None,
            methods: vec![],
            // sentinel TyVid(0) — solver must not use this directly
            ctx_constraints: vec![Constraint::single(ctx_class, TyVid(0))],
            head_var_positions: vec![arg_pos],
            origin: InstanceOrigin::Explicit,
            span: dummy_span(),
        }
    }

    /// Build a 2-constraint parametric `InstanceInfo` for a 2-arg head like `Result a e`.
    fn make_parametric_instance_2(ctx_class: ClassId, pos0: usize, pos1: usize) -> InstanceInfo {
        InstanceInfo {
            def_module: None,
            methods: vec![],
            ctx_constraints: vec![
                Constraint::single(ctx_class, TyVid(0)),
                Constraint::single(ctx_class, TyVid(0)),
            ],
            head_var_positions: vec![pos0, pos1],
            origin: InstanceOrigin::Explicit,
            span: dummy_span(),
        }
    }

    /// `Encode (List Int)` — the solver must enqueue `Encode Int` (NOT a free var).
    ///
    /// After solving: no error, `Encode Int` is also discharged via its concrete instance.
    #[test]
    fn parametric_list_int_enqueues_encode_int() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        let list_tycon = b.list;
        let int_tycon = b.int;

        // The constraint variable resolves to `List Int`.
        let a = ctx.fresh_tyvid();
        ctx.tyvids.union_value(
            TyVidKey(a.0),
            TyValue(Some(Type::Con(
                list_tycon,
                vec![Type::Con(int_tycon, vec![])],
            ))),
        );

        ctx.deferred_constraints
            .push(Constraint::single(ENCODE_CLASS, a));

        let ct = make_class_table();
        let mut env = InstanceEnv::new();
        // Register `Encode (List a) where Encode a` — parametric.
        env.insert(
            (ENCODE_CLASS, list_tycon),
            make_parametric_instance(ENCODE_CLASS, 0),
            "Encode",
            "List",
        )
        .expect("parametric Encode List insert");
        // Register `Encode Int` — the sub-constraint must resolve here.
        env.insert(
            (ENCODE_CLASS, int_tycon),
            make_instance_info(),
            "Encode",
            "Int",
        )
        .expect("Encode Int insert");

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let (retained, dict_res) =
            solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.is_empty(),
            "Encode (List Int) with Encode Int present must produce no errors; got {:?}",
            ctx.errors
        );
        assert!(retained.is_empty(), "all constraints must be discharged");

        // A Static plan for the outer Encode (List _) constraint must exist.
        let outer_plan = dict_res.get(&(ENCODE_CLASS, a));
        assert!(
            matches!(outer_plan, Some(DictPlan::Static { tycon, .. }) if *tycon == list_tycon),
            "outer plan must be Static for List; got {outer_plan:?}"
        );
    }

    /// A multi-parameter instance with a heterogeneous context (`instance Demo
    /// (Box a) (Tag b) where Encode a`) threads the context sub-dictionary across
    /// the flattened head: solving `Demo (Box Int) (Tag Bool)` yields a Static
    /// plan whose one arg is the `Encode Int` sub-dictionary, tagged with its own
    /// class. Exercises `resolve_ctx_dict_args_multi` and the `Static.class` field.
    #[test]
    fn multi_param_instance_context_threads_sub_dict() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let int_tycon = b.int;
        // Two arbitrary builtin tycons stand in for the head constructors.
        let box_tycon = b.list;
        let tag_tycon = b.set;

        let mut ct = make_class_table();
        let demo = ct.intern("Demo");
        ct.insert_with_id(
            demo,
            crate::class_env::ClassInfo {
                name: "Demo".to_string(),
                arity: 2,
                method_sigs: vec![],
                superclasses: vec![],
                def_module: None,
            },
        );

        // q = Box Int, p = Tag Bool.
        let q = ctx.fresh_tyvid();
        let p = ctx.fresh_tyvid();
        ctx.tyvids.union_value(
            TyVidKey(q.0),
            TyValue(Some(Type::Con(
                box_tycon,
                vec![Type::Con(int_tycon, vec![])],
            ))),
        );
        ctx.tyvids.union_value(
            TyVidKey(p.0),
            TyValue(Some(Type::Con(tag_tycon, vec![Type::Con(b.bool, vec![])]))),
        );
        ctx.deferred_constraints
            .push(Constraint::new(demo, [q, p].into_iter().collect()));

        let mut env = InstanceEnv::new();
        // instance Demo (Box a) (Tag b) where Encode a — `a` at flattened pos 0
        // (Box is the first atom; its single arg is position 0).
        let head: crate::class_env::InstanceHead = [box_tycon, tag_tycon].into_iter().collect();
        env.instances.insert(
            (demo, head),
            InstanceInfo {
                def_module: None,
                methods: vec![],
                ctx_constraints: vec![Constraint::single(ENCODE_CLASS, TyVid(0))],
                head_var_positions: vec![0],
                origin: InstanceOrigin::Explicit,
                span: dummy_span(),
            },
        );
        env.insert(
            (ENCODE_CLASS, int_tycon),
            make_instance_info(),
            "Encode",
            "Int",
        )
        .expect("Encode Int insert");

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let (_retained, dict_res) =
            solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), Some(&b));

        assert!(ctx.errors.is_empty(), "no errors; got {:?}", ctx.errors);
        match dict_res.get(&(demo, q)) {
            Some(DictPlan::Static { tycon, args, .. }) => {
                assert_eq!(*tycon, box_tycon, "outer plan keyed on the first head atom");
                assert_eq!(args.len(), 1, "one context sub-dict (Encode a)");
                assert!(
                    matches!(&args[0], DictPlan::Static { tycon: t, class, .. }
                        if *t == int_tycon && *class == ENCODE_CLASS),
                    "sub-dict must be `Encode Int` tagged with ENCODE_CLASS; got {:?}",
                    args[0]
                );
            }
            other => panic!("expected a Static plan for Demo; got {other:?}"),
        }
    }

    /// `Encode (List Int)` without `Encode Int` in the env — must emit T029 for Int.
    #[test]
    fn parametric_list_int_missing_element_instance_emits_t029() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        let list_tycon = b.list;
        let int_tycon = b.int;

        let a = ctx.fresh_tyvid();
        ctx.tyvids.union_value(
            TyVidKey(a.0),
            TyValue(Some(Type::Con(
                list_tycon,
                vec![Type::Con(int_tycon, vec![])],
            ))),
        );
        ctx.deferred_constraints
            .push(Constraint::single(ENCODE_CLASS, a));

        let ct = make_class_table();
        let mut env = InstanceEnv::new();
        // Only `Encode (List a)` — no `Encode Int`.
        env.insert(
            (ENCODE_CLASS, list_tycon),
            make_parametric_instance(ENCODE_CLASS, 0),
            "Encode",
            "List",
        )
        .expect("parametric Encode List insert");

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let _ = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.iter().any(|e| e.code() == "T029"),
            "missing Encode Int must emit T029; errors: {:?}",
            ctx.errors
        );
    }

    /// `Encode (Map Text Bool)` — the solver must bind position-1 arg (Bool),
    /// NOT position-0 (Text). Only Bool is the constrained element.
    #[test]
    fn parametric_map_text_bool_binds_bool_not_text() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        let map_tycon = b.map;
        let text_tycon = b.text;
        let bool_tycon = b.bool;

        // a resolves to `Map Text Bool`.
        let a = ctx.fresh_tyvid();
        ctx.tyvids.union_value(
            TyVidKey(a.0),
            TyValue(Some(Type::Con(
                map_tycon,
                vec![Type::Con(text_tycon, vec![]), Type::Con(bool_tycon, vec![])],
            ))),
        );
        ctx.deferred_constraints
            .push(Constraint::single(ENCODE_CLASS, a));

        let ct = make_class_table();
        let mut env = InstanceEnv::new();
        // `Encode (Map Text a) where Encode a` — var at position 1.
        env.insert(
            (ENCODE_CLASS, map_tycon),
            make_parametric_instance(ENCODE_CLASS, 1),
            "Encode",
            "Map",
        )
        .expect("parametric Encode Map insert");
        // Register `Encode Bool` — the correctly-positioned arg.
        env.insert(
            (ENCODE_CLASS, bool_tycon),
            make_instance_info(),
            "Encode",
            "Bool",
        )
        .expect("Encode Bool insert");
        // Do NOT register `Encode Text` — if the solver mistakenly binds pos 0
        // it would emit T029 for Text.

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.is_empty(),
            "Encode (Map Text Bool) with Encode Bool must have no errors; got {:?}",
            ctx.errors
        );
        assert!(retained.is_empty());
    }

    /// `Encode (Result Int Text)` with two `ctx_constraints` — both must be
    /// substituted at their correct positions.
    #[test]
    fn parametric_result_int_text_both_positions() {
        let mut ctx = InferCtx::new();
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        let result_tycon = b.result;
        let int_tycon = b.int;
        let text_tycon = b.text;

        // a resolves to `Result Int Text`.
        let a = ctx.fresh_tyvid();
        ctx.tyvids.union_value(
            TyVidKey(a.0),
            TyValue(Some(Type::Con(
                result_tycon,
                vec![Type::Con(int_tycon, vec![]), Type::Con(text_tycon, vec![])],
            ))),
        );
        ctx.deferred_constraints
            .push(Constraint::single(ENCODE_CLASS, a));

        let ct = make_class_table();
        let mut env = InstanceEnv::new();
        // `Encode (Result a e) where Encode a, Encode e` — vars at positions 0 and 1.
        env.insert(
            (ENCODE_CLASS, result_tycon),
            make_parametric_instance_2(ENCODE_CLASS, 0, 1),
            "Encode",
            "Result",
        )
        .expect("parametric Encode Result insert");
        env.insert(
            (ENCODE_CLASS, int_tycon),
            make_instance_info(),
            "Encode",
            "Int",
        )
        .expect("Encode Int insert");
        env.insert(
            (ENCODE_CLASS, text_tycon),
            make_instance_info(),
            "Encode",
            "Text",
        )
        .expect("Encode Text insert");

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span(), None);

        assert!(
            ctx.errors.is_empty(),
            "Encode (Result Int Text) must have no errors; got {:?}",
            ctx.errors
        );
        assert!(retained.is_empty());
    }
}
