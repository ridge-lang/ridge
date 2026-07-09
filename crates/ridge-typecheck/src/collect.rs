//! Workspace-level class and instance collection with coherence checking.
//!
//! [`collect_workspace`] is the entry point. It walks every module's AST for
//! [`ridge_ast::Item::ClassDecl`] and [`ridge_ast::Item::InstanceDecl`] items,
//! populates the [`ClassTable`] and [`InstanceEnv`], then runs the four
//! coherence checks:
//!
//! 1. **T031 Orphan rule** — an instance must be in the class's module or the
//!    type's module.
//! 2. **T032 Overlapping instances** — detected during [`InstanceEnv::insert`]
//!    (duplicate `(ClassId, TyConId)` key).
//! 3. **T033 Missing superclass instance** — `instance Ord T` requires
//!    `instance Eq T`.
//! 4. **T035 Superclass cycle** — the class hierarchy must be acyclic.
//!
//! 5. **T034 `ToText` conflict** — an auto-promoted `pub fn toText` instance
//!    conflicts with an explicit `instance ToText T` for the same type.
//!
//! Auto-promotion runs in Step 3b: every `pub fn toText (x: T) -> Text`
//! declaration is synthesized into an `instance ToText T` with
//! [`InstanceOrigin::AutoPromoted`] before explicit instances are collected in
//! Step 4. A subsequent explicit `instance ToText T` then fires T034
//! automatically via [`InstanceEnv::insert`]'s duplicate-key routing.

use std::collections::HashMap;

use ridge_ast::{self, Item, Module};
use ridge_types::{Constraint, TyConId, TyVid};
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

use crate::class_env::{
    register_prelude_classes, register_prelude_instances_gated, register_stdlib_classes,
    register_stdlib_instances, ClassInfo, ClassTable, FunDepIdx, InstanceEnv, InstanceHead,
    InstanceInfo, InstanceOrigin, MethodSig,
};
use crate::derive::derive_instances;
use crate::error::TypeError;

// ── Public entry point ────────────────────────────────────────────────────────

/// Result of the workspace collect pass.
pub struct CollectResult {
    /// Populated class registry (includes prelude classes).
    pub class_table: ClassTable,
    /// Populated instance registry (all modules, all classes).
    pub instance_env: InstanceEnv,
    /// Coherence diagnostics (T031–T035 + T034 from [`InstanceEnv::insert`]).
    pub errors: Vec<TypeError>,
    /// Derived instances generated from `deriving` clauses.
    ///
    /// Each entry maps `(module_id, type_name)` → list of derived instances,
    /// stored so the lowering pass can emit the method fns and dict values for
    /// each derived class. Instances that failed coherence (T032) are absent.
    pub derived_instances: Vec<crate::derive::DerivedInstance>,
    /// Structurally-synthesised `Row` instances for records that did not write
    /// `deriving (Row)`, keyed by the record's `TyConId`.
    ///
    /// Registered in `instance_env` (so the solver discharges `Row` for them)
    /// but kept OUT of `derived_instances` until a module actually demands one.
    /// The workspace driver moves the demanded entries across, so a record that
    /// never touches the row machinery emits no codec.
    pub implicit_row_instances: FxHashMap<TyConId, crate::derive::DerivedInstance>,
}

/// Runs the collect + coherence pass over every module in a workspace.
///
/// `modules` is an ordered slice of `(module_id, ast)` pairs. The module id
/// is the raw `u32` from [`ridge_resolve::ModuleId`] and is used for the
/// orphan-rule check.
///
/// `user_tycon_names` is a name → [`TyConId`] map pre-collected from the
/// workspace's `TyCon` arena. It is used to resolve user-defined type names in
/// instance heads (e.g. `instance ToText Color` → `TyConId` for `Color`).
/// Pass an empty map if the arena has not been populated yet; user-type
/// instances will be silently skipped in that case.
///
/// The function always returns a fully-populated result, even when coherence
/// errors are found. Callers append `result.errors` to the global error list
/// and continue typechecking; the registry is usable even in the presence of
/// errors (the conflicting instance simply was not inserted).
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashMap is the canonical hasher for this crate; matches the pattern in solve.rs and ctx.rs"
)]
pub fn collect_workspace(
    modules: &[(u32, &Module)],
    user_tycon_names: &FxHashMap<String, TyConId>,
) -> CollectResult {
    collect_workspace_gated(modules, user_tycon_names, false)
}

/// Like [`collect_workspace`] but gated on whether this run is the stdlib's own
/// self-compile.
///
/// When `is_stdlib` is true, prelude instances the stdlib declares from source
/// are not seeded here (see [`register_prelude_instances_gated`]); every user
/// build passes `false`.
#[must_use]
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashMap is the canonical hasher for this crate; matches the pattern in solve.rs and ctx.rs"
)]
pub fn collect_workspace_gated(
    modules: &[(u32, &Module)],
    user_tycon_names: &FxHashMap<String, TyConId>,
    is_stdlib: bool,
) -> CollectResult {
    let mut class_table = ClassTable::new();
    let mut instance_env = InstanceEnv::new();
    let mut errors: Vec<TypeError> = Vec::new();

    // Step 1: Seed the ClassTable with built-in prelude classes.
    register_prelude_classes(&mut class_table);
    // Step 1a: Register stdlib-defined typeclasses (e.g. SqlType from std.sql).
    // Must follow prelude classes so their dynamically-assigned ClassIds are
    // contiguous and precede any user-declared classes.
    register_stdlib_classes(&mut class_table);

    // Step 1b: Seed the InstanceEnv with built-in prelude instances (ToText,
    // Eq, and Ord for the primitive types). These live in the prelude module
    // (`def_module = None`) and are not subject to the orphan check below. The
    // stdlib's own build declares the base `Encode` instances from source, so
    // those are skipped here when `is_stdlib` (avoids a spurious T032).
    register_prelude_instances_gated(&mut instance_env, is_stdlib);

    // Step 2: Walk all ClassDecl items, registering user-defined classes.
    // The two-pass approach in collect_class_decls ensures forward references
    // in superclass lists resolve correctly.
    collect_class_decls(modules, &mut class_table, &mut errors);

    // Step 3: Check for superclass cycles in the class graph (T035).
    // This must run before instance collection so that superclass DAG traversal
    // in T033 is guaranteed to terminate.
    check_superclass_cycles(&class_table, &mut errors);

    // Step 3b: Auto-promote every `pub fn toText (x: T) -> Text` to a
    // synthesized `instance ToText T`.  This happens BEFORE explicit instance
    // collection (Step 4) so that a subsequent explicit `instance ToText T`
    // for the same type correctly fires T034 (instead of T032).
    for &(module_id, ast) in modules {
        collect_auto_promoted_to_text(
            ast,
            module_id,
            &class_table,
            user_tycon_names,
            &mut instance_env,
            &mut errors,
        );
    }

    // Step 4: Walk all InstanceDecl items, inserting into InstanceEnv.
    // InstanceEnv::insert already detects T032 / T034 on duplicate keys.
    for &(module_id, ast) in modules {
        collect_instance_decls(
            ast,
            module_id,
            &class_table,
            user_tycon_names,
            &mut instance_env,
            &mut errors,
        );
    }

    // Step 4c: Seed stdlib instances (SqlType for Int/Text/Bool/Float).
    // Runs after collect_instance_decls so that if sql.ridge itself is being
    // compiled (stdlib tier-5 build), its source-level declarations are already
    // registered and this call becomes a no-op for those keys. For user
    // workspaces, the source-level declarations are absent and all four entries
    // are inserted here so the constraint solver can discharge them.
    register_stdlib_instances(&mut instance_env, &class_table, user_tycon_names);

    // Step 4b: Synthesise instances for every `TypeDecl` that has a
    // `deriving (…)` clause. Derived instances are registered into the same
    // InstanceEnv as explicit ones; coherence (T032 overlap) is enforced
    // by the same InstanceEnv::insert path.
    let (derived_instances, implicit_row_instances) = collect_derived_instances(
        modules,
        user_tycon_names,
        &class_table,
        &mut instance_env,
        &mut errors,
    );

    // Step 5: Orphan-rule check (T031) for all collected instances.
    check_orphan_rule(&instance_env, &class_table, &mut errors);

    // Step 6: Missing superclass instance check (T033).
    check_missing_superclass_instances(&instance_env, &class_table, &mut errors);

    CollectResult {
        class_table,
        instance_env,
        errors,
        derived_instances,
        implicit_row_instances,
    }
}

