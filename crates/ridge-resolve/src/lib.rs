//! Ridge name-resolution crate. Phase 3.
//!
//! Transforms in-memory [`ridge_ast::Module`]s produced by `ridge-parser`
//! into a fully-resolved workspace in which every identifier, import, and
//! architectural rule is bound (or carries a carried `R###` diagnostic).
//!
//! # Front-door entry points
//!
//! ```rust,ignore
//! let disc = ridge_resolve::discover_workspace(root)?;
//! let ws = disc.graph.expect("workspace manifest found");
//! let resolved = ridge_resolve::resolve_workspace(ws);
//! for (mid, err) in &resolved.errors { /* render — mid identifies the source module */ }
//! ```
//!
//! [`resolve_workspace`] orchestrates all resolver passes in plan-spec order.
//! [`resolve_module`] resolves a single module given an already-discovered
//! workspace (for incremental / LSP use-cases).
//!
//! # Phase 3 modules
//!
//! Error taxonomy and opaque newtype IDs.
//! [`manifest`] and [`globs`] — workspace/project manifest parsing
//! and compiled module-path glob patterns.
//! [`discovery`] — filesystem walk, module FQN derivation, and
//! [`WorkspaceGraph`] construction (edges populated by the module-graph pass).
//! [`module_graph`] — parse every module, collect tentative edges.
//! [`module_graph::detect_cycles`] — iterative Tarjan SCC.
//! [`symbol`] — per-module top-level symbol collection.
//! [`imports`] + [`stdlib_builtin`] — import resolution, visibility,
//! manifest cross-validation (M013/M015), and authoritative cycle detection.
//! [`node_id`] + [`scope`] + [`walker`] — `NodeId` assignment,
//! lexical scope stack, and intra-module use-site binding.
//! [`qualified`] — qualified-name resolution (`Mod.symbol`).
//! [`capabilities`] — capability-keyword allow/deny enforcement.
//! §4.8 shadowing policy — `R011 DuplicateLocal` for
//! same-scope duplicates (incl. duplicates within a single pattern), and
//! `R017 StateFieldShadowedByLocal` as a [`Severity::Warning`] for actor
//! state vs handler-local shadowing.
//! [`forbid`] — workspace `[workspace.rules].forbid` rule
//! enforcement (`R013 ForbidViolation`) over every resolved import edge.
//! [`suggest`] — Damerau-Levenshtein "did you mean?" engine
//! wired into `R008`, `R010`, `R012`, and `R014` diagnostics so every
//! name-resolution miss surfaces up to 3 distance-≤ 2 suggestions.
//! Deterministic `insta` snapshot tests for the four canonical
//! example programs and the two synthetic `acme_*` workspace fixtures
//! (`tests/snapshots.rs`, `tests/workspace.rs`).  Snapshots
//! capture the post-pipeline `R-error` set, per-binding-kind counts, and
//! import-alias summary so any drift in resolver behaviour is caught by
//! `cargo insta test`.
//! Per-`R###` negative-fixture harness in `tests/errors.rs`
//! covering every reachable diagnostic from §5.1 of the plan, plus the
//! `R021 ActorStateMissingDefaultOrInit` emitter wired into [`symbol`].
//! [`resolve_workspace`] + [`resolve_module`] front-door API;
//! [`ResolvedWorkspace`], [`ResolvedModule`], [`ModuleResolveResult`], and
//! [`BindingMap`] / [`ScopeTree`] type aliases.
//! [`SymbolEntry::exported_externally`] flag, populated by
//! [`apply_external_exports`] post-pass in [`resolve_workspace`];
//! `M020 ExportNotFound` manifest error for non-`pub` export patterns.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod capabilities;
pub mod decl;
pub mod discovery;
pub mod error;
pub mod forbid;
pub mod globs;
pub mod imports;
pub mod manifest;
pub mod module_graph;
pub mod node_id;
pub mod qualified;
pub mod scope;
pub mod stdlib_builtin;
pub mod suggest;
pub mod symbol;
pub mod visibility;
pub mod walker;

