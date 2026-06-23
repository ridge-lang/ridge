//! Retained analysis index for editor queries.
//!
//! After each successful compile the server keeps a [`WorkspaceIndex`]: the
//! fully type-checked and resolved workspace plus a per-module spatial index
//! that maps a byte offset to the narrowest enclosing [`NodeId`]. Hover,
//! go-to-definition, and completion read this index instead of re-running the
//! compiler.
//!
//! The index is immutable once built. A new compile generation produces a fresh
//! [`WorkspaceIndex`] that wholesale-replaces the previous one behind an
//! `Arc`, so a query that cloned the `Arc` always sees a complete, consistent
//! snapshot even while a newer compile is in flight.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use ridge_driver::WorkspaceSourceCache;
use ridge_lexer::{LineIndex, Span};
use ridge_resolve::imports::{Binding, ImportResolution, ImportTarget};
use ridge_resolve::{
    LocalId, LocalKind, ModuleId, NodeId, NodeIdMap, NodeKind, ProjectKind, ResolvedVisibility,
    ResolvedWorkspace, ScopeIndex, StdlibModuleId, SymbolKind, SymbolTable, BUILTINS,
};
use ridge_typecheck::{render_type_with, TypeError, TypedWorkspace};
use ridge_types::{CapabilitySet, TyConDecl, TyConKind, Type};
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, CodeLens, Command,
    CompletionItemKind, DocumentHighlight, DocumentHighlightKind, DocumentSymbol, FileRename,
    FoldingRange, FoldingRangeKind, InlayHint, InlayHintKind, InlayHintLabel, Location,
    ParameterInformation, ParameterLabel, Position, PrepareRenameResponse, Range, SelectionRange,
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensEdit,
    SignatureHelp, SignatureInformation, SymbolInformation, SymbolKind as LspSymbolKind, TextEdit,
    TypeHierarchyItem, Url, WorkspaceEdit,
};

use crate::cancel::Cancel;
use crate::completion::{detect_context, symbol_kind, CompletionItemData, Context, KEYWORDS};
use crate::diagnostics::{source_id_to_uri, uri_key};

/// A position-indexed view of one module's stamped nodes.
///
/// Backs the `byte offset → NodeId` lookup. Entries are sorted by `(start, end)`
/// so the narrowest-span tie-break is deterministic.
#[derive(Debug)]
pub struct NodeSpatialIndex {
    /// `(span, kind, id)` for every node the resolver stamped in this module.
    entries: Vec<(Span, NodeKind, NodeId)>,
}

impl NodeSpatialIndex {
    /// Build a spatial index from a module's [`NodeIdMap`].
    #[must_use]
    pub fn from_node_ids(map: &NodeIdMap) -> Self {
        let mut entries: Vec<(Span, NodeKind, NodeId)> = map.iter().collect();
        entries.sort_by_key(|(span, _, _)| (span.start, span.end));
        Self { entries }
    }

    /// Return the narrowest node whose span contains `offset` and whose kind is
    /// listed in `prefer`.
    ///
    /// When `prefer` is empty, every kind is eligible. Returns `None` when no
    /// eligible node covers the offset (whitespace, a comment, a keyword, or a
    /// position past the end of the source).
    #[must_use]
    pub fn narrowest_containing(
        &self,
        offset: u32,
        prefer: &[NodeKind],
    ) -> Option<(NodeId, NodeKind, Span)> {
        self.entries
            .iter()
            .filter(|(span, kind, _)| {
                span.start <= offset
                    && offset < span.end
                    && (prefer.is_empty() || prefer.contains(kind))
            })
            .min_by_key(|(span, _, _)| span.end - span.start)
            .map(|&(span, kind, id)| (id, kind, span))
    }

    /// All nodes covering `offset` whose kind is in `prefer`, narrowest first.
    ///
    /// Used when the wanted node is the narrowest one that *also* carries some
    /// data (e.g. a binding): a qualified name records its binding on the whole
    /// `QualifiedName` node, which is wider than the segment idents under the
    /// cursor.
    fn enclosing(&self, offset: u32, prefer: &[NodeKind]) -> Vec<(NodeId, NodeKind, Span)> {
        let mut hits: Vec<(NodeId, NodeKind, Span)> = self
            .entries
            .iter()
            .filter(|(span, kind, _)| {
                span.start <= offset
                    && offset < span.end
                    && (prefer.is_empty() || prefer.contains(kind))
            })
            .map(|&(span, kind, id)| (id, kind, span))
            .collect();
        hits.sort_by_key(|(_, _, span)| span.end - span.start);
        hits
    }

    /// The id of a node of kind `kind` whose span is exactly `span`, or — when
    /// none matches exactly — the widest such node contained within `span`.
    ///
    /// Used to recover the inferred type stamped on a specific sub-expression
    /// (the base of a `base.field` access) whose span the AST hands us directly.
    fn node_for_span_or_inner(&self, span: Span, kind: NodeKind) -> Option<NodeId> {
        let mut widest: Option<(u32, NodeId)> = None;
        for &(s, k, id) in &self.entries {
            if k != kind {
                continue;
            }
            if s == span {
                return Some(id);
            }
            if s.start >= span.start && s.end <= span.end {
                let width = s.end - s.start;
                if widest.is_none_or(|(w, _)| width > w) {
                    widest = Some((width, id));
                }
            }
        }
        widest.map(|(_, id)| id)
    }
}

/// The per-module data the editor-query methods read.
///
/// Extracted (cloned) from the resolved and typed workspaces at build time so
/// the index does not own — or pin — the workspaces that the incremental engine
/// keeps mutating. Indexed by `ModuleId.0`.
#[derive(Debug)]
struct ModuleView {
    /// Type stamped on each expression `NodeId`, indexed by `NodeId.0`.
    node_types: Vec<Option<ridge_types::Type>>,
    /// Binding stamped on each name `NodeId`, indexed by `NodeId.0`.
    bindings: Vec<Option<Binding>>,
    /// This module's top-level symbol table.
    symbols: SymbolTable,
    /// This module's lexical scope tree (for locals-in-scope completion).
    scopes: ScopeIndex,
    /// This module's resolved imports.
    imports: Vec<ImportResolution>,
    /// The module's typed AST, retained so inlay hints can walk `let`/`var`
    /// bindings. `None` when the module was not type-checked.
    ast: Option<Arc<ridge_ast::Module>>,
}

/// The full analysis result the server keeps between compiles.
///
/// Immutable once built; replaced wholesale by a newer generation.
#[derive(Debug)]
pub struct WorkspaceIndex {
    /// Monotonic compile generation. A later compile carries a strictly higher
    /// generation, which the install guard uses to reject a stale result.
    pub generation: u64,
    /// All `TyCon` declarations (builtins + user). The per-module `node_types`
    /// index into this list when rendering hovered types.
    tycons: Vec<TyConDecl>,
    /// Per-module query data, indexed by `ModuleId.0`.
    modules: Vec<ModuleView>,
    /// Document URI → [`ModuleId`]. Keyed the same way diagnostics are published
    /// (workspace root joined with the source id), so an editor-sent URI matches.
    pub uri_to_module: HashMap<Url, ModuleId>,
    /// Document URI → [`ModuleId`], keyed by a normalization-stable [`uri_key`]
    /// so a client-sent URI resolves regardless of drive-letter case or colon
    /// encoding (the Windows `file:///c%3A/…` vs `file:///C:/…` split). Every
    /// position query routes through this map; `uri_to_module` above is kept for
    /// publishing diagnostics under the exact URI the path round-trips to.
    uri_key_to_module: HashMap<String, ModuleId>,
    /// Per-module spatial index, indexed by `ModuleId.0`.
    pub spatial: Vec<NodeSpatialIndex>,
    /// Per-module UTF-16 ↔ byte line index, indexed by `ModuleId.0`.
    pub line_indices: Vec<LineIndex>,
    /// Per-module source text the spans index into, indexed by `ModuleId.0`.
    pub module_text: Vec<Arc<str>>,
    /// Per-module document URI, indexed by `ModuleId.0` (for cross-file
    /// go-to-definition targets). `None` if the path had no valid URI.
    pub module_uris: Vec<Option<Url>>,
    /// Per-module fully-qualified name, indexed by `ModuleId.0`. Used by the
    /// file-rename import fixups to recover a moved module's path depth.
    module_fqns: Vec<String>,
    /// Per-module owning-project name, indexed by `ModuleId.0` (the FQN prefix,
    /// which may itself contain dots). The new name a renamed module takes is
    /// this project name plus its new path below the source root.
    module_project_names: Vec<String>,
    /// Per-module flag: is the module part of a runnable project (`app`/`service`)?
    /// Indexed by `ModuleId.0`. Drives the "Run" code lens on a `fn main`.
    module_runnable: Vec<bool>,
    /// Quick-fixes for `T014 CapabilityNotDeclared` on capability-free
    /// functions: each carries the edit that adds the inferred capabilities to
    /// the signature. Populated after the compile (which holds the structured
    /// type errors); empty otherwise.
    pub capability_fixes: Vec<CapabilityFix>,
}

/// A ready-to-apply quick-fix that declares the inferred capabilities on a
/// function flagged by `T014`. Spans are already resolved to LSP ranges.
#[derive(Debug, Clone)]
pub struct CapabilityFix {
    /// The document the function lives in.
    pub uri: Url,
    /// The whole declaration, used to decide whether a code-action request
    /// (which carries a cursor range) lands on this function.
    pub decl_range: Range,
    /// The empty range just before the function name where the capability
    /// keywords are inserted.
    pub edit_range: Range,
    /// The text to insert (the capabilities plus a trailing space).
    pub new_text: String,
    /// The code-action title shown in the editor.
    pub title: String,
}

/// Client command the "Run" code lens invokes. Argument: the project name.
///
/// Handled client-side (the editor extension opens a terminal and runs the CLI),
/// so it is deliberately kept out of the server's `executeCommand` provider — a
/// client-side handler does not fire if the server also claims the command id.
pub const RUN_COMMAND: &str = "ridge.run";
/// Client command the "Run test" code lens invokes. Arguments: project name and
/// the test's `@test` display name. Client-side, like [`RUN_COMMAND`].
pub const RUN_TEST_COMMAND: &str = "ridge.test";

/// Which code lenses the client opted into via `initializationOptions.codeLens`.
///
/// Every kind defaults to off. A lens carries a command only the editor
/// integrations register, so a client that does not opt in is served nothing
/// rather than inert lenses it can't act on.
// Four independent opt-in flags, one per lens kind; a struct of named bools is
// the clearest representation.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CodeLensConfig {
    /// "N references" above each referenceable top-level declaration.
    pub references: bool,
    /// "N implementations" above each `class` declaration.
    pub implementations: bool,
    /// "Run" above the `fn main` of an app/service project.
    pub run: bool,
    /// "Run test" above each `@test` function.
    pub run_test: bool,
}

impl CodeLensConfig {
    /// True when at least one lens kind is enabled.
    #[must_use]
    pub const fn any(self) -> bool {
        self.references || self.implementations || self.run || self.run_test
    }
}

impl WorkspaceIndex {
    /// Build an index from a completed compile.
    ///
    /// `generation` must be the compile's generation stamp (see
    /// [`WorkspaceIndex::generation`]).
    #[must_use]
    pub fn build(
        generation: u64,
        typed: &TypedWorkspace,
        resolved: &ResolvedWorkspace,
        sources: &WorkspaceSourceCache,
    ) -> Self {
        let n = resolved.modules.len();
        let root: &Path = &resolved.graph.root;

        let mut uri_to_module: HashMap<Url, ModuleId> = HashMap::new();
        let mut uri_key_to_module: HashMap<String, ModuleId> = HashMap::new();
        // All per-module vecs are addressed by `ModuleId.0`; pre-size and fill by
        // id so iteration order over `graph.modules` (sorted by name) doesn't
        // matter.
        let mut module_text: Vec<Arc<str>> = vec![Arc::from(""); n];
        let mut line_indices: Vec<LineIndex> = (0..n).map(|_| LineIndex::new("")).collect();
        let mut module_uris: Vec<Option<Url>> = vec![None; n];
        let mut module_fqns: Vec<String> = vec![String::new(); n];
        let mut module_project_names: Vec<String> = vec![String::new(); n];
        let mut module_runnable: Vec<bool> = vec![false; n];

        for module in &resolved.graph.modules {
            let i = module.id.0 as usize;
            if i >= n {
                continue;
            }
            // Key URIs the same way diagnostics are published — workspace root
            // joined with the source id — so an editor-sent `textDocument.uri`
            // matches even when discovery canonicalised the on-disk path.
            let source_id = sources.id_for_module(module.id);
            let uri = source_id_to_uri(root, source_id.as_str());
            uri_to_module.insert(uri.clone(), module.id);
            uri_key_to_module.insert(uri_key(&uri), module.id);
            module_uris[i] = Some(uri);
            module_fqns[i].clone_from(&module.fully_qualified_name);
            // `graph.projects` is indexed by `ProjectId.0`.
            if let Some(project) = resolved.graph.projects.get(module.project.0 as usize) {
                module_project_names[i].clone_from(&project.name);
                module_runnable[i] =
                    matches!(project.kind, ProjectKind::App | ProjectKind::Service);
            }
            if let Some(text) = sources.text(source_id.as_str()) {
                module_text[i] = Arc::from(text);
                line_indices[i] = LineIndex::new(text);
            }
        }

        // `resolved.modules` is indexed by `ModuleId.0`, so the spatial vec built
        // in iteration order is addressable by `ModuleId.0` too.
        let mut spatial: Vec<NodeSpatialIndex> = Vec::with_capacity(n);
        let mut modules: Vec<ModuleView> = Vec::with_capacity(n);
        for (i, rm) in resolved.modules.iter().enumerate() {
            debug_assert_eq!(
                rm.id.0 as usize, i,
                "resolved.modules must be indexed by ModuleId.0"
            );
            spatial.push(NodeSpatialIndex::from_node_ids(&rm.node_ids));
            // Clone the per-module query data out of the borrowed workspaces so
            // the index can outlive them.
            let typed_mod = typed.modules.get(i);
            let node_types = typed_mod
                .map(|tm| tm.node_types.clone())
                .unwrap_or_default();
            let ast = typed_mod.map(|tm| Arc::clone(&tm.ast));
            modules.push(ModuleView {
                node_types,
                bindings: rm.bindings.clone(),
                symbols: rm.symbols.clone(),
                scopes: rm.scopes.clone(),
                imports: rm.imports.clone(),
                ast,
            });
        }

        Self {
            generation,
            tycons: typed.tycons.clone(),
            modules,
            uri_to_module,
            uri_key_to_module,
            spatial,
            line_indices,
            module_text,
            module_uris,
            module_fqns,
            module_project_names,
            module_runnable,
            capability_fixes: Vec::new(),
        }
    }

    /// Resolve a document `uri` to its module, tolerant of how the client spelled
    /// the path. Routes through [`uri_key`] so a VS Code URI (`file:///c%3A/…`)
    /// matches a module keyed from the server's own path round-trip
    /// (`file:///C:/…`) — without it, every position query misses on Windows.
    fn module_id_for(&self, uri: &Url) -> Option<ModuleId> {
        self.uri_key_to_module.get(&uri_key(uri)).copied()
    }

    /// Whether this index owns `uri` as one of its modules. Used to route a
    /// request to the right workspace when several are open at once: each open
    /// folder has its own index, and a document belongs to exactly one of them.
    #[must_use]
    pub fn contains_uri(&self, uri: &Url) -> bool {
        self.module_id_for(uri).is_some()
    }

    /// Map an LSP document `uri` and a byte `offset` to the narrowest enclosing
    /// node, restricted to the kinds in `prefer`.
    ///
    /// Hover passes the expression kinds (`Expr`, `Block`, `Try`, `Type`);
    /// go-to-definition passes the name kinds (`Ident`, `QualifiedName`).
    /// Returns `None` when the URI is not a workspace module or no eligible node
    /// covers the offset.
    #[must_use]
    pub fn node_at(
        &self,
        uri: &Url,
        offset: u32,
        prefer: &[NodeKind],
    ) -> Option<(ModuleId, NodeId, NodeKind, Span)> {
        let mid = self.module_id_for(uri)?;
        let spatial = self.spatial.get(mid.0 as usize)?;
        let (nid, kind, span) = spatial.narrowest_containing(offset, prefer)?;
        Some((mid, nid, kind, span))
    }

    /// Answer a hover request at an LSP `(line, utf16_col)` position.
    ///
    /// Returns the markdown to show and the source span to underline, or `None`
    /// for whitespace, a keyword, an unresolved name, a position past the end,
    /// or a node with no inferred type. Reads only this immutable snapshot — a
    /// hover never triggers a compile.
    #[must_use]
    pub fn hover_at(&self, uri: &Url, line: u32, utf16_col: u32) -> Option<(String, Span)> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        // The type lives on the narrowest expression-like node (idents do not
        // carry a written-back type).
        let (_, type_node, _, expr_span) = self.node_at(
            uri,
            offset,
            &[
                NodeKind::Expr,
                NodeKind::Block,
                NodeKind::Try,
                NodeKind::Type,
            ],
        )?;
        let ty = self
            .modules
            .get(mi)?
            .node_types
            .get(type_node.0 as usize)?
            .as_ref()?;
        if matches!(ty, ridge_types::Type::Error) {
            return None;
        }
        let type_str = render_type_with(ty, &self.tycons);

