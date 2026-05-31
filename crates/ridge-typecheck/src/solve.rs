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
use ridge_types::{ClassId, Constraint, TyConId, TyVid, Type};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::class_env::{ClassTable, InstanceEnv, InstanceInfo};
use crate::ctx::InferCtx;
use crate::error::TypeError;

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
        /// Instance metadata (method names, origin, etc.).
        info: Box<InstanceInfo>,
        /// The concrete type that was resolved.
        tycon: TyConId,
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
        let resolved = ctx.deep_resolve(&Type::Var(c.ty));
        dispatch_constraint(
            ctx,
            instance_env,
            class_table,
            env_snap_ty,
            scc_span,
            c,
            &resolved,
            &mut work,
            &mut visited,
            &mut retained,
            &mut dict_resolution,
        );
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
    c: Constraint,
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
            discharge_concrete(
                ctx,
                instance_env,
                class_table,
                scc_span,
                &c,
                tyconid,
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
                // resulting scheme.
                if !retained.iter().any(|r| r == &c) {
                    retained.push(c.clone());
                }
                // Record a Forward plan: callers of this function must
                // thread their own incoming dict parameter for this class.
                dict_resolution
                    .entry((c.class, c.ty))
                    .or_insert(DictPlan::Forward(c));
            }
        }

        // ── Other resolved types (Error, Alias, Fn, Tuple …) ─────────────────
        // Error: already in an error path — skip silently to avoid cascading.
        // Alias: should have been resolved by deep_resolve; treat as unknown.
        // Fn / Tuple: not valid class heads in single-param 0.2.13.
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

/// Attempt to discharge a concrete `(ClassId, TyConId)` constraint.
///
/// On success: record a [`DictPlan::Static`] entry and enqueue the instance's
/// superclass and `ctx_constraints` requirements. On failure: push T029.
#[allow(clippy::too_many_arguments)]
fn discharge_concrete(
    ctx: &mut InferCtx,
    instance_env: &InstanceEnv,
    class_table: &ClassTable,
    scc_span: Span,
    c: &Constraint,
    tyconid: TyConId,
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
            // Record the static resolution plan for the lowering pass.
            // Include the concrete TyConId so the lowering pass can look up
            // the type name without re-resolving the instance.
            dict_resolution
                .entry((c.class, c.ty))
                .or_insert_with(|| DictPlan::Static {
                    info: Box::new(inst_info.clone()),
                    tycon: tyconid,
                });

            // Enqueue superclass requirements for the same concrete type.
            // Termination: the class DAG is acyclic (T035 checked earlier).
            if let Some(class_info) = class_table.get(c.class) {
                for &superclass_id in &class_info.superclasses {
                    let super_key = (superclass_id, tyconid);
                    if !visited.contains(&super_key) {
                        // Use the same TyVid from the original constraint —
                        // the solver will deep_resolve it again.
                        work.push(Constraint {
                            class: superclass_id,
                            ty: c.ty,
                        });
                    }
                }
            }

            // Enqueue the instance's own ctx_constraints.
            // For 0.2.13 single-param non-generic instances these are always
            // empty; the hook exists for parametric instances in future cuts.
            for ctx_c in &inst_info.ctx_constraints {
                work.push(ctx_c.clone());
            }
        }
    }
}