pub use capabilities::check_capabilities;
pub use decl::check_ffi_outside_stdlib;
pub use discovery::discover_workspace;
pub use error::{ManifestError, ResolveError, Severity};
pub use forbid::check_forbid_rules;
pub use globs::GlobPattern;
pub use imports::{
    detect_cycles_authoritative, prelude_resolutions, resolve_imports, Binding, EffectiveBinding,
    ImportResolution, ImportResolutionResult, ImportTarget, ImportedItem,
};
pub use manifest::{
    parse_project_manifest, parse_workspace_manifest, ForbidRule, GitRev, Project,
    ProjectDependency, ProjectKind, SharedDependency, WorkspaceManifest,
};
pub use module_graph::{
    build_module_graph, detect_cycles, ModuleGraph, ParsedModule, TentativeEdge,
};
pub use node_id::{assign_node_ids, NodeIdMap, NodeKind};
pub use qualified::{resolve_qualified_name, resolve_qualified_record_constructor};
pub use scope::{
    LocalEntry, LocalId, LocalKind, Scope, ScopeIndex, ScopeKind, ScopeNode, ScopeStack,
};
pub use stdlib_builtin::{lookup_stdlib, BuiltinStdlibModule, StdlibModuleId, BUILTINS};
pub use symbol::{
    apply_external_exports, collect_symbols, ClassMethodIndex, HandlerSig, StateField, SymbolEntry,
    SymbolKind, SymbolTable,
};
pub use visibility::{resolve_visibility, ResolvedVisibility};
pub use walker::resolve_module_uses;

// ── Type aliases (DR-01) ─────────────────────────────────────────────────────

/// Bindings side-table for one module, indexed by `NodeId.0`.
///
/// Produced by [`resolve_module_uses`] (walker pass).  `None` entries
/// are AST positions that carry no resolvable name (e.g. literal tokens).
/// Phase 4 (type checker) reads this table to locate every identifier's
/// definition site.
pub type BindingMap = Vec<Option<imports::Binding>>;

/// Persisted lexical scope tree for one module.
///
/// Empty unless the walker was asked to record scopes (`retain_indices`), which
/// the LSP enables to answer "which locals are visible at this offset". The
/// batch compiler leaves it empty.
pub type ScopeTree = scope::ScopeIndex;

// ── Workspace-level artefacts ────────────────────────────────────────────────

/// A fully-walked workspace: manifest, projects, modules, no import edges yet.
///
/// `deps` is populated by the module-graph pass.  Discovery leaves it
/// as `vec![vec![]; modules.len()]`.
#[derive(Debug)]
pub struct WorkspaceGraph {
    /// Absolute path to the workspace root directory.
    pub root: std::path::PathBuf,
    /// Parsed workspace manifest.
    pub manifest: WorkspaceManifest,
    /// All projects in the workspace, indexed by `ProjectId.0`.
    pub projects: Vec<manifest::Project>,
    /// All modules across all projects, sorted by `fully_qualified_name`.
    ///
    /// Sorted for snapshot stability (plan §4.1 invariant).
    pub modules: Vec<ModuleMetadata>,
    /// Directed module-dependency edges: `deps[a]` = modules that `a` imports.
    ///
    /// Discovery initialises this as `vec![vec![]; modules.len()]`.
    /// The module-graph pass fills the actual edges after import resolution.
    pub deps: Vec<Vec<ModuleId>>,
    /// Whether this workspace is the Ridge standard library.
    ///
    /// Discovery sets this to `false`; the stdlib build paths flip it to `true`
    /// after discovery. It gates the `@ffi` privilege (R022): standard-library
    /// modules may declare `@ffi`, user code may not. The flag is threaded from
    /// the driver instead of being inferred from the source path, which cannot
    /// be trusted (the stdlib is built from copied sources under a throwaway
    /// path, and a user directory could be named `ridge-stdlib`).
    pub is_stdlib: bool,
}