        // If an identifier covers the same offset, build an enriched card over
        // the identifier; otherwise this is a literal/expression and we show the
        // bare type, fenced, over the expression span.
        if let Some((_, id_node, _, id_span)) =
            self.node_at(uri, offset, &[NodeKind::Ident, NodeKind::QualifiedName])
        {
            let name = self.text_slice(mi, id_span);
            let binding = self
                .modules
                .get(mi)
                .and_then(|m| m.bindings.get(id_node.0 as usize))
                .and_then(Option::as_ref);
            Some((
                self.hover_markdown(mi, offset, name, binding, &type_str),
                id_span,
            ))
        } else {
            Some((fenced_ridge(&type_str), expr_span))
        }
    }

    /// Assemble the markdown shown when hovering an identifier.
    ///
    /// Three tiers, most specific first:
    /// 1. a record-field use renders `field : T` and names the record it
    ///    belongs to;
    /// 2. a name that resolves to a workspace declaration renders that
    ///    declaration's written header — visibility, capabilities, named
    ///    parameters, return type — followed by its doc comment, if any;
    /// 3. anything else falls back to the role-labelled inferred type.
    ///
    /// Every tier wraps the signature in a `ridge` code fence so the editor
    /// syntax-highlights it.
    fn hover_markdown(
        &self,
        mi: usize,
        offset: u32,
        name: &str,
        binding: Option<&Binding>,
        inferred: &str,
    ) -> String {
        // Tier 1 — a record field's name node carries no binding; resolve it
        // through the base expression's type to the owning record.
        if binding.is_none() {
            if let Some((tycon, field, _)) = self.field_access_at(mi, offset) {
                let mut md = fenced_ridge(&format!("{field} : {inferred}"));
                if let Some(owner) = self.tycon_display_name(tycon) {
                    use std::fmt::Write as _;
                    let _ = write!(md, "\n\nfield of `{owner}`");
                }
                return md;
            }
        }

        // Tier 2 — a top-level fn/const/type/actor name: show its written header
        // and doc comment, sourced from the declaring module's AST.
        if let Some((module, symbol)) = workspace_symbol_of(binding) {
            if let Some((header, doc)) = self.decl_header_and_doc(module, symbol) {
                let mut md = fenced_ridge(&header);
                if let Some(doc) = doc {
                    md.push_str("\n\n");
                    md.push_str(&doc);
                }
                return md;
            }
        }

        // Tier 3 — role-labelled inferred type (locals, params, constructors,
        // stdlib symbols, class methods, or any name without a reachable decl).
        let label = binding_label(binding);
        fenced_ridge(&format!("{label}{name} : {inferred}"))
    }

    /// The display name of the named type constructor `raw`, or `None` for an
    /// anonymous inline-record tycon (which has no user-visible name).
    fn tycon_display_name(&self, raw: u32) -> Option<String> {
        self.tycons
            .iter()
            .find(|d| d.id.0 == raw && !d.is_anon)
            .map(|d| d.name.clone())
    }

    /// The written header and doc comment of the workspace declaration named by
    /// `symbol` in `module`. Picks the top-level item whose span encloses the
    /// symbol's definition site. `None` when the module was not type-checked or
    /// the symbol is not a `fn`/`const`/`type`/`actor` declaration.
    fn decl_header_and_doc(
        &self,
        module: ModuleId,
        symbol: ridge_resolve::SymbolId,
    ) -> Option<(String, Option<String>)> {
        let def_span = self.symbol_def_span(module, symbol)?;
        let mi = module.0 as usize;
        let ast = self.modules.get(mi)?.ast.as_ref()?;
        let text: &str = self.module_text.get(mi).map_or("", |t| &**t);
        ast.items.iter().find_map(|item| match item {
            ridge_ast::Item::Fn(d) if span_encloses(d.span, def_span) => {
                Some((fn_header(text, d), doc_text(d.doc.as_ref())))
            }
            ridge_ast::Item::Const(d) if span_encloses(d.span, def_span) => {
                Some((const_header(text, d), doc_text(d.doc.as_ref())))
            }
            ridge_ast::Item::Type(d) if span_encloses(d.span, def_span) => {
                Some((type_header(text, d), doc_text(d.doc.as_ref())))
            }
            ridge_ast::Item::Actor(d) if span_encloses(d.span, def_span) => {
                Some((format!("actor {}", d.name.text), doc_text(d.doc.as_ref())))
            }
            _ => None,
        })
    }

    /// Slice the source text of module `mi` over `span` (empty on out-of-range).
    fn text_slice(&self, mi: usize, span: Span) -> &str {
        let text = self.module_text.get(mi).map_or("", |t| &**t);
        let (start, end) = (span.start as usize, span.end as usize);
        text.get(start..end).unwrap_or("")
    }

    /// Convert a byte `span` in `uri`'s module to an LSP UTF-16 [`Range`].
    #[must_use]
    pub fn span_to_range(&self, uri: &Url, span: Span) -> Option<Range> {
        let mid = self.module_id_for(uri)?;
        self.range_in(mid, span)
    }

    /// Answer a go-to-definition request at an LSP `(line, utf16_col)` position.
    ///
    /// Returns the definition site, or `None` for whitespace, a keyword, a
    /// literal, or an unresolved name. A stdlib symbol, stdlib module alias, or
    /// stdlib class method resolves into the materialised stdlib source (see
    /// [`crate::stdlib_defs`]). Reads only this immutable snapshot — never
    /// triggers a compile.
    #[must_use]
    pub fn definition_at(&self, uri: &Url, line: u32, utf16_col: u32) -> Option<Location> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        // A record field use (`user.age`) carries no binding on its name node;
        // it resolves through the base expression's type to the field's
        // declaration in the owning `type`. The cursor on the base (`user`)
        // falls outside the field range, so this never shadows the binding path.
        if let Some(loc) = self.field_definition_at(mi, offset) {
            return Some(loc);
        }

        // The binding sits on the narrowest name node that actually carries one:
        // for `Mod.item` the binding is on the whole `QualifiedName`, not the
        // segment ident under the cursor.
        let bindings = &self.modules.get(mi)?.bindings;
        let binding = self
            .spatial
            .get(mi)?
            .enclosing(offset, &[NodeKind::Ident, NodeKind::QualifiedName])
            .into_iter()
            .find_map(|(nid, _, _)| bindings.get(nid.0 as usize).and_then(Option::as_ref))?;

        match binding {
            Binding::Local(local_id) => {
                let span = self.find_local_def_span(mi, *local_id)?;
                self.location_in(mid, span)
            }
            Binding::ModuleSymbol { module, symbol }
            | Binding::ImportedSymbol { module, symbol, .. } => {
                let span = self.symbol_def_span(*module, *symbol)?;
                self.location_in(*module, span)
            }
            Binding::ActorName { module, actor } => {
                let span = self.symbol_def_span(*module, *actor)?;
                self.location_in(*module, span)
            }
            Binding::Constructor { owner_type, .. } => {
                // The owning type is a symbol of the current module.
                let span = self.symbol_def_span(mid, *owner_type)?;
                self.location_in(mid, span)
            }
            Binding::ModuleAlias {
                target: ImportTarget::WorkspaceModule(target),
                ..
            } => self.location_in(*target, Span::point(0)),
            Binding::StdlibSymbol { module, name } => {
                crate::stdlib_defs::stdlib_location(*module, name)
            }
            Binding::ModuleAlias {
                target: ImportTarget::BuiltinStdlib(id),
                ..
            } => crate::stdlib_defs::stdlib_module_location(*id),
            Binding::ClassMethod { class_name, method } => {
                // A stdlib verb (`filter`, `joinOn`, …) resolves into the
                // materialised stdlib source; a class declared in the workspace
                // resolves to the method signature in that `class` declaration.
                crate::stdlib_defs::stdlib_class_method_location(class_name, method)
                    .or_else(|| self.workspace_class_method_location(class_name, method))
            }
            // The bare `(.field)` accessor shorthand (owner type unknown) and
            // errors have no resolvable definition site. A `base.field` access
            // is handled earlier, through the base's type.
            _ => None,
        }
    }

    /// Answer a `textDocument/declaration` request at an LSP `(line, utf16_col)`.
    ///
    /// Where a name enters this module through a selective import clause
    /// (`import other (foo)`), this returns that clause item — the site that
    /// *declares* the name in the current scope — whereas [`Self::definition_at`]
    /// jumps past the import to the original `fn`/`type`/`const` in the owning
    /// module. A name with no separate import site — a local, a same-module
    /// symbol, an alias-qualified `Mod.item` use, a record field — has its
    /// declaration and definition at one place, so this falls back to the
    /// definition. Reads only this immutable snapshot; never triggers a compile.
    #[must_use]
    pub fn declaration_at(&self, uri: &Url, line: u32, utf16_col: u32) -> Option<Location> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        // The referent under the cursor, resolved exactly as go-to-definition
        // picks its binding: the narrowest name node that carries one.
        let bindings = &self.modules.get(mi)?.bindings;
        let key = self
            .spatial
            .get(mi)?
            .enclosing(offset, &[NodeKind::Ident, NodeKind::QualifiedName])
            .into_iter()
            .find_map(|(nid, _, _)| bindings.get(nid.0 as usize).and_then(Option::as_ref))
            .and_then(|b| referent_key(b, mid));

        // A referent brought in by a selective import declares locally at its
        // clause item; jump there. Otherwise declaration and definition coincide.
        key.and_then(|k| self.import_clause_location(mid, &k))
            .or_else(|| self.definition_at(uri, line, utf16_col))
    }

    /// Answer a `textDocument/typeDefinition` request at an LSP `(line, col)`.
    ///
    /// Jumps from a value to the declaration of its type: the inferred type of
    /// the narrowest expression under the cursor, resolved to the `type`
    /// declaration that introduces it. Returns `None` for a built-in type (no
    /// source), a function, a type variable, or whitespace. Reads only this
    /// immutable snapshot; never triggers a compile.
    #[must_use]
    pub fn type_definition_at(&self, uri: &Url, line: u32, utf16_col: u32) -> Option<Location> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        let (_, type_node, _, _) = self.node_at(
            uri,
            offset,
            &[
                NodeKind::Expr,
                NodeKind::Block,
                NodeKind::Try,
                NodeKind::Type,
            ],
        )?;
        let ty = self
            .modules
            .get(mi)?
            .node_types
            .get(type_node.0 as usize)?
            .as_ref()?;
        let tycon = named_tycon_of(ty)?;
        self.tycon_location(tycon)
    }

    /// Answer a `textDocument/implementation` request at an LSP `(line, col)`.
    ///
    /// Navigates from a typeclass abstraction to its concrete implementations.
    /// On a `class` name (or the class name of an `instance` head) it returns
    /// every `instance` of that class; on a class method — at a call site, in the
    /// `class` signature, or in an `instance` body — it returns that method's
    /// definition in each `instance`. Returns `None` off any class or instance
    /// name. Reads only this immutable snapshot; never triggers a compile.
    #[must_use]
    pub fn implementations_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
    ) -> Option<Vec<Location>> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        // The class — and, for a method, its name — the cursor refers to. A class
        // method call site already carries a `ClassMethod` binding; a class name,
        // a method signature, or an instance method definition carry none, so
        // those are recovered from the retained AST of the cursor's module.
        let (class_name, method) = self
            .class_method_use_at(mi, offset)
            .or_else(|| self.class_target_in_ast(mi, offset))?;

        let mut locations: Vec<Location> = Vec::new();
        for (smi, view) in self.modules.iter().enumerate() {
            let Some(ast) = view.ast.as_ref() else {
                continue;
            };
            let Ok(raw) = u32::try_from(smi) else {
                continue;
            };
            let smid = ModuleId(raw);
            for item in &ast.items {
                let ridge_ast::Item::InstanceDecl(decl) = item else {
                    continue;
                };
                if decl.class.text != class_name {
                    continue;
                }
                // A method target lands on the matching definition's name; a bare
                // class target lands on the class name in the instance head.
                let span = match &method {
                    Some(name) => match decl.methods.iter().find(|d| d.name.text == *name) {
                        Some(def) => def.name.span,
                        None => continue,
                    },
                    None => decl.class.span,
                };
                if let Some(loc) = self.location_in(smid, span) {
                    locations.push(loc);
                }
            }
        }

        if locations.is_empty() {
            return None;
        }
        locations.sort_by(|a, b| {
            (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
                b.uri.as_str(),
                b.range.start.line,
                b.range.start.character,
            ))
        });
        locations.dedup();
        Some(locations)
    }

    /// The `(class, method)` named by a class-method use site under `offset`,
    /// when the cursor sits on a name bound to a `ClassMethod`.
    fn class_method_use_at(&self, mi: usize, offset: u32) -> Option<(String, Option<String>)> {
        match self.binding_at(mi, offset)? {
            Binding::ClassMethod { class_name, method } => {
                Some((class_name.clone(), Some(method.clone())))
            }
            _ => None,
        }
    }

    /// The `(class, optional method)` named by a `class`/`instance` declaration
    /// token under `offset`, scanning the cursor module's retained AST. A class
    /// name in a `class` or `instance` head yields `(class, None)`; a method name
    /// in a `class` signature or an `instance` body yields `(class, Some(name))`.
    /// `class` and `instance` declarations carry no bindings, so the spans come
    /// straight from the parsed items.
    fn class_target_in_ast(&self, mi: usize, offset: u32) -> Option<(String, Option<String>)> {
        let ast = self.modules.get(mi)?.ast.as_ref()?;
        let here = |s: Span| s.start <= offset && offset <= s.end;
        for item in &ast.items {
            match item {
                ridge_ast::Item::ClassDecl(decl) => {
                    if here(decl.name.span) {
                        return Some((decl.name.text.clone(), None));
                    }
                    if let Some(m) = decl.methods.iter().find(|m| here(m.name.span)) {
                        return Some((decl.name.text.clone(), Some(m.name.text.clone())));
                    }
                }
                ridge_ast::Item::InstanceDecl(decl) => {
                    if here(decl.class.span) {
                        return Some((decl.class.text.clone(), None));
                    }
                    if let Some(m) = decl.methods.iter().find(|m| here(m.name.span)) {
                        return Some((decl.class.text.clone(), Some(m.name.text.clone())));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Answer a `textDocument/prepareTypeHierarchy` request at `(line, col)`.
    ///
    /// Anchors the type hierarchy on a typeclass: the cursor must sit on a class
    /// name, either in a `class` declaration or in an `instance` head. The item
    /// always points at the `class` declaration, so an instance-head anchor walks
    /// up to its class. Returns `None` off a class name, or for a class with no
    /// workspace declaration (a prelude/stdlib class). Reads only this immutable
    /// snapshot; never triggers a compile.
    #[must_use]
    pub fn prepare_type_hierarchy_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
    ) -> Option<Vec<TypeHierarchyItem>> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        // Only a class-name token (in a `class` decl or an `instance` head) is an
        // anchor; on a method name the scan returns `Some((_, Some(_)))`, which
        // is rejected here.
        let (class, None) = self.class_target_in_ast(mi, offset)? else {
            return None;
        };
        let (cmi, decl) = self.find_workspace_class(&class)?;
        Some(vec![self.class_hierarchy_item(cmi, decl)?])
    }

    /// Resolve a `typeHierarchy/supertypes` request from an item's `data`. A
    /// class's supertypes are its superclasses (`where C a`); an instance's lone
    /// supertype is the class it implements. Each resolves to a workspace `class`
    /// declaration; superclasses with no workspace declaration are skipped.
    #[must_use]
    pub fn type_supertypes(&self, data: &serde_json::Value) -> Option<Vec<TypeHierarchyItem>> {
        let (class, is_instance) = decode_type_item(data)?;
        let mut items: Vec<TypeHierarchyItem> = Vec::new();
        if is_instance {
            if let Some((cmi, decl)) = self.find_workspace_class(&class) {
                if let Some(item) = self.class_hierarchy_item(cmi, decl) {
                    items.push(item);
                }
            }
        } else {
            let (_, decl) = self.find_workspace_class(&class)?;
            for sup in &decl.superclasses {
                if let Some((smi, sdecl)) = self.find_workspace_class(&sup.class.text) {
                    if let Some(item) = self.class_hierarchy_item(smi, sdecl) {
                        items.push(item);
                    }
                }
            }
        }
        Some(sorted_dedup_items(items))
    }

    /// Resolve a `typeHierarchy/subtypes` request from an item's `data`. A
    /// class's subtypes are its direct subclasses (classes that name it as a
    /// superclass) and its instances; an instance has none.
    #[must_use]
    pub fn type_subtypes(&self, data: &serde_json::Value) -> Option<Vec<TypeHierarchyItem>> {
        let (class, is_instance) = decode_type_item(data)?;
        if is_instance {
            return Some(Vec::new());
        }
        let mut items: Vec<TypeHierarchyItem> = Vec::new();
        for (smi, view) in self.modules.iter().enumerate() {
            let Some(ast) = view.ast.as_ref() else {
                continue;
            };
            for item in &ast.items {
                match item {
                    ridge_ast::Item::ClassDecl(decl)
                        if decl.superclasses.iter().any(|c| c.class.text == class) =>
                    {
                        if let Some(i) = self.class_hierarchy_item(smi, decl) {
                            items.push(i);
                        }
                    }
                    ridge_ast::Item::InstanceDecl(decl) if decl.class.text == class => {
                        if let Some(i) = self.instance_hierarchy_item(smi, decl) {
                            items.push(i);
                        }
                    }
                    _ => {}
                }
            }
        }
        Some(sorted_dedup_items(items))
    }

    /// Locate a workspace `class` declaration by name, scanning every module's
    /// top-level items.
    fn find_workspace_class(&self, name: &str) -> Option<(usize, &ridge_ast::ClassDecl)> {
        for (mi, view) in self.modules.iter().enumerate() {
            let Some(ast) = view.ast.as_ref() else {
                continue;
            };
            for item in &ast.items {
                if let ridge_ast::Item::ClassDecl(decl) = item {
                    if decl.name.text == name {
                        return Some((mi, decl));
                    }
                }
            }
        }
        None
    }

    /// Build a type-hierarchy item for a workspace `class` declaration. The range
    /// is the whole declaration (trailing whitespace trimmed); the selection
    /// range is the class name.
    fn class_hierarchy_item(
        &self,
        mi: usize,
        decl: &ridge_ast::ClassDecl,
    ) -> Option<TypeHierarchyItem> {
        let mid = ModuleId(u32::try_from(mi).ok()?);
        let text: &str = self.module_text.get(mi).map_or("", |t| &**t);
        let decl_loc = self.location_in(mid, trim_trailing_ws(text, decl.span))?;
        let name_loc = self.location_in(mid, decl.name.span)?;
        Some(TypeHierarchyItem {
            name: decl.name.text.clone(),
            kind: LspSymbolKind::INTERFACE,
            tags: None,
            detail: None,
            uri: decl_loc.uri,
            range: decl_loc.range,
            selection_range: name_loc.range,
            data: Some(serde_json::json!({ "name": decl.name.text, "kind": "class" })),
        })
    }

    /// Build a type-hierarchy item for an `instance` declaration. The display
    /// name is the instance head (`Class Head…`) as written; the data carries the
    /// class name so the instance's lone supertype resolves back to it.
    fn instance_hierarchy_item(
        &self,
        mi: usize,
        decl: &ridge_ast::InstanceDecl,
    ) -> Option<TypeHierarchyItem> {
        let mid = ModuleId(u32::try_from(mi).ok()?);
        let text: &str = self.module_text.get(mi).map_or("", |t| &**t);
        let decl_loc = self.location_in(mid, trim_trailing_ws(text, decl.span))?;
        let head_loc = self.location_in(mid, decl.class.span)?;
        // The head as written: from the class name to the end of the last head
        // type — e.g. `Greeter Int` or `Convert Celsius Fahrenheit`.
        let head_end = decl
            .head
            .last()
            .map_or(decl.class.span.end, |t| t.span().end);
        let name = text
            .get(decl.class.span.start as usize..head_end as usize)
            .map_or_else(|| decl.class.text.clone(), squeeze_ws);
        Some(TypeHierarchyItem {
            name,
            kind: LspSymbolKind::OBJECT,
            tags: None,
            detail: None,
            uri: head_loc.uri,
            range: decl_loc.range,
            selection_range: head_loc.range,
            data: Some(serde_json::json!({ "name": decl.class.text, "kind": "instance" })),
        })
    }

    /// Answer a `textDocument/signatureHelp` request at an LSP `(line, col)`.
    ///
    /// Ridge calls are juxtaposition (`joinOn left right cond`), so the active
    /// parameter is the count of argument atoms already written before the
    /// cursor. Resolves the callee of the enclosing call — or, with no argument
    /// typed yet, the function name under or just left of the cursor — to a
    /// signature read from the stdlib source or the workspace declaration.
    /// Returns `None` off any call so the editor popup stays quiet. Reads only
    /// this immutable snapshot; never triggers a compile.
    #[must_use]
    pub fn signature_help_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
        label_offsets: bool,
    ) -> Option<SignatureHelp> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);
        let src = self.module_text.get(mi)?;

        // The callee to describe and the argument spans already written, from the
        // enclosing call; failing that, the bare name under or just left of the
        // cursor (no arguments typed yet).
        let (callee_span, arg_spans) = self
            .enclosing_call(mi, offset, src)
            .or_else(|| self.bare_callee(mi, offset, src).map(|s| (s, Vec::new())))?;

        let binding = self.binding_at(mi, callee_span.start)?;
        let sig = self.signature_for_binding(binding)?;
        let active = active_param(&arg_spans, offset, sig.params.len());
        Some(make_signature_help(sig, active, label_offsets))
    }

    /// The innermost call whose argument region the cursor sits in, as
    /// `(callee span, argument spans)`. Includes the trailing whitespace right
    /// after the last argument (you just typed a space to add the next one).
    fn enclosing_call(&self, mi: usize, offset: u32, src: &str) -> Option<(Span, Vec<Span>)> {
        let ast = self.modules.get(mi)?.ast.as_ref()?;
        let mut finder = CallFinder {
            offset,
            src,
            best: None,
        };
        ridge_ast::visit::Visit::visit_module(&mut finder, ast);
        finder.best.map(|(callee, args, _)| (callee, args))
    }

    /// The span of a callable name under the cursor, or the nearest one ending
    /// to its left across same-line whitespace. Used when no argument is typed
    /// yet, so there is no call node to walk.
    fn bare_callee(&self, mi: usize, offset: u32, src: &str) -> Option<Span> {
        let spatial = self.spatial.get(mi)?;
        if let Some((_, _, span)) =
            spatial.narrowest_containing(offset, &[NodeKind::Ident, NodeKind::QualifiedName])
        {
            return Some(span);
        }
        let mut best: Option<Span> = None;
        for (span, kind, _) in &spatial.entries {
            if !matches!(kind, NodeKind::Ident | NodeKind::QualifiedName) || span.end > offset {
                continue;
            }
            let Some(gap) = src.get(span.end as usize..offset as usize) else {
                continue;
            };
            let reachable = !gap.is_empty() && gap.chars().all(|c| c == ' ' || c == '\t');
            if reachable && best.is_none_or(|b| span.end > b.end) {
                best = Some(*span);
            }
        }
        best
    }

    /// The binding stamped on the name node enclosing `offset`, if any — the
    /// shared name lookup behind go-to-definition, references, and signature
    /// help.
    fn binding_at(&self, mi: usize, offset: u32) -> Option<&Binding> {
        let bindings = &self.modules.get(mi)?.bindings;
        self.spatial
            .get(mi)?
            .enclosing(offset, &[NodeKind::Ident, NodeKind::QualifiedName])
            .into_iter()
            .find_map(|(nid, _, _)| bindings.get(nid.0 as usize).and_then(Option::as_ref))
    }

    /// The signature for the thing a call's callee binding refers to: a stdlib
    /// function or class method (read from the materialised stdlib source) or a
    /// workspace function or class method (read from its declaration).
    fn signature_for_binding(&self, binding: &Binding) -> Option<SignatureSig> {
        match binding {
            Binding::StdlibSymbol { module, name } => {
                crate::stdlib_defs::stdlib_fn_signature(*module, name)
            }
            Binding::ClassMethod { class_name, method } => {
                crate::stdlib_defs::stdlib_class_method_signature(class_name, method)
                    .or_else(|| self.workspace_class_method_signature(class_name, method))
            }
            Binding::ModuleSymbol { module, symbol }
            | Binding::ImportedSymbol { module, symbol, .. } => {
                self.workspace_fn_signature(*module, *symbol)
            }
            _ => None,
        }
    }

    /// Build the signature of a workspace top-level `fn` from its declaration.
    fn workspace_fn_signature(
        &self,
        module: ModuleId,
        symbol: ridge_resolve::SymbolId,
    ) -> Option<SignatureSig> {
        let mi = module.0 as usize;
        let view = self.modules.get(mi)?;
        let name = &view.symbols.entries.get(symbol.0 as usize)?.name;
        let src = self.module_text.get(mi)?;
        view.ast.as_ref()?.items.iter().find_map(|item| match item {
            ridge_ast::Item::Fn(decl) if decl.name.text == *name => Some(build_signature(
                src,
                &decl.name.text,
                &decl.params,
                decl.ret.as_ref(),
            )),
            _ => None,
        })
    }

    /// Build the signature of a workspace `class` method from its declaration.
    fn workspace_class_method_signature(&self, class: &str, method: &str) -> Option<SignatureSig> {
        let (mi, m) = self.find_workspace_class_method(class, method)?;
        let src = self.module_text.get(mi)?;
        Some(build_signature(src, &m.name.text, &m.params, Some(&m.ret)))
    }

    /// The definition site of a workspace `class` method's name signature.
    fn workspace_class_method_location(&self, class: &str, method: &str) -> Option<Location> {
        let (mi, m) = self.find_workspace_class_method(class, method)?;
        let mid = ModuleId(u32::try_from(mi).ok()?);
        self.location_in(mid, m.name.span)
    }

    /// Locate a workspace `class` method declaration by class and method name,
    /// scanning every module's top-level items.
    fn find_workspace_class_method(
        &self,
        class: &str,
        method: &str,
    ) -> Option<(usize, &ridge_ast::MethodSig)> {
        for (mi, view) in self.modules.iter().enumerate() {
            let Some(ast) = view.ast.as_ref() else {
                continue;
            };
            for item in &ast.items {
                if let ridge_ast::Item::ClassDecl(decl) = item {
                    if decl.name.text == class {
                        if let Some(m) = decl.methods.iter().find(|m| m.name.text == method) {
                            return Some((mi, m));
                        }
                    }
                }
            }
        }
        None
    }

    /// Answer a `textDocument/semanticTokens/full` request: classify every
    /// resolved name and capability annotation in the document into a semantic
    /// token, relative-encoded per the LSP spec. Supplements the `TextMate`
    /// grammar — it colours identifiers the grammar cannot disambiguate
    /// (function vs variable vs type vs stdlib vs capability). Reads only this
    /// immutable snapshot; never triggers a compile.
    #[must_use]
    pub fn semantic_tokens(&self, uri: &Url) -> Option<SemanticTokens> {
        let mid = self.module_id_for(uri)?;
        let tokens = self.collect_semantic_tokens(mid)?;
        Some(encode_tokens(&tokens))
    }

    /// The document's relative-encoded token stream without a `result_id`, the
    /// raw material the server stamps and caches to answer a follow-up
    /// `semantic_tokens/full/delta` against. Same tokens as
    /// [`Self::semantic_tokens`], just the bare `data`.
    #[must_use]
    pub fn semantic_token_data(&self, uri: &Url) -> Option<Vec<SemanticToken>> {
        let mid = self.module_id_for(uri)?;
        let tokens = self.collect_semantic_tokens(mid)?;
        Some(encode_token_data(&tokens))
    }

    /// As [`Self::semantic_tokens`], restricted to the tokens that intersect
    /// `range` — the request an editor makes for a large file's visible region.
    #[must_use]
    pub fn semantic_tokens_in_range(&self, uri: &Url, range: Range) -> Option<SemanticTokens> {
        let mid = self.module_id_for(uri)?;
        let mut tokens = self.collect_semantic_tokens(mid)?;
        tokens.retain(|t| {
            let start = Position::new(t.line, t.start);
            let end = Position::new(t.line, t.start + t.len);
            start <= range.end && range.start <= end
        });
        Some(encode_tokens(&tokens))
    }

    /// Collect, sort, and de-overlap the document's semantic tokens.
    ///
    /// Three sources feed the list: every resolved name use (classified by its
    /// binding), every top-level and member declaration name (so decls colour
    /// too — their name nodes carry no binding), and every capability keyword.
    fn collect_semantic_tokens(&self, mid: ModuleId) -> Option<Vec<RawToken>> {
        let mi = mid.0 as usize;
        let li = self.line_indices.get(mi)?;
        let src = self.module_text.get(mi)?;
        let spatial = self.spatial.get(mi)?;
        let bindings = &self.modules.get(mi)?.bindings;

        let mut out: Vec<RawToken> = Vec::new();
        // (A) Use sites.
        for (span, kind, nid) in &spatial.entries {
            match kind {
                NodeKind::Ident => {
                    if let Some(b) = bindings.get(nid.0 as usize).and_then(Option::as_ref) {
                        if let Some((ty, mods)) = self.classify_use(b, mi, *span) {
                            push_raw(li, &mut out, span.start, span.end, ty, mods);
                        }
                    }
                }
                NodeKind::QualifiedName => {
                    if let Some(b) = bindings.get(nid.0 as usize).and_then(Option::as_ref) {
                        self.push_qualified(li, src, &mut out, *span, b, mi);
                    }
                }
                _ => {}
            }
        }
        // (B) Declaration sites.
        self.collect_decl_tokens(mid, li, &mut out);
        // (C) Capability annotations, scanned from the source: the resolver does
        // not stamp them and the AST keeps no per-capability span.
        if let Some(ast) = self.modules.get(mi).and_then(|m| m.ast.as_ref()) {
            let mut caps = CapabilityCollector {
                li,
                src,
                out: &mut out,
            };
            ridge_ast::visit::Visit::visit_module(&mut caps, ast);
        }

        // Sort by position; on a tie the widest span comes first so it wins the
        // de-overlap that follows.
        out.sort_by(|a, b| {
            (a.line, a.start)
                .cmp(&(b.line, b.start))
                .then(b.len.cmp(&a.len))
        });
        // Semantic tokens must not overlap: keep a token only when it starts at
        // or after the end of the last kept one (on the same line).
        let mut kept: Vec<RawToken> = Vec::with_capacity(out.len());
        let mut last_line = u32::MAX;
        let mut last_end = 0u32;
        for t in out {
            if t.line != last_line || t.start >= last_end {
                last_line = t.line;
                last_end = t.start + t.len;
                kept.push(t);
            }
        }
        Some(kept)
    }

    /// The token type and modifier bitset for a name use carrying `binding`.
    /// `span` is the use site, used to flag a local's own binder as a
    /// declaration.
    fn classify_use(&self, binding: &Binding, mi: usize, span: Span) -> Option<(u32, u32)> {
        match binding {
            Binding::Local(id) => {
                let (kind, def_span) = self.local_info(mi, *id)?;
                let ty = match kind {
                    LocalKind::FnParam
                    | LocalKind::LambdaParam
                    | LocalKind::HandlerParam
                    | LocalKind::InitParam => TT_PARAMETER,
                    LocalKind::StateField => TT_PROPERTY,
                    _ => TT_VARIABLE,
                };
                let mut mods = 0;
                if matches!(kind, LocalKind::LetImmutable | LocalKind::StateField) {
                    mods |= MOD_READONLY;
                }
                if def_span == span {
                    mods |= MOD_DECLARATION;
                }
                Some((ty, mods))
            }
            Binding::ModuleSymbol { module, symbol }
            | Binding::ImportedSymbol { module, symbol, .. } => {
                let kind = &self
                    .modules
                    .get(module.0 as usize)?
                    .symbols
                    .entries
                    .get(symbol.0 as usize)?
                    .kind;
                Some(symbol_token(kind))
            }
            Binding::StdlibSymbol { name, .. } => {
                let ty = if name.chars().next().is_some_and(char::is_uppercase) {
                    TT_TYPE
                } else {
                    TT_FUNCTION
                };
                Some((ty, MOD_DEFAULT_LIBRARY))
            }
            Binding::ClassMethod { class_name, method } => {
                let stdlib =
                    crate::stdlib_defs::stdlib_class_method_signature(class_name, method).is_some();
                Some((TT_METHOD, if stdlib { MOD_DEFAULT_LIBRARY } else { 0 }))
            }
            Binding::FieldAccessor { .. } => Some((TT_PROPERTY, 0)),
            Binding::ActorName { .. } => Some((TT_CLASS, 0)),
            Binding::Constructor { .. } => Some((TT_ENUM_MEMBER, 0)),
            Binding::ModuleAlias { target, .. } => {
                let stdlib = matches!(target, ImportTarget::BuiltinStdlib(_));
                Some((TT_NAMESPACE, if stdlib { MOD_DEFAULT_LIBRARY } else { 0 }))
            }
            _ => None,
        }
    }

    /// The `LocalKind` and definition span of a local by id, searched across the
    /// module's scopes (mirrors [`Self::find_local_def_span`]).
    fn local_info(&self, mi: usize, id: LocalId) -> Option<(LocalKind, Span)> {
        self.modules
            .get(mi)?
            .scopes
            .nodes
            .iter()
            .flat_map(|node| &node.locals)
            .find(|entry| entry.id == id)
            .map(|entry| (entry.kind, entry.def_span))
    }

    /// Emit one token per segment of a qualified name. The leading segments are
    /// the namespace (or the owning type for a constructor path); the final
    /// segment carries the resolved binding's classification.
    fn push_qualified(
        &self,
        li: &LineIndex,
        src: &str,
        out: &mut Vec<RawToken>,
        span: Span,
        binding: &Binding,
        mi: usize,
    ) {
        let Some((last_ty, last_mods)) = self.classify_use(binding, mi, span) else {
            return;
        };
        let head_ty = if matches!(binding, Binding::Constructor { .. }) {
            TT_TYPE
        } else {
            TT_NAMESPACE
        };
        let head_mods = last_mods & MOD_DEFAULT_LIBRARY;
        let segments = segment_byte_ranges(src, span);
        let last = segments.len().saturating_sub(1);
        for (i, (start, end)) in segments.into_iter().enumerate() {
            let (ty, mods) = if i == last {
                (last_ty, last_mods)
            } else {
                (head_ty, head_mods)
            };
            push_raw(li, out, start, end, ty, mods);
        }
    }

    /// Emit a declaration token for every top-level and member declaration in
    /// the module. Declaration names carry no binding, so this is their only
    /// source; each is tagged with the `declaration` modifier.
    fn collect_decl_tokens(&self, mid: ModuleId, li: &LineIndex, out: &mut Vec<RawToken>) {
        let mi = mid.0 as usize;
        let Some(view) = self.modules.get(mi) else {
            return;
        };
        for entry in &view.symbols.entries {
            let (ty, mods, name): (u32, u32, &str) = match &entry.kind {
                SymbolKind::Fn { .. } => (TT_FUNCTION, MOD_DECLARATION, &entry.name),
                SymbolKind::Const => (TT_VARIABLE, MOD_DECLARATION | MOD_READONLY, &entry.name),
                SymbolKind::Type { .. } => (TT_TYPE, MOD_DECLARATION, &entry.name),
                SymbolKind::Actor { .. } => (TT_CLASS, MOD_DECLARATION, &entry.name),
                SymbolKind::Constructor {
                    is_record: false, ..
                } => (TT_ENUM_MEMBER, MOD_DECLARATION, &entry.name),
                SymbolKind::FieldAccessor { field, .. } => (TT_PROPERTY, MOD_DECLARATION, field),
                // Record auto-constructors share the type's name span; skip them.
                _ => continue,
            };
            if let Some(name_span) = self.decl_name_span(mid, entry.def_span, name) {
                push_raw(li, out, name_span.start, name_span.end, ty, mods);
            }
            if let SymbolKind::Actor { state, handlers } = &entry.kind {
                for field in state {
                    if let Some(s) = self.decl_name_span(mid, field.def_span, &field.name) {
                        push_raw(
                            li,
                            out,
                            s.start,
                            s.end,
                            TT_PROPERTY,
                            MOD_DECLARATION | MOD_READONLY,
                        );
                    }
                }
                for handler in handlers {
                    if let Some(s) = self.decl_name_span(mid, handler.def_span, &handler.name) {
                        push_raw(li, out, s.start, s.end, TT_METHOD, MOD_DECLARATION);
                    }
                }
            }
        }
    }

    /// Answer a find-references request at an LSP `(line, utf16_col)` position.
    ///
    /// Returns every use-site of the symbol under the cursor across the whole
    /// workspace, or `None` for whitespace, a keyword, or a name with no
    /// findable referent (a field accessor, a module alias, or an unresolved
    /// name). `include_declaration` mirrors the LSP `context.includeDeclaration`
    /// flag: when set, the definition site is part of the result. Reads only
    /// this immutable snapshot — never triggers a compile.
    #[must_use]
    pub fn references_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
        include_declaration: bool,
        cancel: &Cancel,
    ) -> Option<Vec<Location>> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        // Record fields take a dedicated, type-directed path: the field name
        // carries no binding, and a bare field name would conflate distinct
        // records that happen to share it. Fires on a field use (`user.age`) or
        // a field declaration in a `type` body.
        if let Some(locs) = self.field_references_at(mi, offset, include_declaration) {
            return Some(locs);
        }

        // The binding under the cursor — same lookup as go-to-definition.
        let bindings = &self.modules.get(mi)?.bindings;
        let binding = self
            .spatial
            .get(mi)?
            .enclosing(offset, &[NodeKind::Ident, NodeKind::QualifiedName])
            .into_iter()
            .find_map(|(nid, _, _)| bindings.get(nid.0 as usize).and_then(Option::as_ref))?;
        let target = referent_key(binding, mid)?;

        self.references_to_key(&target, include_declaration, cancel)
    }

    /// Scan the whole workspace for every use of `target`, honouring
    /// `include_declaration` and cooperative cancellation.
    ///
    /// Shared by [`references_at`](Self::references_at) and the "N references" code
    /// lens. The lens needs this symbol-keyed entry point because a top-level
    /// declaration's name node carries no binding, so the cursor path
    /// `references_at` takes (binding under the offset) resolves only at use sites,
    /// not on the declaration the lens sits above. Returns `None` only when the
    /// scan was cancelled mid-flight.
    fn references_to_key(
        &self,
        target: &ReferentKey,
        include_declaration: bool,
        cancel: &Cancel,
    ) -> Option<Vec<Location>> {
        // Locals never escape their module, so a local search stays in the target's
        // own module; everything else can be referenced from any importer, so scan
        // the whole workspace.
        let (scan_self_only, self_mi) = match target {
            ReferentKey::Local(module, _) => (true, module.0 as usize),
            _ => (false, usize::MAX),
        };

        let mut locations: Vec<Location> = Vec::new();
        for (smi, view) in self.modules.iter().enumerate() {
            // Cooperative cancellation: bail between modules if the request was
            // cancelled. The partial result is discarded — the handler future is
            // already gone — so an early `None` is safe.
            if cancel.is_cancelled() {
                return None;
            }
            if scan_self_only && smi != self_mi {
                continue;
            }
            let Ok(raw) = u32::try_from(smi) else {
                continue;
            };
            let smid = ModuleId(raw);
            let Some(spatial) = self.spatial.get(smi) else {
                continue;
            };
            for (span, _kind, nid) in &spatial.entries {
                let Some(b) = view.bindings.get(nid.0 as usize).and_then(Option::as_ref) else {
                    continue;
                };
                if referent_key(b, smid).as_ref() != Some(target) {
                    continue;
                }
                if let Some(loc) = self.location_in(smid, *span) {
                    locations.push(loc);
                }
            }
        }

        // The declaration site: included when the client asked for it (the scan
        // may already carry it — dedup below collapses the duplicate), dropped
        // otherwise. Moot for stdlib symbols and class methods, whose definition
        // lives outside the workspace.
        if let Some(def) = self.referent_def_location(target) {
            if include_declaration {
                locations.push(def);
            } else {
                locations.retain(|loc| *loc != def);
            }
        }

        // Deterministic order, no duplicates.
        locations.sort_by(|a, b| {
            (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
                b.uri.as_str(),
                b.range.start.line,
                b.range.start.character,
            ))
        });
        locations.dedup();
        Some(locations)
    }

    /// Answer a `documentHighlight` request at an LSP `(line, utf16_col)`
    /// position.
    ///
    /// The same-file companion to find-references: every occurrence of the
    /// symbol under the cursor *within this document*, each tagged read or
    /// write. The definition — a local's binder, or a `fn` / `const` / `type` /
    /// actor declaration name that lives in this file — is the write; every use
    /// is a read. Coverage matches find-references restricted to one module, so a
    /// name with no findable referent (whitespace, a keyword, a field accessor, a
    /// module alias) yields `None`. A qualified use (`Mod.item`) highlights only
    /// its final segment. Reads only this immutable snapshot.
    #[must_use]
    pub fn document_highlights_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
    ) -> Option<Vec<DocumentHighlight>> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        // A record field carries no binding on its name node; it highlights
        // through the base expression's type, like find-references restricted to
        // this file.
        if let Some(highlights) = self.field_highlights_at(mi, offset) {
            return Some(highlights);
        }

        let (target, name) = self.highlight_target_at(mid, offset)?;
        let view = self.modules.get(mi)?;
        let spatial = self.spatial.get(mi)?;

        // The definition's name span, but only when it lives in this file: a
        // local's binder always does; a workspace symbol's only if declared here.
        // It is the write site; everything the scan finds is a read.
        let decl_se: Option<(u32, u32)> = self
            .referent_decl_name_span(&target, &name)
            .and_then(|(decl_mid, span)| (decl_mid == mid).then_some(span))
            .map(|span| (span.start, span.end));

        // Keyed by byte span so each occurrence is emitted once; the write
        // overrides a read on the same span (a local's binder is both stamped as
        // a use and the definition).
        let mut spots: HashMap<(u32, u32), DocumentHighlightKind> = HashMap::new();
        for (span, _kind, nid) in &spatial.entries {
            let Some(b) = view.bindings.get(nid.0 as usize).and_then(Option::as_ref) else {
                continue;
            };
            if referent_key(b, mid).as_ref() != Some(&target) {
                continue;
            }
            let s = self.final_ident_span(mi, *span);
            let kind = if decl_se == Some((s.start, s.end)) {
                DocumentHighlightKind::WRITE
            } else {
                DocumentHighlightKind::READ
            };
            spots.entry((s.start, s.end)).or_insert(kind);
        }
        // A top-level declaration name carries no binding, so the scan misses it;
        // add it as the write site.
        if let Some((start, end)) = decl_se {
            spots.insert((start, end), DocumentHighlightKind::WRITE);
        }
        if spots.is_empty() {
            return None;
        }

        let mut highlights: Vec<DocumentHighlight> = spots
            .into_iter()
            .filter_map(|((start, end), kind)| {
                Some(DocumentHighlight {
                    range: self.range_in(mid, Span::new(start, end))?,
                    kind: Some(kind),
                })
            })
            .collect();
        highlights.sort_by(|a, b| {
            (a.range.start.line, a.range.start.character)
                .cmp(&(b.range.start.line, b.range.start.character))
        });
        Some(highlights)
    }

    /// The referent under the cursor for a same-file highlight, plus its name.
    ///
    /// Resolves a use-site or local binder (a name node carrying a binding) for
    /// any referent kind, and — when the cursor sits on a top-level declaration
    /// name, which carries no binding — the symbol it declares. `None` when the
    /// cursor is not on a highlightable name.
    fn highlight_target_at(&self, mid: ModuleId, offset: u32) -> Option<(ReferentKey, String)> {
        let mi = mid.0 as usize;
        let spatial = self.spatial.get(mi)?;
        let view = self.modules.get(mi)?;

        if let Some((binding, span)) = spatial
            .enclosing(offset, &[NodeKind::Ident, NodeKind::QualifiedName])
            .into_iter()
            .find_map(|(nid, _, span)| {
                view.bindings
                    .get(nid.0 as usize)
                    .and_then(Option::as_ref)
                    .map(|b| (b, span))
            })
        {
            let key = referent_key(binding, mid)?;
            let name_span = self.final_ident_span(mi, span);
            return Some((key, self.text_slice(mi, name_span).to_owned()));
        }

        let (_, _, name_span) = spatial.narrowest_containing(offset, &[NodeKind::Ident])?;
        let name = self.text_slice(mi, name_span);
        if name.is_empty() {
            return None;
        }
        let key = self.decl_referent_at(mid, offset, name)?;
        Some((key, name.to_owned()))
    }

    /// The top-level symbol whose declaration name sits at `offset`, as a
    /// [`ReferentKey::Symbol`]. Unlike [`Self::symbol_decl_referent`] (rename,
    /// `fn` / `const` only) this accepts any declaration whose uses key to a
    /// symbol — `fn`, `const`, `type`, actor — because a highlight is read-only
    /// and same-file. A constructor's declaration is excluded: its uses key to
    /// [`ReferentKey::Constructor`], so a symbol key would match nothing.
    fn decl_referent_at(&self, mid: ModuleId, offset: u32, name: &str) -> Option<ReferentKey> {
        let entries = &self.modules.get(mid.0 as usize)?.symbols.entries;
        entries.iter().enumerate().find_map(|(i, e)| {
            if e.name != name || !(e.def_span.start <= offset && offset < e.def_span.end) {
                return None;
            }
            match e.kind {
                SymbolKind::Fn { .. }
                | SymbolKind::Const
                | SymbolKind::Type { .. }
                | SymbolKind::Actor { .. } => {
                    let raw = u32::try_from(i).ok()?;
                    Some(ReferentKey::Symbol(mid, ridge_resolve::SymbolId(raw)))
                }
                _ => None,
            }
        })
    }

    /// The definition site of a referent, when it lives in the workspace.
    ///
    /// `None` for stdlib symbols and class methods — their definitions sit in
    /// the materialised stdlib sources, not in a workspace module, so the
    /// `includeDeclaration` toggle has nothing to add or remove for them.
    fn referent_def_location(&self, key: &ReferentKey) -> Option<Location> {
        match key {
            ReferentKey::Local(module, local_id) => {
                let span = self.find_local_def_span(module.0 as usize, *local_id)?;
                self.location_in(*module, span)
            }
            ReferentKey::Symbol(module, symbol) => {
                let span = self.symbol_def_span(*module, *symbol)?;
                self.location_in(*module, span)
            }
            ReferentKey::Constructor(owner_module, owner_type, _) => {
                let span = self.symbol_def_span(*owner_module, *owner_type)?;
                self.location_in(*owner_module, span)
            }
            ReferentKey::Stdlib(..) | ReferentKey::ClassMethod(..) => None,
        }
    }

    /// Answer a `prepareRename` request at an LSP `(line, utf16_col)` position.
    ///
    /// Returns the exact identifier range the editor should select plus its
    /// current text (the rename placeholder), or `None` when the cursor is not
    /// on a name this server can rename. Renameable: a local (parameter, `let`,
    /// state field), a top-level `fn` / `const`, a `type` (renaming a record
    /// type also rewrites its `User { .. }` constructions and patterns), and a
    /// record field (`user.age` and its declaration move together, scoped to the
    /// field's owner record). Not yet renameable (returns `None`): a union
    /// constructor, an actor, a stdlib symbol, a class method, or a module
    /// alias — see the deferred follow-ups. Reads only this immutable snapshot.
    #[must_use]
    pub fn prepare_rename_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
    ) -> Option<PrepareRenameResponse> {
        // A record field carries no binding on its name node; it takes the
        // type-directed path before the binding-keyed one.
        if let Some((mi, offset)) = self.field_offset(uri, line, utf16_col) {
            if let Some(resp) = self.field_prepare_rename_at(mi, offset) {
                return Some(resp);
            }
        }
        let target = self.rename_target_at(uri, line, utf16_col)?;
        Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: target.cursor_range,
            placeholder: target.name,
        })
    }

    /// Answer a `rename` request: a [`WorkspaceEdit`] that renames every
    /// occurrence of the symbol under the cursor to `new_name`.
    ///
    /// The edit set is exact: a qualified use (`Mod.item`) renames only the
    /// final segment, a top-level declaration renames only its name, and a
    /// selectively-imported symbol (`import p (item)`) has its import-clause
    /// item rewritten too, so a workspace `fn` / `const` rename stays
    /// compilable across files.
    ///
    /// Returns `Ok(None)` when the cursor is not on a renameable name (the same
    /// gate as [`Self::prepare_rename_at`]) and `Err(message)` when `new_name`
    /// is not a valid replacement (empty, a reserved keyword, or not a lowercase
    /// identifier) — the server forwards the message to the editor. Reads only
    /// this immutable snapshot.
    pub fn rename_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
        new_name: &str,
        cancel: &Cancel,
    ) -> Result<Option<WorkspaceEdit>, String> {
        // A record field renames through the type-directed path: its name node
        // carries no binding, and the edit set is scoped by the field's owner
        // record so a same-named field on another record is left untouched.
        if let Some((mi, offset)) = self.field_offset(uri, line, utf16_col) {
            if let Some(result) = self.field_rename_at(mi, offset, new_name) {
                return result.map(Some);
            }
        }
        let Some(target) = self.rename_target_at(uri, line, utf16_col) else {
            return Ok(None);
        };
        validate_new_name(new_name, &target.name)?;

        // Every site to edit, as a name-only `(module, span)`. Locals never
        // escape their module and their binder is stamped, so the scan of that
        // one module already covers the declaration. A workspace symbol can be
        // referenced (and selectively imported) from any module, and its
        // top-level declaration name carries no binding, so it is added apart.
        let mut sites: Vec<(ModuleId, Span)> = Vec::new();
        if let ReferentKey::Local(module, _) = &target.key {
            let smi = module.0 as usize;
            if let (Some(view), Some(spatial)) = (self.modules.get(smi), self.spatial.get(smi)) {
                for (span, _kind, nid) in &spatial.entries {
                    let Some(b) = view.bindings.get(nid.0 as usize).and_then(Option::as_ref) else {
                        continue;
                    };
                    if referent_key(b, *module).is_some_and(|k| target.matches(&k)) {
                        sites.push((*module, self.final_ident_span(smi, *span)));
                    }
                }
            }
        } else {
            for (smi, view) in self.modules.iter().enumerate() {
                // Cooperative cancellation: bail between modules. A cancelled
                // rename yields no edit; the handler future has already gone.
                if cancel.is_cancelled() {
                    return Ok(None);
                }
                let Ok(raw) = u32::try_from(smi) else {
                    continue;
                };
                let smid = ModuleId(raw);
                if let Some(spatial) = self.spatial.get(smi) {
                    for (span, _kind, nid) in &spatial.entries {
                        let Some(b) = view.bindings.get(nid.0 as usize).and_then(Option::as_ref)
                        else {
                            continue;
                        };
                        if referent_key(b, smid).is_some_and(|k| target.matches(&k)) {
                            sites.push((smid, self.final_ident_span(smi, *span)));
                        }
                    }
                }
                // A selective import (`import p (item)`) names the symbol in its
                // clause; rewrite that name too so the import stays valid.
                for imp in &view.imports {
                    let Some(items) = &imp.explicit_items else {
                        continue;
                    };
                    for item in items {
                        if let Some(b) = &item.resolved {
                            if referent_key(b, smid).is_some_and(|k| target.matches(&k)) {
                                if let Some(name_span) =
                                    self.import_item_span(smid, item.span, &item.name)
                                {
                                    sites.push((smid, name_span));
                                }
                            }
                        }
                    }
                }
            }
            if let Some(site) = self.referent_decl_name_span(&target.key, &target.name) {
                sites.push(site);
            }
        }

        // Resolve every site to a `Location`, bucket by file, and dedup so an
        // overlapping declaration/use collapses to one edit.
        let mut ranges: HashMap<Url, Vec<Range>> = HashMap::new();
        for (mid, span) in sites {
            if let Some(loc) = self.location_in(mid, span) {
                ranges.entry(loc.uri).or_default().push(loc.range);
            }
        }
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (edit_uri, mut rs) in ranges {
            rs.sort_by(|a, b| {
                (a.start.line, a.start.character, a.end.line, a.end.character).cmp(&(
                    b.start.line,
                    b.start.character,
                    b.end.line,
                    b.end.character,
                ))
            });
            rs.dedup();
            changes.insert(
                edit_uri,
                rs.into_iter()
                    .map(|range| TextEdit {
                        range,
                        new_text: new_name.to_owned(),
                    })
                    .collect(),
            );
        }
        if changes.is_empty() {
            return Ok(None);
        }
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    /// The renameable thing under the cursor: its referent, the exact name range
    /// to select, and the current name text. Resolves both a use-site (a name
    /// node carrying a binding, including a local binder) and a top-level
    /// declaration name (which carries no binding — resolved via the symbol
    /// table). `None` when the cursor is not on a renameable name.
    fn rename_target_at(&self, uri: &Url, line: u32, utf16_col: u32) -> Option<RenameTarget> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);
        let spatial = self.spatial.get(mi)?;
        let view = self.modules.get(mi)?;

        // A name node carrying a binding: a use-site, or a local binder (locals
        // are stamped at their definition site).
        if let Some((binding, span)) = spatial
            .enclosing(offset, &[NodeKind::Ident, NodeKind::QualifiedName])
            .into_iter()
            .find_map(|(nid, _, span)| {
                view.bindings
                    .get(nid.0 as usize)
                    .and_then(Option::as_ref)
                    .map(|b| (b, span))
            })
        {
            let key = self.renameable_referent(binding, mid)?;
            let extra = self.record_ctor_key(&key);
            let name_span = self.final_ident_span(mi, span);
            return Some(RenameTarget {
                key,
                extra,
                cursor_range: self.range_in(mid, name_span)?,
                name: self.text_slice(mi, name_span).to_owned(),
            });
        }

        // A top-level declaration name carries no binding; resolve it through
        // the symbol table (its def span encloses the cursor and the name
        // matches the ident under it).
        let (_, _, name_span) = spatial.narrowest_containing(offset, &[NodeKind::Ident])?;
        let name = self.text_slice(mi, name_span);
        if name.is_empty() {
            return None;
        }
        let key = self.symbol_decl_referent(mid, offset, name)?;
        let extra = self.record_ctor_key(&key);
        Some(RenameTarget {
            key,
            extra,
            cursor_range: self.range_in(mid, name_span)?,
            name: name.to_owned(),
        })
    }

    /// The [`ReferentKey`] a binding denotes, but only for the kinds this server
    /// can rename completely: a local, or a top-level `fn` / `const` / `type`.
    /// A record type reached through its shared-name constructor maps back to the
    /// type symbol so a rename started on `User { .. }` renames the type too; the
    /// constructor uses are then picked up via [`Self::record_ctor_key`]. Union
    /// and other constructors, actors, field accessors, stdlib symbols, class
    /// methods, and module aliases are deferred.
    fn renameable_referent(&self, binding: &Binding, module: ModuleId) -> Option<ReferentKey> {
        match binding {
            Binding::Local(id) => Some(ReferentKey::Local(module, *id)),
            Binding::ModuleSymbol { module, symbol }
            | Binding::ImportedSymbol { module, symbol, .. } => {
                match self.symbol_kind(*module, *symbol)? {
                    SymbolKind::Fn { .. } | SymbolKind::Const | SymbolKind::Type { .. } => {
                        Some(ReferentKey::Symbol(*module, *symbol))
                    }
                    _ => None,
                }
            }
            // A record type's auto-constructor shares the type's name (it is the
            // only constructor flagged `is_record`); renaming from a `User { .. }`
            // use renames the type and every reference together.
            Binding::Constructor {
                owner_module,
                owner_type,
                is_record: true,
                ..
            } => Some(ReferentKey::Symbol(*owner_module, *owner_type)),
            _ => None,
        }
    }

    /// The additional referent keys a rename of `key` must rewrite. A record
    /// type is the only case: its auto-constructor (variant 0) shares the type's
    /// name, so the `User { .. }` constructions and patterns — which key to a
    /// [`ReferentKey::Constructor`] — have to move with the type. Returns an
    /// empty vec for a union, an alias, or any non-type symbol.
    fn record_ctor_key(&self, key: &ReferentKey) -> Vec<ReferentKey> {
        let ReferentKey::Symbol(module, symbol) = key else {
            return Vec::new();
        };
        let Some(view) = self.modules.get(module.0 as usize) else {
            return Vec::new();
        };
        let is_record_type = view.symbols.entries.iter().any(|e| {
            matches!(
                &e.kind,
                SymbolKind::Constructor { owner_type, is_record: true, .. } if *owner_type == *symbol
            )
        });
        if is_record_type {
            vec![ReferentKey::Constructor(*module, *symbol, 0)]
        } else {
            Vec::new()
        }
    }

    /// Resolve a top-level declaration name at `offset` to a renameable
    /// referent: the symbol whose def span encloses the cursor and whose name
    /// matches. `fn`, `const`, and `type` declarations are renameable here. A
    /// record type's name resolves to its `Type` entry (which is collected
    /// before the auto-constructor), so a cursor on `type User` renames the type
    /// and — via [`Self::record_ctor_key`] — its constructor uses.
    fn symbol_decl_referent(&self, mid: ModuleId, offset: u32, name: &str) -> Option<ReferentKey> {
        let entries = &self.modules.get(mid.0 as usize)?.symbols.entries;
        entries.iter().enumerate().find_map(|(i, e)| {
            if e.name != name {
                return None;
            }
            if !(e.def_span.start <= offset && offset < e.def_span.end) {
                return None;
            }
            match e.kind {
                SymbolKind::Fn { .. } | SymbolKind::Const | SymbolKind::Type { .. } => {
                    let raw = u32::try_from(i).ok()?;
                    Some(ReferentKey::Symbol(mid, ridge_resolve::SymbolId(raw)))
                }
                _ => None,
            }
        })
    }

    /// The name-only span of a referent's declaration, for the rename edit set.
    ///
    /// Locals are stamped at their binder (the scan covers them), so this is
    /// used only for workspace symbols: their `def_span` covers the whole
    /// declaration, so the name is recovered as the leftmost stamped `Ident`
    /// inside it whose text matches.
    fn referent_decl_name_span(&self, key: &ReferentKey, name: &str) -> Option<(ModuleId, Span)> {
        match key {
            ReferentKey::Local(module, local_id) => {
                let span = self.find_local_def_span(module.0 as usize, *local_id)?;
                Some((*module, span))
            }
            ReferentKey::Symbol(module, symbol) => {
                let decl = self.symbol_def_span(*module, *symbol)?;
                let span = self.decl_name_span(*module, decl, name)?;
                Some((*module, span))
            }
            _ => None,
        }
    }

    /// The leftmost stamped `Ident` span inside `decl_span` whose text equals
    /// `name` — the declaration's name token (params and body uses come after).
    fn decl_name_span(&self, mid: ModuleId, decl_span: Span, name: &str) -> Option<Span> {
        let mi = mid.0 as usize;
        self.spatial
            .get(mi)?
            .entries
            .iter()
            .filter(|(span, kind, _)| {
                *kind == NodeKind::Ident
                    && decl_span.start <= span.start
                    && span.end <= decl_span.end
                    && self.text_slice(mi, *span) == name
            })
            .map(|(span, _, _)| *span)
            .min_by_key(|span| span.start)
    }

    /// The span of the import-list item named `name` inside the import whose
    /// declaration span is `import_span`. A resolved import item carries the
    /// whole-import span rather than the item-name token, so the token is
    /// recovered as the *rightmost* matching `Ident`: the item list always
    /// follows the module path and any `as` alias, so the last occurrence is the
    /// clause item, never a path segment or alias of the same name.
    fn import_item_span(&self, mid: ModuleId, import_span: Span, name: &str) -> Option<Span> {
        let mi = mid.0 as usize;
        self.spatial
            .get(mi)?
            .entries
            .iter()
            .filter(|(span, kind, _)| {
                *kind == NodeKind::Ident
                    && import_span.start <= span.start
                    && span.end <= import_span.end
                    && self.text_slice(mi, *span) == name
            })
            .map(|(span, _, _)| *span)
            .max_by_key(|span| span.start)
    }

    /// The selective-import clause item in module `mid` whose resolved binding
    /// denotes `key` — the `import … (item)` site that introduces the referent
    /// into this module. `None` when no selective import of this module brings
    /// the referent in (it is local, same-module, alias-qualified, or
    /// unresolved), so the caller falls back to the definition site.
    fn import_clause_location(&self, mid: ModuleId, key: &ReferentKey) -> Option<Location> {
        let imports = &self.modules.get(mid.0 as usize)?.imports;
        for imp in imports {
            let Some(items) = &imp.explicit_items else {
                continue;
            };
            for item in items {
                let denotes = item
                    .resolved
                    .as_ref()
                    .and_then(|b| referent_key(b, mid))
                    .is_some_and(|k| k == *key);
                if denotes {
                    if let Some(span) = self.import_item_span(mid, item.span, &item.name) {
                        return self.location_in(mid, span);
                    }
                }
            }
        }
        None
    }

    /// Narrow a reference `span` in module `mi` to its trailing identifier run.
    ///
    /// A plain ident returns itself; a qualified name (`Mod.item`) returns just
    /// the final `item` segment, so renaming never disturbs the qualifier.
    /// Identifiers are ASCII (`[A-Za-z0-9_]`), so byte offsets are char
    /// boundaries.
    fn final_ident_span(&self, mi: usize, span: Span) -> Span {
        let bytes = self.text_slice(mi, span).as_bytes();
        let mut start = bytes.len();
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        Span::new(span.start + u32::try_from(start).unwrap_or(0), span.end)
    }

    /// The [`SymbolKind`] of `symbol` in `module`, if present.
    fn symbol_kind(
        &self,
        module: ModuleId,
        symbol: ridge_resolve::SymbolId,
    ) -> Option<&SymbolKind> {
        self.modules
            .get(module.0 as usize)?
            .symbols
            .entries
            .get(symbol.0 as usize)
            .map(|e| &e.kind)
    }

    /// `def_span` of symbol `symbol` in module `module`.
    fn symbol_def_span(&self, module: ModuleId, symbol: ridge_resolve::SymbolId) -> Option<Span> {
        self.modules
            .get(module.0 as usize)?
            .symbols
            .entries
            .get(symbol.0 as usize)
            .map(|e| e.def_span)
    }

    /// `def_span` of the local `local_id`, searched across module `mi`'s scopes.
    fn find_local_def_span(&self, mi: usize, local_id: LocalId) -> Option<Span> {
        self.modules
            .get(mi)?
            .scopes
            .nodes
            .iter()
            .flat_map(|node| &node.locals)
            .find(|entry| entry.id == local_id)
            .map(|entry| entry.def_span)
    }

    /// Build a [`Location`] for `span` in module `mid`.
    fn location_in(&self, mid: ModuleId, span: Span) -> Option<Location> {
        let uri = self.module_uris.get(mid.0 as usize)?.clone()?;
        let range = self.range_in(mid, span)?;
        Some(Location { uri, range })
    }

    /// Convert a byte `span` in module `mid` to an LSP UTF-16 [`Range`].
    fn range_in(&self, mid: ModuleId, span: Span) -> Option<Range> {
        let li = self.line_indices.get(mid.0 as usize)?;
        let (start_line, start_char) = li.byte_to_utf16(span.start);
        let (end_line, end_char) = li.byte_to_utf16(span.end);
        Some(Range {
            start: Position {
                line: start_line,
                character: start_char,
            },
            end: Position {
                line: end_line,
                character: end_char,
            },
        })
    }

    /// Answer `workspace/willRenameFiles`: when `.ridge` files move, rewrite the
    /// `import` path of every other module that referenced them so the imports
    /// still resolve after the move.
    ///
    /// Each renamed file is mapped to the new fully-qualified name it takes at
    /// its destination; then every workspace import whose target is one of the
    /// moved modules has its dotted path replaced (the `as`/item list is left
    /// untouched). Returns `None` when no import needs to change — including the
    /// common cases of renaming a leaf module nobody imports, standalone files,
    /// or a move out of the project's source tree.
    #[must_use]
    pub fn rename_files_edit(&self, files: &[FileRename]) -> Option<WorkspaceEdit> {
        // Resolve each rename to (moved module id, its new fully-qualified name).
        let mut moved: Vec<(ModuleId, String)> = Vec::new();
        for file in files {
            let Ok(old_url) = Url::parse(&file.old_uri) else {
                continue;
            };
            let Some(old_mid) = self.module_id_for(&old_url) else {
                continue;
            };
            let (Ok(old_path), Ok(new_url)) = (old_url.to_file_path(), Url::parse(&file.new_uri))
            else {
                continue;
            };
            let Ok(new_path) = new_url.to_file_path() else {
                continue;
            };
            if let Some(new_fqn) = self.renamed_module_fqn(old_mid, &old_path, &new_path) {
                moved.push((old_mid, new_fqn));
            }
        }
        if moved.is_empty() {
            return None;
        }

        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (mi, view) in self.modules.iter().enumerate() {
            let Some(ast) = view.ast.as_ref() else {
                continue;
            };
            let Some(uri) = self.module_uris.get(mi).cloned().flatten() else {
                continue;
            };
            let Ok(mid_raw) = u32::try_from(mi) else {
                continue;
            };
            let mid = ModuleId(mid_raw);
            for imp in &view.imports {
                let ImportTarget::WorkspaceModule(target) = imp.target else {
                    continue;
                };
                let Some((_, new_fqn)) = moved.iter().find(|(m, _)| *m == target) else {
                    continue;
                };
                // Correlate the resolved import with its AST declaration by the
                // full span both record, then rewrite only the dotted path. The
                // path span runs from the first segment to the last — narrower
                // than `ModulePath::span`, which the parser anchors back at the
                // `import` keyword — so the `import ` prefix and any `as`/item
                // list are left untouched.
                let Some(path_span) = ast.items.iter().find_map(|item| match item {
                    ridge_ast::Item::Import(decl) if decl.span == imp.span => {
                        let segments = &decl.path.segments;
                        match (segments.first(), segments.last()) {
                            (Some(first), Some(last)) => Some(Span {
                                start: first.span.start,
                                end: last.span.end,
                            }),
                            _ => Some(decl.path.span),
                        }
                    }
                    _ => None,
                }) else {
                    continue;
                };
                let Some(range) = self.range_in(mid, path_span) else {
                    continue;
                };
                changes.entry(uri.clone()).or_default().push(TextEdit {
                    range,
                    new_text: new_fqn.clone(),
                });
            }
        }
        if changes.is_empty() {
            return None;
        }
        Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        })
    }

    /// Compute the fully-qualified name a module takes when its file moves from
    /// `old_path` to `new_path`.
    ///
    /// The work stays in the client's own path coordinates so it is immune to
    /// the canonical-vs-client normalization gap on Windows (the same gap that
    /// nulled every query before the URI-key fix): the project's source root is
    /// recovered by trimming the module's path depth off `old_path`, never by
    /// comparing against the canonical paths the workspace graph stores. The
    /// depth is the number of dotted segments the module's name carries below
    /// its project prefix (a file directly in `src/` has depth 1). `None` when
    /// the move leaves the project's source tree — there is no name for it then.
    fn renamed_module_fqn(
        &self,
        mid: ModuleId,
        old_path: &Path,
        new_path: &Path,
    ) -> Option<String> {
        let i = mid.0 as usize;
        let project = self.module_project_names.get(i)?;
        let fqn = self.module_fqns.get(i)?;
        let tail = fqn
            .strip_prefix(project.as_str())
            .and_then(|t| t.strip_prefix('.'));
        let depth = tail.map_or(1, |t| t.split('.').count());
        let mut src_root = old_path;
        for _ in 0..depth {
            src_root = src_root.parent()?;
        }
        if !new_path.starts_with(src_root) {
            return None;
        }
        Some(ridge_resolve::derive_module_fqn(
            project, src_root, new_path,
        ))
    }

    /// Go-to-definition for a record-field use under the cursor (`user.age` →
    /// the field's declaration in the owning `type`). `None` off any field name
    /// or when the base's type is structural, unresolved, or not a record.
    fn field_definition_at(&self, mi: usize, offset: u32) -> Option<Location> {
        let (tycon, field, _field_span) = self.field_access_at(mi, offset)?;
        self.field_decl_location(tycon, &field)
    }

    /// The record field under the cursor as `(owner record TyConId raw, field
    /// name, field name span)`. Walks this module's AST for the narrowest
    /// `base.field` access whose field range covers `offset`, then reads the base
    /// expression's inferred type and peels it to the nominal record it belongs
    /// to. The span is the field token under the cursor (used to select the
    /// rename range and to highlight the use).
    fn field_access_at(&self, mi: usize, offset: u32) -> Option<(u32, String, Span)> {
        let ast = self.modules.get(mi)?.ast.clone()?;
        let mut finder = FieldAccessFinder { offset, best: None };
        ridge_ast::visit::Visit::visit_module(&mut finder, &ast);
        let (base_span, field_span, field) = finder.best?;
        let base_ty = self.type_of_base(mi, base_span)?;
        let tycon = record_tycon_of(base_ty, &self.tycons)?;
        Some((tycon, field, field_span))
    }

    /// The inferred type stamped on the expression node spanning exactly
    /// `span` in module `mi` (or the widest node contained within it).
    fn type_of_base(&self, mi: usize, span: Span) -> Option<&Type> {
        let nid = self
            .spatial
            .get(mi)?
            .node_for_span_or_inner(span, NodeKind::Expr)?;
        self.modules
            .get(mi)?
            .node_types
            .get(nid.0 as usize)?
            .as_ref()
    }

    /// Location of the declaration of `field` on the nominal record `tycon`
    /// (raw `TyConId`): the field name's span in the `type` that declares it.
    fn field_decl_location(&self, tycon_raw: u32, field: &str) -> Option<Location> {
        let decl = self.tycons.iter().find(|d| d.id.0 == tycon_raw)?;
        let owner = ModuleId(decl.def_module_raw?);
        let ast = self.modules.get(owner.0 as usize)?.ast.as_ref()?;
        let span = field_decl_span(ast, &decl.name, field)?;
        self.location_in(owner, span)
    }

    /// Find-references for the record field under the cursor. Returns `Some`
    /// (possibly empty) when the cursor sits on a field use or a field
    /// declaration, so the caller stops before the binding-keyed path; `None`
    /// otherwise. Use sites are collected across every module and filtered by
    /// the declaration their base type resolves to, so two records sharing a
    /// field name never cross-contaminate. Keying on the declaration location —
    /// rather than the raw `TyConId` — also unifies the several tycon ids the
    /// type checker may mint for one nominal record.
    fn field_references_at(
        &self,
        mi: usize,
        offset: u32,
        include_declaration: bool,
    ) -> Option<Vec<Location>> {
        let (tycon, field, _) = self.field_target_at(mi, offset)?;
        let target_decl = self.field_decl_location(tycon, &field)?;

        let mut locations: Vec<Location> = self
            .field_use_sites(&field, &target_decl, None)
            .into_iter()
            .filter_map(|(smid, span)| self.location_in(smid, span))
            .collect();

        // The declaration site: included on request, otherwise dropped (the use
        // scan never carries it, but the retain keeps the two paths symmetric).
        if include_declaration {
            locations.push(target_decl.clone());
        } else {
            locations.retain(|loc| *loc != target_decl);
        }

        locations.sort_by(|a, b| {
            (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
                b.uri.as_str(),
                b.range.start.line,
                b.range.start.character,
            ))
        });
        locations.dedup();
        Some(locations)
    }

    /// Every `base.field` use whose base type resolves to the same declaration
    /// as `target_decl`, as `(module, field-name span)`. Scans the whole
    /// workspace, or a single module when `only` is set (the same-file
    /// `documentHighlight` case). Keying on the resolved declaration — rather
    /// than the raw `TyConId` — unifies the several tycon ids the type checker
    /// may mint for one nominal record and keeps two records that share a field
    /// name apart. Shared by find-references, rename, and documentHighlight.
    fn field_use_sites(
        &self,
        field: &str,
        target_decl: &Location,
        only: Option<usize>,
    ) -> Vec<(ModuleId, Span)> {
        // Many use sites share an owner tycon; resolve each tycon's declaration
        // once. A `None` entry records a tycon that does not declare the field.
        let mut decl_of: HashMap<u32, Option<Location>> = HashMap::new();
        let mut sites: Vec<(ModuleId, Span)> = Vec::new();
        let (lo, hi) = only.map_or((0, self.modules.len()), |m| (m, m + 1));
        for smi in lo..hi {
            let Some(ast) = self.modules.get(smi).and_then(|v| v.ast.clone()) else {
                continue;
            };
            let Ok(raw) = u32::try_from(smi) else {
                continue;
            };
            let smid = ModuleId(raw);
            let mut collector = FieldSiteCollector {
                field: field.to_owned(),
                sites: Vec::new(),
            };
            ridge_ast::visit::Visit::visit_module(&mut collector, &ast);
            for (base_span, field_span) in collector.sites {
                let Some(base_ty) = self.type_of_base(smi, base_span) else {
                    continue;
                };
                let Some(use_tycon) = record_tycon_of(base_ty, &self.tycons) else {
                    continue;
                };
                let decl = decl_of
                    .entry(use_tycon)
                    .or_insert_with(|| self.field_decl_location(use_tycon, field));
                if decl.as_ref() == Some(target_decl) {
                    sites.push((smid, field_span));
                }
            }
        }
        sites
    }

    /// The `(owner record TyConId raw, field name, field name span)` targeted by
    /// the cursor — from a field use (`user.age`) or a field declaration inside a
    /// `type` body. The span is the token under the cursor. `None` when the
    /// cursor is on neither.
    fn field_target_at(&self, mi: usize, offset: u32) -> Option<(u32, String, Span)> {
        self.field_access_at(mi, offset)
            .or_else(|| self.field_decl_target_at(mi, offset))
    }

    /// The `(owner TyConId raw, field name, field name span)` when the cursor
    /// sits on a field name in a record `type` declaration in module `mi`.
    fn field_decl_target_at(&self, mi: usize, offset: u32) -> Option<(u32, String, Span)> {
        let ast = self.modules.get(mi)?.ast.as_ref()?;
        for item in &ast.items {
            let ridge_ast::Item::Type(td) = item else {
                continue;
            };
            let ridge_ast::TypeBody::Record(rb) = &td.body else {
                continue;
            };
            for f in &rb.fields {
                if f.name.span.start <= offset && offset < f.name.span.end {
                    let tycon = self.tycon_raw_for(mi, &td.name.text)?;
                    return Some((tycon, f.name.text.clone(), f.name.span));
                }
            }
        }
        None
    }

    /// Resolve an LSP `(line, utf16_col)` position to a `(module index, byte
    /// offset)`, the form the record-field helpers work in. `None` when the URI
    /// is not indexed.
    fn field_offset(&self, uri: &Url, line: u32, utf16_col: u32) -> Option<(usize, u32)> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);
        Some((mi, offset))
    }

    /// `prepareRename` for a record field under the cursor: the field token's
    /// range plus its current name. `None` when the cursor is not on a field
    /// use or a field declaration, so the caller falls through to the
    /// binding-keyed rename path.
    fn field_prepare_rename_at(&self, mi: usize, offset: u32) -> Option<PrepareRenameResponse> {
        let (_tycon, field, cursor_span) = self.field_target_at(mi, offset)?;
        let mid = ModuleId(u32::try_from(mi).ok()?);
        Some(PrepareRenameResponse::RangeWithPlaceholder {
            range: self.range_in(mid, cursor_span)?,
            placeholder: field,
        })
    }

    /// Rename a record field under the cursor to `new_name`.
    ///
    /// Returns `None` when the cursor is not on a field (the caller then runs the
    /// binding-keyed rename), `Some(Err(message))` when `new_name` is invalid (an
    /// empty name, a keyword, a non-lowercase identifier, or a name the record
    /// already uses for another field), and `Some(Ok(edit))` otherwise. The edit
    /// rewrites the field's declaration and every use whose base type resolves to
    /// it across the workspace — never a same-named field on a different record.
    fn field_rename_at(
        &self,
        mi: usize,
        offset: u32,
        new_name: &str,
    ) -> Option<Result<WorkspaceEdit, String>> {
        let (tycon, field, _) = self.field_target_at(mi, offset)?;
        let target_decl = self.field_decl_location(tycon, &field)?;
        if let Err(message) = validate_new_name(new_name, &field) {
            return Some(Err(message));
        }
        // Renaming onto an existing field name would collapse two fields into a
        // duplicate; reject it before producing an edit.
        if new_name != field && self.record_has_field(tycon, new_name) {
            return Some(Err(format!(
                "`{new_name}` is already a field of this record."
            )));
        }

        let mut ranges: HashMap<Url, Vec<Range>> = HashMap::new();
        for (smid, span) in self.field_use_sites(&field, &target_decl, None) {
            if let Some(loc) = self.location_in(smid, span) {
                ranges.entry(loc.uri).or_default().push(loc.range);
            }
        }
        // The declaration name always moves with the field.
        ranges
            .entry(target_decl.uri.clone())
            .or_default()
            .push(target_decl.range);

        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (edit_uri, mut rs) in ranges {
            rs.sort_by(|a, b| {
                (a.start.line, a.start.character, a.end.line, a.end.character).cmp(&(
                    b.start.line,
                    b.start.character,
                    b.end.line,
                    b.end.character,
                ))
            });
            rs.dedup();
            changes.insert(
                edit_uri,
                rs.into_iter()
                    .map(|range| TextEdit {
                        range,
                        new_text: new_name.to_owned(),
                    })
                    .collect(),
            );
        }
        Some(Ok(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }

    /// `documentHighlight` for a record field under the cursor: every use of the
    /// field *in this file* as a read, plus its declaration name as a write when
    /// the declaring `type` lives in this file. `None` when the cursor is not on
    /// a field, so the caller falls through to the binding-keyed path.
    fn field_highlights_at(&self, mi: usize, offset: u32) -> Option<Vec<DocumentHighlight>> {
        let (tycon, field, _) = self.field_target_at(mi, offset)?;
        let target_decl = self.field_decl_location(tycon, &field)?;
        let mid = ModuleId(u32::try_from(mi).ok()?);

        // Keyed by byte span so an occurrence is emitted once.
        let mut spots: HashMap<(u32, u32), DocumentHighlightKind> = HashMap::new();
        for (_smid, span) in self.field_use_sites(&field, &target_decl, Some(mi)) {
            spots
                .entry((span.start, span.end))
                .or_insert(DocumentHighlightKind::READ);
        }
        // The declaration token is the write site, but only when the `type` that
        // declares the field sits in this file (highlights never leave it).
        if let Some(decl) = self.tycons.iter().find(|d| d.id.0 == tycon) {
            if decl.def_module_raw == Some(mid.0) {
                if let Some(span) = self
                    .modules
                    .get(mi)
                    .and_then(|m| m.ast.as_ref())
                    .and_then(|ast| field_decl_span(ast, &decl.name, &field))
                {
                    spots.insert((span.start, span.end), DocumentHighlightKind::WRITE);
                }
            }
        }
        if spots.is_empty() {
            return None;
        }

        let mut highlights: Vec<DocumentHighlight> = spots
            .into_iter()
            .filter_map(|((start, end), kind)| {
                Some(DocumentHighlight {
                    range: self.range_in(mid, Span::new(start, end))?,
                    kind: Some(kind),
                })
            })
            .collect();
        highlights.sort_by(|a, b| {
            (a.range.start.line, a.range.start.character)
                .cmp(&(b.range.start.line, b.range.start.character))
        });
        Some(highlights)
    }

    /// True when the nominal record `tycon_raw` declares a field named `name`.
    /// Used to reject a field rename that would duplicate an existing field.
    fn record_has_field(&self, tycon_raw: u32, name: &str) -> bool {
        self.tycons
            .iter()
            .find(|d| d.id.0 == tycon_raw)
            .is_some_and(|d| match &d.kind {
                TyConKind::Record(schema) => schema.record_fields().iter().any(|f| f.name == name),
                _ => false,
            })
    }

    /// Raw `TyConId` of the type named `name` declared in module `mi`.
    fn tycon_raw_for(&self, mi: usize, name: &str) -> Option<u32> {
        let raw = u32::try_from(mi).ok()?;
        self.tycons
            .iter()
            .find(|d| d.def_module_raw == Some(raw) && d.name == name)
            .map(|d| d.id.0)
    }

    /// Location of the `type` declaration for tycon `raw` — its name span when
    /// the declaring module's AST is available, else the whole-declaration
    /// span. `None` for a built-in (no source span) or a type whose module
    /// carries no URI.
    fn tycon_location(&self, raw: u32) -> Option<Location> {
        let decl = self.tycons.iter().find(|d| d.id.0 == raw)?;
        let owner = ModuleId(decl.def_module_raw?);
        let span = self
            .modules
            .get(owner.0 as usize)
            .and_then(|m| m.ast.as_ref())
            .and_then(|ast| type_decl_name_span(ast, &decl.name))
            .or(decl.def_span)?;
        self.location_in(owner, span)
    }

    /// Build the outline (`textDocument/documentSymbol`) for one document.
    ///
    /// Top-level `fn`/`const`/`type`/`actor` declarations become symbols in
    /// source order. A type nests its union variants or record fields; an actor
    /// nests its state fields and message handlers. Compiler-synthesised mirror
    /// symbols (from `deriving (Table)`/`(Schema)`, which share the entity's
    /// declaration span) are folded into the entity rather than listed
    /// separately. `class`/`instance` declarations are not modelled by the
    /// resolver yet, so they do not appear.
    #[must_use]
    pub fn document_symbols_at(&self, uri: &Url) -> Option<Vec<DocumentSymbol>> {
        let mid = self.module_id_for(uri)?;
        let entries = &self.modules.get(mid.0 as usize)?.symbols.entries;

        let mut seen_spans: Vec<Span> = Vec::new();
        let mut out: Vec<DocumentSymbol> = Vec::new();
        for entry in entries {
            // Members (constructors, field accessors) attach to their owning type
            // below, not at the top level.
            if matches!(
                entry.kind,
                SymbolKind::Constructor { .. } | SymbolKind::FieldAccessor { .. }
            ) {
                continue;
            }
            // Drop the synthesised mirror symbols that share their entity's span.
            if seen_spans.contains(&entry.def_span) {
                continue;
            }
            if let Some(sym) = self.document_symbol_for(mid, entry, entries) {
                seen_spans.push(entry.def_span);
                out.push(sym);
            }
        }
        out.sort_by_key(|s| (s.range.start.line, s.range.start.character));
        Some(out)
    }

    /// Build the code lenses (`textDocument/codeLens`) for one document, limited
    /// to the kinds the client enabled in `cfg`.
    ///
    /// The list phase stays cheap: navigational lenses ("N references", "N
    /// implementations") carry only their anchor and resolve their count lazily
    /// in [`resolve_code_lens`](Self::resolve_code_lens), so the workspace-wide
    /// scan runs only for the lenses the editor actually shows. The executable
    /// lenses ("Run", "Run test") carry a ready command, since their target is
    /// known without a scan. Reads only this immutable snapshot; never compiles.
    #[must_use]
    pub fn code_lenses_at(&self, uri: &Url, cfg: CodeLensConfig) -> Option<Vec<CodeLens>> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let ast = self.modules.get(mi)?.ast.as_ref()?;
        let runnable = self.module_runnable.get(mi).copied().unwrap_or(false);
        let project = self.module_project_names.get(mi).map_or("", String::as_str);

        let mut out: Vec<CodeLens> = Vec::new();
        for item in &ast.items {
            match item {
                ridge_ast::Item::Fn(f) => {
                    if cfg.run && runnable && f.name.text == "main" {
                        if let Some(range) = self.range_in(mid, f.name.span) {
                            out.push(command_code_lens(
                                range,
                                "Run",
                                RUN_COMMAND,
                                vec![serde_json::Value::String(project.to_owned())],
                            ));
                        }
                    }
                    if cfg.run_test {
                        if let Some(name) = test_display_name(&f.attrs) {
                            if let Some(range) = self.range_in(mid, f.name.span) {
                                out.push(command_code_lens(
                                    range,
                                    "Run test",
                                    RUN_TEST_COMMAND,
                                    vec![
                                        serde_json::Value::String(project.to_owned()),
                                        serde_json::Value::String(name.to_owned()),
                                    ],
                                ));
                            }
                        }
                    }
                    if cfg.references {
                        if let Some(lens) = self.reference_lens(uri, mid, &f.name.text, f.name.span)
                        {
                            out.push(lens);
                        }
                    }
                }
                ridge_ast::Item::Const(d) if cfg.references => {
                    if let Some(lens) = self.reference_lens(uri, mid, &d.name.text, d.name.span) {
                        out.push(lens);
                    }
                }
                ridge_ast::Item::Type(d) if cfg.references => {
                    if let Some(lens) = self.reference_lens(uri, mid, &d.name.text, d.name.span) {
                        out.push(lens);
                    }
                }
                ridge_ast::Item::Actor(d) if cfg.references => {
                    if let Some(lens) = self.reference_lens(uri, mid, &d.name.text, d.name.span) {
                        out.push(lens);
                    }
                }
                ridge_ast::Item::ClassDecl(d) if cfg.implementations => {
                    if let Some(range) = self.range_in(mid, d.name.span) {
                        out.push(nav_code_lens(uri, range, "implementations"));
                    }
                }
                _ => {}
            }
        }
        Some(out)
    }

    /// Build the "N references" lens for a top-level declaration.
    ///
    /// The count is a workspace-wide scan, so it is left for `codeLens/resolve`;
    /// the lens records the declaration's resolved symbol because the name node it
    /// sits above carries no binding — the resolve step keys off `{module, symbol}`
    /// rather than re-deriving the referent from the cursor (which only works at a
    /// use site). `None` for a declaration whose uses don't key to a symbol.
    fn reference_lens(
        &self,
        uri: &Url,
        mid: ModuleId,
        name: &str,
        name_span: Span,
    ) -> Option<CodeLens> {
        let ReferentKey::Symbol(module, symbol) =
            self.decl_referent_at(mid, name_span.start, name)?
        else {
            return None;
        };
        let range = self.range_in(mid, name_span)?;
        Some(CodeLens {
            range,
            command: None,
            data: Some(serde_json::json!({
                "kind": "references",
                "uri": uri.as_str(),
                "line": range.start.line,
                "character": range.start.character,
                "module": module.0,
                "symbol": symbol.0,
            })),
        })
    }

    /// Resolve a navigational code lens (`codeLens/resolve`): fill in the
    /// reference / implementation count and the `editor.action.showReferences`
    /// command that opens the peek. The workspace-wide scan runs here, lazily, so
    /// only the lenses the editor actually shows pay for it. A lens that already
    /// carries a command (the Run / Run-test lenses) or whose payload can't be read
    /// is returned unchanged.
    #[must_use]
    pub fn resolve_code_lens(&self, mut lens: CodeLens, cancel: &Cancel) -> CodeLens {
        if lens.command.is_some() {
            return lens;
        }
        let Some(data) = lens.data.as_ref() else {
            return lens;
        };
        let read_u32 = |key: &str| {
            data.get(key)
                .and_then(serde_json::Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        let (Some(kind), Some(uri), Some(line), Some(character)) = (
            data.get("kind").and_then(serde_json::Value::as_str),
            data.get("uri")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| Url::parse(s).ok()),
            read_u32("line"),
            read_u32("character"),
        ) else {
            return lens;
        };

        let (locations, noun) = match kind {
            "references" => {
                let (Some(module), Some(symbol)) = (read_u32("module"), read_u32("symbol")) else {
                    return lens;
                };
                let key = ReferentKey::Symbol(ModuleId(module), ridge_resolve::SymbolId(symbol));
                (
                    self.references_to_key(&key, false, cancel)
                        .unwrap_or_default(),
                    "reference",
                )
            }
            "implementations" => (
                self.implementations_at(&uri, line, character)
                    .unwrap_or_default(),
                "implementation",
            ),
            _ => return lens,
        };

        let position = Position { line, character };
        let arguments = vec![
            serde_json::Value::String(uri.to_string()),
            serde_json::to_value(position).unwrap_or(serde_json::Value::Null),
            serde_json::to_value(&locations).unwrap_or(serde_json::Value::Null),
        ];
        lens.command = Some(Command {
            title: pluralize(locations.len(), noun),
            command: "editor.action.showReferences".to_owned(),
            arguments: Some(arguments),
        });
        lens
    }

    /// Foldable regions for one document (`textDocument/foldingRange`).
    ///
    /// Two kinds of fold are produced from the retained AST: a run of one or more
    /// consecutive `import` declarations collapses as a single `Imports` fold, and
    /// every other top-level declaration (const, type, fn, actor, class, instance)
    /// that spans more than one line collapses as a `Region` fold. Single-line
    /// declarations yield nothing — there is nothing to hide. Finer-grained folds
    /// (nested blocks, match arms) are intentionally left for a later cut.
    #[must_use]
    pub fn folding_ranges_at(&self, uri: &Url) -> Option<Vec<FoldingRange>> {
        let mid = self.module_id_for(uri)?;
        let ast = self.modules.get(mid.0 as usize)?.ast.as_ref()?;

        let mut out: Vec<FoldingRange> = Vec::new();
        let items = &ast.items;
        let mut i = 0;
        while i < items.len() {
            if matches!(items[i], ridge_ast::Item::Import(_)) {
                // Fold a maximal run of consecutive imports as one block.
                let start = item_span(&items[i]).start;
                let mut end = item_span(&items[i]).end;
                while i < items.len() && matches!(items[i], ridge_ast::Item::Import(_)) {
                    end = item_span(&items[i]).end;
                    i += 1;
                }
                if let Some(fold) =
                    self.folding_range_for(mid, Span::new(start, end), &FoldingRangeKind::Imports)
                {
                    out.push(fold);
                }
            } else {
                if let Some(fold) =
                    self.folding_range_for(mid, item_span(&items[i]), &FoldingRangeKind::Region)
                {
                    out.push(fold);
                }
                i += 1;
            }
        }
        Some(out)
    }

    /// A line-level [`FoldingRange`] for `span`, or `None` when it does not cross
    /// a line boundary (nothing to fold).
    ///
    /// The parser ends a declaration span at the start of the following token, so
    /// the raw span trails into the blank lines (and the next item's first line)
    /// after the declaration. Trailing whitespace is trimmed off the span first,
    /// so the fold ends on the declaration's own last line of content.
    fn folding_range_for(
        &self,
        mid: ModuleId,
        span: Span,
        kind: &FoldingRangeKind,
    ) -> Option<FoldingRange> {
        let text: &str = self.module_text.get(mid.0 as usize).map_or("", |t| &**t);
        let range = self.range_in(mid, trim_trailing_ws(text, span))?;
        if range.end.line <= range.start.line {
            return None;
        }
        Some(FoldingRange {
            start_line: range.start.line,
            start_character: None,
            end_line: range.end.line,
            end_character: None,
            kind: Some(kind.clone()),
            collapsed_text: None,
        })
    }

    /// Selection-range hierarchies for `textDocument/selectionRange` — the
    /// editor's smart expand/shrink-selection command.
    ///
    /// For each requested position, returns the chain of progressively larger
    /// source ranges the command steps through: the narrowest stamped node under
    /// the cursor, then each enclosing node, the whole enclosing top-level
    /// declaration, and finally the whole file. The result holds one entry per
    /// input position, in order, as the protocol requires. Reads only this
    /// immutable snapshot; never triggers a compile.
    #[must_use]
    pub fn selection_ranges_at(
        &self,
        uri: &Url,
        positions: &[Position],
    ) -> Option<Vec<SelectionRange>> {
        let mid = self.module_id_for(uri)?;
        let li = self.line_indices.get(mid.0 as usize)?;
        let out = positions
            .iter()
            .map(|pos| self.selection_chain_at(mid, li.utf16_to_byte(pos.line, pos.character)))
            .collect();
        Some(out)
    }

    /// The nested [`SelectionRange`] for a single byte `offset` in module `mid`.
    ///
    /// Collects every level that brackets the offset — each stamped node, the
    /// enclosing top-level declaration, and the whole file — then keeps only a
    /// strictly-nesting chain (each level fully contains the previous one and is
    /// strictly larger). Building from a strict chain tolerates sibling or
    /// span-bled entries without ever emitting a parent that fails to contain
    /// its child, which the protocol forbids.
    fn selection_chain_at(&self, mid: ModuleId, offset: u32) -> SelectionRange {
        let mi = mid.0 as usize;
        let text: &str = self.module_text.get(mi).map_or("", |t| &**t);

        // Every level that brackets the offset is a candidate. The end is
        // inclusive so the cursor at the end of a token still selects it.
        let brackets = |s: &Span| s.start <= offset && offset <= s.end;
        let mut spans: Vec<Span> = Vec::new();
        if let Some(spatial) = self.spatial.get(mi) {
            spans.extend(
                spatial
                    .entries
                    .iter()
                    .filter(|(span, _, _)| brackets(span))
                    .map(|(span, _, _)| *span),
            );
        }
        // The enclosing top-level declaration adds an "expand to the whole
        // item" step; trim its parser span-bleed first (see `trim_trailing_ws`).
        if let Some(ast) = self.modules.get(mi).and_then(|m| m.ast.as_ref()) {
            for item in &ast.items {
                let s = item_span(item);
                if brackets(&s) {
                    spans.push(trim_trailing_ws(text, s));
                }
            }
        }
        // The whole file: the guaranteed outermost level, so expand-selection
        // can always step out to the document.
        spans.push(Span::new(0, u32::try_from(text.len()).unwrap_or(u32::MAX)));

        // Narrowest first; on equal width prefer the span starting later (the one
        // closer to the cursor from the left). Identical spans collapse.
        spans.sort_by_key(|s| (s.end - s.start, std::cmp::Reverse(s.start)));
        spans.dedup();

        let mut chain: Vec<Span> = Vec::new();
        for s in spans {
            let keep = chain.last().is_none_or(|prev| {
                s.start <= prev.start
                    && prev.end <= s.end
                    && (s.start < prev.start || s.end > prev.end)
            });
            if keep {
                chain.push(s);
            }
        }

        // Fold the chain outermost-inward so the returned (innermost) range
        // carries the full parent chain.
        let mut acc: Option<Box<SelectionRange>> = None;
        for span in chain.iter().rev() {
            if let Some(range) = self.range_in(mid, *span) {
                acc = Some(Box::new(SelectionRange { range, parent: acc }));
            }
        }
        acc.map_or_else(
            || SelectionRange {
                range: self.range_in(mid, Span::point(offset)).unwrap_or_default(),
                parent: None,
            },
            |b| *b,
        )
    }

    /// Answer `textDocument/prepareCallHierarchy` at an LSP `(line, col)`.
    ///
    /// Anchors a call-hierarchy session on the workspace `fn` under the cursor —
    /// either a use of it or its own declaration name. The single returned item
    /// carries the `(module, symbol)` identity in its `data`, which the
    /// incoming/outgoing requests decode. `None` for anything that is not a
    /// workspace function (a local, a stdlib symbol, a type, whitespace). Reads
    /// only this immutable snapshot; never triggers a compile.
    #[must_use]
    pub fn prepare_call_hierarchy_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
    ) -> Option<Vec<CallHierarchyItem>> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);
        let (module, symbol) = self.callable_fn_at(mid, offset)?;
        Some(vec![self.call_hierarchy_item(module, symbol)?])
    }

    /// The workspace `fn` denoted at `offset`: a use site whose binding resolves
    /// to a top-level `fn`, or a `fn` declaration name (which carries no
    /// binding, so it is matched against the symbol table directly).
    fn callable_fn_at(
        &self,
        mid: ModuleId,
        offset: u32,
    ) -> Option<(ModuleId, ridge_resolve::SymbolId)> {
        // Use site → follow the binding to its defining (module, symbol).
        if let Some(binding) = self.binding_at(mid.0 as usize, offset) {
            if let Some(ReferentKey::Symbol(m, s)) = referent_key(binding, mid) {
                if matches!(self.symbol_kind(m, s), Some(SymbolKind::Fn { .. })) {
                    return Some((m, s));
                }
            }
            return None;
        }
        // Declaration name → the top-level `fn` whose name token sits here.
        let spatial = self.spatial.get(mid.0 as usize)?;
        let (_, _, name_span) = spatial.narrowest_containing(offset, &[NodeKind::Ident])?;
        let name = self.text_slice(mid.0 as usize, name_span);
        let entries = &self.modules.get(mid.0 as usize)?.symbols.entries;
        entries.iter().enumerate().find_map(|(i, e)| {
            (e.name == name
                && matches!(e.kind, SymbolKind::Fn { .. })
                && e.def_span.start <= offset
                && offset < e.def_span.end)
                .then(|| Some((mid, ridge_resolve::SymbolId(u32::try_from(i).ok()?))))
                .flatten()
        })
    }

    /// Build a [`CallHierarchyItem`] for a workspace declaration, embedding its
    /// `(module, symbol)` identity in `data`. Only a `fn`, `const`, or actor is
    /// emitted (the kinds that can contain or be a call); `None` otherwise.
    fn call_hierarchy_item(
        &self,
        module: ModuleId,
        symbol: ridge_resolve::SymbolId,
    ) -> Option<CallHierarchyItem> {
        let mi = module.0 as usize;
        let entry = self
            .modules
            .get(mi)?
            .symbols
            .entries
            .get(symbol.0 as usize)?;
        let kind = match entry.kind {
            SymbolKind::Fn { .. } => LspSymbolKind::FUNCTION,
            SymbolKind::Const => LspSymbolKind::CONSTANT,
            SymbolKind::Actor { .. } => LspSymbolKind::CLASS,
            _ => return None,
        };
        let uri = self.module_uris.get(mi)?.clone()?;
        let range = self.range_in(module, entry.def_span)?;
        let name_span = self
            .decl_name_span(module, entry.def_span, &entry.name)
            .unwrap_or(entry.def_span);
        let selection_range = self.range_in(module, name_span)?;
        Some(CallHierarchyItem {
            name: entry.name.clone(),
            kind,
            tags: None,
            detail: None,
            uri,
            range,
            selection_range,
            data: Some(serde_json::json!({ "module": module.0, "symbol": symbol.0 })),
        })
    }

    /// Answer `callHierarchy/incomingCalls` for a prepared item.
    ///
    /// Every call site of the item's `fn` across the workspace, grouped by the
    /// top-level declaration (`fn`, `const`, or actor) that contains it: each
    /// group is one caller plus the ranges it calls from. `None` when the item's
    /// `data` does not decode to a workspace symbol. Reads only this immutable
    /// snapshot.
    #[must_use]
    pub fn incoming_calls(
        &self,
        data: &serde_json::Value,
        cancel: &Cancel,
    ) -> Option<Vec<CallHierarchyIncomingCall>> {
        let (module, symbol) = decode_call_item(data)?;
        let target = ReferentKey::Symbol(module, symbol);

        // caller (module.0, symbol.0) → the ranges it calls the target from.
        let mut groups: HashMap<(u32, u32), Vec<Range>> = HashMap::new();
        for (smi, view) in self.modules.iter().enumerate() {
            // Cooperative cancellation: bail between modules.
            if cancel.is_cancelled() {
                return None;
            }
            let Ok(raw) = u32::try_from(smi) else {
                continue;
            };
            let smid = ModuleId(raw);
            let Some(spatial) = self.spatial.get(smi) else {
                continue;
            };
            for (span, _kind, nid) in &spatial.entries {
                let Some(b) = view.bindings.get(nid.0 as usize).and_then(Option::as_ref) else {
                    continue;
                };
                if referent_key(b, smid).as_ref() != Some(&target) {
                    continue;
                }
                let Some((cm, cs)) = self.enclosing_caller(smid, span.start) else {
                    continue;
                };
                if let Some(range) = self.range_in(smid, self.final_ident_span(smi, *span)) {
                    groups.entry((cm.0, cs.0)).or_default().push(range);
                }
            }
        }

        Some(
            self.sorted_call_items(groups)
                .into_iter()
                .map(|(from, from_ranges)| CallHierarchyIncomingCall { from, from_ranges })
                .collect(),
        )
    }

    /// Answer `callHierarchy/outgoingCalls` for a prepared item.
    ///
    /// Every workspace `fn` called from within the item's body, grouped by
    /// callee with the call-site ranges (inside this item) for each. Stdlib and
    /// class-method callees are not yet listed — see the deferred follow-up.
    /// `None` when the item's `data` does not decode to a workspace symbol.
    #[must_use]
    pub fn outgoing_calls(
        &self,
        data: &serde_json::Value,
    ) -> Option<Vec<CallHierarchyOutgoingCall>> {
        let (module, symbol) = decode_call_item(data)?;
        let mi = module.0 as usize;
        let view = self.modules.get(mi)?;
        let def_span = view.symbols.entries.get(symbol.0 as usize)?.def_span;
        let body = self.clamp_to_next_decl(module, symbol.0 as usize, def_span);
        let spatial = self.spatial.get(mi)?;

        // callee (module.0, symbol.0) → the ranges this item calls it from.
        let mut groups: HashMap<(u32, u32), Vec<Range>> = HashMap::new();
        for (span, _kind, nid) in &spatial.entries {
            if !(body.start <= span.start && span.end <= body.end) {
                continue;
            }
            let Some(b) = view.bindings.get(nid.0 as usize).and_then(Option::as_ref) else {
                continue;
            };
            let Some(ReferentKey::Symbol(cm, cs)) = referent_key(b, module) else {
                continue;
            };
            if !matches!(self.symbol_kind(cm, cs), Some(SymbolKind::Fn { .. })) {
                continue;
            }
            if let Some(range) = self.range_in(module, self.final_ident_span(mi, *span)) {
                groups.entry((cm.0, cs.0)).or_default().push(range);
            }
        }

        Some(
            self.sorted_call_items(groups)
                .into_iter()
                .map(|(to, from_ranges)| CallHierarchyOutgoingCall { to, from_ranges })
                .collect(),
        )
    }

    /// The narrowest top-level declaration (`fn`, `const`, or actor) whose
    /// `def_span` brackets `offset` — the caller a use site is attributed to.
    fn enclosing_caller(
        &self,
        mid: ModuleId,
        offset: u32,
    ) -> Option<(ModuleId, ridge_resolve::SymbolId)> {
        let entries = &self.modules.get(mid.0 as usize)?.symbols.entries;
        let (i, _) = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                matches!(
                    e.kind,
                    SymbolKind::Fn { .. } | SymbolKind::Const | SymbolKind::Actor { .. }
                ) && e.def_span.start <= offset
                    && offset < e.def_span.end
            })
            .min_by_key(|(_, e)| e.def_span.end - e.def_span.start)?;
        Some((mid, ridge_resolve::SymbolId(u32::try_from(i).ok()?)))
    }

    /// Shrink a declaration's `def_span` so it ends before the next top-level
    /// declaration begins, guarding the body scan against the parser's
    /// span-bleed pulling the following item's names into this one.
    fn clamp_to_next_decl(&self, module: ModuleId, symbol_idx: usize, span: Span) -> Span {
        let Some(view) = self.modules.get(module.0 as usize) else {
            return span;
        };
        let next_start = view
            .symbols
            .entries
            .iter()
            .enumerate()
            .filter(|(i, e)| *i != symbol_idx && e.def_span.start > span.start)
            .map(|(_, e)| e.def_span.start)
            .min();
        match next_start {
            Some(n) if n < span.end => Span::new(span.start, n),
            _ => span,
        }
    }

    /// Build sorted call-hierarchy items from grouped call sites.
    ///
    /// Each `(module, symbol)` key becomes one [`CallHierarchyItem`]; its ranges
    /// are sorted and de-duplicated, and the groups themselves are ordered by
    /// document then position so the result is deterministic.
    fn sorted_call_items(
        &self,
        groups: HashMap<(u32, u32), Vec<Range>>,
    ) -> Vec<(CallHierarchyItem, Vec<Range>)> {
        let mut out: Vec<(CallHierarchyItem, Vec<Range>)> = groups
            .into_iter()
            .filter_map(|((m, s), mut ranges)| {
                let item = self.call_hierarchy_item(ModuleId(m), ridge_resolve::SymbolId(s))?;
                ranges.sort_by_key(|r| (r.start.line, r.start.character));
                ranges.dedup();
                Some((item, ranges))
            })
            .collect();
        out.sort_by(|a, b| {
            (
                a.0.uri.as_str(),
                a.0.range.start.line,
                a.0.range.start.character,
            )
                .cmp(&(
                    b.0.uri.as_str(),
                    b.0.range.start.line,
                    b.0.range.start.character,
                ))
        });
        out
    }

    /// Build one [`DocumentSymbol`] for a top-level entry, attaching members.
    fn document_symbol_for(
        &self,
        mid: ModuleId,
        entry: &ridge_resolve::SymbolEntry,
        entries: &[ridge_resolve::SymbolEntry],
    ) -> Option<DocumentSymbol> {
        let range = self.range_in(mid, entry.def_span)?;
        let selection_range = self
            .decl_name_span(mid, entry.def_span, &entry.name)
            .and_then(|s| self.range_in(mid, s))
            .unwrap_or(range);

        let (kind, children) = match &entry.kind {
            SymbolKind::Fn { .. } => (LspSymbolKind::FUNCTION, Vec::new()),
            SymbolKind::Const => (LspSymbolKind::CONSTANT, Vec::new()),
            SymbolKind::Type { .. } => {
                let children = self.type_member_symbols(mid, entry.id, entries);
                let kind = if children
                    .iter()
                    .any(|c| c.kind == LspSymbolKind::ENUM_MEMBER)
                {
                    LspSymbolKind::ENUM
                } else {
                    LspSymbolKind::STRUCT
                };
                (kind, children)
            }
            SymbolKind::Actor { state, handlers } => (
                LspSymbolKind::CLASS,
                self.actor_member_symbols(mid, state, handlers),
            ),
            _ => return None,
        };

        Some(Self::make_document_symbol(
            entry.name.clone(),
            kind,
            range,
            selection_range,
            children,
        ))
    }

    /// Union variants (as enum members) and record fields owned by `owner`.
    fn type_member_symbols(
        &self,
        mid: ModuleId,
        owner: ridge_resolve::SymbolId,
        entries: &[ridge_resolve::SymbolEntry],
    ) -> Vec<DocumentSymbol> {
        entries
            .iter()
            .filter_map(|e| {
                let (kind, name) = match &e.kind {
                    SymbolKind::Constructor {
                        owner_type,
                        is_record: false,
                        ..
                    } if *owner_type == owner => (LspSymbolKind::ENUM_MEMBER, e.name.clone()),
                    SymbolKind::FieldAccessor { owner_type, field } if *owner_type == owner => {
                        (LspSymbolKind::FIELD, field.clone())
                    }
                    _ => return None,
                };
                let range = self.range_in(mid, e.def_span)?;
                let selection_range = self
                    .decl_name_span(mid, e.def_span, &name)
                    .and_then(|s| self.range_in(mid, s))
                    .unwrap_or(range);
                Some(Self::make_document_symbol(
                    name,
                    kind,
                    range,
                    selection_range,
                    Vec::new(),
                ))
            })
            .collect()
    }

    /// State fields (as fields) and message handlers (as methods) of an actor.
    fn actor_member_symbols(
        &self,
        mid: ModuleId,
        state: &[ridge_resolve::StateField],
        handlers: &[ridge_resolve::HandlerSig],
    ) -> Vec<DocumentSymbol> {
        let fields = state
            .iter()
            .map(|f| (LspSymbolKind::FIELD, &f.name, f.def_span));
        let methods = handlers
            .iter()
            .map(|h| (LspSymbolKind::METHOD, &h.name, h.def_span));
        fields
            .chain(methods)
            .filter_map(|(kind, name, def_span)| {
                let range = self.range_in(mid, def_span)?;
                let selection_range = self
                    .decl_name_span(mid, def_span, name)
                    .and_then(|s| self.range_in(mid, s))
                    .unwrap_or(range);
                Some(Self::make_document_symbol(
                    name.clone(),
                    kind,
                    range,
                    selection_range,
                    Vec::new(),
                ))
            })
            .collect()
    }

    #[allow(deprecated)] // `DocumentSymbol::deprecated` is deprecated; set to None.
    fn make_document_symbol(
        name: String,
        kind: LspSymbolKind,
        range: Range,
        selection_range: Range,
        children: Vec<DocumentSymbol>,
    ) -> DocumentSymbol {
        DocumentSymbol {
            name,
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range,
            selection_range,
            children: if children.is_empty() {
                None
            } else {
                Some(children)
            },
        }
    }

    /// Answer a `workspace/symbol` request: top-level declarations across every
    /// module whose name matches `query` (case-insensitive substring; an empty
    /// query returns everything). Union variants are included; synthesised
    /// members (record auto-constructors, field accessors) and mirror symbols
    /// are not.
    #[must_use]
    #[allow(deprecated)] // `SymbolInformation::deprecated` is deprecated; set to None.
    pub fn workspace_symbols(&self, query: &str, cancel: &Cancel) -> Vec<SymbolInformation> {
        let needle = query.to_lowercase();
        let mut out: Vec<SymbolInformation> = Vec::new();
        for view in &self.modules {
            // Cooperative cancellation: stop between modules and return what we
            // have. Discarded by the handler when the request was cancelled.
            if cancel.is_cancelled() {
                break;
            }
            let mid = view.symbols.module;
            let Some(Some(uri)) = self.module_uris.get(mid.0 as usize) else {
                continue;
            };
            let mut seen_spans: Vec<Span> = Vec::new();
            for entry in &view.symbols.entries {
                let kind = match &entry.kind {
                    SymbolKind::Fn { .. } => LspSymbolKind::FUNCTION,
                    SymbolKind::Const => LspSymbolKind::CONSTANT,
                    SymbolKind::Type { .. } => LspSymbolKind::STRUCT,
                    SymbolKind::Actor { .. } => LspSymbolKind::CLASS,
                    SymbolKind::Constructor {
                        is_record: false, ..
                    } => LspSymbolKind::ENUM_MEMBER,
                    // Record auto-constructors and field accessors are members.
                    _ => continue,
                };
                if seen_spans.contains(&entry.def_span) {
                    continue;
                }
                seen_spans.push(entry.def_span);
                if !needle.is_empty() && !entry.name.to_lowercase().contains(&needle) {
                    continue;
                }
                let Some(span) = self
                    .decl_name_span(mid, entry.def_span, &entry.name)
                    .or(Some(entry.def_span))
                else {
                    continue;
                };
                let Some(range) = self.range_in(mid, span) else {
                    continue;
                };
                out.push(SymbolInformation {
                    name: entry.name.clone(),
                    kind,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range,
                    },
                    container_name: None,
                });
            }
        }
        out
    }

    /// Answer an inlay-hint request for the visible `range`.
    ///
    /// Shows the inferred type after each `let`/`var` binder that carries no
    /// written annotation (`let total = ...` renders `total: Int`). The type is
    /// read from the bound value expression's `node_types` entry. Destructuring
    /// patterns are skipped — a single trailing type does not describe them.
    /// Reads only this immutable snapshot; never triggers a compile.
    #[must_use]
    pub fn inlay_hints(&self, uri: &Url, range: Range) -> Option<Vec<InlayHint>> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let ast = self.modules.get(mi)?.ast.as_ref()?;
        let li = self.line_indices.get(mi)?;
        let lo = li.utf16_to_byte(range.start.line, range.start.character);
        let hi = li.utf16_to_byte(range.end.line, range.end.character);

        let mut collector = InlayCollector {
            index: self,
            mid,
            lo,
            hi,
            hints: Vec::new(),
        };
        ridge_ast::visit::Visit::visit_module(&mut collector, ast);
        Some(collector.hints)
    }

    /// The inferred type stamped on the expression-like node whose span is
    /// exactly `span` (the `value` of a `let`/`var`). `None` if there is no such
    /// node, no type was inferred, or the type is the error sentinel.
    fn expr_type_at(&self, mid: ModuleId, span: Span) -> Option<&ridge_types::Type> {
        let mi = mid.0 as usize;
        let nid = self
            .spatial
            .get(mi)?
            .entries
            .iter()
            .find(|(s, k, _)| {
                *s == span && matches!(k, NodeKind::Expr | NodeKind::Block | NodeKind::Try)
            })
            .map(|(_, _, nid)| *nid)?;
        let ty = self
            .modules
            .get(mi)?
            .node_types
            .get(nid.0 as usize)?
            .as_ref()?;
        (!matches!(ty, ridge_types::Type::Error)).then_some(ty)
    }

    /// Answer a completion request at an LSP `(line, utf16_col)` position.
    ///
    /// Returns the candidates (never errors, never `None`). Reads only this
    /// immutable snapshot — a completion never triggers a compile.
    #[must_use]
    pub fn completions_at(&self, uri: &Url, line: u32, utf16_col: u32) -> Vec<CompletionItemData> {
        self.try_completions(uri, line, utf16_col)
            .unwrap_or_default()
    }

    /// Fill in a completion item's signature and documentation on demand
    /// (`completionItem/resolve`).
    ///
    /// `data` is the `{ "uri", "name" }` payload the completion list attached to
    /// a workspace-symbol item. Returns `(detail, documentation)` rendered from
    /// the symbol's written header and doc comment — the same material hover
    /// shows — or `None` when the payload names no resolvable declaration.
    #[must_use]
    pub fn resolve_completion(&self, data: &serde_json::Value) -> Option<(String, Option<String>)> {
        let uri = Url::parse(data.get("uri")?.as_str()?).ok()?;
        let name = data.get("name")?.as_str()?;
        let mid = self.module_id_for(&uri)?;
        let view = self.modules.get(mid.0 as usize)?;
        view.symbols
            .entries
            .iter()
            .filter(|e| e.name == name)
            .find_map(|e| self.decl_header_and_doc(mid, e.id))
    }

    fn try_completions(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
    ) -> Option<Vec<CompletionItemData>> {
        let mid = self.module_id_for(uri)?;
        let mi = mid.0 as usize;
        let byte = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);
        let offset = byte as usize;
        let src = self.module_text.get(mi)?;
        let m = self.modules.get(mi)?;

        let mut out: Vec<CompletionItemData> = Vec::new();
        match detect_context(src, offset) {
            Context::None => {}
            Context::Member { alias, prefix } => {
                if let Some(target) = alias_target(&m.imports, &alias) {
                    let target_uri = self
                        .module_uris
                        .get(target.0 as usize)
                        .and_then(Option::as_ref);
                    if let Some(tm) = self.modules.get(target.0 as usize) {
                        for e in &tm.symbols.entries {
                            if e.visibility == ResolvedVisibility::Pub
                                && e.name.starts_with(&prefix)
                            {
                                out.push(symbol_item(
                                    e.name.clone(),
                                    symbol_kind(&e.kind),
                                    '0',
                                    target_uri,
                                ));
                            }
                        }
                    }
                } else if let Some(sid) = stdlib_alias_target(&m.imports, &alias) {
                    // A stdlib alias (`import std.repo as Repo`) resolves to a
                    // builtin module whose exported names live in `BUILTINS`.
                    for &name in stdlib_exports(sid) {
                        if name.starts_with(&prefix) {
                            out.push(item(name.to_owned(), stdlib_export_kind(name), '0'));
                        }
                    }
                } else {
                    // Not a module alias: complete the fields of a value of record
                    // type sitting just before the dot.
                    out.extend(self.record_field_completions(mi, byte, &prefix));
                }
            }
            Context::Type { prefix } => {
                for decl in &self.tycons {
                    if !decl.is_anon && decl.name.starts_with(&prefix) {
                        out.push(item(decl.name.clone(), CompletionItemKind::CLASS, '0'));
                    }
                }
            }
            Context::Expr { prefix } => {
                // Locals in scope sort first, then this module's symbols, then
                // import aliases, then keywords.
                for local in m.scopes.visible_at(byte) {
                    if local.name.starts_with(&prefix) {
                        out.push(item(local.name.clone(), CompletionItemKind::VARIABLE, '0'));
                    }
                }
                let self_uri = self.module_uris.get(mi).and_then(Option::as_ref);
                for e in &m.symbols.entries {
                    if e.name.starts_with(&prefix) {
                        out.push(symbol_item(
                            e.name.clone(),
                            symbol_kind(&e.kind),
                            '1',
                            self_uri,
                        ));
                    }
                }
                for imp in &m.imports {
                    if let Some(alias) = &imp.alias {
                        if alias.starts_with(&prefix) {
                            out.push(item(alias.clone(), CompletionItemKind::MODULE, '2'));
                        }
                    }
                }
                for kw in KEYWORDS {
                    if kw.starts_with(&prefix) {
                        out.push(item((*kw).to_owned(), CompletionItemKind::KEYWORD, '3'));
                    }
                }
            }
        }
        Some(out)
    }

    /// Field-name completions for `value.` where the value ending just before the
    /// dot at the cursor has a record type. Empty when that value has no record
    /// type or cannot be located. The value's last byte sits two back from the
    /// cursor past the dot — value and dot are contiguous in any text the index
    /// actually holds, since an incomplete `value.` does not type-check.
    fn record_field_completions(
        &self,
        mi: usize,
        byte: u32,
        prefix: &str,
    ) -> Vec<CompletionItemData> {
        let Some(value_byte) = byte
            .checked_sub(u32::try_from(prefix.len()).unwrap_or(u32::MAX))
            .and_then(|b| b.checked_sub(2))
        else {
            return Vec::new();
        };
        let Some((tnode, _, _)) = self.spatial.get(mi).and_then(|sp| {
            sp.narrowest_containing(
                value_byte,
                &[
                    NodeKind::Expr,
                    NodeKind::Block,
                    NodeKind::Try,
                    NodeKind::Type,
                ],
            )
        }) else {
            return Vec::new();
        };
        let Some(ty) = self
            .modules
            .get(mi)
            .and_then(|m| m.node_types.get(tnode.0 as usize))
            .and_then(Option::as_ref)
        else {
            return Vec::new();
        };
        record_field_names(ty, &self.tycons)
            .into_iter()
            .filter(|name| name.starts_with(prefix))
            .map(|name| item(name, CompletionItemKind::FIELD, '0'))
            .collect()
    }
}