/// Build a T029 fix hint for a given `(class, tyconid)` pair.
///
/// For the special case of `Eq Float` the hint includes the floating-point
/// footgun warning per the spec. All other cases get the generic suggestion.
fn build_fix_hint(class_name: &str, tyconid: TyConId) -> String {
    // TyConId(4) is the Float builtin (see ridge-types/src/builtins.rs).
    // We use the numeric id here rather than comparing a string name because
    // `class_name` is the class, not the type, and we have only the raw id
    // for the type at this point.
    let is_eq_float = class_name == "Eq" && tyconid == ridge_types::TyConId(4);

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
        BuiltinTyCons, Constraint, Scheme, TyConArena, TyConId, TyVid, Type, EQ_CLASS, ORD_CLASS,
        TOTEXT_CLASS,
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
        ctx.deferred_constraints.push(Constraint {
            class: TOTEXT_CLASS,
            ty: a,
        });

        let ct = make_class_table();
        let env = make_instance_env_with(TOTEXT_CLASS, int_tycon);
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, dict_res) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span());

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
        ctx.deferred_constraints.push(Constraint {
            class: TOTEXT_CLASS,
            ty: a,
        });

        let ct = make_class_table();
        let env = InstanceEnv::new(); // empty — no instances
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span());

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
        ctx.deferred_constraints.push(Constraint {
            class: TOTEXT_CLASS,
            ty: a,
        });

        let ct = make_class_table();
        let env = InstanceEnv::new();
        // env_snap does NOT contain `a` → case (b).
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, dict_res) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span());

        assert!(
            ctx.errors.is_empty(),
            "case (b) must produce no errors; got {:?}",
            ctx.errors
        );
        assert_eq!(retained.len(), 1, "case (b): must retain the constraint");
        assert_eq!(
            retained[0],
            Constraint {
                class: TOTEXT_CLASS,
                ty: a
            }
        );
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

        ctx.deferred_constraints.push(Constraint {
            class: TOTEXT_CLASS,
            ty: a,
        });

        let ct = make_class_table();
        let env = InstanceEnv::new();
        // env_snap CONTAINS `a` → case (c): ambiguous.
        let mut env_snap: FxHashSet<TyVid> = FxHashSet::default();
        env_snap.insert(a);

        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span());

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

        let (retained, dict_res) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span());

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
        ctx.deferred_constraints.push(Constraint {
            class: ORD_CLASS,
            ty: a,
        });

        let ct = make_class_table();
        let mut env = InstanceEnv::new();
        // Register BOTH Ord Int and Eq Int so the superclass check passes.
        env.insert((ORD_CLASS, int_tycon), make_instance_info(), "Ord", "Int")
            .expect("Ord Int insert");
        env.insert((EQ_CLASS, int_tycon), make_instance_info(), "Eq", "Int")
            .expect("Eq Int insert");

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span());

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

        ctx.deferred_constraints.push(Constraint {
            class: ORD_CLASS,
            ty: a,
        });

        let ct = make_class_table();
        let mut env = InstanceEnv::new();
        // Only Ord Int, no Eq Int → T029 for Eq.
        env.insert((ORD_CLASS, int_tycon), make_instance_info(), "Ord", "Int")
            .expect("Ord Int insert");

        let env_snap: FxHashSet<TyVid> = FxHashSet::default();
        let (_, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span());

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
            ty: Type::Var(a),
            constraints: vec![Constraint {
                class: TOTEXT_CLASS,
                ty: a,
            }],
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
            dc.ty, a,
            "constraint TyVid must be the fresh var, not the bound var TyVid(99)"
        );
        // The fresh var must match the TyVid allocated inside the ctx (TyVid(1)
        // because TyVid(0) was pre-consumed above).
        assert_eq!(
            dc.ty.0, 1,
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
        ctx.deferred_constraints.push(Constraint {
            class: TOTEXT_CLASS,
            ty: a,
        });
        ctx.deferred_constraints.push(Constraint {
            class: TOTEXT_CLASS,
            ty: b,
        });

        let ct = make_class_table();
        let env = InstanceEnv::new();
        // Neither `a` nor `b` in env_snap → case (b) for both.
        let env_snap: FxHashSet<TyVid> = FxHashSet::default();

        let (retained, _) = solve_constraints(&mut ctx, &env, &ct, &env_snap, dummy_span());

        assert!(ctx.errors.is_empty());
        assert_eq!(retained.len(), 2, "both constraints must be retained");
        assert!(
            retained.iter().any(|c| c.class == TOTEXT_CLASS),
            "retained must include ToText constraint"
        );
    }
}