/// Metadata for a single `.ridge` source file discovered during the filesystem walk.
#[derive(Debug)]
pub struct ModuleMetadata {
    /// Stable module index within the workspace.
    pub id: ModuleId,
    /// Which project this module belongs to.
    pub project: ProjectId,
    /// Fully-qualified dot-separated module name, e.g. `"acme.domain.Models.User"`.
    pub fully_qualified_name: String,
    /// Absolute path to the `.ridge` source file.
    pub file_path: std::path::PathBuf,
    /// Byte span covering the entire module source.
    ///
    /// Set to `Span::point(0)` (placeholder) by discovery. The module-graph pass
    /// fills `0..eof` after reading the source file.
    pub span_within_file: ridge_ast::Span,
}

/// The result of a [`discover_workspace`] call.
///
/// Non-fatal policy: a manifest error for one project does NOT abort discovery
/// for others.  Bad projects are skipped and errors accumulated here.
#[derive(Debug)]
pub struct DiscoveryResult {
    /// The partially or fully constructed workspace graph.
    ///
    /// `None` only when R001 fires (no workspace manifest found at all).
    pub graph: Option<WorkspaceGraph>,
    /// Manifest-level errors accumulated across all projects.
    pub manifest_errors: Vec<ManifestError>,
    /// Resolve-level errors (R001, R002, …) accumulated during discovery.
    pub resolve_errors: Vec<ResolveError>,
}

// ── Opaque newtype IDs ────────────────────────────────────────────────────────

/// Newtype stamped onto every `Ident` / `QualifiedName` / capability position
/// of the parsed AST post-`assign_node_ids`. Index into per-module side-tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

/// Newtype index identifying a module within a [`WorkspaceResolveResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u32);

/// Newtype index identifying a symbol within a symbol table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId(pub u32);

/// Newtype index identifying a project within a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectId(pub u32);

// ── Result type ───────────────────────────────────────────────────────────────

/// Result of resolving an entire workspace.
///
/// Populated by the `discover_workspace` → `build_module_graph` →
/// `collect_symbols` → `resolve_imports` → `assign_node_ids` →
/// `resolve_module_uses` → `check_capabilities` → `check_forbid_rules`
/// pipeline.
#[derive(Debug, Default)]
pub struct WorkspaceResolveResult {
    /// Resolve-layer diagnostics accumulated during the pass.
    pub errors: Vec<ResolveError>,
    /// Manifest-level diagnostics (TOML parse, rule-syntax errors).
    pub manifest_errors: Vec<ManifestError>,
}

// ── DR-01: Public API types ───────────────────────────────────────────────────

/// The fully-resolved view of one module produced by the resolve pipeline.
///
/// Phase 4 (type checker) reads `symbols`, `imports`, and `bindings` to locate
/// definition sites; Phase 8 (LSP) reads them for hover / go-to-definition.
#[derive(Debug)]
pub struct ResolvedModule {
    /// The module's stable index within the workspace.
    pub id: ModuleId,
    /// Top-level symbol table built by the symbol collector.
    pub symbols: symbol::SymbolTable,
    /// Resolved imports for this module.
    pub imports: Vec<imports::ImportResolution>,
    /// Scope snapshot after the walker pass.
    ///
    /// Currently empty — the walker's [`scope::ScopeStack`] is discarded after
    /// use.  TODO(Phase 4): retain for type-checker scope queries.
    pub scopes: ScopeTree,
    /// Node-id–indexed binding side-table produced by the walker.
    pub bindings: BindingMap,
    /// The `(Span, NodeKind) → NodeId` map stamped by [`assign_node_ids`] for
    /// this module.
    ///
    /// Retained so the type checker can consume it instead of rebuilding, and so
    /// the LSP can map a source position to a `NodeId` for hover,
    /// go-to-definition, and completion.
    pub node_ids: NodeIdMap,
}

/// Result of resolving a single module in the context of an already-resolved
/// workspace.
///
/// The companion to [`resolve_workspace`] for incremental or LSP-mode usage.
#[derive(Debug)]
pub struct ModuleResolveResult {
    /// The resolved module.
    pub module: ResolvedModule,
    /// All `R###` diagnostics produced while resolving this module.
    pub errors: Vec<ResolveError>,
}