/// Walks a module's AST collecting inlay hints for un-annotated `let`/`var`
/// binders that fall inside the requested byte range `[lo, hi]`.
struct InlayCollector<'a> {
    index: &'a WorkspaceIndex,
    mid: ModuleId,
    lo: u32,
    hi: u32,
    hints: Vec<InlayHint>,
}

impl InlayCollector<'_> {
    /// Push a `: <type>` hint after `name_span` if the binder is in range and the
    /// `value` expression carries an inferred type.
    fn try_hint(&mut self, name_span: Span, value_span: Span) {
        if name_span.end < self.lo || name_span.start > self.hi {
            return;
        }
        let Some(ty) = self.index.expr_type_at(self.mid, value_span) else {
            return;
        };
        let rendered = render_type_with(ty, &self.index.tycons);
        let Some(range) = self.index.range_in(self.mid, name_span) else {
            return;
        };
        self.hints.push(InlayHint {
            position: range.end,
            label: InlayHintLabel::String(format!(": {rendered}")),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: Some(false),
            padding_right: Some(false),
            data: None,
        });
    }
}

impl<'ast> ridge_ast::visit::Visit<'ast> for InlayCollector<'_> {
    fn visit_expr(&mut self, e: &'ast ridge_ast::Expr) {
        match e {
            ridge_ast::Expr::Let {
                pat: ridge_ast::Pattern::Var { name, .. },
                ty: None,
                value,
                ..
            }
            | ridge_ast::Expr::Var {
                name,
                ty: None,
                value,
                ..
            } => self.try_hint(name.span, value.span()),
            _ => {}
        }
        ridge_ast::visit::walk_expr(self, e);
    }
}