// ── Class collection ─────────────────────────────────────────────────────────

fn collect_class_decls(
    modules: &[(u32, &Module)],
    ct: &mut ClassTable,
    errors: &mut Vec<TypeError>,
) {
    // Pass 1: intern every class name across all modules so that forward
    // references in superclass lists resolve correctly (e.g. `class A where B`
    // can see `B` even when `B` is declared after `A` in source order).
    for &(_, ast) in modules {
        for item in &ast.items {
            if let Item::ClassDecl(decl) = item {
                // `intern` is idempotent — safe to call for prelude names too.
                let _ = ct
                    .id_by_name(&decl.name.text)
                    .unwrap_or_else(|| ct.intern(&decl.name.text));
            }
        }
    }

    // Pass 2: fill in class details, now that all names are interned.
    for &(module_id, ast) in modules {
        for item in &ast.items {
            let Item::ClassDecl(decl) = item else {
                continue;
            };

            let name = &decl.name.text;
            let class_id = ct.id_by_name(name).unwrap_or_else(|| ct.intern(name));

            // All names are now interned so superclass lookups succeed even
            // for classes declared later in the source.
            let superclasses: Vec<ridge_types::ClassId> = decl
                .superclasses
                .iter()
                .filter_map(|sc| ct.id_by_name(&sc.class.text))
                .collect();

            // The class type variables (e.g. `a` in `class Describe a`, or
            // `a b` in `class Convert a b`) are needed by the env-seeding pass so
            // it can map occurrences of those names in param/ret types to the
            // fresh TyVids allocated per call site.
            let class_ty_vars: Vec<String> = decl.ty_vars.iter().map(|t| t.text.clone()).collect();
            // Bare class-method params default to the first class type variable
            // (the single-parameter convention; multi-parameter class methods
            // annotate their params).
            let bare_fallback = class_ty_vars.first().cloned().unwrap_or_default();

            let method_sigs: Vec<MethodSig> = decl
                .methods
                .iter()
                .map(|m| {
                    // Collect AST param types from annotated params. Bare params
                    // have no type annotation; use the class type variable as a
                    // fallback (the convention for single-param class methods).
                    let ast_param_types: Vec<ridge_ast::Type> = m
                        .params
                        .iter()
                        .map(|p| match p {
                            ridge_ast::decl::Param::Annotated { ty, .. }
                            | ridge_ast::decl::Param::PatternAnnotated { ty, .. } => ty.clone(),
                            ridge_ast::decl::Param::Bare(_) => ridge_ast::Type::Named {
                                name: ridge_ast::Ident {
                                    text: bare_fallback.clone(),
                                    span: m.span,
                                },
                                span: m.span,
                            },
                        })
                        .collect();
                    MethodSig {
                        name: m.name.text.clone(),
                        arity: m.params.len(),
                        ast_param_types,
                        ast_ret_type: Some(m.ret.clone()),
                        class_ty_vars: class_ty_vars.clone(),
                    }
                })
                .collect();

            ct.insert_with_id(
                class_id,
                ClassInfo {
                    name: name.clone(),
                    arity: decl.ty_vars.len(),
                    method_sigs,
                    superclasses,
                    def_module: Some(module_id),
                },
            );

            // Record functional dependencies as positions into the class's
            // type-parameter list. Each variable must be one of the class's own
            // parameters; a stray name is T045 and drops that dependency.
            if !decl.fundeps.is_empty() {
                let resolve_side = |side: &[ridge_ast::Ident],
                                    errors: &mut Vec<TypeError>|
                 -> Option<SmallVec<[usize; 2]>> {
                    let mut positions: SmallVec<[usize; 2]> = SmallVec::new();
                    let mut ok = true;
                    for v in side {
                        if let Some(i) = class_ty_vars.iter().position(|cv| cv == &v.text) {
                            positions.push(i);
                        } else {
                            errors.push(TypeError::UnknownFunDepVar {
                                class: name.clone(),
                                var: v.text.clone(),
                                span: v.span,
                            });
                            ok = false;
                        }
                    }
                    ok.then_some(positions)
                };

                let mut deps: Vec<FunDepIdx> = Vec::with_capacity(decl.fundeps.len());
                for fd in &decl.fundeps {
                    let from = resolve_side(&fd.from, errors);
                    let to = resolve_side(&fd.to, errors);
                    if let (Some(from), Some(to)) = (from, to) {
                        deps.push(FunDepIdx { from, to });
                    }
                }
                ct.set_fundeps(class_id, deps);
            }
        }
    }
}

// ── Instance collection ──────────────────────────────────────────────────────

fn collect_instance_decls(
    ast: &Module,
    module_id: u32,
    ct: &ClassTable,
    user_tycon_names: &FxHashMap<String, TyConId>,
    env: &mut InstanceEnv,
    errors: &mut Vec<TypeError>,
) {
    for item in &ast.items {
        let Item::InstanceDecl(decl) = item else {
            continue;
        };

        // Resolve the ClassId for the class name in this instance.
        let Some(class_id) = ct.id_by_name(&decl.class.text) else {
            // Unknown class — skip (the resolver or parser would have flagged
            // this already, or it will surface later as a NoInstance).
            continue;
        };

        // Extract one head TyConId per head atom. For a simple head (`Encode Int`)
        // this is a single named TyCon; for a parametric head (`Encode (List a)`)
        // it is the outer constructor (`List`), with the type argument (`a`)
        // recorded in `head_var_positions`; for a multi-parameter head
        // (`Convert Celsius Fahrenheit`) it is one TyCon per atom. User-defined
        // types resolve via the pre-collected name map.
        let mut head_tycons = InstanceHead::new();
        let mut head_ok = true;
        for atom in &decl.head {
            let Some(id) = extract_tycon_id(atom, user_tycon_names) else {
                head_ok = false;
                break;
            };
            head_tycons.push(id);
        }
        if !head_ok {
            continue; // Unsupported head form — ignored in this pass.
        }

        // Arity check: the head must supply exactly as many type atoms as the
        // class declares type parameters.
        let class_arity = ct.get(class_id).map_or(1, |ci| ci.arity);
        if head_tycons.len() != class_arity {
            errors.push(TypeError::InstanceArityMismatch {
                class: decl.class.text.clone(),
                expected: class_arity,
                found: head_tycons.len(),
                span: decl.span,
            });
            continue;
        }

        let methods: Vec<(String, String)> = decl
            .methods
            .iter()
            .map(|m| (m.name.text.clone(), String::new())) // placeholder symbol
            .collect();

        // Context constraints (parametric element dictionaries). The head's type
        // arguments are flattened across every atom, so a context variable is
        // found whether it sits in a single head (`Encode (List a)`) or in one
        // atom of a multi-parameter head (`Projectable (Query e a) … where
        // Adapter a`).
        let (ctx_constraints, head_var_positions) =
            build_ctx_constraints(&decl.constraints, &decl.head, ct, user_tycon_names);

        let info = InstanceInfo {
            def_module: Some(module_id),
            methods,
            ctx_constraints,
            head_var_positions,
            origin: InstanceOrigin::Explicit,
            span: decl.span,
        };

        let class_name = &decl.class.text;
        let type_name = decl
            .head
            .iter()
            .map(type_display)
            .collect::<Vec<_>>()
            .join(" ");

        // Functional-dependency coherence (T046): an existing instance of this
        // class that agrees on a dependency's determining positions but differs
        // on a determined one would let one determining type map to two
        // determined types — rejected. (An exact-head duplicate is T032, caught
        // by `insert_multi`, so only a *differing* determined position is T046.)
        for fd in ct.fundeps_of(class_id) {
            for ((c, existing_head), existing) in &env.instances {
                if *c != class_id {
                    continue;
                }
                let from_agrees = fd
                    .from
                    .iter()
                    .all(|&p| existing_head.get(p) == head_tycons.get(p));
                let to_differs = fd
                    .to
                    .iter()
                    .any(|&p| existing_head.get(p) != head_tycons.get(p));
                if from_agrees && to_differs {
                    let determining = fd
                        .from
                        .iter()
                        .filter_map(|&p| decl.head.get(p))
                        .map(type_display)
                        .collect::<Vec<_>>()
                        .join(" ");
                    errors.push(TypeError::ConflictingFunDep {
                        class: decl.class.text.clone(),
                        determining,
                        first_span: existing.span,
                        second_span: decl.span,
                    });
                }
            }
        }

        let head_for_record = head_tycons.clone();
        match env.insert_multi(class_id, head_tycons, info, class_name, &type_name) {
            Ok(()) => {
                // Retain the written head types so the solver can run
                // functional-dependency improvement against this instance. Only
                // classes that declare a fundep need it.
                if !ct.fundeps_of(class_id).is_empty() {
                    env.record_head_asts(class_id, head_for_record, decl.head.clone());
                }
            }
            Err(e) => errors.push(e.into_type_error()),
        }
    }
}

