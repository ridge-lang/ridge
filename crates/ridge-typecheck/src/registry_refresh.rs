//! Refresh retained declaration metadata without inferring function bodies.

use ridge_resolve::ResolvedWorkspace;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{cross_module, CollectResult, TypedWorkspace};

/// A registry refresh that requires the driver's complete-check fallback.
#[derive(Debug)]
pub enum RegistryRefreshFailure {
    /// Declaration collection reported errors.
    Collection(Vec<crate::TypeError>),
    /// Retained class identities no longer match the collected declarations.
    ClassIdentity,
    /// A compiler-owned value comparison could not be reconstructed.
    ValueComparison(Vec<crate::TypeError>),
    /// A declared method contract could not be reconstructed.
    MethodContract(Vec<(ridge_resolve::ModuleId, crate::TypeError)>),
    /// A derived comparison could not be reconstructed.
    DerivedComparison(Vec<(ridge_resolve::ModuleId, crate::TypeError)>),
}

/// Rebuild positional registry metadata after a declaration-preserving edit.
/// Declines with the failing stage if the driver must perform a complete check.
pub fn refresh(
    typed: &mut TypedWorkspace,
    resolved: &ResolvedWorkspace,
) -> Result<(), RegistryRefreshFailure> {
    let modules: Vec<_> = resolved
        .module_asts
        .iter()
        .enumerate()
        .filter_map(|(id, ast)| u32::try_from(id).ok().map(|id| (id, ast.as_ref())))
        .collect();
    let fqns: Vec<_> = resolved
        .graph
        .modules
        .iter()
        .map(|m| m.fully_qualified_name.clone())
        .collect();
    let compiler = cross_module::compiler_tycon_names(&typed.tycons);
    let own = cross_module::declared_tycon_names(&typed.tycons, resolved.modules.len());
    let scopes = scoped_names(typed, resolved, &compiler, &own);
    let mut names: FxHashMap<_, _> = compiler.clone();
    names.extend(typed.stdlib_tycons.clone());
    for module in &own {
        names.extend(module.clone());
    }
    let origins = cross_module::TyConOrigins::new(&typed.tycons, &own, &fqns);
    let mut collected = crate::collect::collect_workspace_scoped(
        &modules,
        &names,
        &scopes,
        &origins,
        &typed.builtins,
        resolved.graph.is_stdlib,
    );
    if !collected.errors.is_empty() {
        return Err(RegistryRefreshFailure::Collection(collected.errors));
    }
    if collected.class_table.names_to_ids() != typed.class_table.names_to_ids() {
        return Err(RegistryRefreshFailure::ClassIdentity);
    }
    let errors = crate::prepare_value_comparisons(
        &typed.tycons,
        &typed.builtins,
        &collected.class_table,
        &mut collected.instance_env,
    );
    if !errors.is_empty() {
        return Err(RegistryRefreshFailure::ValueComparison(errors));
    }
    let templates = crate::type_template::TypeTemplates {
        modules: &modules,
        scopes: &scopes,
        tycons: &typed.tycons,
        builtins: &typed.builtins,
    };
    crate::instance_match::prepare(&templates, &mut collected.instance_env);
    let rows = crate::ctx::RowsTycons::from_lookup(|name| {
        compiler
            .get(name)
            .or_else(|| resolved.graph.is_stdlib.then(|| names.get(name)).flatten())
            .copied()
    });
    let errors = crate::class_contract::prepare(
        &templates,
        &collected.class_table,
        &mut collected.instance_env,
        rows,
    );
    if !errors.is_empty() {
        return Err(RegistryRefreshFailure::MethodContract(errors));
    }
    let comparisons = crate::derive_comparison::finish_comparisons(
        &crate::derive_comparison::ComparisonContext {
            templates: &templates,
            rows_tycons: rows,
            scopes: &scopes,
            tycons: &typed.tycons,
            classes: &collected.class_table,
            builtins: &typed.builtins,
            origins: &origins,
        },
        &mut collected.instance_env,
        &mut collected.derived_instances,
    );
    if !comparisons.errors.is_empty() {
        return Err(RegistryRefreshFailure::DerivedComparison(
            comparisons.errors,
        ));
    }
    retain_rows(typed, &mut collected, comparisons.rows);
    typed.class_table = collected.class_table;
    typed.instance_env = collected.instance_env;
    typed.derived_instances = collected.derived_instances;
    Ok(())
}

fn scoped_names(
    typed: &TypedWorkspace,
    resolved: &ResolvedWorkspace,
    compiler: &FxHashMap<String, ridge_types::TyConId>,
    declared: &[FxHashMap<String, ridge_types::TyConId>],
) -> Vec<FxHashMap<String, ridge_types::TyConId>> {
    let tables: Vec<_> = resolved.modules.iter().map(|m| &m.symbols).collect();
    resolved
        .modules
        .iter()
        .enumerate()
        .map(|(id, module)| {
            let mut names = compiler.clone();
            names.extend(typed.stdlib_tycons.clone());
            names.extend(cross_module::imported_tycon_names(
                &module.imports,
                &tables,
                &typed.module_tycon_names,
                declared,
                &typed.stdlib_tycons,
                compiler,
            ));
            names.extend(declared[id].clone());
            names
        })
        .collect()
}

/// Refresh only Row entries demanded before or by finalized comparisons.
fn retain_rows(
    typed: &TypedWorkspace,
    collected: &mut CollectResult,
    rows: FxHashSet<ridge_types::TyConId>,
) {
    let mut demanded: FxHashSet<_> = typed.derived_instances.iter().map(|d| d.key).collect();
    if let Some(row) = collected.class_table.id_by_name("Row") {
        demanded.extend(rows.into_iter().map(|tycon| (row, tycon)));
    }
    for (_, instance) in collected.implicit_row_instances.drain() {
        if demanded.contains(&instance.key)
            && !collected
                .derived_instances
                .iter()
                .any(|d| d.key == instance.key)
        {
            collected.derived_instances.push(instance);
        }
    }
}