/// The custom token type for an effect capability (`io`/`fs`/`net`/`db`/…) —
/// the visible marker of Ridge's capability discipline. Standard token types
/// are mapped by the editor automatically; this one the client theme maps.
const CAPABILITY_TOKEN_TYPE: SemanticTokenType = SemanticTokenType::new("capability");

/// The semantic-token types Ridge emits, in legend order. Each token's
/// `token_type` field indexes into this slice.
pub const SEMANTIC_TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::NAMESPACE,
    SemanticTokenType::TYPE,
    SemanticTokenType::CLASS,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::METHOD,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::PARAMETER,
    SemanticTokenType::ENUM_MEMBER,
    CAPABILITY_TOKEN_TYPE,
];

/// The semantic-token modifiers Ridge emits, in legend order. A token's
/// `token_modifiers_bitset` sets bit `i` to apply the `i`-th modifier here.
pub const SEMANTIC_TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION,
    SemanticTokenModifier::READONLY,
    SemanticTokenModifier::DEFAULT_LIBRARY,
];

// Token-type indices into `SEMANTIC_TOKEN_TYPES`.
const TT_NAMESPACE: u32 = 0;
const TT_TYPE: u32 = 1;
const TT_CLASS: u32 = 2;
const TT_FUNCTION: u32 = 3;
const TT_METHOD: u32 = 4;
const TT_PROPERTY: u32 = 5;
const TT_VARIABLE: u32 = 6;
const TT_PARAMETER: u32 = 7;
const TT_ENUM_MEMBER: u32 = 8;
const TT_CAPABILITY: u32 = 9;

