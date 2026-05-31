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

use ridge_ast::{Item, Module};
use ridge_types::TyConId;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::class_env::{
    register_prelude_classes, register_prelude_instances, ClassInfo, ClassTable, InstanceEnv,
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
    let mut class_table = ClassTable::new();
    let mut instance_env = InstanceEnv::new();
    let mut errors: Vec<TypeError> = Vec::new();

    // Step 1: Seed the ClassTable with built-in prelude classes.
    register_prelude_classes(&mut class_table);

    // Step 1b: Seed the InstanceEnv with built-in prelude instances (ToText,
    // Eq, and Ord for the primitive types). These live in the prelude module
    // (`def_module = None`) and are not subject to the orphan check below.
    register_prelude_instances(&mut instance_env);

    // Step 2: Walk all ClassDecl items, registering user-defined classes.
    // The two-pass approach in collect_class_decls ensures forward references
    // in superclass lists resolve correctly.
    collect_class_decls(modules, &mut class_table);

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

    // Step 4b: Synthesise instances for every `TypeDecl` that has a
    // `deriving (…)` clause. Derived instances are registered into the same
    // InstanceEnv as explicit ones; coherence (T032 overlap) is enforced
    // by the same InstanceEnv::insert path.
    let derived_instances = collect_derived_instances(
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
    }
}

// ── Class collection ─────────────────────────────────────────────────────────

