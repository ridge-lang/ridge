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

use ridge_lexer::Span;
use ridge_resolve::{ModuleId, NodeId, NodeIdMap, NodeKind, ResolvedWorkspace};
use ridge_typecheck::TypedWorkspace;
use tower_lsp::lsp_types::Url;

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
    ) -> Option<(NodeId, NodeKind)> {
        self.entries
            .iter()
            .filter(|(span, kind, _)| {
                span.start <= offset
                    && offset < span.end
                    && (prefer.is_empty() || prefer.contains(kind))
            })
            .min_by_key(|(span, _, _)| span.end - span.start)
            .map(|&(_, kind, id)| (id, kind))
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
    /// Document URI → [`ModuleId`], derived from each module's `file_path`.
    pub uri_to_module: HashMap<Url, ModuleId>,
    /// Per-module spatial index, indexed by `ModuleId.0`.
    pub spatial: Vec<NodeSpatialIndex>,
}

impl WorkspaceIndex {
    /// Build an index from a completed compile.
    ///
    /// `generation` must be the compile's generation stamp (see
    /// [`WorkspaceIndex::generation`]).
    #[must_use]
    pub fn build(generation: u64, typed: TypedWorkspace, resolved: ResolvedWorkspace) -> Self {
        let mut uri_to_module: HashMap<Url, ModuleId> = HashMap::new();
        for module in &resolved.graph.modules {
            if let Ok(uri) = Url::from_file_path(&module.file_path) {
                uri_to_module.insert(uri, module.id);
            }
        }

        // `resolved.modules` is indexed by `ModuleId.0`, so the spatial vec
        // built in iteration order is addressable by `ModuleId.0` too.
        let mut spatial: Vec<NodeSpatialIndex> = Vec::with_capacity(resolved.modules.len());
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
    ) -> Option<(ModuleId, NodeId, NodeKind)> {
        let mid = *self.uri_to_module.get(uri)?;
        let spatial = self.spatial.get(mid.0 as usize)?;
        let (nid, kind) = spatial.narrowest_containing(offset, prefer)?;
        Some((mid, nid, kind))
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
        assert!(matches!(hit, Some((_, NodeKind::Ident))), "got {hit:?}");
    }

    #[test]
    fn expr_offset_resolves_to_expr_kind() {
        let src = "fn foo x = x + 1\n";
        let idx = spatial_for(src);
        // Inside the `x + 1` expression, on the `+`.
        let off = byte_of(src, "+");
        let hit = idx.narrowest_containing(off, &[NodeKind::Expr, NodeKind::Block, NodeKind::Type]);
        assert!(matches!(hit, Some((_, NodeKind::Expr))), "got {hit:?}");
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
