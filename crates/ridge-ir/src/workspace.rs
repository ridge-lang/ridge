//! Workspace-level and module-level lowered IR containers.

use crate::id::IrNodeId;
use crate::item::IrItem;
use ridge_resolve::{ModuleId, NodeId};
use ridge_types::Type;
use rustc_hash::FxHashMap;

// OQ-L001: LoweredWorkspace stores modules as a Vec (parallel-value indexed by ModuleId.0),
// not a HashMap; tycon_count is a plain field, not a method, for zero-cost access.
/// The workspace-level lowered IR.  Indexed by `ModuleId`.
#[derive(Debug)]
#[non_exhaustive]
pub struct LoweredWorkspace {
    /// One entry per module, indexed by `ModuleId.0`.  May be `None` if the
    /// module's typecheck produced errors and lowering was skipped.
    pub modules: Vec<Option<LoweredModule>>,
    /// Re-exported reference into `TypedWorkspace.tycons`.  Phase 6 reads both.
    pub tycon_count: u32,
}

impl LoweredWorkspace {
    /// Construct a fresh empty workspace with `module_count` empty module slots.
    ///
    /// Used by `ridge-lower` stubs and tests that need a default-empty workspace
    /// without populating individual modules.
    #[must_use]
    pub fn empty(module_count: usize, tycon_count: u32) -> Self {
        Self {
            modules: (0..module_count).map(|_| None).collect(),
            tycon_count,
        }
    }

    /// Construct from explicit slots (used by `ridge-lower::lower_workspace`).
    ///
    /// `modules` may contain `None` for any module slot that was skipped due
    /// to typecheck errors.
    #[must_use]
    pub const fn new(modules: Vec<Option<LoweredModule>>, tycon_count: u32) -> Self {
        Self {
            modules,
            tycon_count,
        }
    }
}

/// One module's Core IR.
#[derive(Debug)]
#[non_exhaustive]
pub struct LoweredModule {
    /// The module's stable index.
    pub id: ModuleId,
    /// The lowered top-level items.
    pub items: Vec<IrItem>,
    /// Per-`IrNodeId` type side-table.  Indexed by `IrNodeId.0`; `None` for
    /// statement-only nodes (e.g. an `Assign` whose value is `Unit`).
    /// Mirrors `TypedModule.node_types` but on the IR numbering scheme.
    pub node_types: Vec<Option<Type>>,
    // OQ-L005: per-IR-node source map lives here (IrNodeId → NodeId); sparse because
    // synthesised nodes (e.g. ToText calls from interpolation) have no AST origin.
    /// AST → IR node provenance map.  Each entry: `IrNodeId -> NodeId`.
    /// Sparse — synthesised IR nodes (e.g. interpolation-emitted `ToText`
    /// calls) have no upstream `NodeId` and are absent from this map.
    pub source_map: FxHashMap<IrNodeId, NodeId>,
}

impl LoweredModule {
    /// Construct an empty module shell (T2 scaffold; T3+ will populate `items`).
    ///
    /// `node_type_capacity` sets the length of the `node_types` vec (all `None`),
    /// matching the upstream `TypedModule.node_types` length so index parity is
    /// maintained when T3+ fills in types.
    #[must_use]
    pub fn empty(id: ModuleId, node_type_capacity: usize) -> Self {
        Self {
            id,
            items: Vec::new(),
            node_types: vec![None; node_type_capacity],
            source_map: FxHashMap::default(),
        }
    }

    /// Construct from explicit fields.
    ///
    /// Used by `ridge-lower::lower_module` in T3+ when items and the source map
    /// are fully populated.
    #[must_use]
    pub const fn new(
        id: ModuleId,
        items: Vec<IrItem>,
        node_types: Vec<Option<Type>>,
        source_map: FxHashMap<IrNodeId, NodeId>,
    ) -> Self {
        Self {
            id,
            items,
            node_types,
            source_map,
        }
    }
}