// ── Auto-promotion of pub fn toText ──────────────────────────────────────────

/// Auto-promotes every qualifying `pub fn toText (x: T) -> Text` declaration
/// to a synthesized `instance ToText T` with [`InstanceOrigin::AutoPromoted`].
///
/// A declaration qualifies when:
/// - Its name is exactly `toText` (case-sensitive).
/// - Its visibility is `Pub`.
/// - It has exactly one parameter whose type is a concrete named constructor.
/// - Its declared return type is `Text`.
///
/// The synthesized instance is inserted with the function's module as
/// `def_module`, satisfying the orphan rule (the function and type share the
/// same module by the naming convention). When an explicit `instance ToText T`
/// already exists for the same type, [`InstanceEnv::insert`] fires T034
/// automatically through the `InstanceOrigin` routing.
fn collect_auto_promoted_to_text(
    ast: &ridge_ast::Module,
    module_id: u32,
    ct: &ClassTable,
    user_tycon_names: &FxHashMap<String, TyConId>,
    env: &mut InstanceEnv,
    errors: &mut Vec<TypeError>,
) {
    use ridge_ast::{Item, Type as AstType, Visibility};

    // ToText must be a registered class; if absent there is nothing to promote.
    let Some(totext_id) = ct.id_by_name("ToText") else {
        return;
    };

    for item in &ast.items {
        let Item::Fn(decl) = item else { continue };

        // Must be public and named exactly "toText".
        if decl.vis != Visibility::Pub || decl.name.text != "toText" {
            continue;
        }

        // Must have exactly one parameter.
        if decl.params.len() != 1 {
            continue;
        }

        // The parameter must carry an explicit type annotation.
        let param_ty = match &decl.params[0] {
            ridge_ast::decl::Param::Annotated { ty, .. }
            | ridge_ast::decl::Param::PatternAnnotated { ty, .. } => ty,
            ridge_ast::decl::Param::Bare(_) => continue,
        };

        // The parameter type must be a concrete named constructor.
        let Some(tycon_id) = extract_tycon_id(param_ty, user_tycon_names) else {
            continue;
        };

        // Never synthesize a second instance for a type the prelude already
        // covers. Every builtin scalar with a `ToText` instance (Int, Float,
        // Bool, Text, Timestamp, Ordering, Decimal, Uuid) is seeded in
        // `register_prelude_instances`; auto-promotion targets user-defined
        // types only. Keying on the env — rather than a fixed id range — stays
        // correct as builtins are interned past the historical 0..16 block
        // (Decimal and Uuid sit at 51/52). Explicit and derived instances are
        // registered in later passes, so a hit here can only be a prelude seed.
        if env.get((totext_id, tycon_id)).is_some() {
            continue;
        }

        // The return type must be Text (either as a Primitive or Named type).
        let ret_is_text = decl.ret.as_ref().is_some_and(is_text_type);
        if !ret_is_text {
            continue;
        }

        // Build a synthesized instance with AutoPromoted origin so that a
        // subsequent explicit `instance ToText T` fires T034 via InstanceEnv::insert.
        let info = InstanceInfo {
            def_module: Some(module_id),
            methods: vec![("toText".to_string(), decl.name.text.clone())],
            ctx_constraints: vec![],
            head_var_positions: vec![],
            origin: InstanceOrigin::AutoPromoted,
            span: decl.span,
        };

        let type_name = match param_ty {
            AstType::Named { name, .. } => name.text.clone(),
            AstType::Primitive { name, .. } => format!("{name:?}"),
            _ => "<type>".to_string(),
        };

        match env.insert((totext_id, tycon_id), info, "ToText", &type_name) {
            Ok(()) => {}
            Err(e) => errors.push(e.into_type_error()),
        }
    }
}

// ── Derived instance collection ───────────────────────────────────────────────

/// Synthesises instances for every `TypeDecl` in each module that has a
/// non-empty `deriving` clause.
///
/// The predicted `TyConId` for each user type is looked up from
/// `user_tycon_names`. If a type name is not in the map (e.g. because the
/// pre-scan did not reach it), the `deriving` clause is silently skipped; the
/// type-checker will surface missing instances at call sites via T029.
///
/// Returns all successfully generated [`crate::derive::DerivedInstance`]s so
/// the lowering pass can emit the corresponding method fns and dict values.
fn collect_derived_instances(
    modules: &[(u32, &ridge_ast::Module)],
    user_tycon_names: &FxHashMap<String, TyConId>,
    class_table: &ClassTable,
    instance_env: &mut InstanceEnv,
    errors: &mut Vec<TypeError>,
) -> (
    Vec<crate::derive::DerivedInstance>,
    FxHashMap<TyConId, crate::derive::DerivedInstance>,
) {
    let mut all_derived: Vec<crate::derive::DerivedInstance> = Vec::new();
    // Implicit `Row` instances are stashed here, not in `all_derived`, so the
    // workspace driver emits only the ones a module demands.
    let mut implicit_rows: FxHashMap<TyConId, crate::derive::DerivedInstance> =
        FxHashMap::default();

    for &(module_id, ast) in modules {
        for item in &ast.items {
            let Item::Type(type_decl) = item else {
                continue;
            };
            if type_decl.deriving.is_empty() {
                continue;
            }

            // Look up the TyConId assigned to this type during the arena
            // pre-scan. If the name is absent, skip — the solver will catch
            // any missing instances at use sites.
            let Some(&tycon_id) = user_tycon_names.get(type_decl.name.text.as_str()) else {
                continue;
            };

            let (generated, derive_errors) = derive_instances(
                type_decl,
                tycon_id,
                module_id,
                class_table,
                instance_env,
                user_tycon_names,
            );
            all_derived.extend(generated);
            errors.extend(derive_errors);
        }
    }

    // Implicit structural `Row`: any record whose fields are all `SqlType`
    // primitives can become a row without `deriving (Row)`, so an in-memory
    // `List record` is queryable with no annotation. The instance is registered
    // in `instance_env` here (so the solver discharges it) but its dictionary IR
    // is stashed, not emitted — the workspace driver pulls in only the records a
    // module actually demands. Runs after the explicit pass, so a hand-written
    // or derived `Row` is already registered and wins (its key is skipped).
    for &(module_id, ast) in modules {
        for item in &ast.items {
            let Item::Type(type_decl) = item else {
                continue;
            };
            let Some(&tycon_id) = user_tycon_names.get(type_decl.name.text.as_str()) else {
                continue;
            };
            if let Some(inst) = crate::derive::synthesize_implicit_row(
                type_decl,
                tycon_id,
                module_id,
                class_table,
                instance_env,
            ) {
                implicit_rows.insert(inst.key.1, inst);
            }
        }
    }

    (all_derived, implicit_rows)
}

// ── Coherence checks ─────────────────────────────────────────────────────────

