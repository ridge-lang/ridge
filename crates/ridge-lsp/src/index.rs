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
use ridge_resolve::imports::Binding;
use ridge_resolve::{ModuleId, NodeId, NodeIdMap, NodeKind, ResolvedWorkspace};
use ridge_typecheck::{render_type_with, TypedWorkspace};
use tower_lsp::lsp_types::{Position, Range, Url};

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
}

/// The full analysis result the server keeps between compiles.
///
/// Immutable once built; replaced wholesale by a newer generation.
#[derive(Debug)]
pub struct WorkspaceIndex {
    /// Monotonic compile generation. A later compile carries a strictly higher
    /// generation, which the install guard uses to reject a stale result.
    pub generation: u64,
    /// The fully type-checked workspace (per-module `node_types`, schemes, …).
    pub typed: TypedWorkspace,
    /// The resolved workspace (per-module symbols, bindings, node-id maps, and
    /// the workspace graph).
    pub resolved: ResolvedWorkspace,
    /// Document URI → [`ModuleId`]. Keyed the same way diagnostics are published
    /// (workspace root joined with the source id), so an editor-sent URI matches.
    pub uri_to_module: HashMap<Url, ModuleId>,
    /// Per-module spatial index, indexed by `ModuleId.0`.
    pub spatial: Vec<NodeSpatialIndex>,
    /// Per-module UTF-16 ↔ byte line index, indexed by `ModuleId.0`.
    pub line_indices: Vec<LineIndex>,
    /// Per-module source text the spans index into, indexed by `ModuleId.0`.
    pub module_text: Vec<Arc<str>>,
}

impl WorkspaceIndex {
    /// Build an index from a completed compile.
    ///
    /// `generation` must be the compile's generation stamp (see
    /// [`WorkspaceIndex::generation`]).
    #[must_use]
    pub fn build(
        generation: u64,
        typed: TypedWorkspace,
        resolved: ResolvedWorkspace,
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
            uri_to_module.insert(uri, module.id);
            if let Some(text) = sources.text(source_id.as_str()) {
                module_text[i] = Arc::from(text);
                line_indices[i] = LineIndex::new(text);
            }
        }

        // `resolved.modules` is indexed by `ModuleId.0`, so the spatial vec built
        // in iteration order is addressable by `ModuleId.0` too.
        let mut spatial: Vec<NodeSpatialIndex> = Vec::with_capacity(n);
        for (i, rm) in resolved.modules.iter().enumerate() {
            debug_assert_eq!(
                rm.id.0 as usize, i,
                "resolved.modules must be indexed by ModuleId.0"
            );
            spatial.push(NodeSpatialIndex::from_node_ids(&rm.node_ids));
        }

        Self {
            generation,
            typed,
            resolved,
            uri_to_module,
            spatial,
            line_indices,
            module_text,
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
            .typed
            .modules
            .get(mi)?
            .node_types
            .get(type_node.0 as usize)?
            .as_ref()?;
        if matches!(ty, ridge_types::Type::Error) {
            return None;
        }
        let type_str = render_type_with(ty, &self.typed.tycons);

        // If an identifier covers the same offset, prefix with its role + name
        // and underline the identifier; otherwise this is a literal/expression
        // and we show the bare type over the expression span.
        if let Some((_, id_node, _, id_span)) =
            self.node_at(uri, offset, &[NodeKind::Ident, NodeKind::QualifiedName])
        {
            let name = self.text_slice(mi, id_span);
            let binding = self
                .resolved
                .modules
                .get(mi)
                .and_then(|rm| rm.bindings.get(id_node.0 as usize))
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
}