fn collect_class_decls(modules: &[(u32, &Module)], ct: &mut ClassTable) {
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

            let method_sigs: Vec<MethodSig> = decl
                .methods
                .iter()
                .map(|m| MethodSig {
                    name: m.name.text.clone(),
                    arity: m.params.len(),
                })
                .collect();

            ct.insert_with_id(
                class_id,
                ClassInfo {
                    name: name.clone(),
                    method_sigs,
                    superclasses,
                    def_module: Some(module_id),
                },
            );
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

        // Extract the head TyConId from the instance type. In 0.2.13 only
        // single, concrete type constructors are valid instance heads (no
        // parametric instances, no compound types at the head position).
        // User-defined types are resolved via the pre-collected name map.
        let Some(tycon_id) = extract_tycon_id(&decl.ty, user_tycon_names) else {
            continue; // Unsupported head form — ignored in this pass.
        };

        let methods: Vec<(String, String)> = decl
            .methods
            .iter()
            .map(|m| (m.name.text.clone(), String::new())) // placeholder symbol
            .collect();

        let info = InstanceInfo {
            def_module: Some(module_id),
            methods,
            ctx_constraints: vec![],
            origin: InstanceOrigin::Explicit,
            span: decl.span,
        };

        let class_name = &decl.class.text;
        let type_name = type_display(&decl.ty);

        match env.insert((class_id, tycon_id), info, class_name, &type_name) {
            Ok(()) => {}
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
            ridge_ast::decl::Param::Annotated { ty, .. } => ty,
            ridge_ast::decl::Param::Bare(_) => continue,
        };

        // The parameter type must be a concrete named constructor.
        let Some(tycon_id) = extract_tycon_id(param_ty, user_tycon_names) else {
            continue;
        };

        // Do not auto-promote for builtin/prelude types (TyConId 0–15): they
        // are already covered by instances registered in `register_prelude_instances`.
        // Auto-promotion targets user-defined types only (TyConId >= 16).
        if tycon_id.0 < 16 {
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
) -> Vec<crate::derive::DerivedInstance> {
    let mut all_derived: Vec<crate::derive::DerivedInstance> = Vec::new();

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

    all_derived
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
    for (&(class_id, tycon_id), info) in &env.instances {
        let Some(inst_module) = info.def_module else {
            continue; // prelude-injected instance — always valid
        };

        let class_module = ct.get(class_id).and_then(|ci| ci.def_module);
        // For now, `tycon.def_module_raw` is encoded as the `TyConId.0` index;
        // we do not have direct access to the TyConArena here. Instead we use a
        // sentinel: builtin TyConIds (0..=15) have `def_module_raw = None`.
        // User TyConIds start at 16 and carry the module in a side-channel we
        // do not have here. For now we implement the check conservatively:
        // - If the class has a known def_module AND it matches the instance module
        //   → OK.
        // - If the tycon id is ≥ 16 (user-defined type), we trust that the
        //   instance is in the correct module (the full check arrives once the
        //   TyConArena is threaded through).
        // - Otherwise, if neither class module nor tycon is user-local → orphan.
        let in_class_module = class_module == Some(inst_module);
        let tycon_is_builtin = tycon_id.0 < 16; // builtins have fixed low ids
        let tycon_is_user_local = !tycon_is_builtin; // assume same module for now

        if in_class_module || tycon_is_user_local {
            continue; // valid
        }

        // Neither the class's home module nor the type's home module — orphan.
        let class_name = ct
            .get(class_id)
            .map_or_else(|| format!("#{}", class_id.0), |ci| ci.name.clone());
        let type_name = format!("#{}", tycon_id.0);
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
    // Pre-collect the set of registered (class, tycon) keys for O(1) lookup.
    let registered: FxHashSet<(ridge_types::ClassId, TyConId)> =
        env.instances.keys().copied().collect();

    for (&(class_id, tycon_id), info) in &env.instances {
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

            if !registered.contains(&(super_id, tycon_id)) {
                // Missing superclass instance.
                let class_name = ct
                    .get(class_id)
                    .map_or_else(|| format!("#{}", class_id.0), |ci| ci.name.clone());
                let type_name = format!("#{}", tycon_id.0);
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

/// Extracts the `TyConId` from an AST type in an instance head.
///
/// In 0.2.13, only named type constructors (no polymorphic or compound heads)
/// are supported. Returns `None` for any form we cannot yet resolve.
///
/// Full resolution (looking up user `TyCon`s by name) requires access to the
/// [`ridge_types::TyConArena`], which is not threaded into this pass yet.
/// For now we extract the pre-resolved `TyConId` embedded in `Type::Named`
/// if the AST carries it, or fall back to `None` for forms we cannot resolve.
fn extract_tycon_id(
    ty: &ridge_ast::Type,
    user_tycon_names: &FxHashMap<String, TyConId>,
) -> Option<TyConId> {
    use ridge_ast::Type as AstType;
    match ty {
        // `Named` covers both built-in and user-defined type constructors.
        // We first check the pre-collected user tycon names (which include
        // all user-declared types from the workspace-wide TyCon scan), then
        // fall back to the builtin table for prelude/primitive types.
        AstType::Named { name, .. } => user_tycon_names
            .get(name.text.as_str())
            .copied()
            .or_else(|| builtin_tycon_id_by_name(&name.text)),
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
                #[allow(unreachable_patterns)]
                _ => None,
            }
        }
        _ => None,
    }
}

/// Maps a prelude type name to its fixed `TyConId` index (0-based, matches
/// `BuiltinTyCons::allocate` assignment order).
///
/// Only covers the 16 pre-allocated builtins; user types return `None` here
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
        _ => None,
    }
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
    match ty {
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
                ty_var: ident(&var),
                span: ds(),
            })
            .collect();

        Item::ClassDecl(ClassDecl {
            name: ident(name),
            ty_var: ident("a"),
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
            ty: named_type(ty),
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
            origin: InstanceOrigin::AutoPromoted,
            span: ds(),
        };
        let explicit_info = InstanceInfo {
            def_module: Some(0),
            methods: vec![],
            ctx_constraints: vec![],
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

    /// Builtin types (`TyConId` < 16) must NOT be auto-promoted — they are
    /// already covered by prelude instances.
    #[test]
    fn auto_promote_skips_builtin_types() {
        // `Int` maps to TyConId(0), which is < 16.
        let m = module_with_items(vec![pub_fn_to_text_item("Int")]);
        let result = collect_workspace(&[(0, &m)], &rustc_hash::FxHashMap::default());

        // No T034 because the builtin filter skips TyConId 0.
        let has_t034 = result.errors.iter().any(|e| e.code() == "T034");
        assert!(
            !has_t034,
            "pub fn toText for a builtin type must NOT fire T034; got {:?}",
            result.errors
        );
    }
}