/// T031 — orphan rule.
///
/// An instance must be in the module that defines the class OR the module that
/// defines the type. Builtins/prelude have `def_module = None`; the orphan
/// check treats `None` as matching any module (prelude instances are always
/// valid in prelude) and as NOT matching any user module (a user module cannot
/// declare an instance for a builtin type unless it also defined the class).
///
/// Specifically: if BOTH `class.def_module` and `tycon.def_module_raw` are
/// `None` (prelude class + prelude type), only the prelude itself can write
/// the instance, and since prelude instances are injected directly through
/// `register_prelude_instances`, the orphan check is a no-op for them.
/// If the instance is in a user module, it's an orphan unless one of the two
/// home modules is `Some(module_id)` matching the instance module.
fn check_orphan_rule(env: &InstanceEnv, ct: &ClassTable, errors: &mut Vec<TypeError>) {
    for ((class_id, head), info) in &env.instances {
        let class_id = *class_id;
        let Some(inst_module) = info.def_module else {
            continue; // prelude-injected instance — always valid
        };

        let class_module = ct.get(class_id).and_then(|ci| ci.def_module);
        // For now, `tycon.def_module_raw` is encoded as the `TyConId.0` index;
        // we do not have direct access to the TyConArena here. Instead we use a
        // sentinel: builtin TyConIds (0..=16) have `def_module_raw = None`.
        // User TyConIds start at 17 and carry the module in a side-channel we
        // do not have here. For now we implement the check conservatively:
        // - If the class has a known def_module AND it matches the instance module
        //   → OK.
        // - If the tycon id is ≥ 17 (user-defined type), we trust that the
        //   instance is in the correct module (the full check arrives once the
        //   TyConArena is threaded through).
        // - Otherwise, if neither class module nor tycon is user-local → orphan.
        let in_class_module = class_module == Some(inst_module);
        // A head is user-local if any of its constructors is user-defined
        // (builtins have fixed low ids < 17).
        let any_user_local = head.iter().any(|t| t.0 >= 17);

        if in_class_module || any_user_local {
            continue; // valid
        }

        // Neither the class's home module nor the type's home module — orphan.
        let class_name = ct
            .get(class_id)
            .map_or_else(|| format!("#{}", class_id.0), |ci| ci.name.clone());
        let type_name = head
            .iter()
            .map(|t| format!("#{}", t.0))
            .collect::<Vec<_>>()
            .join(" ");
        errors.push(TypeError::OrphanInstance {
            class: class_name,
            ty: type_name,
            instance_module: format!("module#{inst_module}"),
            span: info.span,
        });
    }
}

/// T035 — superclass cycle detection via DFS on the class graph.
///
/// Runs before instance collection so that the superclass DAG is guaranteed
/// acyclic by the time T033's transitivity walk runs.
fn check_superclass_cycles(ct: &ClassTable, errors: &mut Vec<TypeError>) {
    // Build an adjacency list: ClassId → Vec<ClassId>.
    let edges: HashMap<ridge_types::ClassId, Vec<ridge_types::ClassId>> = ct
        .iter()
        .map(|(id, info)| (id, info.superclasses.clone()))
        .collect();

    let mut visited: FxHashSet<ridge_types::ClassId> = FxHashSet::default();
    let mut in_stack: FxHashSet<ridge_types::ClassId> = FxHashSet::default();

    for &start in edges.keys() {
        if visited.contains(&start) {
            continue;
        }
        let mut stack: Vec<ridge_types::ClassId> = Vec::new();
        if dfs_cycle(start, &edges, &mut visited, &mut in_stack, &mut stack) {
            // `stack` holds the cycle in DFS order. Find the class span.
            let cycle_names: Vec<String> = stack
                .iter()
                .map(|&id| {
                    ct.get(id)
                        .map_or_else(|| format!("#{}", id.0), |ci| ci.name.clone())
                })
                .collect();
            // Use a dummy span; class declarations do not yet carry source
            // spans in the ClassInfo (a later cut threads them through).
            let span = ridge_ast::Span::point(0);
            errors.push(TypeError::SuperclassCycle {
                cycle: cycle_names,
                span,
            });
            // Only report the first cycle found to avoid cascading errors.
            return;
        }
    }
}

/// DFS helper for cycle detection. Returns `true` if a cycle is detected.
/// Appends the cycle nodes to `path` when a cycle is found.
fn dfs_cycle(
    node: ridge_types::ClassId,
    edges: &HashMap<ridge_types::ClassId, Vec<ridge_types::ClassId>>,
    visited: &mut FxHashSet<ridge_types::ClassId>,
    in_stack: &mut FxHashSet<ridge_types::ClassId>,
    path: &mut Vec<ridge_types::ClassId>,
) -> bool {
    visited.insert(node);
    in_stack.insert(node);
    path.push(node);

    if let Some(neighbors) = edges.get(&node) {
        for &neighbor in neighbors {
            if !visited.contains(&neighbor) {
                if dfs_cycle(neighbor, edges, visited, in_stack, path) {
                    return true;
                }
            } else if in_stack.contains(&neighbor) {
                // Back edge — found a cycle.
                path.push(neighbor); // close the cycle for display
                return true;
            }
        }
    }

    in_stack.remove(&node);
    path.pop();
    false
}