// Modifier bits into `SEMANTIC_TOKEN_MODIFIERS`.
const MOD_DECLARATION: u32 = 1 << 0;
const MOD_READONLY: u32 = 1 << 1;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 2;

/// One classified token before relative encoding: an absolute UTF-16 position
/// plus its type and modifier bitset.
#[derive(Debug, Clone, Copy)]
struct RawToken {
    line: u32,
    start: u32,
    len: u32,
    ty: u32,
    mods: u32,
}

/// The token type and modifiers for a top-level symbol by its kind.
const fn symbol_token(kind: &SymbolKind) -> (u32, u32) {
    match kind {
        SymbolKind::Fn { .. } => (TT_FUNCTION, 0),
        SymbolKind::Const => (TT_VARIABLE, MOD_READONLY),
        SymbolKind::Type { .. } => (TT_TYPE, 0),
        SymbolKind::Actor { .. } => (TT_CLASS, 0),
        SymbolKind::Constructor { .. } => (TT_ENUM_MEMBER, 0),
        SymbolKind::FieldAccessor { .. } => (TT_PROPERTY, 0),
        _ => (TT_VARIABLE, 0),
    }
}

/// The byte range of each dot-separated segment within a qualified-name span.
fn segment_byte_ranges(src: &str, span: Span) -> Vec<(u32, u32)> {
    let start = span.start as usize;
    let raw = src.get(start..span.end as usize).unwrap_or_default();
    let mut ranges = Vec::new();
    let mut seg_start = 0usize;
    for (i, ch) in raw.char_indices() {
        if ch == '.' {
            ranges.push((to_u32(start + seg_start), to_u32(start + i)));
            seg_start = i + ch.len_utf8();
        }
    }
    ranges.push((to_u32(start + seg_start), span.end));
    ranges
}