/// The fully-resolved workspace produced by [`resolve_workspace`].
///
/// Contains all per-module resolutions, accumulated diagnostics, and the
/// workspace graph (manifests, projects, dependency edges) for Phase 4 / LSP.
#[derive(Debug)]
pub struct ResolvedWorkspace {
    /// All resolved modules, indexed by `ModuleId.0`.
    pub modules: Vec<ResolvedModule>,
    /// The underlying workspace graph (manifests, projects, dep edges).
    pub graph: WorkspaceGraph,
    /// `M###` manifest-level diagnostics accumulated during resolution.
    pub manifest_errors: Vec<ManifestError>,
    /// All `R###` diagnostics accumulated across every resolver pass, paired with
    /// the originating [`ModuleId`] for source-file attribution in the driver.
    pub errors: Vec<(ModuleId, ResolveError)>,
    /// Parse errors per source module, captured from `ridge-parser` during
    /// the module-graph pass.  Surfaced here so downstream consumers
    /// (driver, LSP) can render them — without this the parse-error path is
    /// silent and `ridge check` falsely reports success on syntactically
    /// invalid sources.
    pub parse_errors: Vec<(ModuleId, ridge_parser::ParseError)>,
    /// Lexer errors per source module, captured from `ridge-lexer` during
    /// the module-graph pass.  Same rationale as `parse_errors`.
    pub lex_errors: Vec<(ModuleId, ridge_lexer::LexError)>,
    /// Parsed AST for every module, retained from the module-graph pass and
    /// indexed by `ModuleId.0` (parallel to `modules`).
    ///
    /// This holds the same `Arc<Module>` the resolver parsed, so later passes
    /// (the type-checker, the LSP) can reuse it instead of reading and parsing
    /// every source file a second time. Entry `i` is the module `ModuleId(i)`.
    pub module_asts: Vec<std::sync::Arc<ridge_ast::Module>>,
}

// ── DR-01: Public entry points ────────────────────────────────────────────────

/// Resolve an entire workspace, running the full pass sequence.
///
/// Pass sequence (plan §2.2):
/// 1. `collect_symbols` (per module); then `apply_external_exports`
/// 2. `resolve_imports` (workspace-wide + M013/M015 cross-validation)
/// 3. `detect_cycles_authoritative` (authoritative cycle detection)
/// 4. `assign_node_ids` + `resolve_module_uses` (per module)
/// 5. `check_capabilities` (per module)
/// 6. `check_forbid_rules` (workspace-wide)
///
/// Returns a [`ResolvedWorkspace`] bundling all per-module results and
/// accumulated diagnostics.  Never panics; all errors are returned in
/// [`ResolvedWorkspace::errors`] / [`ResolvedWorkspace::manifest_errors`].
#[must_use]
pub fn resolve_workspace(ws: WorkspaceGraph) -> ResolvedWorkspace {
    resolve_workspace_with(ws, false)
}