/// T033 — missing superclass instance check.
///
/// For each collected instance `(class_id, tycon_id)`, walk the superclass DAG
/// of `class_id` and verify that every superclass has a corresponding instance
/// for the same `tycon_id`. The DAG is acyclic by this point (T035 checked
/// above).
fn check_missing_superclass_instances(
    env: &InstanceEnv,
    ct: &ClassTable,
    errors: &mut Vec<TypeError>,
) {
    // Pre-collect the set of registered (class, head) keys for O(1) lookup.
    let registered: FxHashSet<(ridge_types::ClassId, InstanceHead)> =
        env.instances.keys().cloned().collect();

    for ((class_id, head), info) in &env.instances {
        let class_id = *class_id;
        // Walk all superclasses transitively.
        let mut to_check: Vec<ridge_types::ClassId> = Vec::new();
        let mut seen: FxHashSet<ridge_types::ClassId> = FxHashSet::default();
        if let Some(class_info) = ct.get(class_id) {
            to_check.extend(class_info.superclasses.iter().copied());
        }

        while let Some(super_id) = to_check.pop() {
            if seen.contains(&super_id) {
                continue;
            }
            seen.insert(super_id);

            if !registered.contains(&(super_id, head.clone())) {
                // Missing superclass instance for the same head.
                let class_name = ct
                    .get(class_id)
                    .map_or_else(|| format!("#{}", class_id.0), |ci| ci.name.clone());
                let type_name = head
                    .iter()
                    .map(|t| format!("#{}", t.0))
                    .collect::<Vec<_>>()
                    .join(" ");
                let super_name = ct
                    .get(super_id)
                    .map_or_else(|| format!("#{}", super_id.0), |ci| ci.name.clone());
                errors.push(TypeError::MissingSuperclassInstance {
                    class: class_name,
                    ty: type_name,
                    superclass: super_name,
                    span: info.span,
                });
            }

            // Recurse into the superclass's own superclasses.
            if let Some(super_info) = ct.get(super_id) {
                to_check.extend(super_info.superclasses.iter().copied());
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extracts the head `TyConId` from an AST type in an instance head.
///
/// For a simple head (`Encode Int`) this is the named `TyCon`. For a parametric
/// head (`Encode (List a)`) this is the outer constructor (`List`); the type
/// argument (`a`) is handled separately by [`build_ctx_constraints`].
///
/// Returns `None` for forms we cannot resolve (e.g. bare type variables,
/// tuples, or other compound types not yet supported as instance heads).
/// Peel any `Type::Paren` wrappers so callers see the underlying type.
///
/// A parenthesised instance head such as `(List a)` parses to
/// `Type::Paren { inner: App { List, [a] } }`; the coherence key, the context
/// constraints, and the dictionary lowering all need the inner `App`.
fn peel_paren(ty: &ridge_ast::Type) -> &ridge_ast::Type {
    let mut cur = ty;
    while let ridge_ast::Type::Paren { inner, .. } = cur {
        cur = inner;
    }
    cur
}

/// The type-variable name of a head argument, or `None` when it is not a bare
/// variable (a concrete type, a nested application, …).
fn head_arg_name(ty: &ridge_ast::Type) -> Option<&str> {
    match peel_paren(ty) {
        ridge_ast::Type::Var { name, .. } => Some(name.text.as_str()),
        _ => None,
    }
}

/// Flatten every head atom's type arguments into one positional list of their
/// variable names (`None` for a non-variable argument). Each `App`/`List`
/// contributes its arguments, each `Fn` its parameters then its return — the
/// same order the solver uses when it flattens the resolved head types. A
/// single-atom head yields exactly that atom's arguments, so single-parameter
/// contexts keep the positions they had before multi-parameter contexts existed.
fn flatten_head_arg_names(head_atoms: &[ridge_ast::Type]) -> Vec<Option<&str>> {
    use ridge_ast::Type as AstType;
    let mut out = Vec::new();
    for atom in head_atoms {
        match peel_paren(atom) {
            AstType::App { args, .. } => out.extend(args.iter().map(head_arg_name)),
            AstType::List { elem, .. } => out.push(head_arg_name(elem)),
            AstType::Fn { fn_ty, .. } => {
                out.extend(fn_ty.params.iter().map(head_arg_name));
                out.push(head_arg_name(&fn_ty.ret));
            }
            _ => {}
        }
    }
    out
}

fn extract_tycon_id(
    ty: &ridge_ast::Type,
    user_tycon_names: &FxHashMap<String, TyConId>,
) -> Option<TyConId> {
    use ridge_ast::Type as AstType;
    match peel_paren(ty) {
        // `Named` covers both built-in and user-defined type constructors.
        // We first check the pre-collected user tycon names (which include
        // all user-declared types from the workspace-wide TyCon scan), then
        // fall back to the builtin table for prelude/primitive types.
        AstType::Named { name, .. } => user_tycon_names
            .get(name.text.as_str())
            .copied()
            .or_else(|| builtin_tycon_id_by_name(&name.text)),
        // `App` covers parametric heads like `List a` or `Map Text a`.
        // The env key uses the outer constructor (`List`, `Map`) only.
        AstType::App { head, .. } => user_tycon_names
            .get(head.text.as_str())
            .copied()
            .or_else(|| builtin_tycon_id_by_name(&head.text)),
        // `Primitive` covers built-in scalars like `Int`, `Float`, `Bool`.
        AstType::Primitive { name, .. } => {
            use ridge_ast::PrimitiveType;
            match name {
                PrimitiveType::Int => Some(TyConId(0)),
                PrimitiveType::Float => Some(TyConId(1)),
                PrimitiveType::Bool => Some(TyConId(2)),
                PrimitiveType::Text => Some(TyConId(3)),
                PrimitiveType::Unit => Some(TyConId(4)),
                PrimitiveType::Timestamp => Some(TyConId(5)),
                // Decimal, Uuid, Bytes and Date are interned last in the builtin
                // arena (ids 51, 52, 53, 54).
                PrimitiveType::Decimal => Some(TyConId(51)),
                PrimitiveType::Uuid => Some(TyConId(52)),
                PrimitiveType::Bytes => Some(TyConId(53)),
                PrimitiveType::Date => Some(TyConId(54)),
                PrimitiveType::Time => Some(TyConId(55)),
                #[allow(unreachable_patterns)]
                _ => None,
            }
        }
        // A function-type instance head (`instance Handler (fn a -> R)`) keys on
        // the synthetic per-arity `Fn/N` constructor. The capability row is NOT
        // part of the key — dispatch is arity-only. `fn_ty.params` holds the
        // parameter atoms; a curried `a -> b -> c` nests its tail in `ret`, so it
        // is arity 1. Arities beyond `FN_ARITY_COUNT` yield `None` (unsupported).
        AstType::Fn { fn_ty, .. } => ridge_types::fn_tycon_id(fn_ty.params.len()),
        _ => None,
    }
}

/// Maps a prelude type name to its fixed `TyConId` index (0-based, matches
/// `BuiltinTyCons::allocate` assignment order).
///
/// Only covers the 17 pre-allocated builtins; user types return `None` here
/// (they need the [`ridge_types::TyConArena`], threaded in the integration phase).
fn builtin_tycon_id_by_name(name: &str) -> Option<TyConId> {
    match name {
        "Int" => Some(TyConId(0)),
        "Float" => Some(TyConId(1)),
        "Bool" => Some(TyConId(2)),
        "Text" => Some(TyConId(3)),
        "Unit" => Some(TyConId(4)),
        "Timestamp" => Some(TyConId(5)),
        "List" => Some(TyConId(6)),
        "Map" => Some(TyConId(7)),
        "Set" => Some(TyConId(8)),
        "Option" => Some(TyConId(9)),
        "Result" => Some(TyConId(10)),
        "Handle" => Some(TyConId(11)),
        "Error" => Some(TyConId(12)),
        "Duration" => Some(TyConId(13)),
        "ProcOutput" => Some(TyConId(14)),
        "Ordering" => Some(TyConId(15)),
        "JsonValue" => Some(TyConId(16)),
        "QExpr" => Some(TyConId(25)),
        "Quote" => Some(TyConId(26)),
        // `Ret/1` — the return-type projection. Surfaced for stdlib query-builder
        // signatures (`Result (List (Ret p)) Error`); reduces during unification.
        // Interned right after the Fn/N block (see `ridge_types::RET_TYCON_ID`).
        "Ret" => Some(TyConId(ridge_types::RET_TYCON_ID)),
        // `Rows/1` — the row-shape projection for the decode terminals
        // (`Result (List (Rows q)) Error`); reduces during unification. Interned
        // right after `Ret/1` (see `ridge_types::ROWS_TYCON_ID`).
        "Rows" => Some(TyConId(ridge_types::ROWS_TYCON_ID)),
        // `JoinCond/2` — the join-condition shape projection for the N-ary
        // `joinOn` (`Quote (JoinCond q f)`); reduces during unification. Interned
        // right after `Rows/1` (see `ridge_types::JOINCOND_TYCON_ID`).
        "JoinCond" => Some(TyConId(ridge_types::JOINCOND_TYCON_ID)),
        // `JoinResult/2` — the result projection for the N-ary `joinOn`
        // (the method's return); reduces during unification. Interned right after
        // `JoinCond/2` (see `ridge_types::JOINRESULT_TYCON_ID`).
        "JoinResult" => Some(TyConId(ridge_types::JOINRESULT_TYCON_ID)),
        // `LeftJoinResult/2` — the result projection for the N-ary LEFT outer-join
        // verb (`leftJoinOn`'s return); reduces during unification. Interned right
        // after `JoinResult/2` (see `ridge_types::LEFTJOINRESULT_TYCON_ID`).
        "LeftJoinResult" => Some(TyConId(ridge_types::LEFTJOINRESULT_TYCON_ID)),
        // `RightJoinResult/2` — the result projection for the N-ary RIGHT outer-join
        // verb (`rightJoinOn`'s return); reduces during unification. Interned right
        // after `LeftJoinResult/2` (see `ridge_types::RIGHTJOINRESULT_TYCON_ID`).
        "RightJoinResult" => Some(TyConId(ridge_types::RIGHTJOINRESULT_TYCON_ID)),
        // `FullJoinResult/2` — the result projection for the N-ary FULL outer-join
        // verb (`fullJoinOn`'s return); reduces during unification. Interned right
        // after `RightJoinResult/2` (see `ridge_types::FULLJOINRESULT_TYCON_ID`).
        "FullJoinResult" => Some(TyConId(ridge_types::FULLJOINRESULT_TYCON_ID)),
        // `InsertShape/1` — the insert-input shape projection for the typed insert
        // verbs (`InsertShape e`, the entity minus its database-generated columns);
        // reduces during unification. Interned right after `FullJoinResult/2`
        // (see `ridge_types::INSERTSHAPE_TYCON_ID`).
        "InsertShape" => Some(TyConId(ridge_types::INSERTSHAPE_TYCON_ID)),
        _ => None,
    }
}

/// Translates an instance `where` clause into the `(ctx_constraints,
/// head_var_positions)` pair stored on [`InstanceInfo`].
///
/// For each [`ridge_ast::ClassConstraint`] in `where_constraints`, this
/// function:
///
/// 1. Resolves the constraint class name to a [`ClassId`].
/// 2. Locates the type variable named by `ty_var` in the head type's argument
///    list (only `Type::App` heads have positional args).
/// 3. Records the argument position in `head_var_positions` and stores a
///    sentinel [`Constraint`] (with [`TyVid(0)`] — never a live inference
///    variable) in `ctx_constraints`.
///
/// The [`TyVid`] stored is a **sentinel** and must not be used directly by
/// inference passes. The solver reads `head_var_positions[i]` to substitute
/// the concrete type from the resolved `Type::Con(_, args)` before enqueuing
/// the constraint.
///
/// Returns `(vec![], vec![])` when `where_constraints` is empty (non-parametric
/// instances). Silently skips constraints whose class is unknown or whose
/// type variable is not found among the head's positional args.
///
/// The positions index a single list formed by flattening every head atom's
/// type arguments in order (see [`flatten_head_arg_names`]); for a single-atom
/// head this is exactly that atom's args, unchanged from before multi-parameter
/// contexts existed. The solver flattens the resolved head types the same way.
fn build_ctx_constraints(
    where_constraints: &[ridge_ast::typeclass::ClassConstraint],
    head_atoms: &[ridge_ast::Type],
    ct: &ClassTable,
    _user_tycon_names: &FxHashMap<String, TyConId>,
) -> (Vec<Constraint>, Vec<usize>) {
    if where_constraints.is_empty() {
        return (vec![], vec![]);
    }

    let head_args = flatten_head_arg_names(head_atoms);

    let mut ctx_constraints = Vec::new();
    let mut head_var_positions = Vec::new();

    for wc in where_constraints {
        // Resolve the class name — skip unknown classes (flagged elsewhere).
        let Some(class_id) = ct.id_by_name(&wc.class.text) else {
            continue;
        };

        // Find the arg position that carries this type variable. A parametric
        // element constraint binds a single head variable (`where Encode a`); use
        // the first listed variable.
        let Some(var_name) = wc.ty_vars.first().map(|t| t.text.as_str()) else {
            continue;
        };
        let Some(pos) = head_args.iter().position(|&v| v == Some(var_name)) else {
            // The variable is not in the head args — malformed instance; skip.
            continue;
        };

        // Store a sentinel constraint. The TyVid(0) is never used directly
        // by the solver; it reads head_var_positions to find the correct
        // concrete type at solve time.
        ctx_constraints.push(Constraint::single(class_id, TyVid(0)));
        head_var_positions.push(pos);
    }

    (ctx_constraints, head_var_positions)
}

/// Returns `true` when the AST type represents `Text`.
///
/// Accepts both the `Primitive` variant (how the parser represents `Text`)
/// and a `Named` variant with the text `"Text"` (defensive fallback).
fn is_text_type(ty: &ridge_ast::Type) -> bool {
    use ridge_ast::Type as AstType;
    match ty {
        AstType::Primitive {
            name: ridge_ast::PrimitiveType::Text,
            ..
        } => true,
        AstType::Named { name, .. } => name.text == "Text",
        _ => false,
    }
}

/// Returns a display-friendly string for an AST type (for error messages).
fn type_display(ty: &ridge_ast::Type) -> String {
    use ridge_ast::Type as AstType;
    match peel_paren(ty) {
        AstType::Named { name, .. } => name.text.clone(),
        AstType::Primitive { name, .. } => format!("{name:?}"),
        AstType::App { head, .. } => head.text.clone(),
        _ => "<type>".to_string(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{
        typeclass::{
            ClassConstraint, ClassDecl, InstanceDecl, MethodDef, MethodSig as AstMethodSig,
        },
        Ident, Item, Module, Span, Type as AstType,
    };
    use ridge_types::{TyConId, EQ_CLASS, ORD_CLASS, TOTEXT_CLASS};

    fn ds() -> Span {
        Span::point(0)
    }

    fn ident(s: &str) -> Ident {
        Ident {
            text: s.to_string(),
            span: ds(),
        }
    }

    fn named_type(name: &str) -> AstType {
        AstType::Named {
            name: ident(name),
            span: ds(),
        }
    }

    fn module_with_items(items: Vec<Item>) -> Module {
        Module {
            items,
            doc: vec![],
            span: ds(),
        }
    }

    fn class_decl_item(name: &str, superclasses: Vec<(String, String)>, method: &str) -> Item {
        let superclasses = superclasses
            .into_iter()
            .map(|(class, var)| ClassConstraint {
                class: ident(&class),
                ty_vars: vec![ident(&var)],
                span: ds(),
            })
            .collect();

        Item::ClassDecl(ClassDecl {
            name: ident(name),
            ty_vars: vec![ident("a")],
            fundeps: vec![],
            superclasses,
            methods: vec![AstMethodSig {
                name: ident(method),
                params: vec![],
                ret: named_type("Text"),
                span: ds(),
            }],
            span: ds(),
            doc: None,
        })
    }

    fn instance_decl_item(class: &str, ty: &str) -> Item {
        use ridge_ast::decl::Param;
        use ridge_ast::Expr;
        use ridge_ast::Literal;

        Item::InstanceDecl(InstanceDecl {
            class: ident(class),
            head: vec![named_type(ty)],
            constraints: vec![],
            methods: vec![MethodDef {
                name: ident("toText"),
                params: vec![Param::Bare(ident("x"))],
                ret: named_type("Text"),
                body: Expr::Literal(Literal::Text {
                    raw: r#""x""#.to_string(),
                    span: ds(),
                }),
                span: ds(),
            }],
            span: ds(),
            doc: None,
        })
    }

    // ── Basic class + instance collection ────────────────────────────────────

    #[test]
    fn collect_basic_class_and_instance() {
        // Use a user-defined class name so collect does not trigger T031
        // (orphan rule). The class is defined in module 0; the instance's type
        // name "Widget" does not match any builtin, so it resolves to None
        // in extract_tycon_id and is silently skipped — the test verifies that
        // the class IS registered without errors and the instance pass runs.
        let m = module_with_items(vec![
            class_decl_item("MyClass", vec![], "myMethod"),
            class_decl_item("OtherClass", vec![], "otherMethod"),
        ]);
        let result = collect_workspace(&[(0, &m)], &rustc_hash::FxHashMap::default());

        assert!(
            result.errors.is_empty(),
            "expected no errors for a basic class collection, got: {:?}",
            result.errors
        );
        assert!(
            result.class_table.id_by_name("MyClass").is_some(),
            "MyClass must be in ClassTable"
        );
        assert!(
            result.class_table.id_by_name("OtherClass").is_some(),
            "OtherClass must be in ClassTable"
        );
    }

    // ── Prelude classes pre-registered ────────────────────────────────────────

    #[test]
    fn prelude_classes_in_class_table() {
        let result = collect_workspace(&[], &rustc_hash::FxHashMap::default());
        let ct = &result.class_table;
        assert_eq!(ct.id_by_name("ToText"), Some(TOTEXT_CLASS));
        assert_eq!(ct.id_by_name("Eq"), Some(EQ_CLASS));
        assert_eq!(ct.id_by_name("Ord"), Some(ORD_CLASS));
        // Ord's superclass must be Eq.
        let ord = ct.get(ORD_CLASS).expect("Ord must be in ClassTable");
        assert_eq!(ord.superclasses, vec![EQ_CLASS]);
    }

    // ── T032 — duplicate explicit instance ───────────────────────────────────

    #[test]
    fn coherence_duplicate_instance_t032() {
        // Two explicit `instance ToText Int` declarations.
        let m = module_with_items(vec![
            instance_decl_item("ToText", "Int"),
            instance_decl_item("ToText", "Int"),
        ]);
        let result = collect_workspace(&[(0, &m)], &rustc_hash::FxHashMap::default());
        let has_t032 = result.errors.iter().any(|e| e.code() == "T032");
        assert!(
            has_t032,
            "two identical explicit instances must produce T032; got {:?}",
            result.errors
        );
    }

    // ── T031 — orphan instance (user class + builtin type, wrong module) ──────

    #[test]
    fn coherence_orphan_instance_t031() {
        // Define a class in module 0 and declare an instance for a builtin type
        // (Int, TyConId 0) in module 1. The class's home module is 0; the
        // builtin type has no user home module. Module 1 is an orphan.
        let mod0 = module_with_items(vec![class_decl_item("MyShow", vec![], "myShow")]);
        let mod1 = module_with_items(vec![instance_decl_item("MyShow", "Int")]);

        let result =
            collect_workspace(&[(0, &mod0), (1, &mod1)], &rustc_hash::FxHashMap::default());
        let has_t031 = result.errors.iter().any(|e| e.code() == "T031");
        assert!(
            has_t031,
            "instance for builtin type in wrong module must produce T031; got {:?}",
            result.errors
        );
    }

    // ── T035 — superclass cycle ───────────────────────────────────────────────

    #[test]
    fn coherence_superclass_cycle_t035() {
        // class A where B; class B where A — cycle
        let m = module_with_items(vec![
            class_decl_item(
                "ClassA",
                vec![("ClassB".to_string(), "a".to_string())],
                "methodA",
            ),
            class_decl_item(
                "ClassB",
                vec![("ClassA".to_string(), "a".to_string())],
                "methodB",
            ),
        ]);
        let result = collect_workspace(&[(0, &m)], &rustc_hash::FxHashMap::default());
        let has_t035 = result.errors.iter().any(|e| e.code() == "T035");
        assert!(
            has_t035,
            "cyclic class hierarchy must produce T035; got {:?}",
            result.errors
        );
    }

    // ── T033 — missing superclass instance ───────────────────────────────────

    #[test]
    fn coherence_missing_superclass_t033() {
        // instance Ord Widget without instance Eq Widget.
        // Widget is a user-defined type with no prelude instances, so the
        // prelude-injected Eq Int / Ord Int entries do not interfere.
        let widget_id = TyConId(100); // well above the 16 prelude TyConIds
        let mut user_types = rustc_hash::FxHashMap::default();
        user_types.insert("Widget".to_string(), widget_id);

        let m = module_with_items(vec![instance_decl_item("Ord", "Widget")]);
        let result = collect_workspace(&[(0, &m)], &user_types);
        let has_t033 = result.errors.iter().any(|e| e.code() == "T033");
        assert!(
            has_t033,
            "Ord without Eq must produce T033; got {:?}",
            result.errors
        );
    }

    // ── T033 passes when superclass instance exists ───────────────────────────

    #[test]
    fn coherence_superclass_present_no_t033() {
        // instance Eq Widget + instance Ord Widget → OK (Eq is Ord's superclass).
        // Using a user type to avoid conflict with prelude-injected Int instances.
        let widget_id = TyConId(101);
        let mut user_types = rustc_hash::FxHashMap::default();
        user_types.insert("Widget".to_string(), widget_id);

        let m = module_with_items(vec![
            instance_decl_item("Eq", "Widget"),
            instance_decl_item("Ord", "Widget"),
        ]);
        let result = collect_workspace(&[(0, &m)], &user_types);
        let has_t033 = result.errors.iter().any(|e| e.code() == "T033");
        assert!(
            !has_t033,
            "Ord with Eq present must not produce T033; got {:?}",
            result.errors
        );
    }

    // ── InstanceOrigin flag routes T034 vs T032 ───────────────────────────────

    #[test]
    fn instance_origin_auto_vs_explicit_t034() {
        use crate::class_env::{InstanceEnv, InstanceInfo, InstanceOrigin};

        let mut env = InstanceEnv::new();
        let key = (TOTEXT_CLASS, TyConId(3));

        let auto_info = InstanceInfo {
            def_module: Some(0),
            methods: vec![],
            ctx_constraints: vec![],
            head_var_positions: vec![],
            origin: InstanceOrigin::AutoPromoted,
            span: ds(),
        };
        let explicit_info = InstanceInfo {
            def_module: Some(0),
            methods: vec![],
            ctx_constraints: vec![],
            head_var_positions: vec![],
            origin: InstanceOrigin::Explicit,
            span: ds(),
        };

        env.insert(key, auto_info, "ToText", "Widget")
            .expect("first insert ok");
        let err = env
            .insert(key, explicit_info, "ToText", "Widget")
            .expect_err("second insert must fail");
        assert!(
            matches!(err, crate::class_env::CoherenceError::ToTextConflict { .. }),
            "auto+explicit must produce ToTextConflict (T034), got {err:?}"
        );
    }

    #[test]
    fn instance_origin_two_explicit_t032() {
        use crate::class_env::{InstanceEnv, InstanceInfo, InstanceOrigin};

        let mut env = InstanceEnv::new();
        let key = (EQ_CLASS, TyConId(0));

        let mk = |origin| InstanceInfo {
            def_module: Some(0),
            methods: vec![],
            ctx_constraints: vec![],
            head_var_positions: vec![],
            origin,
            span: ds(),
        };

        env.insert(key, mk(InstanceOrigin::Explicit), "Eq", "Int")
            .expect("first insert ok");
        let err = env
            .insert(key, mk(InstanceOrigin::Explicit), "Eq", "Int")
            .expect_err("second insert must fail");
        assert!(
            matches!(
                err,
                crate::class_env::CoherenceError::OverlappingInstance { .. }
            ),
            "two explicit instances must produce OverlappingInstance (T032), got {err:?}"
        );
    }

    // ── Auto-promotion of pub fn toText ───────────────────────────────────────

    /// Build a minimal `pub fn toText (x: UserType) -> Text` `FnDecl` item.
    ///
    /// Uses `TyConId(100)` as the user type id, which is above the 16-entry
    /// builtin range so auto-promotion is not filtered out.
    fn pub_fn_to_text_item(param_type_name: &str) -> Item {
        use ridge_ast::{
            decl::{Body, FnDecl, Param},
            Expr, Literal, Visibility,
        };

        Item::Fn(FnDecl {
            attrs: vec![],
            vis: Visibility::Pub,
            caps: vec![],
            name: ident("toText"),
            params: vec![Param::Annotated {
                name: ident("x"),
                ty: named_type(param_type_name),
                span: ds(),
            }],
            ret: Some(ridge_ast::Type::Primitive {
                name: ridge_ast::PrimitiveType::Text,
                span: ds(),
            }),
            constraints: vec![],
            body: Body::Expr(Expr::Literal(Literal::Text {
                raw: r#""placeholder""#.to_string(),
                span: ds(),
            })),
            span: ds(),
            doc: None,
        })
    }

    /// A `pub fn toText` for a user type registers an auto-promoted `ToText`
    /// instance in the environment.
    #[test]
    fn auto_promote_pub_fn_to_text_registers_instance() {
        use crate::class_env::InstanceOrigin;

        let user_id = TyConId(100);
        let mut user_types = rustc_hash::FxHashMap::default();
        user_types.insert("Widget".to_string(), user_id);

        let m = module_with_items(vec![pub_fn_to_text_item("Widget")]);
        let result = collect_workspace(&[(0, &m)], &user_types);

        assert!(
            result.errors.is_empty(),
            "auto-promotion must not produce errors; got: {:?}",
            result.errors
        );
        let inst = result.instance_env.get((TOTEXT_CLASS, user_id));
        assert!(
            inst.is_some(),
            "expected ToText Widget instance in registry after auto-promotion"
        );
        assert_eq!(
            inst.unwrap().origin,
            InstanceOrigin::AutoPromoted,
            "auto-promoted instance must have AutoPromoted origin"
        );
    }

    /// A `pub fn toText` followed by an explicit `instance ToText T` for the
    /// same type fires T034.
    #[test]
    fn auto_promote_then_explicit_instance_fires_t034() {
        let user_id = TyConId(101);
        let mut user_types = rustc_hash::FxHashMap::default();
        user_types.insert("Color".to_string(), user_id);

        let m = module_with_items(vec![
            pub_fn_to_text_item("Color"),
            instance_decl_item("ToText", "Color"),
        ]);
        let result = collect_workspace(&[(0, &m)], &user_types);

        let has_t034 = result.errors.iter().any(|e| e.code() == "T034");
        assert!(
            has_t034,
            "pub fn toText + explicit instance ToText T must produce T034; got {:?}",
            result.errors
        );
    }

    // ── Parametric instance collect: build_ctx_constraints unit tests ────────

    /// Helper to build a registered `ClassTable` with the prelude classes.
    fn prelude_class_table() -> ClassTable {
        let mut ct = ClassTable::new();
        register_prelude_classes(&mut ct);
        ct
    }

    /// Build a parametric `App` type head, e.g. `List a`.
    fn app_type(head: &str, var: &str) -> AstType {
        AstType::App {
            head: ident(head),
            args: vec![AstType::Var {
                name: ident(var),
                span: ds(),
            }],
            span: ds(),
        }
    }

    /// Build a 2-arg `App` type where the first arg is a named type (e.g. `Text`)
    /// and the second is a type variable, e.g. `Map Text a`.
    fn app2_named_var_type(head: &str, arg0_name: &str, arg1_var: &str) -> AstType {
        AstType::App {
            head: ident(head),
            args: vec![
                AstType::Named {
                    name: ident(arg0_name),
                    span: ds(),
                },
                AstType::Var {
                    name: ident(arg1_var),
                    span: ds(),
                },
            ],
            span: ds(),
        }
    }

    /// Build a two-var `App` type, e.g. `Result a e`.
    fn app2_vars_type(head: &str, var0: &str, var1: &str) -> AstType {
        AstType::App {
            head: ident(head),
            args: vec![
                AstType::Var {
                    name: ident(var0),
                    span: ds(),
                },
                AstType::Var {
                    name: ident(var1),
                    span: ds(),
                },
            ],
            span: ds(),
        }
    }

    fn make_class_constraint(class: &str, var: &str) -> ClassConstraint {
        ClassConstraint {
            class: ident(class),
            ty_vars: vec![ident(var)],
            span: ds(),
        }
    }

    /// `build_ctx_constraints` for `Encode (List a) where Encode a` returns one
    /// constraint at position 0.
    #[test]
    fn ctx_constraints_list_a_pos0() {
        use ridge_types::ENCODE_CLASS;

        let ct = prelude_class_table();
        let head = app_type("List", "a");
        let wcs = vec![make_class_constraint("Encode", "a")];

        let (constraints, positions) = build_ctx_constraints(
            &wcs,
            std::slice::from_ref(&head),
            &ct,
            &FxHashMap::default(),
        );

        assert_eq!(constraints.len(), 1, "one ctx_constraint for Encode a");
        assert_eq!(constraints[0].class, ENCODE_CLASS);
        assert_eq!(positions, vec![0], "a is at arg position 0 in List a");
    }

    /// `Map Text a` — the constrained var `a` is at position 1 (after `Text`).
    #[test]
    fn ctx_constraints_map_text_a_pos1() {
        let ct = prelude_class_table();
        let head = app2_named_var_type("Map", "Text", "a");
        let wcs = vec![make_class_constraint("Encode", "a")];

        let (_, positions) = build_ctx_constraints(
            &wcs,
            std::slice::from_ref(&head),
            &ct,
            &FxHashMap::default(),
        );

        assert_eq!(positions, vec![1], "a is at arg position 1 in Map Text a");
    }

    /// `Result a e` with two constraints — positions are 0 and 1 respectively.
    #[test]
    fn ctx_constraints_result_a_e_two_positions() {
        use ridge_types::ENCODE_CLASS;

        let ct = prelude_class_table();
        let head = app2_vars_type("Result", "a", "e");
        let wcs = vec![
            make_class_constraint("Encode", "a"),
            make_class_constraint("Encode", "e"),
        ];

        let (constraints, positions) = build_ctx_constraints(
            &wcs,
            std::slice::from_ref(&head),
            &ct,
            &FxHashMap::default(),
        );

        assert_eq!(constraints.len(), 2, "two constraints for Result a e");
        assert_eq!(
            constraints.iter().map(|c| c.class).collect::<Vec<_>>(),
            vec![ENCODE_CLASS, ENCODE_CLASS]
        );
        assert_eq!(positions, vec![0, 1], "a at 0, e at 1");
    }

    /// A multi-atom head flattens its atoms' arguments in order: a context var in
    /// the first atom keeps its position, one in a later atom lands at its
    /// flattened position. Mirrors `Projectable (Query e a) (Tag s) where
    /// Encode a, Encode s` (two atoms → flattened arg names `[e, a, s]`).
    #[test]
    fn ctx_constraints_multi_atom_flattened_positions() {
        use ridge_types::ENCODE_CLASS;
        let ct = prelude_class_table();
        let head = vec![app2_vars_type("Query", "e", "a"), app_type("Tag", "s")];
        let wcs = vec![
            make_class_constraint("Encode", "a"),
            make_class_constraint("Encode", "s"),
        ];

        let (constraints, positions) =
            build_ctx_constraints(&wcs, &head, &ct, &FxHashMap::default());

        assert_eq!(constraints.len(), 2);
        assert_eq!(
            constraints.iter().map(|c| c.class).collect::<Vec<_>>(),
            vec![ENCODE_CLASS, ENCODE_CLASS]
        );
        assert_eq!(
            positions,
            vec![1, 2],
            "a at flat pos 1 (in Query e a), s at flat pos 2 (in Tag s)"
        );
    }

    /// A plain named head (non-parametric) produces empty constraint/position lists.
    #[test]
    fn ctx_constraints_named_head_empty() {
        let ct = prelude_class_table();
        let head = named_type("Int");

        let (constraints, positions) =
            build_ctx_constraints(&[], std::slice::from_ref(&head), &ct, &FxHashMap::default());

        assert!(constraints.is_empty());
        assert!(positions.is_empty());
    }

    /// A builtin type the prelude already covers must NOT be auto-promoted.
    #[test]
    fn auto_promote_skips_builtin_types() {
        // `Int` (TyConId 0) carries a prelude `ToText` instance.
        let m = module_with_items(vec![pub_fn_to_text_item("Int")]);
        let result = collect_workspace(&[(0, &m)], &rustc_hash::FxHashMap::default());

        // No T034: the prelude already covers `ToText Int`, so auto-promotion
        // skips it rather than inserting a duplicate.
        let has_t034 = result.errors.iter().any(|e| e.code() == "T034");
        assert!(
            !has_t034,
            "pub fn toText for a builtin type must NOT fire T034; got {:?}",
            result.errors
        );
    }

    /// `Decimal` is a builtin scalar the prelude covers, so its stdlib
    /// `pub fn toText (d: Decimal)` — the exact shape `decimal.ridge` declares —
    /// must not be auto-promoted into a second instance. The rich scalars are
    /// interned at 51/52, outside the historical 0..16 builtin block, so the old
    /// id-range skip let auto-promotion fire and collide with the seed (T034).
    /// Keying the skip on the env keeps `Decimal` and `Uuid` in line with `Int`.
    #[test]
    fn auto_promote_skips_prelude_seeded_decimal() {
        use crate::class_env::InstanceOrigin;
        use ridge_ast::{
            decl::{Body, FnDecl, Param},
            Expr, Literal, PrimitiveType, Visibility,
        };

        let decimal_to_text = Item::Fn(FnDecl {
            attrs: vec![],
            vis: Visibility::Pub,
            caps: vec![],
            name: ident("toText"),
            params: vec![Param::Annotated {
                name: ident("d"),
                ty: AstType::Primitive {
                    name: PrimitiveType::Decimal,
                    span: ds(),
                },
                span: ds(),
            }],
            ret: Some(AstType::Primitive {
                name: PrimitiveType::Text,
                span: ds(),
            }),
            constraints: vec![],
            body: Body::Expr(Expr::Literal(Literal::Text {
                raw: r#""placeholder""#.to_string(),
                span: ds(),
            })),
            span: ds(),
            doc: None,
        });

        let m = module_with_items(vec![decimal_to_text]);
        let result = collect_workspace(&[(0, &m)], &rustc_hash::FxHashMap::default());

        let has_t034 = result.errors.iter().any(|e| e.code() == "T034");
        assert!(
            !has_t034,
            "pub fn toText for the prelude-covered Decimal must NOT fire T034; got {:?}",
            result.errors
        );
        // The surviving instance is the prelude seed (Explicit), not an
        // auto-promotion — proof the seed ran and the promotion was skipped.
        let inst = result
            .instance_env
            .get((TOTEXT_CLASS, TyConId(51)))
            .expect("prelude seeds ToText Decimal");
        assert_eq!(
            inst.origin,
            InstanceOrigin::Explicit,
            "the ToText Decimal instance must be the prelude seed, not auto-promoted"
        );
    }
}