/// Convert a `usize` byte offset to `u32`, saturating (spans never exceed u32).
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Convert a byte span to a single-line [`RawToken`] and push it, dropping an
/// empty or multi-line span (a name never spans lines).
fn push_raw(li: &LineIndex, out: &mut Vec<RawToken>, start: u32, end: u32, ty: u32, mods: u32) {
    if end <= start {
        return;
    }
    let (start_line, start_char) = li.byte_to_utf16(start);
    let (end_line, end_char) = li.byte_to_utf16(end);
    if start_line != end_line || end_char <= start_char {
        return;
    }
    out.push(RawToken {
        line: start_line,
        start: start_char,
        len: end_char - start_char,
        ty,
        mods,
    });
}

/// Relative-encode sorted, non-overlapping tokens into the LSP wire format.
fn encode_tokens(tokens: &[RawToken]) -> SemanticTokens {
    SemanticTokens {
        result_id: None,
        data: encode_token_data(tokens),
    }
}

/// Relative-encode sorted, non-overlapping tokens into the flat token stream
/// (each token carried as a 5-field [`SemanticToken`]). The wire `data` array
/// and the delta cache both build on this.
fn encode_token_data(tokens: &[RawToken]) -> Vec<SemanticToken> {
    let mut data = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    for t in tokens {
        let delta_line = t.line - prev_line;
        let delta_start = if delta_line == 0 {
            t.start - prev_start
        } else {
            t.start
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: t.len,
            token_type: t.ty,
            token_modifiers_bitset: t.mods,
        });
        prev_line = t.line;
        prev_start = t.start;
    }
    data
}

