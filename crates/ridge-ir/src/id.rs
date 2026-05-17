//! IR-level node identifier.

// OQ-IR004: IrNodeId is u32 (dense, fits in Vec index; 4 billion nodes per module is sufficient).
/// IR-level node identifier — stable across lowering, distinct from `NodeId`.
///
/// Each `IrNodeId` is dense and per-module; lowering assigns them sequentially
/// as nodes are emitted.  The Phase 3 AST-side `NodeId` is *not* reused here
/// (see D079): a lowering rule can synthesise IR nodes that correspond to no
/// AST node (e.g. the `ToText` call inserted by interpolation lowering), and
/// several IR nodes can collapse from one AST node (e.g. `try` lowers to
/// multiple `Match` nodes). `IrNodeId` decouples the IR numbering from the
/// AST numbering. The original AST `NodeId` is preserved on `IrExpr::origin`
/// for diagnostic / source-map purposes (D079).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IrNodeId(pub u32);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn ir_node_id_equality() {
        let a = IrNodeId(0);
        let b = IrNodeId(0);
        let c = IrNodeId(1);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn ir_node_id_copy() {
        let a = IrNodeId(42);
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn ir_node_id_hash_in_map() {
        let mut map = HashMap::new();
        map.insert(IrNodeId(0), "zero");
        map.insert(IrNodeId(1), "one");
        assert_eq!(map[&IrNodeId(0)], "zero");
        assert_eq!(map[&IrNodeId(1)], "one");
    }

    #[test]
    fn ir_node_id_density_as_vec_index() {
        // The contract: IrNodeId.0 can be used as a Vec index directly.
        let mut node_types: Vec<Option<&str>> = Vec::new();
        for i in 0u32..5 {
            node_types.push(Some("Type"));
            let id = IrNodeId(i);
            assert!(node_types.get(id.0 as usize).is_some());
        }
    }
}
