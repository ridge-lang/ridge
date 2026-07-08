//! Import resolution (T7) — resolves every `ImportDecl` to a concrete target,
//! applies cross-project visibility, and computes per-import effective bindings.
//!
//! ## Bare import semantics
//!
//! Bare `import foo.bar` (no `as Alias`, no item list) binds the **last path
//! segment** (preserving its original case) as a [`Binding::ModuleAlias`].
//! This is equivalent to writing `import foo.bar as bar`.  Flooding the
//! importer's scope with all exported symbols from `foo.bar` is NOT supported.
//!
//! Example: `import std.text` → binds `"text"` as a `ModuleAlias`.
//!
//! ## Unresolved import cascade suppression
//!
//! When an import path fails to resolve (R006 `Unresolved` target), no
//! cascade `R008`/`R009` errors are emitted for items requested via that
//! import.  This suppresses noise when the root cause is a missing module.

use ridge_ast::Ident;
use ridge_lexer::Span;
use rustc_hash::FxHashMap;

use crate::module_graph::ModuleGraph;
use crate::{
    error::{ManifestError, ResolveError},
    manifest::{Project, ProjectDependency, SharedDependency, WorkspaceManifest},
    stdlib_builtin::{lookup_stdlib, StdlibModuleId},
    visibility::ResolvedVisibility,
    ModuleId, ModuleMetadata, NodeId, ProjectId, SymbolId, SymbolTable, WorkspaceGraph,
};

// ── Public types ──────────────────────────────────────────────────────────────

/// The resolution of a single `ImportDecl`.
#[derive(Debug, Clone)]
pub struct ImportResolution {
    /// Stub `NodeId(0)` for this pass; T8 fills the authoritative value once
    /// the `NodeIdMap` assigns stable ids to all AST nodes.
    pub decl_node: NodeId,
    /// What the import path resolved to.
    pub target: ImportTarget,
    /// The `as Alias` rename, if present in the source.
    pub alias: Option<String>,
    /// Explicit item list from `import … (a, b, c)`, if present.
    pub explicit_items: Option<Vec<ImportedItem>>,
    /// Bindings introduced into the importer's module scope.
    pub effective_bindings: Vec<EffectiveBinding>,
    /// Full span of the `ImportDecl` in source.
    pub span: Span,
}

/// What an import path resolved to.
///
/// # Stability
///
/// Marked `#[non_exhaustive]` — new target kinds may be added in future
/// versions (e.g. LSP virtual modules).  Match arms outside this crate
/// must include a wildcard (`_`) arm.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportTarget {
    /// Resolved to a module inside the current workspace.
    WorkspaceModule(ModuleId),
    /// Resolved to a built-in stdlib module.
    BuiltinStdlib(StdlibModuleId),
    /// Resolved to a project-external dependency (placeholder — not used in
    /// 0.1.0; reserved for a future package-manager integration).
    External {
        /// The external project id (placeholder).
        project: ProjectId,
        /// The module FQN within the external project.
        module: String,
    },
    /// The path could not be resolved; a `R006` diagnostic was emitted.
    ///
    /// Downstream passes must treat this as a suppression sentinel: no
    /// cascade `R008`/`R009` errors should be emitted for items referenced
    /// via an `Unresolved` target.
    Unresolved,
}

/// A single named item from an `import … (a, b, c)` clause.
#[derive(Debug, Clone)]
pub struct ImportedItem {
    /// The item name as written in source.
    pub name: String,
    /// Span of just this item name in source.
    pub span: Span,
    /// The resolved binding, or `None` if resolution failed (R008 / R009).
    pub resolved: Option<Binding>,
}

/// A binding introduced into the importer's scope by this import.
#[derive(Debug, Clone)]
pub struct EffectiveBinding {
    /// The local name under which the import is accessible.
    pub local_name: String,
    /// What that name binds to.
    pub binding: Binding,
}

/// What a resolved name binds to.
///
/// T8 extends the enum with `Local`, `ModuleSymbol`, `ActorName`,
/// `Constructor`, and `FieldAccessor` variants for intra-function resolution.
/// T10 will add `Capability`.
///
/// # Stability
///
/// Marked `#[non_exhaustive]` — new binding kinds are anticipated in the
/// type-checker and LSP passes.  Match arms outside this crate must
/// include a wildcard (`_`) arm.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Binding {
    /// A local variable introduced by `let` / `var`, a function/lambda
    /// parameter, or a pattern-bound name.
    Local(crate::scope::LocalId),
    /// A top-level symbol defined in the current module (fn, const, type,
    /// actor, constructor).
    ModuleSymbol {
        /// The module that defines the symbol.
        module: ModuleId,
        /// The symbol within that module's symbol table.
        symbol: SymbolId,
    },
    /// A top-level symbol imported from another workspace module.
    ImportedSymbol {
        /// The module that defines the symbol.
        module: ModuleId,
        /// The symbol within that module's symbol table.
        symbol: SymbolId,
        /// The import declaration that introduced this binding.
        /// Stub `NodeId(0)` in T7; T8 fills the authoritative id.
        via_import: NodeId,
    },
    /// An alias for a whole module (`import std.list as List`).
    ///
    /// Use-sites like `List.map` combine this with a second lookup step
    /// (T9 qualified-name resolution).
    ModuleAlias {
        /// The target the alias points at.
        target: ImportTarget,
        /// The import declaration that introduced this alias.
        /// Stub `NodeId(0)` in T7; T8 fills the authoritative id.
        via_import: NodeId,
    },
    /// A reference to a stdlib symbol whose concrete definition is Phase 7's
    /// responsibility.
    StdlibSymbol {
        /// The stdlib module that exports this symbol.
        module: StdlibModuleId,
        /// The exported symbol name.
        name: String,
    },
    /// The target actor type of a `spawn` expression.
    ActorName {
        /// The module that declares the actor.
        module: ModuleId,
        /// The actor's symbol entry.
        actor: SymbolId,
    },
    /// A constructor pattern or record-construction expression.
    Constructor {
        /// The `SymbolId` of the owning `Type` entry.
        owner_type: SymbolId,
        /// Variant index.  Record auto-constructors are always 0; union
        /// variants are source-ordered starting at 0.  Use `is_record` to
        /// discriminate — `variant == 0` alone is NOT sufficient.
        variant: u32,
        /// True iff this constructor is the auto-constructor of a
        /// `type T = { ... }` record declaration, false for union variants.
        is_record: bool,
        /// The module that declares the owning type, carried through from the
        /// constructor symbol. Equality against the use-site module is the
        /// opaque-type construction/pattern gate (an opaque constructor used
        /// outside its defining module is rejected).
        owner_module: ModuleId,
    },
    /// A field-accessor shorthand `(.name)`.
    ///
    /// The type checker fills the concrete type; at resolve time we only
    /// record the field name.
    FieldAccessor {
        /// The field name.
        field: String,
    },
    /// A class method referenced by its bare name.
    ///
    /// Produced when a bare identifier resolves to a class method rather than a
    /// local binding or module-level symbol. The lower pass projects the method
    /// out of the appropriate dictionary (Static or Forward).
    ClassMethod {
        /// The class that owns this method.
        class_name: String,
        /// The method name.
        method: String,
    },

    /// Name resolution failed; a diagnostic has been emitted.
    Error,
}