/// Compute the minimal edit turning the previously returned token stream `old`
/// into the freshly computed `new`, for `textDocument/semanticTokens/full/delta`.
///
/// Both arrays are already relative-encoded, so the edit is a pure splice: trim
/// the longest common prefix and suffix and replace the differing middle band.
/// Applying the result to `old` reproduces `new` exactly — no re-encoding, since
/// the replacement tokens are lifted verbatim out of `new` and the shared prefix
/// keeps the first changed token's relative anchor identical in both streams.
///
/// `start` and `delete_count` are in flat-integer units (five per token), per the
/// LSP wire format; `data` is the slice of `new` that replaces the deleted band.
/// An identical stream yields no edits.
pub(crate) fn diff_tokens(old: &[SemanticToken], new: &[SemanticToken]) -> Vec<SemanticTokensEdit> {
    let common = old.len().min(new.len());
    let mut prefix = 0;
    while prefix < common && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < common - prefix && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix] {
        suffix += 1;
    }
    let deleted = old.len() - prefix - suffix;
    let inserted = &new[prefix..new.len() - suffix];
    if deleted == 0 && inserted.is_empty() {
        return Vec::new();
    }
    vec![SemanticTokensEdit {
        start: u32::try_from(prefix * 5).unwrap_or(u32::MAX),
        delete_count: u32::try_from(deleted * 5).unwrap_or(u32::MAX),
        data: if inserted.is_empty() {
            None
        } else {
            Some(inserted.to_vec())
        },
    }]
}

/// Whether `word` is one of Ridge's capability keywords.
fn is_capability_keyword(word: &str) -> bool {
    matches!(
        word,
        "io" | "fs" | "net" | "time" | "random" | "env" | "proc" | "spawn" | "ffi" | "db"
    )
}

/// Walks `fn`/`on`/`init` declarations and emits a `capability` token for each
/// capability keyword in the annotation region (between the introducing keyword
/// and the declaration name). The resolver does not stamp capability positions
/// and the AST keeps no per-capability span, so they are recovered from source.
struct CapabilityCollector<'a> {
    li: &'a LineIndex,
    src: &'a str,
    out: &'a mut Vec<RawToken>,
}

impl CapabilityCollector<'_> {
    /// Emit a capability token for every capability keyword in `src[start..end)`.
    fn scan(&mut self, start: u32, end: u32) {
        let from = start as usize;
        let to = (end as usize).min(self.src.len());
        let Some(region) = self.src.get(from..to) else {
            return;
        };
        let mut word_start: Option<usize> = None;
        for (i, ch) in region.char_indices() {
            if ch.is_alphanumeric() || ch == '_' {
                word_start.get_or_insert(i);
            } else if let Some(ws) = word_start.take() {
                if is_capability_keyword(&region[ws..i]) {
                    push_raw(
                        self.li,
                        self.out,
                        to_u32(from + ws),
                        to_u32(from + i),
                        TT_CAPABILITY,
                        0,
                    );
                }
            }
        }
        if let Some(ws) = word_start {
            if is_capability_keyword(&region[ws..]) {
                let end = to_u32(from + region.len());
                push_raw(self.li, self.out, to_u32(from + ws), end, TT_CAPABILITY, 0);
            }
        }
    }
}

impl<'ast> ridge_ast::visit::Visit<'ast> for CapabilityCollector<'_> {
    fn visit_fn_decl(&mut self, d: &'ast ridge_ast::FnDecl) {
        if !d.caps.is_empty() {
            self.scan(d.span.start, d.name.span.start);
        }
        ridge_ast::visit::walk_fn_decl(self, d);
    }

    fn visit_on_handler(&mut self, h: &'ast ridge_ast::OnHandler) {
        if !h.caps.is_empty() {
            self.scan(h.span.start, h.name.span.start);
        }
        ridge_ast::visit::walk_on_handler(self, h);
    }

    fn visit_init_decl(&mut self, d: &'ast ridge_ast::InitDecl) {
        if !d.caps.is_empty() {
            // `init` has no name; the capabilities sit before the parameter list.
            let end = d.params.first().map_or(d.span.start, |p| p.span().start);
            self.scan(d.span.start, end);
        }
        ridge_ast::visit::walk_init_decl(self, d);
    }
}

/// A function or method signature rendered for `textDocument/signatureHelp`.
#[derive(Debug, Clone)]
pub(crate) struct SignatureSig {
    /// The whole signature on one line, e.g. `joinOn (left: …) (right: …) -> …`.
    pub label: String,
    /// `[start, end)` UTF-16 offsets into `label`, one per parameter, in order.
    pub params: Vec<[u32; 2]>,
}

/// Build a one-line signature label from `src` and a declaration's pieces.
///
/// `name` is the callee display name; each parameter and the return type are
/// sliced verbatim from `src` (whitespace squeezed to one space) so the label
/// reads as written, with no type re-rendering. Returns the label plus the
/// UTF-16 offset range of every parameter, for active-parameter highlighting.
pub(crate) fn build_signature(
    src: &str,
    name: &str,
    params: &[ridge_ast::Param],
    ret: Option<&ridge_ast::Type>,
) -> SignatureSig {
    let mut label = name.to_owned();
    let mut cursor = utf16_len(&label);
    let mut ranges = Vec::with_capacity(params.len());
    for p in params {
        label.push(' ');
        cursor += 1;
        let frag = slice_span(src, p.span());
        let start = cursor;
        cursor += utf16_len(&frag);
        label.push_str(&frag);
        ranges.push([start, cursor]);
    }
    if let Some(ty) = ret {
        let frag = slice_span(src, ty.span());
        label.push_str(" -> ");
        label.push_str(&frag);
    }
    SignatureSig {
        label,
        params: ranges,
    }
}

/// The `src` text covered by `span`, with whitespace runs squeezed to one space.
fn slice_span(src: &str, span: Span) -> String {
    let raw = src
        .get(span.start as usize..span.end as usize)
        .unwrap_or_default();
    squeeze_ws(raw)
}

/// Collapse every run of whitespace to a single space and trim the ends, so a
/// multi-line source slice still reads as a one-line label.
fn squeeze_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out.trim().to_owned()
}

/// UTF-16 code-unit length of `s` (LSP parameter label offsets are UTF-16).
fn utf16_len(s: &str) -> u32 {
    u32::try_from(s.chars().map(char::len_utf16).sum::<usize>()).unwrap_or(u32::MAX)
}

/// The substring of `s` spanning the `[start, end)` UTF-16 code-unit range, the
/// inverse of the offsets [`build_signature`] records. Used to hand a parameter
/// label as text to clients that don't support label offsets. A char straddling
/// a boundary (only possible with malformed offsets) is excluded.
fn utf16_slice(s: &str, start: u32, end: u32) -> String {
    let mut col: u32 = 0;
    let mut out = String::new();
    for ch in s.chars() {
        let next = col + u32::try_from(ch.len_utf16()).unwrap_or(0);
        if col >= start && next <= end {
            out.push(ch);
        }
        if next >= end {
            break;
        }
        col = next;
    }
    out
}

/// The index of the parameter the cursor is filling in: the number of argument
/// atoms already completed before it, clamped to the last parameter.
fn active_param(arg_spans: &[Span], offset: u32, nparams: usize) -> u32 {
    let done = arg_spans.iter().filter(|s| s.end <= offset).count();
    let active = if nparams == 0 {
        0
    } else {
        done.min(nparams - 1)
    };
    u32::try_from(active).unwrap_or(0)
}

/// Assemble the LSP [`SignatureHelp`] for one resolved signature, marking
/// `active` as the parameter being filled in.
///
/// `label_offsets` reflects the client's
/// `parameterInformation.labelOffsetSupport`: when set, each parameter is given
/// as `[start, end)` UTF-16 offsets into the label; otherwise as the substring it
/// covers, which is all a client without offset support can match.
fn make_signature_help(sig: SignatureSig, active: u32, label_offsets: bool) -> SignatureHelp {
    let parameters = sig
        .params
        .iter()
        .map(|&offsets| ParameterInformation {
            label: if label_offsets {
                ParameterLabel::LabelOffsets(offsets)
            } else {
                ParameterLabel::Simple(utf16_slice(&sig.label, offsets[0], offsets[1]))
            },
            documentation: None,
        })
        .collect();
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label: sig.label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    }
}

/// Finds the innermost call whose argument region contains an offset, for
/// signature help. Records the callee span, the argument spans, and the call
/// span (the last drives the narrowest-enclosing tie-break).
struct CallFinder<'a> {
    offset: u32,
    src: &'a str,
    best: Option<(Span, Vec<Span>, Span)>,
}

impl CallFinder<'_> {
    /// Whether `end..offset` is non-empty and only spaces/tabs — the cursor sits
    /// in the trailing whitespace right after a call, on the same line.
    fn trailing_gap(&self, end: u32) -> bool {
        self.offset > end
            && self
                .src
                .get(end as usize..self.offset as usize)
                .is_some_and(|g| !g.is_empty() && g.chars().all(|c| c == ' ' || c == '\t'))
    }
}

impl<'ast> ridge_ast::visit::Visit<'ast> for CallFinder<'_> {
    fn visit_expr(&mut self, e: &'ast ridge_ast::Expr) {
        if let ridge_ast::Expr::Call { callee, args, span } = e {
            // The cursor is in the argument region (past the callee) and within
            // the call, or in the same-line whitespace just past the last
            // argument where the next one will go.
            let in_args = span.start <= self.offset
                && callee.span().end <= self.offset
                && (self.offset <= span.end || self.trailing_gap(span.end));
            let narrower = match &self.best {
                None => true,
                Some((_, _, prev)) => span.end - span.start <= prev.end - prev.start,
            };
            if in_args && narrower {
                let arg_spans = args.iter().map(ridge_ast::Expr::span).collect();
                self.best = Some((callee.span(), arg_spans, *span));
            }
        }
        ridge_ast::visit::walk_expr(self, e);
    }
}

/// Identity of the thing a [`Binding`] refers to, for grouping use-sites in
/// find-references: two bindings denote the same definition exactly when their
/// keys compare equal. `ModuleSymbol`, `ImportedSymbol`, and `ActorName` all
/// collapse to [`ReferentKey::Symbol`] so a reference from an importing module
/// unifies with the definition in the owning module.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReferentKey {
    /// A local, scoped to the module that introduced it.
    Local(ModuleId, LocalId),
    /// A top-level workspace symbol (fn / const / type / actor).
    Symbol(ModuleId, ridge_resolve::SymbolId),
    /// A constructor, keyed by owning module, owning type, and variant index.
    Constructor(ModuleId, ridge_resolve::SymbolId, u32),
    /// A stdlib export, globally unique by (module, name).
    Stdlib(StdlibModuleId, String),
    /// A class method, globally unique by (class, method).
    ClassMethod(String, String),
}

/// The renameable thing under the cursor: what it denotes, the exact name range
/// to select (the `prepareRename` result), and its current text.
///
/// `extra` holds additional referents that must be renamed in lockstep with
/// `key`. The only case today is a record type, whose auto-constructor shares
/// the type's name: renaming `type User` must also rewrite every `User { .. }`
/// construction and pattern, which key to a [`ReferentKey::Constructor`] rather
/// than the type's [`ReferentKey::Symbol`].
struct RenameTarget {
    key: ReferentKey,
    extra: Vec<ReferentKey>,
    cursor_range: Range,
    name: String,
}

impl RenameTarget {
    /// True when `key` is one of the referents this rename rewrites.
    fn matches(&self, key: &ReferentKey) -> bool {
        self.key == *key || self.extra.iter().any(|k| k == key)
    }
}

/// The referent a binding points at, as a value comparable across modules.
///
/// `module` is the module the binding was found in — used only to scope a
/// [`Binding::Local`]. Returns `None` for bindings with no findable workspace
/// referent: [`Binding::Error`], and — deferred to a follow-up —
/// [`Binding::FieldAccessor`] (keyed only by a field name, which would conflate
/// distinct records) and [`Binding::ModuleAlias`] (a module-local alias).
fn referent_key(binding: &Binding, module: ModuleId) -> Option<ReferentKey> {
    match binding {
        Binding::Local(id) => Some(ReferentKey::Local(module, *id)),
        Binding::ModuleSymbol { module, symbol }
        | Binding::ImportedSymbol { module, symbol, .. } => {
            Some(ReferentKey::Symbol(*module, *symbol))
        }
        Binding::ActorName { module, actor } => Some(ReferentKey::Symbol(*module, *actor)),
        Binding::Constructor {
            owner_module,
            owner_type,
            variant,
            ..
        } => Some(ReferentKey::Constructor(
            *owner_module,
            *owner_type,
            *variant,
        )),
        Binding::StdlibSymbol { module, name } => Some(ReferentKey::Stdlib(*module, name.clone())),
        Binding::ClassMethod { class_name, method } => {
            Some(ReferentKey::ClassMethod(class_name.clone(), method.clone()))
        }
        _ => None,
    }
}

/// Build the capability quick-fixes for one compile.
///
/// Walks the structured `T014 CapabilityNotDeclared` errors and, for each one
/// raised against a top-level `fn` that declares NO capabilities, produces an
/// edit inserting the inferred capability keywords just before the function
/// name. Functions that already declare some capabilities, message handlers,
/// `init` blocks, and inner functions are left to a follow-up: inserting the
/// full inferred set ahead of an existing annotation would duplicate it, and
/// the others are not matched by the top-level decl span used here.
#[must_use]
pub fn collect_capability_fixes(
    line_indices: &[LineIndex],
    module_uris: &[Option<Url>],
    typed: &TypedWorkspace,
    type_errors: &[(ModuleId, TypeError)],
) -> Vec<CapabilityFix> {
    let mut out: Vec<CapabilityFix> = Vec::new();
    for (mid, err) in type_errors {
        let TypeError::CapabilityNotDeclared {
            inferred,
            missing: _,
            span,
            decl,
            ..
        } = err
        else {
            continue;
        };
        let mi = mid.0 as usize;
        let (Some(module), Some(Some(uri)), Some(li)) = (
            typed.modules.get(mi),
            module_uris.get(mi),
            line_indices.get(mi),
        ) else {
            continue;
        };
        // Match the whole-declaration span to a top-level `fn` with no written
        // capability annotation.
        let Some(name_start) = module.ast.items.iter().find_map(|item| match item {
            ridge_ast::Item::Fn(f) if f.span == *span && f.caps.is_empty() => {
                Some(f.name.span.start)
            }
            _ => None,
        }) else {
            continue;
        };
        let caps = render_caps(*inferred);
        if caps.is_empty() {
            continue;
        }
        let (pl, pc) = li.byte_to_utf16(name_start);
        let pos = Position::new(pl, pc);
        let (sl, sc) = li.byte_to_utf16(span.start);
        let (el, ec) = li.byte_to_utf16(span.end);
        let noun = if caps.contains(' ') {
            "capabilities"
        } else {
            "capability"
        };
        out.push(CapabilityFix {
            uri: uri.clone(),
            decl_range: Range {
                start: Position::new(sl, sc),
                end: Position::new(el, ec),
            },
            edit_range: Range {
                start: pos,
                end: pos,
            },
            new_text: format!("{caps} "),
            title: format!("Add {noun} `{caps}` to `{decl}`"),
        });
    }
    out
}

/// Render a capability set as the space-separated keyword list used in a
/// signature (e.g. `io fs`), in the canonical declaration order.
fn render_caps(set: CapabilitySet) -> String {
    use ridge_ast::Capability::{Db, Env, Ffi, Fs, Io, Net, Proc, Random, Spawn, Time};
    [
        (Io, "io"),
        (Fs, "fs"),
        (Net, "net"),
        (Time, "time"),
        (Random, "random"),
        (Env, "env"),
        (Proc, "proc"),
        (Spawn, "spawn"),
        (Ffi, "ffi"),
        (Db, "db"),
    ]
    .into_iter()
    .filter(|(cap, _)| set.contains(*cap))
    .map(|(_, name)| name)
    .collect::<Vec<_>>()
    .join(" ")
}

/// Validate a rename's `new_name` against `old_name`.
///
/// A no-op rename (same name) is allowed. Otherwise the new name must be
/// non-empty, not a reserved keyword, and of the same identifier case class as
/// the old name: a type (an `UPPER_IDENT`) stays an `UPPER_IDENT`, and a value —
/// a local, `fn`, or `const`, all `LOWER_IDENT` — stays a `LOWER_IDENT`. Mixing
/// the classes would produce code that no longer parses. The `Err` message is
/// surfaced to the editor.
fn validate_new_name(new_name: &str, old_name: &str) -> Result<(), String> {
    if new_name == old_name {
        return Ok(());
    }
    if new_name.is_empty() {
        return Err("Cannot rename to an empty name.".to_owned());
    }
    if KEYWORDS.contains(&new_name) {
        return Err(format!("`{new_name}` is a reserved keyword."));
    }
    let old_is_upper = old_name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase());
    if old_is_upper {
        if !is_upper_ident(new_name) {
            return Err(format!(
                "`{new_name}` is not a valid type name — use an uppercase identifier \
                 (letters, digits and underscore, starting with a capital)."
            ));
        }
    } else if !is_lower_ident(new_name) {
        return Err(format!(
            "`{new_name}` is not a valid name — use a lowercase identifier \
             (letters, digits and underscore, not starting with a digit or capital)."
        ));
    }
    Ok(())
}