/// Resolve a workspace, optionally recording per-module scope trees.
///
/// Identical to [`resolve_workspace`] except that when `retain_indices` is set
/// each module's [`ScopeTree`] is populated from the walker. The LSP enables
/// this so completion can enumerate locals in scope; the batch compiler passes
/// `false` and pays nothing for the scope tree.
#[must_use]
pub fn resolve_workspace_with(ws: WorkspaceGraph, retain_indices: bool) -> ResolvedWorkspace {
    let mut all_errors: Vec<(ModuleId, ResolveError)> = Vec::new();
    let mut all_manifest_errors: Vec<ManifestError> = Vec::new();

    // Build module graph (parse source files, collect tentative edges).
    let g = module_graph::build_module_graph(&ws);

    // Capture parse + lex errors per module.  These were silently dropped by
    // earlier revisions, causing `ridge check`/`ridge build` to false-OK any
    // syntactically invalid source.  Surfacing them here lets the driver
    // include them in its diagnostics output.
    let mut all_parse_errors: Vec<(ModuleId, ridge_parser::ParseError)> = Vec::new();
    let mut all_lex_errors: Vec<(ModuleId, ridge_lexer::LexError)> = Vec::new();
    for pm in &g.modules {
        for e in &pm.parse_errors {
            all_parse_errors.push((pm.id, e.clone()));
        }
        for e in &pm.lex_errors {
            all_lex_errors.push((pm.id, e.clone()));
        }
    }

    // Collect top-level symbols for every module.
    let mut symbol_tables: Vec<symbol::SymbolTable> = Vec::with_capacity(g.modules.len());
    for pm in &g.modules {
        let (mut table, errs) = symbol::collect_symbols(pm.id, &pm.ast);
        all_errors.extend(errs.into_iter().map(|e| (pm.id, e)));

        // DR-08 post-pass: cross-reference [project.exports].public.
        let project_idx = ws.modules[pm.id.0 as usize].project.0 as usize;
        let project = &ws.projects[project_idx];
        let export_errors = symbol::apply_external_exports(
            &mut table,
            &project.exports_public,
            &project.manifest_path,
        );
        all_manifest_errors.extend(export_errors);

        symbol_tables.push(table);
    }

    // Resolve imports (also validates M013/M015).
    let mut ws = ws;
    let import_result = imports::resolve_imports(&mut ws, &g, &symbol_tables);
    all_errors.extend(import_result.resolve_errors);
    all_manifest_errors.extend(import_result.manifest_errors);

    // Authoritative cycle detection over resolved import edges.
    // Returns Vec<(ModuleId, ResolveError)> — already in the right shape.
    let cycle_errors = imports::detect_cycles_authoritative(&ws, &import_result.imports);
    all_errors.extend(cycle_errors);

    // Build the workspace-scoped class method index from all parsed modules.
    // The index maps bare method names to the single class that declares them;
    // cross-class name collisions are recorded so R024 fires at the use site.
    let all_asts: Vec<&ridge_ast::Module> = g.modules.iter().map(|pm| &*pm.ast).collect();
    let class_method_index = symbol::ClassMethodIndex::build(&all_asts);

    // Collect R024 errors for ambiguous method names discovered during indexing.
    // These are workspace-level errors (no per-module attribution needed); we
    // attribute them to module 0 as a sentinel since they have no use-site yet.
    // The actual per-use-site R024 is emitted by the walker below.
    // (No workspace-level R024 needed here — the walker emits them at use sites.)

    // NodeId assignment + walker + capability enforcement + build ResolvedModule per module.
    let mut resolved_modules: Vec<ResolvedModule> = Vec::with_capacity(g.modules.len());
    for pm in &g.modules {
        let (nid_map, nid_errors) = node_id::assign_node_ids(&pm.ast);
        all_errors.extend(nid_errors.into_iter().map(|e| (pm.id, e)));

        let module_imports = import_result
            .imports
            .get(pm.id.0 as usize)
            .map_or([].as_slice(), Vec::as_slice);

        // Walker + qualified-name resolution.
        let (bindings, walker_errors, scopes) = walker::resolve_module_uses(
            pm.id,
            &pm.ast,
            &nid_map,
            &symbol_tables,
            module_imports,
            Some(&class_method_index),
            retain_indices,
        );
        all_errors.extend(walker_errors.into_iter().map(|e| (pm.id, e)));

        // Capability enforcement.
        let project_idx = ws.modules[pm.id.0 as usize].project.0 as usize;
        let project = &ws.projects[project_idx];
        let mut cap_errors = Vec::new();
        capabilities::check_capabilities(&pm.ast, project, &ws.manifest, &mut cap_errors);
        all_errors.extend(cap_errors.into_iter().map(|e| (pm.id, e)));

        // `@ffi` gate (R022). User-authored modules may not declare `@ffi`;
        // only the standard library can. Whether this workspace is the stdlib
        // is decided by the driver and carried on the graph, not guessed from
        // the source path.
        let ffi_errors = decl::check_ffi_outside_stdlib(&pm.ast, ws.is_stdlib);
        all_errors.extend(ffi_errors.into_iter().map(|e| (pm.id, e)));

        let module_imports_owned: Vec<imports::ImportResolution> = import_result
            .imports
            .get(pm.id.0 as usize)
            .cloned()
            .unwrap_or_default();

        resolved_modules.push(ResolvedModule {
            id: pm.id,
            symbols: symbol_tables
                .get(pm.id.0 as usize)
                .cloned()
                .unwrap_or_else(|| symbol::SymbolTable::empty(pm.id)),
            imports: module_imports_owned,
            scopes,
            bindings,
            node_ids: nid_map,
        });
    }

    // Forbid-rule enforcement.
    let mut forbid_errors: Vec<(ModuleId, ResolveError)> = Vec::new();
    forbid::check_forbid_rules(&ws, &import_result.imports, &mut forbid_errors);
    all_errors.extend(forbid_errors);

    ResolvedWorkspace {
        modules: resolved_modules,
        graph: ws,
        manifest_errors: all_manifest_errors,
        errors: all_errors,
        parse_errors: all_parse_errors,
        lex_errors: all_lex_errors,
        module_asts: g
            .modules
            .iter()
            .map(|pm| std::sync::Arc::clone(&pm.ast))
            .collect(),
    }
}