/// Aggregated result of running import resolution over the entire workspace.
#[derive(Debug)]
pub struct ImportResolutionResult {
    /// `imports[ModuleId.0]` = that module's import resolutions in source order.
    pub imports: Vec<Vec<ImportResolution>>,
    /// `R006`, `R007`, `R008`, `R009` errors produced during import resolution,
    /// paired with the importing module's [`ModuleId`] for source attribution.
    pub resolve_errors: Vec<(ModuleId, ResolveError)>,
    /// `M013`, `M015` errors produced during manifest cross-validation.
    pub manifest_errors: Vec<ManifestError>,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Resolve every import in every module of the workspace.
///
/// Mutates [`WorkspaceGraph::deps`] to record authoritative module →
/// workspace-module dependency edges (superseding the tentative edges from T4).
///
/// Also validates `[dependencies]` entries in project manifests for
/// `M013 UnknownWorkspaceMember` and `M015 WorkspaceDependencyAbsent`.
///
/// # Algorithm
///
/// For each module's `ImportDecl` (via [`ModuleGraph::tentative_edges`]):
/// 1. Build the dotted path from `TentativeEdge::path_dotted`.
/// 2. Look up: workspace FQN → `WorkspaceModule`, then stdlib → `BuiltinStdlib`,
///    else `Unresolved` + `R006`.
/// 3. If `WorkspaceModule` and cross-project: check `[project.exports].public`
///    / `.internal` → maybe `R007`.
/// 4. (Step 4 = T12 `R013 ForbidViolation`; skipped here.)
/// 5. Compute effective bindings:
///    - `import p as Alias` → `ModuleAlias`.
///    - `import p (a, b)` → one binding per item; `R008`/`R009` on errors.
///    - Bare `import p` → `ModuleAlias` with `local_name` = last segment.
/// 6. Populate `ws.deps[from.0]` from workspace-module targets.
/// 7. Re-run cycle detection on authoritative edges.
#[must_use]
pub fn resolve_imports(
    ws: &mut WorkspaceGraph,
    graph: &ModuleGraph,
    symbol_tables: &[SymbolTable],
) -> ImportResolutionResult {
    let mut resolve_errors: Vec<(ModuleId, ResolveError)> = Vec::new();
    let mut manifest_errors: Vec<ManifestError> = Vec::new();

    // Build a fast FQN → ModuleId index.
    let fqn_index: FxHashMap<&str, ModuleId> = ws
        .modules
        .iter()
        .map(|m| (m.fully_qualified_name.as_str(), m.id))
        .collect();

    // Build ModuleId → ProjectId index.
    let module_project: Vec<ProjectId> = ws.modules.iter().map(|m| m.project).collect();

    // Initialise per-module import resolution storage (indexed by ModuleId.0).
    let module_count = ws.modules.len();
    let mut imports: Vec<Vec<ImportResolution>> = (0..module_count).map(|_| Vec::new()).collect();

    // Reset deps — we will populate authoritatively below.
    ws.deps = (0..module_count).map(|_| Vec::new()).collect();

    // Group tentative edges by their source module for ordered processing.
    for edge in &graph.tentative_edges {
        let from_idx = edge.from.0 as usize;
        let from_project_id = module_project
            .get(from_idx)
            .copied()
            .unwrap_or(ProjectId(u32::MAX));

        // ── Step 1–2: resolve the path ────────────────────────────────────────
        let (target, r006_err) = resolve_path(&edge.path_dotted, edge.span, &fqn_index);

        if let Some(err) = r006_err {
            resolve_errors.push((edge.from, err));
        }

        // ── Step 3: cross-project visibility (R007) ───────────────────────────
        if let ImportTarget::WorkspaceModule(target_mid) = target {
            let target_project_id = module_project
                .get(target_mid.0 as usize)
                .copied()
                .unwrap_or(ProjectId(u32::MAX));

            if from_project_id != target_project_id {
                if let Some(err) = check_project_export_visibility(
                    ws,
                    from_project_id,
                    target_project_id,
                    target_mid,
                    edge.span,
                ) {
                    resolve_errors.push((edge.from, err));
                }
            }
        }

        // ── Step 4: R013 ForbidViolation — owned by T12, skipped here ─────────

        // ── Step 5: compute effective bindings ────────────────────────────────
        let (effective_bindings, explicit_items, item_errors) = compute_effective_bindings(
            &target,
            edge.alias.as_ref(),
            edge.items.as_ref(),
            &edge.path_dotted,
            edge.span,
            symbol_tables,
            &module_project,
            from_project_id,
        );

        resolve_errors.extend(item_errors.into_iter().map(|e| (edge.from, e)));

        // ── Step 6: populate ws.deps ──────────────────────────────────────────
        if let ImportTarget::WorkspaceModule(target_mid) = target {
            let deps_row = ws.deps.get_mut(from_idx);
            if let Some(row) = deps_row {
                if !row.contains(&target_mid) {
                    row.push(target_mid);
                }
            }
        }

        let alias = edge.alias.clone();
        if let Some(v) = imports.get_mut(from_idx) {
            v.push(ImportResolution {
                decl_node: NodeId(0),
                target,
                alias,
                explicit_items,
                effective_bindings,
                span: edge.span,
            });
        }
    }

    // ── Implicit prelude injection ────────────────────────────────────────────
    //
    // For each module, append synthetic prelude ImportResolutions for
    // std.option (Option/Some/None) and std.result (Result/Ok/Err).
    // User-explicit bindings for the same local_name take priority:
    // the prelude binding is dropped if the name is already claimed.
    for module_imports in &mut imports {
        let user_names: rustc_hash::FxHashSet<String> = module_imports
            .iter()
            .flat_map(|ir| ir.effective_bindings.iter())
            .map(|eb| eb.local_name.clone())
            .collect();

        for mut prelude_ir in prelude_resolutions() {
            // Filter out prelude bindings whose name conflicts with a user import.
            prelude_ir
                .effective_bindings
                .retain(|eb| !user_names.contains(&eb.local_name));
            if !prelude_ir.effective_bindings.is_empty() {
                module_imports.push(prelude_ir);
            }
        }
    }

    // ── Manifest cross-validation: M013 / M015 ────────────────────────────────
    let ws_manifest = &ws.manifest;
    for project in &ws.projects {
        validate_project_dependencies(project, ws_manifest, &ws.projects, &mut manifest_errors);
    }

    ImportResolutionResult {
        imports,
        resolve_errors,
        manifest_errors,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Resolve a dotted import path to an [`ImportTarget`].
///
/// Returns `(target, Option<R006 error>)`.
fn resolve_path(
    path_dotted: &str,
    span: Span,
    fqn_index: &FxHashMap<&str, ModuleId>,
) -> (ImportTarget, Option<ResolveError>) {
    // a. Workspace module?
    if let Some(&mid) = fqn_index.get(path_dotted) {
        return (ImportTarget::WorkspaceModule(mid), None);
    }

    // b. Built-in stdlib?
    if let Some(builtin) = lookup_stdlib(path_dotted) {
        return (ImportTarget::BuiltinStdlib(builtin.id), None);
    }

    // c. Unresolved → R006.
    let err = ResolveError::UnresolvedImportPath {
        path: path_dotted.to_owned(),
        span,
    };
    (ImportTarget::Unresolved, Some(err))
}

/// Check `[project.exports]` visibility for a cross-project import.
///
/// Returns `Some(R007)` when the target module is not covered by any of the
/// target project's `public` or `internal` globs.
fn check_project_export_visibility(
    ws: &WorkspaceGraph,
    from_project_id: ProjectId,
    target_project_id: ProjectId,
    target_mid: ModuleId,
    span: Span,
) -> Option<ResolveError> {
    let target_project = ws.projects.get(target_project_id.0 as usize)?;
    let target_meta = ws.modules.get(target_mid.0 as usize)?;
    let target_fqn = &target_meta.fully_qualified_name;

    // Check public globs first.
    let public_ok = target_project
        .exports_public
        .iter()
        .any(|g| g.matches(target_fqn));
    if public_ok {
        return None;
    }

    // Check internal namespace match:
    // Both projects share the same first dotted segment AND target's
    // exports_internal glob matches the target module FQN.
    let from_project = ws.projects.get(from_project_id.0 as usize)?;
    let from_ns = first_segment(&from_project.name);
    let target_ns = first_segment(&target_project.name);

    if from_ns == target_ns {
        // Same top-level namespace — check internal globs.
        let internal_ok = target_project
            .exports_internal
            .iter()
            .any(|g| g.matches(target_fqn));
        if internal_ok {
            return None;
        }
    }

    // Neither public nor internal — R007.
    Some(ResolveError::ProjectExportViolation {
        target: target_fqn.clone(),
        target_project: target_project.name.clone(),
        span,
    })
}

/// Extract the first dot-separated segment from a project name.
///
/// `"acme.domain"` → `"acme"`. If there is no dot, the whole name is returned.
fn first_segment(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// Compute `effective_bindings` and `explicit_items` for one import.
///
/// Returns `(bindings, items, errors)`.
#[allow(clippy::too_many_arguments)]
fn compute_effective_bindings(
    target: &ImportTarget,
    alias: Option<&String>,
    items: Option<&Vec<Ident>>,
    path_dotted: &str,
    import_span: Span,
    symbol_tables: &[SymbolTable],
    module_project: &[ProjectId],
    from_project_id: ProjectId,
) -> (
    Vec<EffectiveBinding>,
    Option<Vec<ImportedItem>>,
    Vec<ResolveError>,
) {
    let mut bindings: Vec<EffectiveBinding> = Vec::new();
    let mut resolved_items: Option<Vec<ImportedItem>> = None;
    let mut errors: Vec<ResolveError> = Vec::new();

    // Per grammar §2.2 `ImportDecl ::= "import" ModulePath [ "as" UPPER_IDENT ]
    // [ "(" ImportList ")" ]`, the alias and item-list clauses are orthogonal:
    // both, either, or neither may be present.  Generate bindings for whichever
    // clauses are present.  Only the bare `import path` form (neither alias nor
    // items) falls back to the last-segment ModuleAlias.

    if let Some(alias_str) = alias {
        // `as Alias` — bind the module under the user-chosen alias name.
        bindings.push(EffectiveBinding {
            local_name: alias_str.clone(),
            binding: Binding::ModuleAlias {
                target: target.clone(),
                via_import: NodeId(0),
            },
        });
    }

    if let Some(item_names) = items {
        // `(item, …)` — bind each named export directly.  Items accept
        // both `LOWER_IDENT` and `UPPER_IDENT` (fns + types).
        //
        // If target is Unresolved, skip silently — no R008 cascade.
        if matches!(target, ImportTarget::Unresolved) {
            let skipped: Vec<ImportedItem> = item_names
                .iter()
                .map(|item| ImportedItem {
                    name: item.text.clone(),
                    span: item.span,
                    resolved: None,
                })
                .collect();
            resolved_items = Some(skipped);
        } else {
            let mut ri: Vec<ImportedItem> = Vec::new();
            for item in item_names {
                let (binding, item_err) = resolve_item(
                    &item.text,
                    import_span,
                    target,
                    path_dotted,
                    symbol_tables,
                    module_project,
                    from_project_id,
                );
                if let Some(err) = item_err {
                    errors.push(err);
                }
                ri.push(ImportedItem {
                    name: item.text.clone(),
                    span: item.span,
                    resolved: binding.clone(),
                });
                if let Some(b) = binding {
                    bindings.push(EffectiveBinding {
                        local_name: item.text.clone(),
                        binding: b,
                    });
                }
            }
            resolved_items = Some(ri);
        }
    } else if alias.is_none() {
        // Bare form: `import path` (no alias, no items).
        //
        // Bind the last path segment (preserving case) as a ModuleAlias.
        // Example: `import std.text` → local name = "text".
        let last_seg = last_segment(path_dotted);
        bindings.push(EffectiveBinding {
            local_name: last_seg.to_owned(),
            binding: Binding::ModuleAlias {
                target: target.clone(),
                via_import: NodeId(0),
            },
        });
    }

    (bindings, resolved_items, errors)
}

/// Resolve a single item from an `import path (item)` clause.
///
/// Returns `(Option<Binding>, Option<error>)`.  A `None` binding means the
/// item was not resolved (an error was emitted).
fn resolve_item(
    item_name: &str,
    import_span: Span,
    target: &ImportTarget,
    path_dotted: &str,
    symbol_tables: &[SymbolTable],
    module_project: &[ProjectId],
    from_project_id: ProjectId,
) -> (Option<Binding>, Option<ResolveError>) {
    match target {
        ImportTarget::WorkspaceModule(target_mid) => {
            let target_project_id = module_project
                .get(target_mid.0 as usize)
                .copied()
                .unwrap_or(ProjectId(u32::MAX));

            let Some(table) = symbol_tables.get(target_mid.0 as usize) else {
                return (
                    None,
                    Some(ResolveError::UnresolvedImportItem {
                        name: item_name.to_owned(),
                        module: path_dotted.to_owned(),
                        suggestions: Vec::new(),
                        span: import_span,
                    }),
                );
            };

            let Some(entry) = table.lookup(item_name) else {
                // T13: suggest visible exported names from the target module
                // closest to the typoed item_name.
                let candidates = workspace_visible_names(table, from_project_id, target_project_id);
                let suggestions = crate::suggest::suggest(item_name, candidates);
                return (
                    None,
                    Some(ResolveError::UnresolvedImportItem {
                        name: item_name.to_owned(),
                        module: path_dotted.to_owned(),
                        suggestions,
                        span: import_span,
                    }),
                );
            };

            // Visibility check (R009).
            let visible = is_symbol_visible(entry.visibility, from_project_id, target_project_id);

            if visible {
                (
                    Some(Binding::ImportedSymbol {
                        module: *target_mid,
                        symbol: entry.id,
                        via_import: NodeId(0),
                    }),
                    None,
                )
            } else {
                (
                    None,
                    Some(ResolveError::VisibilityViolation {
                        name: item_name.to_owned(),
                        defined_at: entry.def_span,
                        use_span: import_span,
                    }),
                )
            }
        }

        ImportTarget::BuiltinStdlib(stdlib_id) => {
            // Look up in the BUILTINS table.
            let builtin = crate::stdlib_builtin::BUILTINS.get(stdlib_id.0 as usize);
            let found = builtin.is_some_and(|m| m.exports.contains(&item_name));

            if found {
                (
                    Some(Binding::StdlibSymbol {
                        module: *stdlib_id,
                        name: item_name.to_owned(),
                    }),
                    None,
                )
            } else {
                // T13: suggest closest stdlib export name.  Stdlib exports are
                // always visible to every importer (no per-project restriction).
                let suggestions = builtin
                    .map(|m| {
                        crate::suggest::suggest(
                            item_name,
                            m.exports.iter().map(|s| (*s).to_owned()),
                        )
                    })
                    .unwrap_or_default();
                (
                    None,
                    Some(ResolveError::UnresolvedImportItem {
                        name: item_name.to_owned(),
                        module: path_dotted.to_owned(),
                        suggestions,
                        span: import_span,
                    }),
                )
            }
        }

        // External and Unresolved are handled before we reach resolve_item.
        ImportTarget::External { .. } | ImportTarget::Unresolved => (None, None),
    }
}

/// Return every name in `table` that is visible to an importer in
/// `from_project_id` (T13 helper for R008 suggestions).
///
/// Filters out `FilePrivate` (`_foo`) symbols and project-restricted
/// symbols when crossing project boundaries.  Mirrors [`is_symbol_visible`].
fn workspace_visible_names(
    table: &SymbolTable,
    from_project_id: ProjectId,
    target_project_id: ProjectId,
) -> Vec<String> {
    table
        .entries
        .iter()
        .filter(|e| is_symbol_visible(e.visibility, from_project_id, target_project_id))
        .map(|e| e.name.clone())
        .collect()
}

/// Return `true` if a symbol with the given visibility is accessible from the
/// given importer project.
fn is_symbol_visible(
    vis: ResolvedVisibility,
    from_project_id: ProjectId,
    target_project_id: ProjectId,
) -> bool {
    match vis {
        // `pub` symbols and `pub(internal)` are always accessible within the workspace.
        // For simplicity in T7, NamespaceInternal is treated as same-workspace accessible;
        // T12 refines this to same-namespace only.
        ResolvedVisibility::Pub | ResolvedVisibility::NamespaceInternal => true,
        // `ProjectPrivate` (no modifier) — only accessible within the same project.
        ResolvedVisibility::ProjectPrivate => from_project_id == target_project_id,
        // `FilePrivate` (_foo) — never importable.
        ResolvedVisibility::FilePrivate => false,
    }
}

/// Return the last dot-separated segment of a path.
///
/// `"std.net.http"` → `"http"`. If there is no dot, the whole path is returned.
fn last_segment(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// Validate a project's `[dependencies]` entries for M013 / M015.
///
/// - `WorkspaceMember { member }` → name must match an existing project name.
/// - `Workspace { local_name }` → `local_name` must exist as a key in
///   `WorkspaceManifest::dependencies`.
/// - `Path` / `Git` dependencies are not re-validated here (T3 handles path).
fn validate_project_dependencies(
    project: &Project,
    ws_manifest: &WorkspaceManifest,
    all_projects: &[Project],
    errors: &mut Vec<ManifestError>,
) {
    for dep in &project.dependencies {
        match dep {
            ProjectDependency::WorkspaceMember { member, .. } => {
                // M013: member name must match an existing project.
                let exists = all_projects.iter().any(|p| &p.name == member);
                if !exists {
                    errors.push(ManifestError::UnknownWorkspaceMember {
                        name: member.clone(),
                        path: project.manifest_path.clone(),
                    });
                }
            }
            ProjectDependency::Workspace { local_name } => {
                // M015: local_name must be a key in workspace.dependencies.
                let exists = ws_manifest
                    .dependencies
                    .iter()
                    .any(|d| shared_dep_name(d) == local_name.as_str());
                if !exists {
                    errors.push(ManifestError::WorkspaceDependencyAbsent {
                        name: local_name.clone(),
                        path: project.manifest_path.clone(),
                    });
                }
            }
            ProjectDependency::Path { .. } | ProjectDependency::Git { .. } => {
                // Not re-validated in T7.
            }
        }
    }
}

/// Extract the key name from a [`SharedDependency`].
const fn shared_dep_name(dep: &SharedDependency) -> &str {
    match dep {
        SharedDependency::Version { name, .. }
        | SharedDependency::Git { name, .. }
        | SharedDependency::Path { name, .. } => name.as_str(),
    }
}

// ── Implicit prelude ──────────────────────────────────────────────────────────

/// The implicit prelude — names always in scope of every Ridge module
/// without an explicit `import` declaration.
///
/// The prelude binds `Option`/`Some`/`None` from `std.option`,
/// `Result`/`Ok`/`Err` from `std.result`, and `JsonValue` plus its seven
/// `J*` constructors from `std.json` as `StdlibSymbol` entries
/// (constructor/type bindings).
///
/// The prelude also binds `ModuleAlias` entries for all 8 pure-data stdlib
/// modules so that qualified names like `Int.parse`, `Text.padLeft`,
/// `Float.fromInt`, `List.map`, `Map.empty`, `Set.fromList`, `Bool.not`,
/// `Json.encode` are usable without an explicit `import std.X as X`
/// declaration.  Capability-bearing modules (`std.io`, `std.fs`,
/// `std.net.http`, etc.) are NOT included — every side-effecting import must
/// remain visible at the import level.
///
/// User imports for the same `local_name` take priority: the prelude binding
/// is suppressed.
///
/// Returns synthetic `ImportResolution` entries. The walker treats them
/// identically to user imports.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "flat list of prelude IR literals; splitting would obscure the one-place-per-binding layout"
)]
pub fn prelude_resolutions() -> Vec<ImportResolution> {
    use crate::stdlib_builtin::{lookup_stdlib, StdlibModuleId};

    let synth_span = Span::point(0);
    let opt_id = StdlibModuleId(7); // std.option
    let res_id = StdlibModuleId(8); // std.result
    let json_id = StdlibModuleId(17); // std.json
    let query_id = StdlibModuleId(22); // std.query

    let opt_binding = |name: &str| EffectiveBinding {
        local_name: name.to_string(),
        binding: Binding::StdlibSymbol {
            module: opt_id,
            name: name.to_string(),
        },
    };
    let res_binding = |name: &str| EffectiveBinding {
        local_name: name.to_string(),
        binding: Binding::StdlibSymbol {
            module: res_id,
            name: name.to_string(),
        },
    };
    // JsonValue (§3.17) is a prelude union like Option/Result: the type name and
    // its seven `J*` constructors are in scope in every module. The constructors
    // lower to the lowercase-snake BEAM atoms `ridge_rt:json_*` produces.
    let json_binding = |name: &str| EffectiveBinding {
        local_name: name.to_string(),
        binding: Binding::StdlibSymbol {
            module: json_id,
            name: name.to_string(),
        },
    };
    // QExpr (the quotation expression tree) and Quote (the captured-expression
    // wrapper) are prelude builtins like JsonValue: the type names and the
    // `Q*` constructors are in scope everywhere so the quotation runtime can
    // match the tree and a quoted predicate's type resolves without an import.
    let query_binding = |name: &str| EffectiveBinding {
        local_name: name.to_string(),
        binding: Binding::StdlibSymbol {
            module: query_id,
            name: name.to_string(),
        },
    };

    // Pure-data module aliases injected into every module's scope.
    // Each entry is (stdlib_path, local_alias).  Adding a future pure-data
    // module is a one-line change here.
    let pure_data_modules: &[(&str, &str)] = &[
        ("std.int", "Int"),
        ("std.float", "Float"),
        ("std.decimal", "Decimal"),
        ("std.uuid", "Uuid"),
        ("std.bytes", "Bytes"),
        ("std.bool", "Bool"),
        ("std.text", "Text"),
        ("std.list", "List"),
        ("std.map", "Map"),
        ("std.set", "Set"),
        ("std.json", "Json"),
        // std.option and std.result are also pure-data, but their prelude
        // entries are handled above as StdlibSymbol constructor bindings.
        // The ModuleAlias for Option/Result is omitted here to avoid a second
        // conflicting prelude IR for the same target; the StdlibSymbol
        // bindings are sufficient for unqualified constructor use.
    ];

    let alias_bindings: Vec<EffectiveBinding> = pure_data_modules
        .iter()
        .filter_map(|(path, alias)| {
            let module = lookup_stdlib(path)?;
            Some(EffectiveBinding {
                local_name: (*alias).to_string(),
                binding: Binding::ModuleAlias {
                    target: ImportTarget::BuiltinStdlib(module.id),
                    via_import: crate::NodeId(0),
                },
            })
        })
        .collect();

    // Synthetic IR for the module-alias prelude.
    // Uses a sentinel BuiltinStdlib(0) target (std.int) for the IR's own
    // target field — the meaningful data is in the effective_bindings, each of
    // which carries its own ModuleAlias target.
    let aliases_ir = ImportResolution {
        decl_node: crate::NodeId(0),
        target: ImportTarget::BuiltinStdlib(StdlibModuleId(0)), // sentinel
        alias: None,
        explicit_items: None,
        effective_bindings: alias_bindings,
        span: synth_span,
    };

    vec![
        ImportResolution {
            decl_node: crate::NodeId(0),
            target: ImportTarget::BuiltinStdlib(opt_id),
            alias: None,
            explicit_items: None,
            effective_bindings: vec![
                opt_binding("Option"),
                opt_binding("Some"),
                opt_binding("None"),
            ],
            span: synth_span,
        },
        ImportResolution {
            decl_node: crate::NodeId(0),
            target: ImportTarget::BuiltinStdlib(res_id),
            alias: None,
            explicit_items: None,
            effective_bindings: vec![res_binding("Result"), res_binding("Ok"), res_binding("Err")],
            span: synth_span,
        },
        ImportResolution {
            decl_node: crate::NodeId(0),
            target: ImportTarget::BuiltinStdlib(json_id),
            alias: None,
            explicit_items: None,
            effective_bindings: vec![
                json_binding("JsonValue"),
                json_binding("JNull"),
                json_binding("JBool"),
                json_binding("JInt"),
                json_binding("JFloat"),
                json_binding("JText"),
                json_binding("JList"),
                json_binding("JObject"),
            ],
            span: synth_span,
        },
        ImportResolution {
            decl_node: crate::NodeId(0),
            target: ImportTarget::BuiltinStdlib(query_id),
            alias: None,
            explicit_items: None,
            effective_bindings: vec![
                query_binding("Quote"),
                query_binding("QExpr"),
                query_binding("QCol"),
                query_binding("QLitInt"),
                query_binding("QLitText"),
                query_binding("QLitBool"),
                query_binding("QLitFloat"),
                query_binding("QLitDecimal"),
                query_binding("QLitUuid"),
                query_binding("QLitInstant"),
                query_binding("QLitBytes"),
                query_binding("QAnd"),
                query_binding("QOr"),
                query_binding("QNot"),
                query_binding("QNotTrue"),
                query_binding("QEq"),
                query_binding("QNe"),
                query_binding("QLt"),
                query_binding("QGt"),
                query_binding("QLe"),
                query_binding("QGe"),
                query_binding("QProj"),
                query_binding("QColR"),
                query_binding("QColAt"),
                query_binding("QGroupKey"),
                query_binding("QAggCount"),
                query_binding("QAggSum"),
                query_binding("QAggAvg"),
                query_binding("QAggMin"),
                query_binding("QAggMax"),
                query_binding("QLike"),
                query_binding("QIn"),
                query_binding("QAdd"),
                query_binding("QSub"),
                query_binding("QMul"),
                query_binding("QDiv"),
                query_binding("QMod"),
                query_binding("QCase"),
                query_binding("QExists"),
                // `Ret/1` — the return-type projection, in scope for query-builder
                // signatures that name the element of a projection's result.
                query_binding("Ret"),
                // `Rows/1` — the row-shape projection, in scope for the decode
                // terminals' signatures that name the row of their receiver.
                query_binding("Rows"),
                // `JoinCond/2` / `JoinResult/2` — the N-ary join builder's
                // condition-shape and result projections, in scope for the
                // `Joinable` method signature.
                query_binding("JoinCond"),
                query_binding("JoinResult"),
                // `LeftJoinResult/2` — the LEFT outer-join verb's result projection,
                // in scope for the `LeftJoinable` method signature.
                query_binding("LeftJoinResult"),
                // `RightJoinResult/2` — the RIGHT outer-join verb's result projection.
                query_binding("RightJoinResult"),
                // `FullJoinResult/2` — the FULL outer-join verb's result projection.
                query_binding("FullJoinResult"),
                // `InsertShape/1` — the typed insert verbs' input-shape projection,
                // in scope for the reconciled `insert`/`insertMany` schemes that name
                // the entity minus its database-generated columns.
                query_binding("InsertShape"),
            ],
            span: synth_span,
        },
        aliases_ir,
    ]
}

// ── detect_cycles_authoritative ───────────────────────────────────────────────

/// Detect import cycles using the **authoritative** `ws.deps` built by
/// [`resolve_imports`] rather than the tentative edges from T4/T5.
///
/// Supersedes the tentative-edge cycle check from `detect_cycles` (which used
/// path-string matching and could miss cycles through unresolved aliases).
///
/// Uses the same [`crate::module_graph::tarjan_scc`] algorithm as T5.
///
/// # When to call
///
/// Call this **after** [`resolve_imports`] has populated `ws.deps`.  The two
/// cycle-detection passes are complementary:
///
/// - `detect_cycles` (T5): fast, runs on tentative edges before resolution;
///   useful as an early warning.
/// - `detect_cycles_authoritative` (T7): definitive, runs on resolved edges;
///   the result is the one carried into diagnostics.
#[must_use]
pub fn detect_cycles_authoritative(
    ws: &WorkspaceGraph,
    imports_by_module: &[Vec<ImportResolution>],
) -> Vec<(crate::ModuleId, ResolveError)> {
    use crate::module_graph::tarjan_scc;

    let n = ws.modules.len();
    if n == 0 {
        return Vec::new();
    }

    // Convert ws.deps (Vec<Vec<ModuleId>>) to Vec<Vec<usize>> for tarjan_scc.
    let adj_usize: Vec<Vec<usize>> = ws
        .deps
        .iter()
        .map(|row| row.iter().map(|m| m.0 as usize).collect())
        .collect();

    // Build a span lookup: (from_mid, to_mid) → Span.
    // Used to attach the first edge span to R003 cycles.
    let mut span_map: FxHashMap<(usize, usize), Span> = FxHashMap::default();
    for (from_idx, module_imports) in imports_by_module.iter().enumerate() {
        for res in module_imports {
            if let ImportTarget::WorkspaceModule(target_mid) = res.target {
                span_map
                    .entry((from_idx, target_mid.0 as usize))
                    .or_insert(res.span);
            }
        }
    }

    let mut errors: Vec<(crate::ModuleId, ResolveError)> = Vec::new();

    // Self-imports (edge a → a).
    for (from_idx, module_imports) in imports_by_module.iter().enumerate() {
        for res in module_imports {
            if let ImportTarget::WorkspaceModule(target_mid) = res.target {
                if target_mid.0 as usize == from_idx {
                    let mid = crate::ModuleId(u32::try_from(from_idx).unwrap_or(u32::MAX));
                    errors.push((mid, ResolveError::SelfImport { span: res.span }));
                }
            }
        }
    }

    // SCCs of size > 1 → R003.
    for scc in tarjan_scc(&adj_usize) {
        if scc.len() < 2 {
            continue;
        }

        let mut cycle: Vec<crate::ModuleId> = scc
            .iter()
            .map(|&i| crate::ModuleId(u32::try_from(i).unwrap_or(u32::MAX)))
            .collect();
        cycle.sort_by_key(|m| m.0);

        let lowest = cycle[0].0 as usize;
        let cycle_set: rustc_hash::FxHashSet<usize> = scc.iter().copied().collect();

        // Find the first edge span from the lowest-id node in the cycle.
        let first_edge_span = adj_usize
            .get(lowest)
            .and_then(|targets| {
                targets
                    .iter()
                    .filter(|&&t| cycle_set.contains(&t))
                    .find_map(|&t| span_map.get(&(lowest, t)).copied())
            })
            .or_else(|| {
                // Fallback: any edge within the cycle.
                scc.iter().find_map(|&from| {
                    adj_usize.get(from).and_then(|targets| {
                        targets
                            .iter()
                            .filter(|&&t| cycle_set.contains(&t))
                            .find_map(|&t| span_map.get(&(from, t)).copied())
                    })
                })
            })
            .unwrap_or_else(|| {
                ws.modules
                    .get(lowest)
                    .map_or(Span::point(0), |m: &ModuleMetadata| m.span_within_file)
            });

        let rep = crate::ModuleId(u32::try_from(lowest).unwrap_or(u32::MAX));
        errors.push((
            rep,
            ResolveError::CyclicImport {
                cycle,
                first_edge: first_edge_span,
            },
        ));
    }

    errors
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_module_graph, collect_symbols, SymbolTable};
    use std::fs;
    use tempfile::TempDir;

    // ── Test utilities ─────────────────────────────────────────────────────────

    fn write_file(dir: &std::path::Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        fs::write(full, content).expect("write file");
    }

    fn workspace_toml(members: &[&str]) -> String {
        let list = members
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[workspace]\nname = \"test-ws\"\nversion = \"0.1.0\"\nmembers = [{list}]\n")
    }

    fn project_toml(name: &str) -> String {
        format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\nkind = \"library\"\n")
    }