/// True when `s` is a Ridge `LOWER_IDENT`: `[a-z][a-zA-Z0-9_]*` or the private
/// form `_[a-zA-Z0-9][a-zA-Z0-9_]*`.
fn is_lower_ident(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    let first_ok = first.is_ascii_lowercase()
        || (first == '_' && s.chars().nth(1).is_some_and(|c| c.is_ascii_alphanumeric()));
    first_ok && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// True when `s` is a Ridge `UPPER_IDENT`: `[A-Z][a-zA-Z0-9_]*` — the spelling of
/// every type, constructor, and actor name.
fn is_upper_ident(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    first.is_ascii_uppercase() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The workspace module an import `alias` resolves to, if any.
fn alias_target(imports: &[ImportResolution], alias: &str) -> Option<ModuleId> {
    imports
        .iter()
        .find_map(|imp| match (&imp.alias, &imp.target) {
            (Some(a), ImportTarget::WorkspaceModule(m)) if a == alias => Some(*m),
            _ => None,
        })
}

/// The builtin stdlib module an import `alias` resolves to, if any.
fn stdlib_alias_target(imports: &[ImportResolution], alias: &str) -> Option<StdlibModuleId> {
    imports
        .iter()
        .find_map(|imp| match (&imp.alias, &imp.target) {
            (Some(a), ImportTarget::BuiltinStdlib(id)) if a == alias => Some(*id),
            _ => None,
        })
}

/// Exported symbol names of a builtin stdlib module, or an empty slice when the
/// id is out of range. The builtin manifest carries names only — no kinds or
/// definition spans — so completion infers the icon from the name's case (see
/// [`stdlib_export_kind`]) and go-to-definition does not yet reach these.
fn stdlib_exports(id: StdlibModuleId) -> &'static [&'static str] {
    match BUILTINS.get(id.0 as usize) {
        Some(m) => m.exports,
        None => &[],
    }
}

/// Heuristic completion kind for a stdlib export: an uppercase-initial name is a
/// type or constructor, anything else a function or value.
fn stdlib_export_kind(name: &str) -> CompletionItemKind {
    if name.chars().next().is_some_and(char::is_uppercase) {
        CompletionItemKind::CLASS
    } else {
        CompletionItemKind::FUNCTION
    }
}

/// Raw `TyConId` of the nominal record a value of type `ty` belongs to,
/// peeling alias wrappers. `None` for structural records (no nominal
/// declaration to point at), functions, primitives, and unresolved types.
fn record_tycon_of(ty: &Type, tycons: &[TyConDecl]) -> Option<u32> {
    match ty {
        Type::Con(id, _) => {
            let decl = tycons.iter().find(|d| d.id.0 == id.0)?;
            match &decl.kind {
                TyConKind::Record(_) => Some(id.0),
                TyConKind::Alias { body, .. } => record_tycon_of(body, tycons),
                _ => None,
            }
        }
        Type::Alias { body, .. } => record_tycon_of(body, tycons),
        _ => None,
    }
}

/// Raw `TyConId` of the named type a value of type `ty` carries, for
/// go-to-type-definition. Unwraps an alias to its own declaration. `None` for
/// functions, tuples, structural records, type variables, and errors — none of
/// which name a `type` declaration to jump to.
const fn named_tycon_of(ty: &Type) -> Option<u32> {
    match ty {
        Type::Con(id, _) => Some(id.0),
        Type::Alias { name, .. } => Some(name.0),
        _ => None,
    }
}

/// Span of `field`'s name within the record `type type_name` of `ast`, or
/// `None` if the type is absent, not a record, or has no such field.
fn field_decl_span(ast: &ridge_ast::Module, type_name: &str, field: &str) -> Option<Span> {
    for item in &ast.items {
        let ridge_ast::Item::Type(td) = item else {
            continue;
        };
        if td.name.text != type_name {
            continue;
        }
        let ridge_ast::TypeBody::Record(rb) = &td.body else {
            return None;
        };
        return rb
            .fields
            .iter()
            .find(|f| f.name.text == field)
            .map(|f| f.name.span);
    }
    None
}

/// Span of the name of the `type type_name` declaration in `ast`.
fn type_decl_name_span(ast: &ridge_ast::Module, type_name: &str) -> Option<Span> {
    ast.items.iter().find_map(|item| match item {
        ridge_ast::Item::Type(td) if td.name.text == type_name => Some(td.name.span),
        _ => None,
    })
}

/// Finds the narrowest `base.field` access whose `field` name range covers the
/// cursor, capturing the base expression's span and the field name. Powers
/// go-to-definition and find-references on record fields.
struct FieldAccessFinder {
    offset: u32,
    /// `(base span, field name span, field name)` of the best match so far.
    best: Option<(Span, Span, String)>,
}

impl<'ast> ridge_ast::visit::Visit<'ast> for FieldAccessFinder {
    fn visit_expr(&mut self, e: &'ast ridge_ast::Expr) {
        // Field name ranges are disjoint (a chain `a.b.c` has separate `b`/`c`
        // idents), so at most one covers the cursor — the first wins.
        if self.best.is_none() {
            if let ridge_ast::Expr::FieldAccess { base, field, .. } = e {
                if field.span.start <= self.offset && self.offset < field.span.end {
                    self.best = Some((base.span(), field.span, field.text.clone()));
                }
            }
        }
        ridge_ast::visit::walk_expr(self, e);
    }
}

/// Collects every `base.field` access whose field name equals `field`,
/// recording `(base span, field name span)` for later type-based filtering.
struct FieldSiteCollector {
    field: String,
    sites: Vec<(Span, Span)>,
}

impl<'ast> ridge_ast::visit::Visit<'ast> for FieldSiteCollector {
    fn visit_expr(&mut self, e: &'ast ridge_ast::Expr) {
        if let ridge_ast::Expr::FieldAccess { base, field, .. } = e {
            if field.text == self.field {
                self.sites.push((base.span(), field.span));
            }
        }
        ridge_ast::visit::walk_expr(self, e);
    }
}

/// Field names of a record type, resolving through aliases and nominal record
/// `TyCon`s. Empty for any non-record type. `tycons` is the index's declaration
/// table, looked up by id for nominal records (`Type::Con`).
fn record_field_names(ty: &Type, tycons: &[TyConDecl]) -> Vec<String> {
    match ty {
        Type::Record { fields, .. } => fields.iter().map(|(label, _)| label.clone()).collect(),
        Type::Alias { body, .. } => record_field_names(body, tycons),
        Type::Con(id, _) => tycons
            .iter()
            .find(|d| d.id.0 == id.0)
            .map(|d| match &d.kind {
                TyConKind::Record(schema) => schema
                    .record_fields()
                    .iter()
                    .map(|f| f.name.clone())
                    .collect(),
                TyConKind::Alias { body, .. } => record_field_names(body, tycons),
                _ => Vec::new(),
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Shape a completion candidate, grouping it by a leading sort digit.
fn item(label: String, kind: CompletionItemKind, group: char) -> CompletionItemData {
    CompletionItemData {
        sort_text: format!("{group}{label}"),
        label,
        kind,
        detail: None,
        data: None,
    }
}

/// A completion candidate for a workspace symbol, carrying the resolve payload
/// (`{ "uri", "name" }`) so `completionItem/resolve` can fill in its signature
/// and doc on demand. `owner_uri` is the module that declares the symbol.
fn symbol_item(
    name: String,
    kind: CompletionItemKind,
    group: char,
    owner_uri: Option<&Url>,
) -> CompletionItemData {
    let mut it = item(name, kind, group);
    if let Some(uri) = owner_uri {
        it.data = Some(serde_json::json!({ "uri": uri.as_str(), "name": it.label }));
    }
    it
}

/// Resolve a binding to the `(module, symbol)` of a top-level workspace
/// declaration whose written header can be rendered. `None` for locals, fields,
/// constructors, stdlib symbols, and class methods (which have no workspace
/// `fn`/`const`/`type`/`actor` declaration to read).
fn workspace_symbol_of(binding: Option<&Binding>) -> Option<(ModuleId, ridge_resolve::SymbolId)> {
    match binding? {
        Binding::ModuleSymbol { module, symbol }
        | Binding::ImportedSymbol { module, symbol, .. } => Some((*module, *symbol)),
        Binding::ActorName { module, actor } => Some((*module, *actor)),
        _ => None,
    }
}

/// Whether `outer` fully encloses `inner` — used to pick the top-level item that
/// owns a symbol's definition site.
const fn span_encloses(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Shrink `span` so it ends just past its last non-whitespace byte.
///
/// The parser closes a declaration span at the start of the *following* token,
/// so a raw span trails into the blank lines (and the next item's first line)
/// after the declaration. Trimming gives the span the editor expects — folding
/// and selection should stop at the declaration's own last line of content.
/// Ridge's whitespace is ASCII, so the trimmed end stays on a UTF-8 boundary.
fn trim_trailing_ws(text: &str, span: Span) -> Span {
    let bytes = text.as_bytes();
    let mut end = (span.end as usize).min(bytes.len());
    while end > span.start as usize && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    #[allow(clippy::cast_possible_truncation)]
    Span::new(span.start, end as u32)
}

/// Build a navigational code lens whose count and command are filled in lazily
/// by [`WorkspaceIndex::resolve_code_lens`]. `kind` is `"references"` or
/// `"implementations"`; the anchor (the document URI and the name position) is
/// carried in `data` so resolve can re-run the query for it.
fn nav_code_lens(uri: &Url, range: Range, kind: &str) -> CodeLens {
    CodeLens {
        range,
        command: None,
        data: Some(serde_json::json!({
            "kind": kind,
            "uri": uri.as_str(),
            "line": range.start.line,
            "character": range.start.character,
        })),
    }
}

/// Build an executable code lens carrying a ready client command — no resolve
/// round-trip, since the target is known up front.
fn command_code_lens(
    range: Range,
    title: &str,
    command: &str,
    arguments: Vec<serde_json::Value>,
) -> CodeLens {
    CodeLens {
        range,
        command: Some(Command {
            title: title.to_owned(),
            command: command.to_owned(),
            arguments: Some(arguments),
        }),
        data: None,
    }
}

/// The display name of the function's first `@test` attribute, if any.
fn test_display_name(attrs: &[ridge_ast::Attribute]) -> Option<&str> {
    attrs
        .iter()
        .map(|attr| match attr {
            ridge_ast::Attribute::Test { name, .. } => name.as_str(),
        })
        .next()
}

/// Render a code-lens count as `"1 reference"` / `"N references"`.
fn pluralize(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Decode a [`CallHierarchyItem`]'s `data` payload back to `(module, symbol)`.
///
/// The payload is the `{ "module", "symbol" }` object [`call_hierarchy_item`]
/// stamps; a missing or malformed field yields `None`.
fn decode_call_item(data: &serde_json::Value) -> Option<(ModuleId, ridge_resolve::SymbolId)> {
    let module = u32::try_from(data.get("module")?.as_u64()?).ok()?;
    let symbol = u32::try_from(data.get("symbol")?.as_u64()?).ok()?;
    Some((ModuleId(module), ridge_resolve::SymbolId(symbol)))
}

/// Decode a type-hierarchy item's `data` into its class name and whether the
/// item is an instance (vs a class).
fn decode_type_item(data: &serde_json::Value) -> Option<(String, bool)> {
    let name = data.get("name")?.as_str()?.to_owned();
    let is_instance = data.get("kind").and_then(serde_json::Value::as_str) == Some("instance");
    Some((name, is_instance))
}

/// Order type-hierarchy items by source location and drop exact duplicates.
fn sorted_dedup_items(mut items: Vec<TypeHierarchyItem>) -> Vec<TypeHierarchyItem> {
    items.sort_by(|a, b| {
        (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
            b.uri.as_str(),
            b.range.start.line,
            b.range.start.character,
        ))
    });
    items.dedup();
    items
}

/// The source span of a top-level item, covering its whole declaration.
const fn item_span(item: &ridge_ast::Item) -> Span {
    match item {
        ridge_ast::Item::Import(d) => d.span,
        ridge_ast::Item::Const(d) => d.span,
        ridge_ast::Item::Type(d) => d.span,
        ridge_ast::Item::Fn(d) => d.span,
        ridge_ast::Item::Actor(d) => d.span,
        ridge_ast::Item::ClassDecl(d) => d.span,
        ridge_ast::Item::InstanceDecl(d) => d.span,
    }
}

/// Wrap a declaration head in a Ridge-highlighted markdown code fence.
fn fenced_ridge(sig: &str) -> String {
    format!("```ridge\n{sig}\n```")
}

/// The visibility prefix (with a trailing space) for a declaration head.
const fn vis_prefix(vis: ridge_ast::Visibility) -> &'static str {
    match vis {
        ridge_ast::Visibility::Pub => "pub ",
        ridge_ast::Visibility::PubInternal => "pub(internal) ",
        ridge_ast::Visibility::Private => "",
    }
}

/// Capability keywords for a function head, space-separated in canonical order.
fn render_caps_slice(caps: &[ridge_ast::Capability]) -> String {
    use ridge_ast::Capability::{Db, Env, Ffi, Fs, Io, Net, Proc, Random, Spawn, Time};
    [
        (Io, "io"),
        (Fs, "fs"),
        (Net, "net"),
        (Time, "time"),
        (Random, "random"),
        (Env, "env"),
        (Proc, "proc"),
        (Spawn, "spawn"),
        (Ffi, "ffi"),
        (Db, "db"),
    ]
    .into_iter()
    .filter(|(cap, _)| caps.contains(cap))
    .map(|(_, kw)| kw)
    .collect::<Vec<_>>()
    .join(" ")
}

/// The written header of a function: `pub fn io name (a: T) (b: U) -> R`.
///
/// Capabilities and visibility are reconstructed; parameter and return-type
/// text is sliced from source so it reads exactly as written.
fn fn_header(text: &str, d: &ridge_ast::decl::FnDecl) -> String {
    let mut s = String::new();
    s.push_str(vis_prefix(d.vis));
    s.push_str("fn ");
    let caps = render_caps_slice(&d.caps);
    if !caps.is_empty() {
        s.push_str(&caps);
        s.push(' ');
    }
    s.push_str(&d.name.text);
    for p in &d.params {
        s.push(' ');
        s.push_str(slice_span(text, p.span()).trim());
    }
    if let Some(ret) = &d.ret {
        s.push_str(" -> ");
        s.push_str(slice_span(text, ret.span()).trim());
    }
    s
}

/// The written header of a const: `pub const NAME: T`.
fn const_header(text: &str, d: &ridge_ast::decl::ConstDecl) -> String {
    format!(
        "{}const {}: {}",
        vis_prefix(d.vis),
        d.name.text,
        slice_span(text, d.ty.span()).trim()
    )
}

/// The written head of a type declaration: `pub type Name a = body`, sliced from
/// the name through the end of the declaration so params, body, and any
/// `deriving` clause read exactly as written.
fn type_header(text: &str, d: &ridge_ast::decl::TypeDecl) -> String {
    let opaque = if d.opaque { "opaque " } else { "" };
    let tail = slice_span(text, Span::new(d.name.span.start, d.span.end));
    format!("{}{}type {}", vis_prefix(d.vis), opaque, tail.trim())
}

/// The doc-comment body, trimmed, or `None` when absent or empty.
fn doc_text(doc: Option<&ridge_ast::DocComment>) -> Option<String> {
    doc.map(|d| d.text.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Human-readable role prefix for a hovered binding (trailing space included).
///
/// Top-level fns/consts and anything unrecognised get no prefix — the rendered
/// type already carries the information.
const fn binding_label(binding: Option<&Binding>) -> &'static str {
    match binding {
        Some(Binding::Local(_)) => "(local) ",
        Some(Binding::Constructor { .. }) => "constructor ",
        Some(Binding::StdlibSymbol { .. }) => "(stdlib) ",
        Some(Binding::ClassMethod { .. }) => "(class method) ",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_resolve::{assign_node_ids, NodeKind};

    // Build a NodeSpatialIndex straight from source so tests exercise the real
    // stamping pass rather than hand-rolled spans.
    fn spatial_for(src: &str) -> NodeSpatialIndex {
        let parsed = ridge_parser::parse_source(src);
        let (map, _errors) = assign_node_ids(&parsed.module);
        NodeSpatialIndex::from_node_ids(&map)
    }

    fn byte_of(src: &str, needle: &str) -> u32 {
        u32::try_from(src.find(needle).expect("needle present")).expect("offset fits u32")
    }

    // ── semantic-token delta (diff_tokens) ────────────────────────────────────

    /// A token whose fields double as an identity marker, so a diff that
    /// preserves the wrong span is caught.
    fn tok(delta_line: u32, delta_start: u32, length: u32) -> SemanticToken {
        SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: 0,
            token_modifiers_bitset: 0,
        }
    }

    /// Apply a delta the way a client would: `start`/`delete_count` are flat
    /// integer offsets (five per token), so divide by five to splice tokens.
    fn apply(old: &[SemanticToken], edits: &[SemanticTokensEdit]) -> Vec<SemanticToken> {
        let mut out = old.to_vec();
        // A server emits at most one edit, but apply them right-to-left so
        // multiple edits would compose without index drift either way.
        for edit in edits.iter().rev() {
            let start = (edit.start / 5) as usize;
            let del = (edit.delete_count / 5) as usize;
            let data = edit.data.clone().unwrap_or_default();
            out.splice(start..start + del, data);
        }
        out
    }

    #[test]
    fn diff_identical_streams_have_no_edits() {
        let a = vec![tok(0, 0, 3), tok(0, 4, 2), tok(1, 0, 5)];
        assert!(diff_tokens(&a, &a).is_empty());
    }

    #[test]
    fn diff_pure_insert_in_the_middle() {
        let old = vec![tok(0, 0, 3), tok(1, 0, 5)];
        let new = vec![tok(0, 0, 3), tok(0, 4, 2), tok(1, 0, 5)];
        let edits = diff_tokens(&old, &new);
        assert_eq!(edits.len(), 1);
        // One token of shared prefix → start at flat offset 5, deleting nothing.
        assert_eq!(edits[0].start, 5);
        assert_eq!(edits[0].delete_count, 0);
        assert_eq!(apply(&old, &edits), new);
    }

    #[test]
    fn diff_pure_delete_leaves_no_insertion() {
        let old = vec![tok(0, 0, 3), tok(0, 4, 2), tok(1, 0, 5)];
        let new = vec![tok(0, 0, 3), tok(1, 0, 5)];
        let edits = diff_tokens(&old, &new);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 5);
        assert_eq!(edits[0].delete_count, 5);
        assert!(edits[0].data.is_none());
        assert_eq!(apply(&old, &edits), new);
    }

    #[test]
    fn diff_replaces_only_the_changed_band() {
        let old = vec![tok(0, 0, 3), tok(0, 4, 2), tok(0, 7, 4), tok(1, 0, 5)];
        // Middle two tokens change length; first and last are untouched.
        let new = vec![tok(0, 0, 3), tok(0, 4, 9), tok(0, 7, 1), tok(1, 0, 5)];
        let edits = diff_tokens(&old, &new);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 5); // one shared prefix token
        assert_eq!(edits[0].delete_count, 10); // two tokens replaced
        assert_eq!(apply(&old, &edits), new);
    }

    #[test]
    fn diff_handles_empty_endpoints() {
        let some = vec![tok(0, 0, 3), tok(0, 4, 2)];
        let empty: Vec<SemanticToken> = Vec::new();
        // Building a document from nothing.
        let to_full = diff_tokens(&empty, &some);
        assert_eq!(apply(&empty, &to_full), some);
        // Clearing it back out.
        let to_empty = diff_tokens(&some, &empty);
        assert_eq!(apply(&some, &to_empty), empty);
        assert!(diff_tokens(&empty, &empty).is_empty());
    }

    #[test]
    fn diff_always_roundtrips() {
        // A spread of prefix/suffix/middle shapes: applying the edit to `old`
        // must reproduce `new` byte-for-byte every time.
        let cases: &[(Vec<SemanticToken>, Vec<SemanticToken>)] = &[
            (vec![tok(0, 0, 1)], vec![tok(0, 0, 1), tok(0, 2, 1)]),
            (vec![tok(0, 0, 1), tok(0, 2, 1)], vec![tok(0, 0, 1)]),
            (
                vec![tok(0, 0, 1), tok(0, 2, 1), tok(0, 4, 1)],
                vec![tok(0, 0, 9), tok(0, 2, 1), tok(0, 4, 9)],
            ),
            (
                vec![tok(1, 0, 2), tok(2, 0, 2)],
                vec![tok(1, 0, 2), tok(1, 3, 1), tok(2, 0, 2)],
            ),
        ];
        for (old, new) in cases {
            let edits = diff_tokens(old, new);
            assert_eq!(
                &apply(old, &edits),
                new,
                "roundtrip failed for {old:?} -> {new:?}"
            );
        }
    }

    #[test]
    fn ident_offset_resolves_to_ident_kind() {
        let src = "fn foo x = x + 1\n";
        let idx = spatial_for(src);
        // The parameter use-site `x` in the body (the second `x`).
        let off = byte_of(src, "= x") + 2;
        let hit = idx.narrowest_containing(off, &[NodeKind::Ident, NodeKind::QualifiedName]);
        assert!(matches!(hit, Some((_, NodeKind::Ident, _))), "got {hit:?}");
    }

    #[test]
    fn expr_offset_resolves_to_expr_kind() {
        let src = "fn foo x = x + 1\n";
        let idx = spatial_for(src);
        // Inside the `x + 1` expression, on the `+`.
        let off = byte_of(src, "+");
        let hit = idx.narrowest_containing(off, &[NodeKind::Expr, NodeKind::Block, NodeKind::Type]);
        assert!(matches!(hit, Some((_, NodeKind::Expr, _))), "got {hit:?}");
    }

    #[test]
    fn whitespace_offset_resolves_to_nothing() {
        let src = "fn foo = 1\n";
        let idx = spatial_for(src);
        // The space immediately after `fn`.
        let off = byte_of(src, "fn") + 2;
        assert_eq!(idx.narrowest_containing(off, &[NodeKind::Ident]), None);
    }

    #[test]
    fn past_eof_offset_resolves_to_nothing() {
        let src = "fn foo = 1\n";
        let idx = spatial_for(src);
        let off = u32::try_from(src.len()).unwrap() + 50;
        assert_eq!(idx.narrowest_containing(off, &[NodeKind::Expr]), None);
        assert_eq!(idx.narrowest_containing(off, &[]), None);
    }

    #[test]
    fn nested_spans_return_narrowest() {
        let src = "fn foo = 1 + 2\n";
        let idx = spatial_for(src);
        // On the literal `2`: the narrowest Expr is the literal itself, not the
        // enclosing `1 + 2` binary expression.
        let off = byte_of(src, "2");
        let lit = idx
            .narrowest_containing(off, &[NodeKind::Expr])
            .expect("an expression covers the literal");
        // The whole `1 + 2` expression also covers this offset but is wider, so
        // it must not be the one returned: confirm the hit's span is the literal.
        let lit_span_width = idx
            .entries
            .iter()
            .find(|(_, _, id)| *id == lit.0)
            .map(|(span, _, _)| span.end - span.start)
            .expect("hit present in entries");
        assert_eq!(lit_span_width, 1, "expected the 1-byte literal `2`");
    }

    #[test]
    fn stdlib_export_kind_uses_initial_case() {
        // The builtin manifest carries names only; the icon is inferred from case.
        assert_eq!(stdlib_export_kind("filter"), CompletionItemKind::FUNCTION);
        assert_eq!(stdlib_export_kind("Query"), CompletionItemKind::CLASS);
    }

    #[test]
    fn stdlib_exports_out_of_range_is_empty() {
        assert!(stdlib_exports(StdlibModuleId(u32::MAX)).is_empty());
    }
}
