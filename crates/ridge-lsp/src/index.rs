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
    LocalId, ModuleId, NodeId, NodeIdMap, NodeKind, ResolvedVisibility, ResolvedWorkspace,
    ScopeIndex, StdlibModuleId, SymbolKind, SymbolTable, BUILTINS,
};
use ridge_typecheck::{render_type_with, TypeError, TypedWorkspace};
use ridge_types::{CapabilitySet, TyConDecl, TyConKind, Type};
use tower_lsp::lsp_types::{
    CompletionItemKind, DocumentHighlight, DocumentHighlightKind, DocumentSymbol, InlayHint,
    InlayHintKind, InlayHintLabel, Location, ParameterInformation, ParameterLabel, Position,
    PrepareRenameResponse, Range, SignatureHelp, SignatureInformation, SymbolInformation,
    SymbolKind as LspSymbolKind, TextEdit, Url, WorkspaceEdit,
};

use crate::completion::{detect_context, symbol_kind, CompletionItemData, Context, KEYWORDS};
use crate::diagnostics::source_id_to_uri;

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
    /// Per-module spatial index, indexed by `ModuleId.0`.
    pub spatial: Vec<NodeSpatialIndex>,
    /// Per-module UTF-16 ↔ byte line index, indexed by `ModuleId.0`.
    pub line_indices: Vec<LineIndex>,
    /// Per-module source text the spans index into, indexed by `ModuleId.0`.
    pub module_text: Vec<Arc<str>>,
    /// Per-module document URI, indexed by `ModuleId.0` (for cross-file
    /// go-to-definition targets). `None` if the path had no valid URI.
    pub module_uris: Vec<Option<Url>>,
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
        // All per-module vecs are addressed by `ModuleId.0`; pre-size and fill by
        // id so iteration order over `graph.modules` (sorted by name) doesn't
        // matter.
        let mut module_text: Vec<Arc<str>> = vec![Arc::from(""); n];
        let mut line_indices: Vec<LineIndex> = (0..n).map(|_| LineIndex::new("")).collect();
        let mut module_uris: Vec<Option<Url>> = vec![None; n];

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
            module_uris[i] = Some(uri);
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
            spatial,
            line_indices,
            module_text,
            module_uris,
            capability_fixes: Vec::new(),
        }
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
        let mid = *self.uri_to_module.get(uri)?;
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
        let mid = *self.uri_to_module.get(uri)?;
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

        // If an identifier covers the same offset, prefix with its role + name
        // and underline the identifier; otherwise this is a literal/expression
        // and we show the bare type over the expression span.
        if let Some((_, id_node, _, id_span)) =
            self.node_at(uri, offset, &[NodeKind::Ident, NodeKind::QualifiedName])
        {
            let name = self.text_slice(mi, id_span);
            let binding = self
                .modules
                .get(mi)
                .and_then(|m| m.bindings.get(id_node.0 as usize))
                .and_then(Option::as_ref);
            let label = binding_label(binding);
            Some((format!("{label}{name} : {type_str}"), id_span))
        } else {
            Some((type_str, expr_span))
        }
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
        let mid = *self.uri_to_module.get(uri)?;
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
        let mid = *self.uri_to_module.get(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

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
            // Field accessors and errors have no resolvable definition site.
            _ => None,
        }
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
    pub fn signature_help_at(&self, uri: &Url, line: u32, utf16_col: u32) -> Option<SignatureHelp> {
        let mid = *self.uri_to_module.get(uri)?;
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
        Some(make_signature_help(sig, active))
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
    ) -> Option<Vec<Location>> {
        let mid = *self.uri_to_module.get(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);

        // The binding under the cursor — same lookup as go-to-definition.
        let bindings = &self.modules.get(mi)?.bindings;
        let binding = self
            .spatial
            .get(mi)?
            .enclosing(offset, &[NodeKind::Ident, NodeKind::QualifiedName])
            .into_iter()
            .find_map(|(nid, _, _)| bindings.get(nid.0 as usize).and_then(Option::as_ref))?;
        let target = referent_key(binding, mid)?;

        // Locals never escape their module, so a local search stays in the
        // cursor's module. Everything else can be referenced from any module
        // that imports it, so scan the whole workspace.
        let scan_self_only = matches!(target, ReferentKey::Local(..));

        let mut locations: Vec<Location> = Vec::new();
        for (smi, view) in self.modules.iter().enumerate() {
            if scan_self_only && smi != mi {
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
                if referent_key(b, smid).as_ref() != Some(&target) {
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
        if let Some(def) = self.referent_def_location(&target) {
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
        let mid = *self.uri_to_module.get(uri)?;
        let mi = mid.0 as usize;
        let offset = self.line_indices.get(mi)?.utf16_to_byte(line, utf16_col);
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
    /// state field), a top-level `fn` / `const`, and a `type` (renaming a record
    /// type also rewrites its `User { .. }` constructions and patterns). Not yet
    /// renameable (returns `None`): a union constructor, an actor, a field
    /// accessor, a stdlib symbol, a class method, or a module alias — see the
    /// deferred follow-ups. Reads only this immutable snapshot.
    #[must_use]
    pub fn prepare_rename_at(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
    ) -> Option<PrepareRenameResponse> {
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
    ) -> Result<Option<WorkspaceEdit>, String> {
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
        let mid = *self.uri_to_module.get(uri)?;
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
        let mid = *self.uri_to_module.get(uri)?;
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
    pub fn workspace_symbols(&self, query: &str) -> Vec<SymbolInformation> {
        let needle = query.to_lowercase();
        let mut out: Vec<SymbolInformation> = Vec::new();
        for view in &self.modules {
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
        let mid = *self.uri_to_module.get(uri)?;
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

    fn try_completions(
        &self,
        uri: &Url,
        line: u32,
        utf16_col: u32,
    ) -> Option<Vec<CompletionItemData>> {
        let mid = *self.uri_to_module.get(uri)?;
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
                    if let Some(tm) = self.modules.get(target.0 as usize) {
                        for e in &tm.symbols.entries {
                            if e.visibility == ResolvedVisibility::Pub
                                && e.name.starts_with(&prefix)
                            {
                                out.push(item(e.name.clone(), symbol_kind(&e.kind), '0'));
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
                for e in &m.symbols.entries {
                    if e.name.starts_with(&prefix) {
                        out.push(item(e.name.clone(), symbol_kind(&e.kind), '1'));
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
fn make_signature_help(sig: SignatureSig, active: u32) -> SignatureHelp {
    let parameters = sig
        .params
        .iter()
        .map(|&offsets| ParameterInformation {
            label: ParameterLabel::LabelOffsets(offsets),
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
    }
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