/// Resolve a single module in the context of an already-resolved workspace.
///
/// Runs `assign_node_ids` + `resolve_module_uses` and `check_capabilities`
/// for the module identified by `id`.  Does **not**
/// re-run symbol collection or import resolution (pre-computed in `ws`).
///
/// Intended for incremental / LSP re-resolution of a single file after an
/// edit.  For a full workspace resolution use [`resolve_workspace`].
#[must_use]
pub fn resolve_module(ws: &WorkspaceGraph, id: ModuleId) -> ModuleResolveResult {
    let mut errors: Vec<ResolveError> = Vec::new();

    // Parse the single module's source.
    let g = module_graph::build_module_graph(ws);

    // Find the parsed module entry.
    let Some(pm) = g.modules.iter().find(|pm| pm.id == id) else {
        return ModuleResolveResult {
            module: ResolvedModule {
                id,
                symbols: symbol::SymbolTable::empty(id),
                imports: Vec::new(),
                scopes: ScopeIndex::new(),
                bindings: Vec::new(),
                node_ids: NodeIdMap::default(),
            },
            errors,
        };
    };

    // Collect symbols for this module only.
    let (symbols, sym_errs) = symbol::collect_symbols(pm.id, &pm.ast);
    errors.extend(sym_errs);

    // Assign node ids + walker pass.
    let (nid_map, nid_errors) = node_id::assign_node_ids(&pm.ast);
    errors.extend(nid_errors);

    // Build a minimal class method index from this single module.
    let single_ast: Vec<&ridge_ast::Module> = vec![&pm.ast];
    let cmi = symbol::ClassMethodIndex::build(&single_ast);

    let all_tables = vec![symbol::SymbolTable::empty(id)];
    let (bindings, walker_errors, scopes) =
        walker::resolve_module_uses(pm.id, &pm.ast, &nid_map, &all_tables, &[], Some(&cmi), true);
    errors.extend(walker_errors);

    // T10: capability enforcement.
    let project_idx = ws.modules[pm.id.0 as usize].project.0 as usize;
    let project = &ws.projects[project_idx];
    let mut cap_errors = Vec::new();
    capabilities::check_capabilities(&pm.ast, project, &ws.manifest, &mut cap_errors);
    errors.extend(cap_errors);

    ModuleResolveResult {
        module: ResolvedModule {
            id,
            symbols,
            imports: Vec::new(), // No import-resolution context available in single-module mode
            scopes,
            bindings,
            node_ids: nid_map,
        },
        errors,
    }
}

