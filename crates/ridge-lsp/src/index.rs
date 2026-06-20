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
    ScopeIndex, StdlibModuleId, SymbolTable, BUILTINS,
};
use ridge_typecheck::{render_type_with, TypedWorkspace};
use ridge_types::{TyConDecl, TyConKind, Type};
use tower_lsp::lsp_types::{CompletionItemKind, Location, Position, Range, Url};

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
            let node_types = typed
                .modules
                .get(i)
                .map(|tm| tm.node_types.clone())
                .unwrap_or_default();
            modules.push(ModuleView {
                node_types,
                bindings: rm.bindings.clone(),
                symbols: rm.symbols.clone(),
                scopes: rm.scopes.clone(),
                imports: rm.imports.clone(),
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
    /// literal, or an unresolved name. A stdlib symbol or stdlib module alias
    /// resolves into the materialised stdlib source (see [`crate::stdlib_defs`]).
    /// Reads only this immutable snapshot — never triggers a compile.
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
            // Field accessors, class methods, and errors have no resolvable
            // definition site here.
            _ => None,
        }
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