    /// Build a minimal 1-module workspace with `src` as the source, run
    /// `resolve_imports` on it, and return the result.
    fn resolve_single(src: &str) -> (TempDir, ImportResolutionResult) {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(td.path(), "libs/proj/src/Main.ridge", src);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("workspace graph");
        let g = crate::build_module_graph(&ws);

        // Build symbol tables.
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (table, _errs) = collect_symbols(pm.id, &pm.ast);
                table
            })
            .collect();

        let result = resolve_imports(&mut ws, &g, &symbol_tables);
        (td, result)
    }

    // ── Tests 1–17 ────────────────────────────────────────────────────────────

    // Test 1: `import std.list as List` → BuiltinStdlib, alias = "List", 1 binding (ModuleAlias)
    // The module gets 3 prelude IRs, so total len = 4.
    // The 'List' name in the module-alias prelude IR is suppressed because the user owns it.
    #[test]
    fn t1_import_std_list_as_list() {
        let (_td, result) = resolve_single("import std.list as List\n");
        assert!(
            result.resolve_errors.is_empty(),
            "errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0");
        // 1 user import + 5 prelude IRs (option + result + json + quotation
        // constructors, module aliases).
        assert_eq!(module_imports.len(), 6);
        let res = &module_imports[0];
        assert!(
            matches!(res.target, ImportTarget::BuiltinStdlib(_)),
            "expected BuiltinStdlib, got {:?}",
            res.target
        );
        assert_eq!(res.alias, Some("List".to_owned()));
        assert_eq!(res.effective_bindings.len(), 1);
        assert!(
            matches!(
                &res.effective_bindings[0].binding,
                Binding::ModuleAlias { .. }
            ),
            "expected ModuleAlias"
        );
        assert_eq!(res.effective_bindings[0].local_name, "List");
    }

    // Test 2: `import std.map (get, insert)` → 2 StdlibSymbol bindings
    #[test]
    fn t2_import_std_map_items() {
        let (_td, result) = resolve_single("import std.map (get, insert)\n");
        assert!(
            result.resolve_errors.is_empty(),
            "errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0");
        let res = &module_imports[0];
        assert_eq!(res.effective_bindings.len(), 2);
        for b in &res.effective_bindings {
            assert!(
                matches!(&b.binding, Binding::StdlibSymbol { .. }),
                "expected StdlibSymbol for '{}'",
                b.local_name
            );
        }
        let names: Vec<&str> = res
            .effective_bindings
            .iter()
            .map(|b| b.local_name.as_str())
            .collect();
        assert!(names.contains(&"get"));
        assert!(names.contains(&"insert"));
    }

    // Each `import … (a, b)` item carries the span of its own name token, not
    // the whole-import span. This lets the LSP point go-to-declaration and
    // rename at the exact clause item.
    #[test]
    fn import_item_carries_its_own_name_span() {
        let src = "import std.map (get, insert)\n";
        let (_td, result) = resolve_single(src);
        let res = &result.imports.first().expect("module 0")[0];
        let items = res.explicit_items.as_ref().expect("explicit item list");
        assert_eq!(items.len(), 2);

        assert_eq!(
            &src[items[0].span.start as usize..items[0].span.end as usize],
            "get"
        );
        assert_eq!(
            &src[items[1].span.start as usize..items[1].span.end as usize],
            "insert"
        );

        // Each item span sits strictly inside the whole-import span. The bug
        // this guards against gave every item the import's own span.
        for item in items {
            assert!(res.span.start <= item.span.start && item.span.end <= res.span.end);
            assert!(item.span.end - item.span.start < res.span.end - res.span.start);
        }
    }

    // Test 3: `import std.list (mapper)` → R008 (mapper not in stdlib exports)
    #[test]
    fn t3_import_std_list_unknown_item_r008() {
        let (_td, result) = resolve_single("import std.list (mapper)\n");
        let r008_count = result
            .resolve_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::UnresolvedImportItem { .. }))
            .count();
        assert_eq!(
            r008_count, 1,
            "expected 1 R008; got: {:?}",
            result.resolve_errors
        );
    }

    // Test 4: `import std.bogus` → R006, target = Unresolved
    // The module gets 4 prelude IRs, so total len = 5.
    #[test]
    fn t4_import_std_bogus_r006() {
        let (_td, result) = resolve_single("import std.bogus\n");
        let r006_count = result
            .resolve_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::UnresolvedImportPath { .. }))
            .count();
        assert_eq!(
            r006_count, 1,
            "expected 1 R006; got: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0");
        // 1 unresolved user import + 5 prelude IRs (option + result + json +
        // quotation constructors, module aliases).
        assert_eq!(module_imports.len(), 6);
        assert_eq!(module_imports[0].target, ImportTarget::Unresolved);
    }

    // Test 5: workspace module import → WorkspaceModule, ModuleAlias binding
    #[test]
    fn t5_import_workspace_module_as_alias() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(td.path(), "libs/proj/src/A.ridge", "import proj.B as B\n");
        write_file(td.path(), "libs/proj/src/B.ridge", "");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        // Find the module that imports proj.B
        let a_imports: Vec<_> = result
            .imports
            .iter()
            .flat_map(|v| v.iter())
            .filter(|r| matches!(r.target, ImportTarget::WorkspaceModule(_)))
            .collect();
        assert!(
            !a_imports.is_empty(),
            "expected at least one WorkspaceModule import"
        );

        let res = a_imports[0];
        assert!(matches!(res.target, ImportTarget::WorkspaceModule(_)));
        assert_eq!(res.alias, Some("B".to_owned()));
        assert_eq!(res.effective_bindings.len(), 1);
        assert!(matches!(
            &res.effective_bindings[0].binding,
            Binding::ModuleAlias { .. }
        ));
    }

    // Test 6: `import workspace.module (pub_fn)` → ImportedSymbol binding
    #[test]
    fn t6_import_workspace_module_pub_fn() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(
            td.path(),
            "libs/proj/src/A.ridge",
            "import proj.B (some_pub_fn)\n",
        );
        write_file(
            td.path(),
            "libs/proj/src/B.ridge",
            "pub fn some_pub_fn () = ()\n",
        );

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        assert!(
            result.resolve_errors.is_empty(),
            "expected no errors; got: {:?}",
            result.resolve_errors
        );

        let all_bindings: Vec<_> = result
            .imports
            .iter()
            .flat_map(|v| v.iter())
            .flat_map(|r| r.effective_bindings.iter())
            .collect();

        let imported_sym = all_bindings
            .iter()
            .find(|b| matches!(b.binding, Binding::ImportedSymbol { .. }));
        assert!(imported_sym.is_some(), "expected an ImportedSymbol binding");
    }

    // Test 7: `import workspace.module (private_fn)` → R009
    #[test]
    fn t7_import_workspace_module_private_fn_r009() {
        let td = TempDir::new().expect("tempdir");
        // Two separate projects so cross-project visibility rules apply.
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj_a/ridge.toml", &project_toml("proj_a"));
        write_file(
            td.path(),
            "libs/proj_a/src/A.ridge",
            "import proj_b.B (private_fn)\n",
        );
        write_file(td.path(), "libs/proj_b/ridge.toml", &project_toml("proj_b"));
        // `fn private_fn` with no `pub` → ProjectPrivate
        write_file(
            td.path(),
            "libs/proj_b/src/B.ridge",
            "fn private_fn () = ()\n",
        );

        // proj_b has no exports — everything is internal by default.
        // Add a wildcard public export so R007 doesn't fire and we isolate R009.
        let proj_b_toml = "[project]\nname = \"proj_b\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"proj_b.**\"]\n";
        write_file(td.path(), "libs/proj_b/ridge.toml", proj_b_toml);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        let r009_count = result
            .resolve_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::VisibilityViolation { .. }))
            .count();
        assert!(
            r009_count >= 1,
            "expected R009; got: {:?}",
            result.resolve_errors
        );
    }

    // Test 8: cross-project import where exports.public excludes target → R007
    #[test]
    fn t8_cross_project_no_export_r007() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj_a/ridge.toml", &project_toml("proj_a"));
        write_file(
            td.path(),
            "libs/proj_a/src/A.ridge",
            "import proj_b.B as B\n",
        );
        // proj_b with no public exports at all.
        write_file(td.path(), "libs/proj_b/ridge.toml", &project_toml("proj_b"));
        write_file(td.path(), "libs/proj_b/src/B.ridge", "");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        let r007_count = result
            .resolve_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::ProjectExportViolation { .. }))
            .count();
        assert!(
            r007_count >= 1,
            "expected R007; got: {:?}",
            result.resolve_errors
        );
    }

    // Test 9: cross-project import where exports.public includes target → clean
    #[test]
    fn t9_cross_project_with_export_clean() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj_a/ridge.toml", &project_toml("proj_a"));
        write_file(
            td.path(),
            "libs/proj_a/src/A.ridge",
            "import proj_b.B as B\n",
        );
        let proj_b_toml = "[project]\nname = \"proj_b\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\npublic = [\"proj_b.**\"]\n";
        write_file(td.path(), "libs/proj_b/ridge.toml", proj_b_toml);
        write_file(td.path(), "libs/proj_b/src/B.ridge", "");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        let r007_count = result
            .resolve_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::ProjectExportViolation { .. }))
            .count();
        assert_eq!(
            r007_count, 0,
            "R007 must not fire when public export covers target"
        );
    }

    // Test 10: internal-namespace import (shared top segment + internal glob) → clean
    #[test]
    fn t10_internal_namespace_import_clean() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj_a/ridge.toml", &project_toml("acme.a"));
        write_file(
            td.path(),
            "libs/proj_a/src/A.ridge",
            "import acme.b.B as B\n",
        );
        let proj_b_toml = "[project]\nname = \"acme.b\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[project.exports]\ninternal = [\"acme.b.**\"]\n";
        write_file(td.path(), "libs/proj_b/ridge.toml", proj_b_toml);
        write_file(td.path(), "libs/proj_b/src/B.ridge", "");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        let r007_count = result
            .resolve_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::ProjectExportViolation { .. }))
            .count();
        assert_eq!(
            r007_count, 0,
            "R007 must not fire for internal-namespace import with matching internal glob; errors: {:?}",
            result.resolve_errors
        );
    }

    // Test 11: bare `import std.text` → ModuleAlias bound to "text"
    #[test]
    fn t11_bare_import_binds_last_segment() {
        let (_td, result) = resolve_single("import std.text\n");
        assert!(
            result.resolve_errors.is_empty(),
            "errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0");
        let res = &module_imports[0];
        assert_eq!(res.effective_bindings.len(), 1);
        assert_eq!(res.effective_bindings[0].local_name, "text");
        assert!(matches!(
            &res.effective_bindings[0].binding,
            Binding::ModuleAlias { .. }
        ));
    }

    // Test 12: R006 cascade suppression — Unresolved target → no R008 for items
    #[test]
    fn t12_unresolved_target_no_r008_cascade() {
        let (_td, result) = resolve_single("import std.bogus (foo, bar)\n");
        let r006_count = result
            .resolve_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::UnresolvedImportPath { .. }))
            .count();
        let r008_count = result
            .resolve_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::UnresolvedImportItem { .. }))
            .count();
        assert_eq!(r006_count, 1, "expected 1 R006");
        assert_eq!(r008_count, 0, "no R008 cascade from Unresolved target");
    }

    // Test 13: ws.deps populated correctly — A imports B → deps[A] contains B
    #[test]
    fn t13_ws_deps_populated_from_workspace_import() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(td.path(), "libs/proj/src/A.ridge", "import proj.B as B\n");
        write_file(td.path(), "libs/proj/src/B.ridge", "");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();

        // Find which module is A (has an import) and B (the target).
        let a_module_id = g
            .modules
            .iter()
            .find(|pm| {
                let fqn = &ws.modules[pm.id.0 as usize].fully_qualified_name;
                fqn.split('.').next_back() == Some("A")
            })
            .map(|pm| pm.id);
        let b_module_id = g
            .modules
            .iter()
            .find(|pm| {
                let fqn = &ws.modules[pm.id.0 as usize].fully_qualified_name;
                fqn.split('.').next_back() == Some("B")
            })
            .map(|pm| pm.id);

        let _ = resolve_imports(&mut ws, &g, &symbol_tables);

        if let (Some(a_id), Some(b_id)) = (a_module_id, b_module_id) {
            let deps_of_a = &ws.deps[a_id.0 as usize];
            assert!(
                deps_of_a.contains(&b_id),
                "deps[A] should contain B; got: {deps_of_a:?}",
            );
        }
    }

    // Test 14: cycle detection on authoritative edges fires R003
    #[test]
    fn t14_cycle_detection_authoritative_r003() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        write_file(td.path(), "libs/proj/src/A.ridge", "import proj.B as B\n");
        write_file(td.path(), "libs/proj/src/B.ridge", "import proj.A as A\n");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);
        let cycle_errors = detect_cycles_authoritative(&ws, &result.imports);

        let r003_count = cycle_errors
            .iter()
            .filter(|(_, e)| matches!(e, ResolveError::CyclicImport { .. }))
            .count();
        assert!(
            r003_count >= 1,
            "expected R003 for A<->B cycle; got: {cycle_errors:?}"
        );
    }

    // Test 15: M013 UnknownWorkspaceMember
    #[test]
    fn t15_m013_unknown_workspace_member() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        let proj_toml = "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[dependencies]\nunknown = { workspace-member = \"nonexistent\" }\n";
        write_file(td.path(), "libs/proj/ridge.toml", proj_toml);
        write_file(td.path(), "libs/proj/src/Main.ridge", "");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        let m013_count = result
            .manifest_errors
            .iter()
            .filter(|e| matches!(e, ManifestError::UnknownWorkspaceMember { .. }))
            .count();
        assert!(
            m013_count >= 1,
            "expected M013; got: {:?}",
            result.manifest_errors
        );
    }

    // Test 16: M015 WorkspaceDependencyAbsent
    #[test]
    fn t16_m015_workspace_dependency_absent() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        let proj_toml = "[project]\nname = \"proj\"\nversion = \"0.1.0\"\nkind = \"library\"\n\n[dependencies]\nmissing_ws_dep = { workspace = true }\n";
        write_file(td.path(), "libs/proj/ridge.toml", proj_toml);
        write_file(td.path(), "libs/proj/src/Main.ridge", "");

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        let m015_count = result
            .manifest_errors
            .iter()
            .filter(|e| matches!(e, ManifestError::WorkspaceDependencyAbsent { .. }))
            .count();
        assert!(
            m015_count >= 1,
            "expected M015; got: {:?}",
            result.manifest_errors
        );
    }

    // Test 17 (DoD): all 4 examples resolve with zero errors
    // This test is in tests/snapshots.rs (extended below).
    // Here we replicate the assertion for log_analyzer inline.
    #[test]
    fn t17_log_analyzer_imports_resolve_clean() {
        let example_src = format!(
            "{}/../../examples/log_analyzer.ridge",
            env!("CARGO_MANIFEST_DIR")
        );
        let src = std::fs::read_to_string(&example_src)
            .unwrap_or_else(|e| panic!("cannot read {example_src}: {e}"));

        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["apps/*"]));
        write_file(td.path(), "apps/demo/ridge.toml", &project_toml("demo"));
        write_file(td.path(), "apps/demo/src/log_analyzer.ridge", &src);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        assert!(
            result.resolve_errors.is_empty(),
            "log_analyzer: R-errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0 imports");
        for res in module_imports {
            assert_ne!(
                res.target,
                ImportTarget::Unresolved,
                "log_analyzer: import '{}' is Unresolved",
                res.alias.as_deref().unwrap_or("?")
            );
        }
    }

    // Test 18: url_shortener imports resolve clean
    #[test]
    fn t18_url_shortener_imports_resolve_clean() {
        let example_src = format!(
            "{}/../../examples/url_shortener.ridge",
            env!("CARGO_MANIFEST_DIR")
        );
        let src = std::fs::read_to_string(&example_src)
            .unwrap_or_else(|e| panic!("cannot read {example_src}: {e}"));

        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["apps/*"]));
        write_file(td.path(), "apps/demo/ridge.toml", &project_toml("demo"));
        write_file(td.path(), "apps/demo/src/url_shortener.ridge", &src);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        assert!(
            result.resolve_errors.is_empty(),
            "url_shortener: R-errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0 imports");
        for res in module_imports {
            assert_ne!(
                res.target,
                ImportTarget::Unresolved,
                "url_shortener: unresolved import"
            );
        }
    }

    // Test 19: game_of_life imports resolve clean
    #[test]
    fn t19_game_of_life_imports_resolve_clean() {
        let example_src = format!(
            "{}/../../examples/game_of_life.ridge",
            env!("CARGO_MANIFEST_DIR")
        );
        let src = std::fs::read_to_string(&example_src)
            .unwrap_or_else(|e| panic!("cannot read {example_src}: {e}"));

        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["apps/*"]));
        write_file(td.path(), "apps/demo/ridge.toml", &project_toml("demo"));
        write_file(td.path(), "apps/demo/src/game_of_life.ridge", &src);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        assert!(
            result.resolve_errors.is_empty(),
            "game_of_life: R-errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0 imports");
        for res in module_imports {
            assert_ne!(
                res.target,
                ImportTarget::Unresolved,
                "game_of_life: unresolved import"
            );
        }
    }

    // ── Prelude tests ─────────────────────────────────────────────────────────

    // Prelude test 1: prelude_resolutions() returns exactly 5 ImportResolutions.
    // The option/result/json/query prelude contributes 4 (std.option +
    // std.result + std.json constructors/types + std.query quotation types); the
    // module-alias prelude adds a fifth synthetic IR that carries the 8
    // pure-data ModuleAlias bindings (Int, Float, Bool, Text, List, Map, Set,
    // Json).
    #[test]
    fn prelude_returns_five_resolutions() {
        let resolutions = super::prelude_resolutions();
        assert_eq!(
            resolutions.len(),
            5,
            "expected exactly 5 prelude ImportResolutions (4 option/result/json/query + 1 module aliases)"
        );
    }

    // Prelude test 2: first IR targets std.option and has 3 bindings (Option, Some, None).
    #[test]
    fn prelude_first_ir_is_std_option_with_three_bindings() {
        let resolutions = super::prelude_resolutions();
        let opt_ir = &resolutions[0];
        assert!(
            matches!(
                opt_ir.target,
                ImportTarget::BuiltinStdlib(crate::stdlib_builtin::StdlibModuleId(7))
            ),
            "expected BuiltinStdlib(7) for std.option, got {:?}",
            opt_ir.target
        );
        assert_eq!(
            opt_ir.effective_bindings.len(),
            3,
            "expected 3 bindings for std.option prelude"
        );
        let names: Vec<&str> = opt_ir
            .effective_bindings
            .iter()
            .map(|eb| eb.local_name.as_str())
            .collect();
        assert!(names.contains(&"Option"), "missing Option");
        assert!(names.contains(&"Some"), "missing Some");
        assert!(names.contains(&"None"), "missing None");
    }

    // Prelude test 3: second IR targets std.result and has 3 bindings (Result, Ok, Err).
    #[test]
    fn prelude_second_ir_is_std_result_with_three_bindings() {
        let resolutions = super::prelude_resolutions();
        let res_ir = &resolutions[1];
        assert!(
            matches!(
                res_ir.target,
                ImportTarget::BuiltinStdlib(crate::stdlib_builtin::StdlibModuleId(8))
            ),
            "expected BuiltinStdlib(8) for std.result, got {:?}",
            res_ir.target
        );
        assert_eq!(
            res_ir.effective_bindings.len(),
            3,
            "expected 3 bindings for std.result prelude"
        );
        let names: Vec<&str> = res_ir
            .effective_bindings
            .iter()
            .map(|eb| eb.local_name.as_str())
            .collect();
        assert!(names.contains(&"Result"), "missing Result");
        assert!(names.contains(&"Ok"), "missing Ok");
        assert!(names.contains(&"Err"), "missing Err");
    }

    // Prelude test 3b: third IR targets std.json and carries JsonValue plus its
    // seven `J*` constructors as StdlibSymbol bindings.
    #[test]
    fn prelude_third_ir_is_std_json_with_eight_bindings() {
        let resolutions = super::prelude_resolutions();
        let json_ir = &resolutions[2];
        assert!(
            matches!(
                json_ir.target,
                ImportTarget::BuiltinStdlib(crate::stdlib_builtin::StdlibModuleId(17))
            ),
            "expected BuiltinStdlib(17) for std.json, got {:?}",
            json_ir.target
        );
        let names: Vec<&str> = json_ir
            .effective_bindings
            .iter()
            .map(|eb| eb.local_name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "JsonValue",
                "JNull",
                "JBool",
                "JInt",
                "JFloat",
                "JText",
                "JList",
                "JObject"
            ],
            "std.json prelude must bind the type name and seven constructors"
        );
    }

    // Prelude test 4: option/result/json/query prelude bindings (IRs 0–3) are
    // Binding::StdlibSymbol; module-alias prelude bindings (IR 4) are
    // Binding::ModuleAlias.
    #[test]
    fn prelude_binding_kinds_by_resolution() {
        let resolutions = super::prelude_resolutions();
        // IRs 0–3: option/result/json/query prelude — all StdlibSymbol, name == local_name.
        for ir in &resolutions[..4] {
            for eb in &ir.effective_bindings {
                match &eb.binding {
                    Binding::StdlibSymbol { name, .. } => {
                        assert_eq!(
                            name, &eb.local_name,
                            "StdlibSymbol name must equal local_name"
                        );
                    }
                    other => panic!(
                        "expected StdlibSymbol for '{}', got {:?}",
                        eb.local_name, other
                    ),
                }
            }
        }
        // IR 4: module-alias prelude — all ModuleAlias pointing to BuiltinStdlib targets.
        for eb in &resolutions[4].effective_bindings {
            match &eb.binding {
                Binding::ModuleAlias {
                    target: ImportTarget::BuiltinStdlib(_),
                    ..
                } => {}
                other => panic!(
                    "expected ModuleAlias(BuiltinStdlib) for '{}', got {:?}",
                    eb.local_name, other
                ),
            }
        }
    }

    // Prelude test 5: 1-module workspace with NO user imports → 5 prelude IRs,
    // 72 total bindings (6 from option/result prelude + 8 from json prelude +
    // 47 from quotation prelude + 11 module aliases).
    #[test]
    fn prelude_injected_when_no_user_imports() {
        // An empty module has no imports → all 72 prelude bindings should appear.
        let (_td, result) = resolve_single("");
        let module_imports = result.imports.first().expect("module 0");
        // Exactly 5 prelude IRs (option + result + json + quotation constructors,
        // module aliases).
        assert_eq!(
            module_imports.len(),
            5,
            "expected 5 prelude IRs for empty module; got {}",
            module_imports.len()
        );
        let total_bindings: usize = module_imports
            .iter()
            .map(|ir| ir.effective_bindings.len())
            .sum();
        assert_eq!(
            total_bindings, 72,
            "expected 72 total prelude bindings (6 option/result + 8 json + 47 quotation + 11 module aliases); got {total_bindings}"
        );
    }

    // Prelude test 6: `import std.option as Option` → prelude IR for std.option has 2 bindings
    // (Some, None — Option is suppressed because the user owns that name).
    #[test]
    fn prelude_option_suppressed_when_user_imports_option_alias() {
        let (_td, result) = resolve_single("import std.option as Option\n");
        assert!(
            result.resolve_errors.is_empty(),
            "errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0");

        // Find the prelude IR that targets std.option (BuiltinStdlib id 7).
        let opt_prelude_ir = module_imports.iter().find(|ir| {
            matches!(
                ir.target,
                ImportTarget::BuiltinStdlib(crate::stdlib_builtin::StdlibModuleId(7))
            ) && ir.alias.is_none()
                && ir.explicit_items.is_none()
        });

        let opt_ir = opt_prelude_ir.expect("expected a prelude IR for std.option");
        assert_eq!(
            opt_ir.effective_bindings.len(),
            2,
            "expected 2 prelude bindings for std.option (Option suppressed); got {:?}",
            opt_ir
                .effective_bindings
                .iter()
                .map(|eb| &eb.local_name)
                .collect::<Vec<_>>()
        );
        let names: Vec<&str> = opt_ir
            .effective_bindings
            .iter()
            .map(|eb| eb.local_name.as_str())
            .collect();
        assert!(!names.contains(&"Option"), "Option must be suppressed");
        assert!(names.contains(&"Some"), "Some must survive");
        assert!(names.contains(&"None"), "None must survive");
    }

    // Prelude test 7: `import std.result (Ok)` → prelude IR for std.result has 2 bindings
    // (Result, Err — Ok is suppressed because the user owns that name).
    #[test]
    fn prelude_ok_suppressed_when_user_imports_ok() {
        let (_td, result) = resolve_single("import std.result (Ok)\n");
        assert!(
            result.resolve_errors.is_empty(),
            "errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0");

        // Find the prelude IR that targets std.result (BuiltinStdlib id 8).
        let res_prelude_ir = module_imports.iter().find(|ir| {
            matches!(
                ir.target,
                ImportTarget::BuiltinStdlib(crate::stdlib_builtin::StdlibModuleId(8))
            ) && ir.alias.is_none()
                && ir.explicit_items.is_none()
        });

        let res_ir = res_prelude_ir.expect("expected a prelude IR for std.result");
        assert_eq!(
            res_ir.effective_bindings.len(),
            2,
            "expected 2 prelude bindings for std.result (Ok suppressed); got {:?}",
            res_ir
                .effective_bindings
                .iter()
                .map(|eb| &eb.local_name)
                .collect::<Vec<_>>()
        );
        let names: Vec<&str> = res_ir
            .effective_bindings
            .iter()
            .map(|eb| eb.local_name.as_str())
            .collect();
        assert!(!names.contains(&"Ok"), "Ok must be suppressed");
        assert!(names.contains(&"Result"), "Result must survive");
        assert!(names.contains(&"Err"), "Err must survive");
    }

    // ── Module-alias prelude tests ────────────────────────────────────────────

    // Prelude test 8: IR[4] has exactly 11 ModuleAlias bindings for
    // Int, Float, Decimal, Uuid, Bytes, Bool, Text, List, Map, Set, Json.
    #[test]
    fn prelude_r015_ir_has_eleven_module_aliases() {
        let resolutions = super::prelude_resolutions();
        let aliases_ir = &resolutions[4];
        assert_eq!(
            aliases_ir.effective_bindings.len(),
            11,
            "expected 11 module-alias prelude bindings; got {:?}",
            aliases_ir
                .effective_bindings
                .iter()
                .map(|eb| &eb.local_name)
                .collect::<Vec<_>>()
        );
        let names: Vec<&str> = aliases_ir
            .effective_bindings
            .iter()
            .map(|eb| eb.local_name.as_str())
            .collect();
        for expected in &[
            "Int", "Float", "Decimal", "Uuid", "Bytes", "Bool", "Text", "List", "Map", "Set",
            "Json",
        ] {
            assert!(names.contains(expected), "missing alias '{expected}'");
        }
    }

    // Prelude test 9: each module alias points to the correct StdlibModuleId.
    #[test]
    fn prelude_r015_aliases_point_to_correct_module_ids() {
        use crate::stdlib_builtin::StdlibModuleId;
        let resolutions = super::prelude_resolutions();
        let aliases_ir = &resolutions[4];

        let expected: &[(&str, u32)] = &[
            ("Int", 0),
            ("Float", 1),
            ("Bool", 2),
            ("Text", 3),
            ("List", 4),
            ("Map", 5),
            ("Set", 6),
            ("Json", 17),
            ("Decimal", 28),
            ("Uuid", 29),
            ("Bytes", 30),
        ];
        for (alias, expected_id) in expected {
            let eb = aliases_ir
                .effective_bindings
                .iter()
                .find(|eb| eb.local_name == *alias)
                .unwrap_or_else(|| panic!("missing prelude alias '{alias}'"));
            match &eb.binding {
                Binding::ModuleAlias {
                    target: ImportTarget::BuiltinStdlib(StdlibModuleId(id)),
                    ..
                } => {
                    assert_eq!(
                        id, expected_id,
                        "alias '{alias}' should point to StdlibModuleId({expected_id}), got {id}"
                    );
                }
                other => {
                    panic!("alias '{alias}' should be ModuleAlias(BuiltinStdlib), got {other:?}")
                }
            }
        }
    }

    // Prelude test 10: capability-bearing modules are NOT in the prelude.
    // Io, Http must not appear as any prelude ModuleAlias binding.
    #[test]
    fn prelude_r015_no_capability_module_aliases() {
        let resolutions = super::prelude_resolutions();
        for ir in &resolutions {
            for eb in &ir.effective_bindings {
                assert!(
                    eb.local_name != "Io",
                    "Io must NOT be in the prelude (capability-bearing)"
                );
                assert!(
                    eb.local_name != "Http",
                    "Http must NOT be in the prelude (capability-bearing)"
                );
                assert!(
                    eb.local_name != "Fs",
                    "Fs must NOT be in the prelude (capability-bearing)"
                );
                assert!(
                    eb.local_name != "Time",
                    "Time must NOT be in the prelude (capability-bearing)"
                );
                assert!(
                    eb.local_name != "Random",
                    "Random must NOT be in the prelude (capability-bearing)"
                );
            }
        }
    }

    // Prelude test 11: `import std.list as MyList` does NOT suppress
    // Int, Text, or any other alias (only the conflicting 'List' name would be
    // suppressed — but 'MyList' is a different name so nothing is suppressed).
    #[test]
    fn prelude_r015_different_alias_does_not_suppress_others() {
        let (_td, result) = resolve_single("import std.list as MyList\n");
        assert!(result.resolve_errors.is_empty());
        let module_imports = result.imports.first().expect("module 0");
        // Find the module-alias prelude IR.  Prelude IRs are synthetic: alias=None,
        // explicit_items=None.  Among those, find the one whose bindings are
        // all ModuleAlias (the option/result prelude IRs have StdlibSymbol).
        let aliases_ir = module_imports
            .iter()
            .find(|ir| {
                ir.alias.is_none()
                    && ir.explicit_items.is_none()
                    && ir
                        .effective_bindings
                        .iter()
                        .all(|eb| matches!(&eb.binding, Binding::ModuleAlias { .. }))
            })
            .expect("expected the module-alias prelude IR");
        let names: Vec<&str> = aliases_ir
            .effective_bindings
            .iter()
            .map(|eb| eb.local_name.as_str())
            .collect();
        // All 11 aliases must survive: 'MyList' is not a prelude name.
        assert_eq!(
            names.len(),
            11,
            "all 11 aliases must survive; got: {names:?}"
        );
        assert!(
            names.contains(&"List"),
            "List alias must survive when user imports 'std.list as MyList'"
        );
        assert!(names.contains(&"Int"), "Int alias must survive");
        assert!(names.contains(&"Text"), "Text alias must survive");
    }

    // Prelude test 12: `import std.list as List` DOES suppress the
    // prelude 'List' alias because the user already owns that local_name.
    #[test]
    fn prelude_r015_same_alias_suppresses_list() {
        let (_td, result) = resolve_single("import std.list as List\n");
        assert!(result.resolve_errors.is_empty());
        let module_imports = result.imports.first().expect("module 0");
        // The 'List' name must not appear in any prelude IR's effective_bindings.
        let prelude_list_binding = module_imports.iter().any(|ir| {
            // Skip user-declared IRs (they have alias or explicit_items set).
            if ir.alias.is_some() || ir.explicit_items.is_some() {
                return false;
            }
            ir.effective_bindings
                .iter()
                .any(|eb| eb.local_name == "List")
        });
        assert!(
            !prelude_list_binding,
            "'List' prelude alias must be suppressed when user imports 'std.list as List'"
        );
    }

    // Test 20: rate_limiter imports resolve clean
    #[test]
    fn t20_rate_limiter_imports_resolve_clean() {
        let example_src = format!(
            "{}/../../examples/rate_limiter.ridge",
            env!("CARGO_MANIFEST_DIR")
        );
        let src = std::fs::read_to_string(&example_src)
            .unwrap_or_else(|e| panic!("cannot read {example_src}: {e}"));

        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["apps/*"]));
        write_file(td.path(), "apps/demo/ridge.toml", &project_toml("demo"));
        write_file(td.path(), "apps/demo/src/rate_limiter.ridge", &src);

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = crate::build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        assert!(
            result.resolve_errors.is_empty(),
            "rate_limiter: R-errors: {:?}",
            result.resolve_errors
        );
        let module_imports = result.imports.first().expect("module 0 imports");
        for res in module_imports {
            assert_ne!(
                res.target,
                ImportTarget::Unresolved,
                "rate_limiter: unresolved import"
            );
        }
    }

    // ── T13 acceptance: R008 carries Levenshtein "did you mean?" suggestions ─

    /// `import std.list (mapp)` — the typo `mapp` should fire R008 with
    /// suggestions including `map` (distance 1).
    #[test]
    fn t13_r008_stdlib_typo_suggests_close_export() {
        let (_td, result) = resolve_single("import std.list (mapp)\n");
        let suggestions = result
            .resolve_errors
            .iter()
            .find_map(|(_, e)| match e {
                ResolveError::UnresolvedImportItem {
                    name, suggestions, ..
                } if name == "mapp" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected R008 for `mapp`");
        assert!(
            suggestions.contains(&"map".to_owned()),
            "R008 must suggest `map`; got {suggestions:?}"
        );
    }

    /// A wildly different typo against `std.list` produces no suggestions
    /// (every export is distance > 2 from `xyzqrs`).
    #[test]
    fn t13_r008_no_suggestion_when_distance_too_large() {
        let (_td, result) = resolve_single("import std.list (xyzqrs)\n");
        let suggestions = result
            .resolve_errors
            .iter()
            .find_map(|(_, e)| match e {
                ResolveError::UnresolvedImportItem {
                    name, suggestions, ..
                } if name == "xyzqrs" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected R008 for `xyzqrs`");
        assert!(
            suggestions.is_empty(),
            "no suggestion expected; got {suggestions:?}"
        );
    }

    /// Workspace-module visibility: `_private` items must NEVER appear in
    /// suggestions (plan §11 risk R14).
    #[test]
    fn t13_r008_does_not_leak_file_private_names() {
        let td = TempDir::new().expect("tempdir");
        write_file(td.path(), "ridge.toml", &workspace_toml(&["libs/*"]));
        write_file(td.path(), "libs/proj/ridge.toml", &project_toml("proj"));
        // B exports a public `helper` and a file-private `_helpr`; A imports
        // a typo `helpor` from B.  Distance(`helpor`, `helper`) = 2 (hit);
        // distance(`helpor`, `_helpr`) = 2 → would also hit if visible, but
        // visibility filtering must drop the underscored one.
        write_file(
            td.path(),
            "libs/proj/src/A.ridge",
            "import proj.B (helpor)\n",
        );
        write_file(
            td.path(),
            "libs/proj/src/B.ridge",
            "pub fn helper x = x\nfn _helpr x = x\n",
        );

        let disc = crate::discover_workspace(td.path());
        let mut ws = disc.graph.expect("graph");
        let g = build_module_graph(&ws);
        let symbol_tables: Vec<SymbolTable> = g
            .modules
            .iter()
            .map(|pm| {
                let (t, _) = collect_symbols(pm.id, &pm.ast);
                t
            })
            .collect();
        let result = resolve_imports(&mut ws, &g, &symbol_tables);

        let suggestions = result
            .resolve_errors
            .iter()
            .find_map(|(_, e)| match e {
                ResolveError::UnresolvedImportItem {
                    name, suggestions, ..
                } if name == "helpor" => Some(suggestions.clone()),
                _ => None,
            })
            .expect("expected R008 for `helpor`");
        assert!(
            suggestions.contains(&"helper".to_owned()),
            "must suggest the visible `helper`; got {suggestions:?}"
        );
        assert!(
            !suggestions.contains(&"_helpr".to_owned()),
            "must NOT leak file-private `_helpr`; got {suggestions:?}"
        );
        drop(td);
    }
}