/// Incrementally re-resolve a single edited module against an already-resolved
/// workspace, updating `cached` in place.
///
/// Unlike [`resolve_module`], this sees the full workspace context. It reuses
/// every unchanged module's cached symbol table and AST (parsing nothing),
/// re-collects the edited module's symbols, and re-runs import resolution for
/// the whole workspace so the edited module's cross-module references bind
/// exactly as a from-scratch [`resolve_workspace`] would. Only the edited
/// module's walker pass runs; its `ResolvedModule`, the workspace dependency
/// edges (`graph.deps`), and the retained AST are written back into `cached`.
/// The returned vector holds the edited module's `R###` diagnostics.
///
/// This re-resolves only the edited module. A caller that changes a module's
/// public surface is responsible for re-resolving the reverse-dependencies that
/// import it — this function does not chase that closure.
#[must_use]
pub fn resolve_module_incremental(
    cached: &mut ResolvedWorkspace,
    edited_id: ModuleId,
    edited_ast: &std::sync::Arc<ridge_ast::Module>,
    retain_indices: bool,
) -> Vec<ResolveError> {
    let n = cached.modules.len();
    let ei = edited_id.0 as usize;
    if ei >= n {
        return Vec::new();
    }

    let mut errors: Vec<ResolveError> = Vec::new();

    // Re-collect the edited module's symbols (+ external exports), exactly as the
    // workspace pass does for every module.
    let (mut edited_symbols, sym_errs) = symbol::collect_symbols(edited_id, edited_ast);
    errors.extend(sym_errs);
    let project_idx = cached.graph.modules[ei].project.0 as usize;
    {
        let project = &cached.graph.projects[project_idx];
        // External-export validation is a manifest-level concern; the symbol
        // table it produces is what matters for resolution, so the M### errors
        // are not threaded into this module's R### result.
        let _ = symbol::apply_external_exports(
            &mut edited_symbols,
            &project.exports_public,
            &project.manifest_path,
        );
    }

    // Full symbol-table vector: every unchanged module's cached table, plus the
    // edited module's freshly collected one.
    let symbol_tables: Vec<symbol::SymbolTable> = (0..n)
        .map(|i| {
            if i == ei {
                edited_symbols.clone()
            } else {
                cached.modules[i].symbols.clone()
            }
        })
        .collect();

    // Reconstruct the workspace's tentative import edges from the retained ASTs
    // (the edited module from its new AST), in the same per-module order
    // `build_module_graph` produces, then re-resolve imports for the whole
    // workspace. This recomputes `graph.deps` and every module's import
    // resolutions without parsing anything.
    let mut tentative_edges: Vec<TentativeEdge> = Vec::new();
    for i in 0..n {
        let ast = if i == ei {
            edited_ast.as_ref()
        } else {
            cached.module_asts[i].as_ref()
        };
        let mid = ModuleId(u32::try_from(i).unwrap_or(u32::MAX));
        tentative_edges.extend(module_graph::collect_import_edges(mid, ast));
    }
    let g = ModuleGraph {
        modules: Vec::new(),
        tentative_edges,
    };
    let import_result = imports::resolve_imports(&mut cached.graph, &g, &symbol_tables);
    errors.extend(
        import_result
            .resolve_errors
            .into_iter()
            .filter(|(m, _)| *m == edited_id)
            .map(|(_, e)| e),
    );

    // Assign node ids and run the walker for the edited module only, against the
    // full symbol tables and its resolved imports.
    let (nid_map, nid_errors) = node_id::assign_node_ids(edited_ast);
    errors.extend(nid_errors);

    let all_asts: Vec<&ridge_ast::Module> = (0..n)
        .map(|i| {
            if i == ei {
                edited_ast.as_ref()
            } else {
                cached.module_asts[i].as_ref()
            }
        })
        .collect();
    let class_method_index = symbol::ClassMethodIndex::build(&all_asts);

    let edited_imports: Vec<imports::ImportResolution> =
        import_result.imports.get(ei).cloned().unwrap_or_default();

    let (bindings, walker_errors, scopes) = walker::resolve_module_uses(
        edited_id,
        edited_ast,
        &nid_map,
        &symbol_tables,
        &edited_imports,
        Some(&class_method_index),
        retain_indices,
    );
    errors.extend(walker_errors);

    // Capability and `@ffi` enforcement for the edited module.
    let project = &cached.graph.projects[project_idx];
    let mut cap_errors = Vec::new();
    capabilities::check_capabilities(edited_ast, project, &cached.graph.manifest, &mut cap_errors);
    errors.extend(cap_errors);
    errors.extend(decl::check_ffi_outside_stdlib(
        edited_ast,
        cached.graph.is_stdlib,
    ));

    // Write the freshly resolved module and its AST back into the cache.
    cached.modules[ei] = ResolvedModule {
        id: edited_id,
        symbols: edited_symbols,
        imports: edited_imports,
        scopes,
        bindings,
        node_ids: nid_map,
    };
    cached.module_asts[ei] = std::sync::Arc::clone(edited_ast);

    errors
}
